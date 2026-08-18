# G1 Tasks 1–6 — insert FX channel, host, mixer strip, and PDC primitive

Landed 2026-08-18. **Do not restart.** Next implementer starts at
**Task 7** (rebuild / offline-bounce wiring) in
`docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`. **Read the
two rulings-needed bullets under "Before Task 7" below first** — Task 6
deliberately left both open as decisions, not oversights.

| Slice | Pointer |
|---|---|
| Task 1 `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Tasks 2–4 ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23` |
| Task 5 mixer strip: source-sum → inserts REPLACE → shared fader | PR #66 `c99293b` |
| Task 6 PDC primitive: `DelayLine` + `compile_pdc` + `track_latency` | `5615dc4` (branch `feat/g1-pdc`) |

What exists now: document slots, four insert ops + undo, Tauri
`insert_add` / `insert_remove` / `insert_reorder` / `insert_set_bypass`,
effect instantiate gate (G-6/G-7), `HostRole::Effect` + Replace IO +
`latency_samples()`, restore via insert membership (undo / journal
replay / project-open), and the unified RT mixer strip
(`audio/insert.rs`: `InsertNode`/`InsertNodeCell`/`InsertNodeRegistry`/
`compile_inserts`; `audio/mixer.rs`: `render_impl` and
`render_live_input_only` walk clips → live → inserts REPLACE → PDC →
the one shared `apply_fader`). `compile_inserts` does not instantiate
CLAP/LV2 and is not yet wired into `engine::rebuild` — that plus real
hosting is Task 7. Bypass is true bypass (skips `process()`).

PDC (Task 6, `audio/pdc.rs`) is now a real, tested primitive:
`DelayLine` (RT-safe fixed-length ring buffer, sized once, never grows),
`track_latency` (instrument + Σ insert latencies), and `compile_pdc`
(`max - each`, per track). `RtTrack.pdc` is `Option<DelayLine>`;
`mixer::process_inserts` calls `d.process(buf)` when `Some`, after the
insert chain and before the fader. **Nothing populates `pdc` in
production code yet** — no `attach_inserts_and_pdc`, no `compile_pdc`
call from `engine::rebuild` or `offline.rs`. Every graph today builds
`RtTrack`s with `pdc: None` (see `RtTrack::clips`), so this is exactly
as inert to the shipped mix as the Task-5 placeholder was. Wiring it up
for real is Task 7.

What does **not** exist yet: rebuild compile (wiring `compile_inserts`
*and* `compile_pdc` into `engine::rebuild`), offline-bounce compile,
frontend IPC types, insert-chain UI. Adding an insert today writes the
document and hosts the plugin, and Task 5's strip *would* run it
through the chain — but nothing calls `compile_inserts` from
production code yet, so **the mix still does not hear it** until
Task 7 wires rebuild through.

## Before Task 7 (two rulings from the Task 6 final review — read first)

- **PDC state does not survive a rebuild.** `pdc: Option<DelayLine>` is
  a plain owned field on `RtTrack`, so every rebuild (there are 26
  `effect.rebuild = true` call sites in `src-tauri/src/control/
  session.rs` — clip add/move/trim, track add/remove, automation edits)
  constructs a fresh, zeroed `DelayLine`. Once Task 7 populates `pdc`
  from real per-track latencies, any rebuild during playback punches a
  silent gap of up to `delay` samples into every PDC-compensated track.
  `InsertNode` already solves this for insert state via
  `Arc<InsertNodeCell>` (state survives RCU graph swaps); PDC has no
  equivalent yet. Before Task 7 wires `attach_inserts_and_pdc`, decide
  whether PDC delay lines need the same cell+registry treatment (keyed
  by `TrackId`) — and if the answer is "not for G1", record that as an
  explicit ruling instead of an oversight.
- **Metronome, count-in, and offline bounce are not latency-aligned.**
  `engine.rs`'s metronome mix-in (~line 702) and count-in mix (~line
  645) both sit at `base`, positioned relative to an *un*compensated
  timeline; once PDC delays a track, they'll drift ahead of the music by
  up to `max_latency`. `offline.rs`'s render (~line 234) emits exactly
  the requested frame range with no pre-roll/post-roll, so a bounce of a
  PDC-compensated project would open with up to `max_latency` samples of
  silence (cold delay lines) and lose the same off the tail.
  `docs/SCALABILITY.md` (~line 115-125) already names the missing
  mechanism ("sources are read ahead by the path latency"). Task 7 needs
  to either implement that read-ahead or record an explicit ruling that
  these paths stay uncompensated for G1.

## Future work (plan order)

7. Rebuild / offline bounce / latency-change recompile — wires
   `compile_inserts` *and* `compile_pdc` into `engine::rebuild` for
   real; this is where an insert (and PDC) first becomes audible
   end-to-end. See the two rulings above before starting.
8. IPC types, backend adapter, plugin-binding.
9. `InsertChain.svelte`.
10. Handoff + dated counts.

## Task 5 deferred minors (do not block Task 7)

From `/code-review high` on PR #66. Parked, not load-bearing:

- `InsertNodeCell`/`InsertNodeRegistry` duplicate `LiveNodeCell`
  (`audio/rt.rs`) / `LiveNodeRegistry` (`midi/playback.rs`) near-verbatim
  — the plan's own interface spec calls for this shape, but a shared
  generic (`RtCell<T>`, `NodeRegistry<T>`) is a clean follow-up once a
  third RCU-cell consumer shows up.
- `process_inserts` runs the full insert chain unconditionally on
  muted/solo-excluded tracks (the `on` flag is only applied afterward
  in `apply_fader`) — plausibly deliberate (keeps plugin tails warm for
  click-free unmute) but undocumented either way; confirm the intent
  before "optimizing" it away.
- `prime_live` / `render_live_into` / `live_all_notes_off` each repeat
  the same `let Some(live) = &tr.live else { return }` + `unsafe {
  live.node.rt_mut() }` boilerplate — a shared accessor would cut it to
  one place.

G2 bus+sends / G3 sidechain / G4 envelope-follower wait. **PDC before
sends.** No stock FX. `OP_FORMAT_VERSION` stays 2.

## Deferred minors (do not block Task 5)

From per-task reviews of #55. Parked, not load-bearing:

- Dead `InsertReorder` empty-list branch; extra G-7 `apply_raw` tests
  (empty id / duplicate `instance_id`); `changeset_from_insert_ops`
  only samples `InsertAdd`; `track_heap` ignores `InsertSlot` strings.
- Duplicated effect-as-instrument gate (`audio/mod.rs` vs
  `ControlPlane`); G-7 insert-membership reject untested in isolation;
  process-global scan seed in a unit test.
- LV2 Effect allows `audio_inputs > 2`; latency name `contains()`
  fallback is broader than "named latency"; CLAP Replace peak pin is
  plugin-dependent; main-out error still says "instruments only";
  `latency_of` comment vs host-map probe.

## How to verify (no UI yet)

```
timeout 180 cargo test --manifest-path src-tauri/Cargo.toml --lib -- \
  insert_ instance_is_insert plugin_add_of_an_insert_rehosts \
  reactivate_restored_hosts instantiate_effect_accepts \
  clap_node_replace lv2_node_replace
```

Owner ear-check of an insert is Task 5+.
