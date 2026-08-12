# AURA — Phase 3 Plan: plugin hosting zones & frozen surface

Status: authoritative for the phase-3 build round. Owned by the architect.
Companion to `ARCHITECTURE.md` (esp. §15) and `SCALABILITY.md` (§2, debts
D-11/D-12). Zone/briefing conventions follow `PHASE2-PLAN.md`.

The scaffold is green: `cargo check` warning-free, `cargo test` = **180
lib tests passing** (157 phase-2 + 23 phase-3 seam/plugin/review-fix tests;
plus the 2 env-gated `tests/real_models.rs` integration tests), `npm run
build` + `svelte-check` clean. Every zone's definition of done includes all
of these staying green.

## 0. What the architect round already built (do not rebuild)

* **The live instrument node seam — REAL and tested.** `dsp::LiveInstrument`
  (`queue_event`/`all_notes_off` + `AudioProcessor`), `rt::LiveNodeCell`
  (nodes shared across RCU snapshots — voice/plugin state survives
  rebuilds), `RtGraph.scratch` (preallocated live-render scratch),
  `mixer::render_live` (loop-aware runs, discontinuity handling),
  `midi::playback::LiveNodeRegistry` + rewritten `append_from` (live tracks,
  no pre-render), engine discontinuity detection (`OutputCb.next_pos`).
  PolySynth and SamplerNode are live in-graph instruments TODAY; plugins are
  the third node kind behind the very same seam.
* **Crates decided & compiling** (frozen `Cargo.toml`): `clack-host` +
  `clack-extensions` 0.1.x (CLAP 1.2), `livi` 0.7 (+ system `liblilv-dev`),
  `walkdir`. Verified on the dev machine: clack reads `/usr/lib/clap/*.clap`
  descriptors; **ZynAddSubFX instantiates and renders audio through livi**.
* **Discovery + registry + commands + schemas**: `plugins::scan` (LV2 world
  + CLAP paths), `PluginRegistry` (instances, stub params),
  `plugin_scan/list/instantiate/remove/get_params/set_param` registered,
  `plugin-descriptor.schema.json` / `plugin-state.schema.json`,
  `set_track_instrument` accepts `plugin:<instanceId>` (stub node = silence,
  PolySynth fallback for unknown refs).

## 1. Zones

| Zone | Feature | Owns (exclusive write access) |
|------|---------|-------------------------------|
| **P1 — CLAP host** | Real CLAP instantiation + RT processing + plugin main thread + out-of-process scanner | `src-tauri/src/plugins/host.rs`, new `plugins/clap_host.rs`, new `plugins/scan_worker.rs` (+ scan.rs's CLAP half) |
| **P2 — LV2 host** | Real LV2 instantiation + RT processing incl. **ZynAddSubFX acceptance** | new `src-tauri/src/plugins/lv2_host.rs` (+ scan.rs's LV2 half) |
| **P3 — Plugin UI** | Plugin browser + generic param UI + instance management in `/src` | `src/` (all frontend), following PHASE2-PLAN §5 store conventions |
| **P4 — State & automation groundwork** | Plugin state persistence (project v2 `plugins[]`, chunked `.state` blobs), param snapshot restore, automation-schema groundwork | `src-tauri/src/plugins/state.rs` (new), `midi/persist.rs` additions for `plugins[]`, LV2 raw-lilv state on livi handles |

Shared-but-guarded (architect arbitrates; itemized edits only):

* `plugins/mod.rs` — P1/P2 flip `instantiate` from stub to real through the
  plugin-main-thread handle; P4 adds state fields. Coordinate via small,
  named diffs; command signatures stay.
* `midi/playback.rs::node_for_track` — P1/P2 replace the
  `plugins::live_node_for` stub resolution result, nothing else.
* `audio/` and `midi/` cores are FROZEN for this round except the two named
  seams above. The `LiveInstrument`/`LiveNodeCell` contracts are frozen.

**FROZEN files** (unchanged list + this round's additions): `lib.rs`,
`Cargo.toml` (crates are in; new ones by request), `tauri.conf.json`,
`capabilities/default.json`, `build.rs`, `package.json`, `vite.config.ts`,
`svelte.config.js`, `tsconfig.json`, `index.html`, everything in `docs/`,
`src-tauri/src/mcp/` and `src-tauri/src/sidecars/jobs.rs` (separate
hardening agent), `/sidecars/*.py` (off-repo verification agent).

## 2. Cross-zone contracts (fixed NOW)

1. **`LiveInstrument` is the only way a plugin makes sound.** P1/P2 nodes
   implement it; they receive `BlockNoteEvent`s (velocity 0 = off) and ADD
   into the zeroed stereo scratch. §2.1 RT rules absolute: preallocate
   everything in construction/`prepare` on the plugin main thread.
2. **Plugin main thread.** One std thread (pattern-copy `engine::start`:
   crossbeam channel + `Reply` oneshots) owns `livi::World` + all CLAP
   entries/instances. P1 builds it (`host.rs`); P2 registers its LV2 world
   into the same thread — never a second FFI thread. All `plugin_*`
   commands stay thin wrappers posting requests.
3. **`plugins::live_node_for(instance_id, rate) -> Option<Box<dyn
   LiveInstrument>>`** stays the engine-facing resolution hook (signature
   frozen). It may block briefly on the plugin main thread (it runs on the
   engine control thread during rebuild, never RT).
4. **Instance `status` lifecycle**: `"stub"` → `"active"` (P1/P2) →
   `"crashed"` reserved. UI (P3) renders all three.
5. **Param bridge**: `plugin_get_params` returns real `ParamInfo` lists
   after activation (CLAP params ext / LV2 control ports; the LV2 param id
   = control-port index). `plugin_set_param` forwards through a wait-free
   ring to the RT node (pattern: `sampler_engine::NoteCmd`) AND mirrors into
   the registry. P3 builds UI purely against these two commands.
6. **State refs** (P4): project v2 gains additive `plugins: [persistedInstance]`
   (`plugin-state.schema.json#/$defs/persistedInstance`); blobs under
   `<project>/plugins/<instanceId>.state`. P1/P2 expose `save_state/load_state`
   on the plugin main thread; P4 wires persistence + restore-on-open.
7. **Zyn acceptance test (P2, gates the round)**: with
   `zynaddsubfx-lv2` installed (apt, universe) — skip cleanly when absent —
   a midi track bound to `plugin:<zyn-instance>` renders **non-silent audio
   through `mixer::render` headlessly**, and voices stop after
   `all_notes_off`. Same shape as
   `playback::tests::instrument_tracks_use_sampler...`.
8. **Scanner subprocess (P1, pays D-11's scan half)**: `scan_worker.rs`
   re-executes the current binary with `AURA_SCAN_WORKER=1` (guard at the
   top of `main`), speaks NDJSON descriptors over stdout, dies on bad
   bundles without taking AURA down. `scan::scan_clap_paths` becomes the
   in-worker body; `plugin_scan` consumes the worker.

## 3. Per-zone briefs (condensed)

* **P1 (CLAP)**: plugin main thread; `PluginInstance::new/activate` via
  clack; audio-ports/note-ports negotiation (stereo out required, reject
  others politely for v1); `StartedPluginAudioProcessor` moved into
  `ClapNode: LiveInstrument`; params ext (list, flush, set via ring);
  sample-rate/block-size from engine (`prepare`; re-activate on rate
  change — registry key already carries the rate); latency ext read and
  logged (PDC lands with the graph compiler, not this round). Scanner
  subprocess (contract 8). Tests: headless render of an installed CLAP
  instrument if present (zam-plugins ships effects only — use a test
  fixture or skip), param round-trip, scanner-worker crash containment.
* **P2 (LV2)**: LV2 world on the plugin main thread;
  `plugin.instantiate(features, rate)`; `Lv2Node: LiveInstrument` with
  preallocated `LV2AtomSequence` in/out + port connections per block
  (exactly the verified smoke-test shape: atom in, atom out, 2 audio outs);
  `WorkerManager::run_workers` driven from the plugin main thread on a
  timer, NOT the RT thread; control-port params (index = ParamId). **Zyn
  acceptance test (contract 7).** Effects with audio inputs: out of scope
  (instrument roster only).
* **P3 (UI)**: plugin browser (scan/list, instrument filter, format badge),
  instance lifecycle UI, generic param panel (sliders from `ParamInfo`,
  batched drags → one `plugin_set_param` per rAF, transient-coalescing rule
  from SCALABILITY §5), track instrument picker extension (sampler ids +
  plugin instances), status badges (stub/active/crashed). Delta-ready keyed
  stores per PHASE2-PLAN §5; no per-frame full-list iteration.
* **P4 (state/automation groundwork)**: `persistedInstance` in project v2
  save/load (additive; `midi/persist.rs` value-round-trip pattern), state
  blob save on project save + restore on open (through P1/P2's
  `save_state/load_state`), param-snapshot fallback, raw-lilv
  `state:interface` for LV2 (Zyn patches!), and the automation schema
  draft (`automation` array of §3 with `targetNode = plugin instance id`) —
  schema + persistence only, no engine automation this round.

## 4. Sequencing / dependencies

P1 and P2 are independent after agreeing (day 1) on the plugin-main-thread
request enum (P1 owns the file; P2 sends a small PR-style diff). P3 needs
only the already-frozen commands (works against stubs from day 1). P4
depends on P1/P2's `save_state/load_state` late in the round; everything
else in P4 is schema/persistence work against the stub registry.

## 5. Verification gate (every zone)

```
cd src-tauri && cargo check && cargo test     # 180 lib + yours, all green
cd .. && npm run build && npx svelte-check    # green (P3 especially)
# P2 additionally, when zynaddsubfx-lv2 is installed:
cargo test lv2 -- --nocapture                 # Zyn acceptance not skipped
```

## 6. Deferred / noted (NOT this round)

* MCP tools for plugins (`list_plugins`, `set_plugin_param`) — roster
  frozen; candidates recorded here for the next MCP round.
* Out-of-process plugin PROCESSING bridge (D-11 second half), VST3, plugin
  effects on audio tracks (needs insert chains), X11 UI embedding, PDC.
* Time-signature map (D-02 remainder), op-log protocol (D-03 remainder).
