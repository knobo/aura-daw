# AURA — Scalability Roadmap

Status: living document, owned by the architecture agent.
Audience: all agents + orchestrator. Companion to `ARCHITECTURE.md`.

Strategic goal: AURA grows from the current prototype (linear audio-track
mixer, record/play, stem/transcribe sidecars) into an **FL Studio-class DAW**:
hundreds-to-thousands of mixer channels, plugin hosting, full MIDI
sequencing with a pattern/channel-rack workflow, millions of timeline events,
and a UI that stays at 60–120 FPS throughout.

This document defines, per axis:

* **Current state (keep)** — what the prototype already does right.
* **Do NOT do now** — shortcuts that look harmless today but block scaling.
* **Migration path** — the concrete staged evolution.

A **Debt register** at the end lists frozen decisions that will need managed
evolution (nothing is changed now; the IPC surface stays frozen for the
prototype milestone).

Scale targets used throughout ("FL-class" numbers):

| Dimension | Prototype | Target |
|---|---|---|
| Mixer tracks/channels | ~16 | 500+ (FL: 500 mixer tracks + unlimited channels) |
| Timeline events (MIDI notes + automation points) | 0 | 10⁶–10⁷ per project |
| Plugin instances | 0 | 1000+ (mostly idle), 100+ active |
| Audio graph nodes | linear track list | 10⁴ nodes (tracks × inserts × sends) |
| Project size on disk | < 1 GB audio | 10s of GB (audio + frozen stems + samples) |
| Undo depth | none | unbounded within session, persisted across crash |

---

## 1. Audio engine: linear mixer → audio graph

### Current state (keep)

* The three-plane topology (RT / control / presentation) and the §2.1 RT
  contract (no alloc, no locks, no syscalls) are exactly the foundation an
  FL-class engine needs. **Nothing about this changes at scale — it only gets
  enforced harder.**
* The prepare-then-pointer-swap rule is already written into ARCHITECTURE §2.3:
  structural changes are built off-thread and installed with a single swap
  command, with the retired graph returned for off-thread deallocation. This
  *is* the RCU pattern; it generalizes unchanged from "list of tracks" to
  "arbitrary graph".
* `EngineCmd` as a POD enum with no heap payloads keeps the command queue
  wait-free. Correct — keep POD forever; anything with a heap payload travels
  as a pre-built object installed by pointer swap.

### Target architecture

```
                ┌────────── AudioGraph (immutable snapshot) ──────────┐
                │  nodes: Source(clip player) · TrackStrip(gain/pan)  │
                │         Bus · Send(pre/post) · PluginChain          │
                │         SidechainTap · MasterOut                    │
                │  edges: audio (N-ch) + event (MIDI/param) ports     │
                └─────────────────────────────────────────────────────┘
   control thread builds Graph vN+1  ──►  Arc swap  ──►  RT renders vN+1
                                          │
                        retired Graph vN ─┘► sent back, dropped off-RT
```

* **Node/port model.** Every processor (track strip, bus, insert chain, send,
  sidechain tap) is a node with typed ports: audio ports (channel-counted) and
  event ports (MIDI + sample-accurate parameter changes). Sends and sidechains
  are ordinary edges — a compressor's sidechain input is just another audio
  input port. No special cases in the renderer.
* **Compiled schedule, not interpreted graph.** The control thread topologically
  sorts the graph *at build time* into a flat render schedule (array of node
  refs + preassigned buffer indices from a buffer pool). The RT thread executes
  the schedule linearly; it never walks the graph. Cycles are legal only
  through explicit one-block delay nodes (feedback sends), detected at compile
  time.
* **Graph swap = RCU.** The schedule + buffer pool + node states are one
  immutable snapshot behind an `Arc` (or triple-buffer). Swap is one atomic
  pointer exchange in `EngineCmd::SwapGraph`. Node *state* that must survive a
  swap (filter memories, delay lines, plugin instances) is keyed by stable
  node ID and **moved, not copied**, from the old snapshot into the new one at
  build time on the control thread — the RT thread only ever sees a complete,
  prepared snapshot. Reverb tails on deleted nodes: keep the node alive,
  disconnected-fading, for a configurable tail window before retiring.
* **Parameter changes stay out of graph swaps.** Gain/pan/plugin-param moves go
  through a preallocated parameter ring (or per-parameter atomics for the hot
  few) with block-rate or sample-accurate smoothing in the node. Graph swaps
  are for *structure* only. At 500 tracks, rebuilding the graph per fader move
  would be catastrophic; the prototype's `set_track_gain` must already route
  to a parameter change, not a rebuild.

### Multicore rendering

* Stage 1 (prototype→v1): single RT render thread. Correct and sufficient
  below ~100 lightweight tracks.
* Stage 2: **graph partitioning + work stealing.** The compile step groups the
  schedule into *chains* (maximal single-successor runs) and emits a DAG of
  chains with atomic dependency counters. A pool of RT-priority worker threads
  (spawned and pinned at engine start, never at runtime) executes ready chains;
  the cpal callback thread becomes the coordinator that publishes the block,
  signals workers (futex/semaphore per worker is acceptable *for workers*; the
  design goal is that the coordinator itself never blocks on a lock), and mixes
  the final outputs. This is the standard JUCE/Bitwig-style graph player.
  Prerequisites: the compiled-schedule model above, and per-node buffer
  ownership so no two workers write one buffer.
  > **Constraint (2026-08-13, CORE-REDESIGN-ROUND-2 §8):** buffer lifetime
  > under nondeterministic multicore completion order will be solved by
  > **per-node buffer ownership or a wait-free arena pool with control-side
  > release** — decided when the schedule compiler is actually designed, not
  > before. Whatever wins, the RT thread never runs a nontrivial destructor
  > (dossier 07).
* Stage 3: latency-aware partitioning — heavy lookahead plugins moved onto
  double-buffer-ahead partitions (FL's "smart disable"/async analog).

### Plugin Delay Compensation (PDC)

* Every node reports `latency_samples()` (already anticipated by the
  `TimeStretcher` trait in ARCHITECTURE §8 — extend it to `AudioProcessor`).
* The graph compiler computes worst-path latency per sink and inserts
  compensating delay lines on the *shorter* paths at compile time; sources are
  additionally read ahead by the path latency so the timeline stays aligned.
* PDC changes are a structural recompile (a plugin reporting new latency
  triggers a graph rebuild + swap), never an RT-thread computation.
* **Do this before sends/sidechains ship** — retrofitting PDC onto a graph
  that users already mix on audibly changes their mixes.

### Do NOT do now

* Do NOT let any code path mutate the running graph in place "because it's
  just one track" — every structural change goes through build-and-swap from
  day one, even in the stub engine.
* Do NOT bake stereo into buffer types. Use channel-counted buffers
  (`&mut [f32]` frames × channels or planar slices) even while everything is
  stereo. Surround/multi-out plugins otherwise force a buffer-type rewrite.
* Do NOT let per-track meter code assume a compile-time `MAX_TRACKS` anywhere
  outside the single POD queue element (see Debt D-05 for the queue element
  itself).
* Do NOT give the RT thread any `String`, `Vec` push, `HashMap` lookup with
  hashing of UUID strings, or `log::` call. Track identity on the RT side is a
  dense `u32` slot index assigned by the graph compiler; UUID↔slot mapping
  lives on the control thread only.

### Migration path

1. **Now (prototype):** linear track list rendered as a degenerate graph —
   implement the render loop *as* a schedule executor over a compiled
   "graph" that happens to be `sources → strips → master`. Costs nothing
   extra; makes every later step additive.
2. Buses + sends as new node kinds (`track.kind: "bus"` is already reserved in
   the schema — good).
3. PDC in the compiler.
4. Insert chains hosting internal effects via `AudioProcessor`.
5. Multicore chain scheduler.
6. Sidechain edges + feedback-delay nodes.

---

## 2. Plugin hosting

### Current state (keep)

* The `AudioProcessor` trait (ARCHITECTURE §8) with `prepare` on the control
  thread and RT-safe `process` is the right internal contract; external plugin
  APIs adapt onto it.
* Nothing plugin-shaped is in the frozen IPC surface — hosting can be added
  with purely additive commands. Good.

### Direction

* **CLAP first.** Rust-native support (`clack`, or `clap-sys` directly), sane
  threading model (explicit `[main]`/`[audio]` contracts that map 1:1 onto our
  control/RT planes), per-note automation and polyphonic modulation built in,
  no licensing friction. The internal node/port/param model should be shaped
  CLAP-like so the CLAP adapter is thin.
* **VST3 later** via the same internal contract (vst3-sys). VST3's
  `IComponent`/`IEditController` split maps onto control-plane vs UI concerns;
  budget for its quirks (its own PDC reporting, bus arrangements) in the
  adapter, not in the core.
* **Process isolation as an option, per plugin.** Architecture rule now:
  the host talks to plugins through an internal handle that has two
  implementations — in-process (fast path, default) and out-of-process bridge
  (shared-memory audio ring + event pipe, one plugin-host subprocess per
  isolation group). Crash of a bridged plugin = node emits silence + UI badge,
  not a lost project. Bridging is also the 32/64-bit and Wine/yabridge story
  on Linux. Do not attempt sample-accurate isolation for the prototype; just
  keep the *handle indirection* so the bridge can be slotted in.
* **Plugin GUIs:** plugin editor windows are native child windows owned by the
  host process (or the bridge process), NOT WebView content. Tauri only ever
  gets embedded generic parameter UIs (auto-generated from the param list over
  IPC).

### Parameter automation model

* Every node (built-in or plugin) exposes a flat list of
  `ParamId (u32, stable per node) → {name, range, flags}`; discovered at load,
  cached in the project.
* Automation is **event-timeline data, not per-block command traffic**: curves
  live in the project (see §3), are compiled into per-block sample-accurate
  event lists by the control thread ahead of the playhead, and delivered to
  nodes through the schedule's event ports.
* Live tweaks (UI knob, MIDI CC, plugin's own GUI) go through the same param
  ring as fader moves, and are *recorded* into automation by the control
  thread — the RT path doesn't distinguish live vs. played-back automation.

### Do NOT do now

* Do NOT expose plugin parameters as per-parameter Tauri commands
  (`set_plugin_param`-style, one invoke per tweak). That path must be born
  batched (see §5 op-log) — a knob drag is 200+ updates/s.
* Do NOT design any project-schema field that assumes an effect is identified
  by *index in a chain* only; use stable node UUIDs so undo/redo and
  automation survive reordering.
* Do NOT let plugin scanning run in the main process when it ships — scanning
  is the #1 crash source in every DAW; it must be a sacrificial subprocess.
  (Phase-3 status: DONE for CLAP — `plugins::scan_worker` re-executes the
  binary with `AURA_SCAN_WORKER=1` and reads descriptors in a sacrificial
  subprocess (D-11's scan half, paid). Runtime plugin processing is still
  in-process — D-11's open half. LV2 discovery is metadata-only/safe.)

### Migration path

1. Internal effects only, behind `AudioProcessor` (validates chains, PDC,
   param model with zero third-party risk).
   **PARTIALLY PAID (phases 2–3): internal INSTRUMENTS are live in-graph
   nodes behind `dsp::LiveInstrument` (PolySynth + SamplerNode through
   `rt::LiveNodeCell` — ARCHITECTURE §15.1); insert-effect chains remain.**
2. CLAP host, in-process, headless params → generic UI.
   **PAID (phase 3, zones P1/P2/P3/P4 + merge): real CLAP hosting via
   clack (instantiation, port/param negotiation, RT `ClapNode`, state ext)
   and real LV2 hosting via livi (`Lv2Node`, ZynAddSubFX pitch-checked
   headless acceptance) — both formats on ONE shared plugin main thread
   (`aura-plugin-host`); plugin browser + generic param UI in the
   frontend; state persistence (APST blobs + `FormatStateBridge`) and
   automation groundwork shipped. Insert-effect chains still remain.**
3. CLAP plugin GUI windows (v1 = plugin-owned/floating windows + generic
   param UI; embedding X11-only later, Wayland embedding does not exist).
4. Out-of-process bridge (opt-in per plugin, default-on for unscanned/flaky).
5. VST3 adapter.

Note (2026 crate reality, verified locally): `clack-host`/`clack-extensions`
0.1.1 on crates.io (CLAP 1.2, maintained, pin the 0.1.x line);
`livi` 0.7 needs system `liblilv-dev` (a build/packaging prerequisite, debt
D-12) and lacks LV2 `state:interface` (paid: `plugins::state` does raw-lilv
state on top of livi handles).

---

## 3. MIDI & sequencing: patterns, piano roll, event storage

### Current state (keep)

* `track.kind` already reserves `"midi"`; `clip.schema.json` is audio-specific
  but clips are referenced from a flat list with `trackId` links — a shape
  that extends to typed clips.
* Nothing in the frozen command surface hardcodes "track has audio clips
  only". Sequencing arrives as additive commands + schema evolution.

### The FL model vs. linear timeline — support BOTH

FL's essence: **patterns** are first-class containers of note/automation data,
authored in a channel rack / piano roll, then *instantiated* on a playlist.
A linear DAW binds clips directly to timeline positions. The unifying model:

* **`Pattern`** = a named, reusable container of MIDI/automation events with
  its own musical length. Owns events in *musical time* (ticks).
* **`Clip`** (generalized) = a *placement*: `{timeline position, length,
  contentRef}` where `contentRef` is either an audio source (today's clip) or
  a `patternId` + transpose/velocity offsets. Multiple clips may reference one
  pattern (instancing — editing the pattern updates every placement, which is
  the channel-rack workflow); a linear-workflow "MIDI clip" is simply a
  pattern with a single placement, created implicitly.
* Arrangements (FL playlists) = named sets of placements; v1 has exactly one.

This makes the pattern workflow a *usage style* of one data model rather than
a second engine.

### Schema evolution (project.json v2, additive in spirit)

```jsonc
{
  "schemaVersion": 2,
  // v1 fields unchanged ...
  "tempoMap":   [ {"tick": 0, "bpm": 120.0}, ... ],   // replaces lone tempoBpm (kept, = first entry)
  "ppq": 960,                                          // ticks per quarter note
  "patterns":   [ {"id": "...", "name": "...", "lengthTicks": ..., "eventsRef": "events/<id>.bin"} ],
  "midiClips":  [ {"id": "...", "trackId": "...", "patternId": "...",
                   "timelineStartTicks": ..., "lengthTicks": ...} ],
  "automation": [ {"id": "...", "targetNode": "...", "paramId": ...,
                   "pointsRef": "events/<id>.bin"} ]
}
```

Key decisions:

* **Musical time (ticks @ PPQ) for MIDI/automation; sample time for audio
  clips.** Audio stays sample-anchored (as today) unless the user opts a clip
  into musical stretch. The tempo map provides the tick↔sample bijection; the
  control thread precomputes a piecewise mapping table so the RT thread never
  does tempo math. This is why the v1 sample-only timeline is registered as
  debt (D-02): every *Samples field is reinterpretable through the map, so v1
  projects migrate losslessly (v1 = trivial one-entry tempo map).
* **Events do NOT live in project.json.** JSON with 10⁶ notes is a
  100 MB+ parse on open. Event payloads live in per-pattern binary chunks
  (`events/<id>.bin`, same design language as waveform tiles: magic, version,
  count, fixed 16-byte records, little-endian), referenced from project.json.
  See §4.

### In-memory event structures (millions of events)

> **Note (2026-08-13):** the in-memory structure specified below is
> **superseded by the summarising COW B-tree** per CORE-REDESIGN-ROUND-2 §6
> (measured Θ(log N) vs Θ(√N) retained bytes per point-edit version;
> benchmark crates live in-repo under `benches/`). The AMEV binary chunk
> **file format is unaffected** — file format and in-memory tree are
> separable. The paragraphs below are kept for the reasoning record.

* Per pattern: a **sorted struct-of-arrays chunk list** (columns: tick, kind,
  key/param, value, duration), immutable chunks of ~4k events with a small
  mutable tail — i.e. a persistent/copy-on-write rope of event chunks.
  Why: (a) playback = merge-cursor over chunks, cache-linear, no tree
  pointer-chasing; (b) undo/redo = keeping old chunk references (see §4);
  (c) the piano roll gets range queries via per-chunk `[minTick, maxTick,
  minKey, maxKey]` summaries — an implicit interval tree.
* Playback pre-roll: the control thread walks cursors ahead of the playhead
  and feeds the RT thread fixed-size event blocks through the existing SPSC
  pattern (one queue per... no — one *event stream buffer* per graph
  snapshot, filled ahead; RT only reads).
* Piano-roll rendering at scale is a UI virtualization problem (§5), not a
  data problem, if range queries are cheap.

### Do NOT do now

* Do NOT store any new musical-position field in seconds or samples. Anything
  rhythmic added from now on (loop markers are grandfathered) is ticks.
* Do NOT put event arrays inline in project.json "temporarily".
* Do NOT design piano-roll UI state that assumes it holds *all* notes of the
  project in one Svelte store.

### Migration path

1. v2 schema: tempo map + ppq (single entry ⇒ v1-equivalent), `patterns`,
   `midiClips`, binary event chunks. Migration v1→v2 is mechanical.
2. Channel-rack UI = filtered view (one pattern, instrument rows) over the
   same store as the playlist.
3. Automation curves join the event system (same chunk format, `kind=point`).
4. Per-note expression (CLAP note expressions / MPE) as extra event columns —
   the SoA chunk format grows columns without breaking old readers (chunk
   header carries a column bitmap).

---

## 4. Project format, persistence, undo/redo

### Current state (keep)

* `schemaVersion` field exists from day one — the single most important
  future-proofing decision, already made. Keep it an integer, keep it
  required.
* `.aura` as a *directory* with `project.json` + assets + regenerable
  `cache/` is the right container (no zip-rewrite problem for multi-GB
  projects; assets are addressable by relative path; cache is deletable).
* Atomic save (tmp + fsync + rename) already specified.
* All intra-project references relative — portability preserved.

### Schema versioning & migration strategy

* **Monotonic integer `schemaVersion`, one migration module per step**
  (`v1→v2`, `v2→v3`…), run as a chain on open. Migrations are pure functions
  `Value → Value`, unit-tested against fixture projects; the original file is
  backed up as `project.json.v<N>.bak` before the first write in a newer
  format.
* **Open policy:** newer-major file + older app ⇒ refuse with a clear message
  (never best-effort-parse someone's session); equal/older ⇒ migrate silently.
* **Additive-tolerant readers:** the Rust deserializer must *ignore unknown
  fields* (serde default) even though the published JSON-Schemas say
  `additionalProperties: false`. The schemas describe what we *emit*; the
  reader is Postel-liberal so a v2 app can open a v2.1 file. Registered as
  debt D-06: the current schemas conflate "wire validation" with "reader
  contract" — resolve by shipping paired `*-strict` (emit) and relaxed (read)
  profiles, or by dropping `additionalProperties: false` in v2 schemas.

### Large-project storage

* project.json stays the human-diffable *structural* manifest (tracks, graph,
  clip placements, refs). Bulk payloads live beside it in versioned binary
  chunk files: `events/` (§3), `cache/waveforms/` (already), later
  `automation/`, `freeze/`. Rule of thumb: **if it can exceed 10k elements,
  it gets a chunk file and a `...Ref` in the manifest.**
* Chunk files are immutable-once-written and content-addressed where cheap
  (`<uuid>.bin`, rewritten under a new UUID on change) — this makes autosave,
  undo persistence, and crash recovery near-free (see below) and plays well
  with cloud sync/backup tools.

### Autosave & crash recovery

* **Journal, don't re-save.** Every committed user operation (see op-log,
  §5) is appended to `<project>/.aura/journal.ndjson` (fsync'd on a debounce,
  e.g. 500 ms idle or 5 s max). Crash recovery = last saved project.json +
  replay journal. This is cheap *because* ops are already first-class for
  undo and IPC — one design, three features.
* Full autosave snapshot (`autosave/project.json`) on a slow timer (2–5 min)
  truncates the journal.
* Recording is already crash-safe by design (WAV written incrementally by the
  disk-writer thread); on recovery, orphaned takes in `audio/` without a clip
  are offered to the user, never silently deleted.

### Undo/redo architecture

> **Note (2026-08-13):** retention mechanics are now specified in
> CORE-REDESIGN-ROUND-2 §2.3/§6 — deleted objects stay alive through
> **version-graph retention** (history versions keep them reachable; no
> limbo registry, no per-object refcounts), and bulk/scattered ops store
> op + inverse payload as **replay-only history nodes** instead of
> materialized snapshots.

* **Command pattern at the op level** (the same ops as the journal and the
  IPC op-log): each op carries its inverse or enough state to derive it
  (`SetTrackGain{track, old, new}`); undo stack = list of op batches
  (one batch per user gesture — a whole knob drag is one entry).
* **Persistent data structures at the bulk level:** pattern event ropes (§3)
  and any large COW structure make "old state" retention O(changed chunks),
  so deep undo on million-event patterns doesn't copy megabytes.
* Undo history is per-project, survives save (persisted as journal segments),
  and is bounded by size, not count.
* Engine interaction: undo/redo *replays ops* through the normal control-plane
  path (including graph rebuild+swap when structural). The RT thread has no
  concept of undo.

### Do NOT do now

* Do NOT add fields to project.json without bumping awareness: v1 is frozen;
  additions go to the v2 design doc (this file) first.
* Do NOT implement "undo" ad hoc in the Svelte stores (UI-local undo diverges
  from engine state the first time two windows or a script touch the
  project). Undo waits for the op-log; the prototype ships without undo
  rather than with a throwaway one.
* Do NOT let any code write absolute paths into project.json (the `path`
  field is the single documented exception and is informational only).

---

## 5. IPC & UI at scale (500+ tracks, 60–120 FPS)

### Current state (keep)

* The three-mechanism split (invoke/JSON for control, Channel for streams,
  raw-bytes `Response` for bulk) is the right taxonomy and scales — the
  *contents* evolve, the mechanisms don't.
* Meter batching (all tracks in ONE 60 Hz frame, aggregation done backend-side,
  peak-hold in UI) is exactly right; per-track messages would already be dead
  at 64 tracks.
* Waveform LOD tile pyramid with binary transfer + versioned magic header is
  the model answer; the same tile discipline extends to piano-roll note
  density maps and automation curves later.
* "Never send per-audio-buffer messages to the UI" — keep as an iron law.

### Where the per-command surface must evolve: the op-log protocol

The frozen surface is one-invoke-per-mutation (`set_track_gain`, …) returning
the full mutated object. Fine at prototype scale; at FL scale it fails on
three axes: **rate** (knob drag = 200 invokes/s), **atomicity** (multi-object
edits: "delete 50 clips" must be one undo entry and one engine swap), and
**sync** (two windows / script API / collaborative future need a single
ordered mutation stream).

> **Note (2026-08-13):** the op log is now designed — CORE-REDESIGN-ROUND-2
> §4 specifies it as closure transactions over one `Session`, a **persisted,
> versioned op format** from day one, and gesture `begin`/`end` IPC with
> transient (coalesced, non-journaled) ops in between. This section's
> envelope draft machinery (`rev`/`baseRev`/`transient`/`origin`,
> `op-envelope.schema.json`) is **inherited by that design**, not replaced.

Target: a **versioned, batched op-log protocol** — additive, standing beside
the frozen commands (which become thin wrappers emitting single-op batches):

* `ops_apply(batch)` — one invoke carrying `{baseRev, ops: [...]}`;
  applied atomically by the control thread; returns `{rev, results}`.
* `ops_subscribe(channel, sinceRev)` — Channel streaming committed batches
  (with their new `rev`) to every window/store; the UI applies deltas to its
  local mirror instead of re-fetching (`get_tracks` becomes cold-start only).
* Monotonic `rev` (u64) per project session gives: optimistic UI (apply
  locally, reconcile on commit), delta-sync after reconnect (`sinceRev`),
  undo (inverse batches), journal (§4) — one concept, four features.
* Draft envelope schema added (additively) as
  `docs/ipc-schemas/op-envelope.schema.json` — v2 DRAFT, not part of the
  frozen surface, safe to iterate.

High-rate *continuous* gestures (fader/knob drags, scrub) additionally get a
coalescing rule: intermediate values stream as transient `param` ops flagged
`transient: true` (not journaled, latest-wins), with one final committed op on
gesture end. This caps op-log growth without losing responsiveness.

### Keeping 60–120 FPS in the WebView

* **Virtualized track list / playlist / mixer:** render only visible rows
  (500 tracks ⇒ ~40 DOM rows). No Svelte component may iterate all tracks
  per frame. Track store keyed by id; visible-range selector drives mounting.
* **Meter decimation, subscription-scoped:** `subscribe_meters` today sends
  all tracks; at 500 tracks × 60 Hz × ~90 B/track JSON ≈ 2.7 MB/s of JSON
  through the serializer — measurable but survivable; the real cost is UI-side
  parse+GC. Evolution (additive, keeps command name): the subscription gains
  options `{trackIds?: [...], maxHz?: n}` so the mixer view subscribes to
  visible strips at 60 Hz and everything else at 10 Hz for overview meters;
  later, a binary meter frame over the same Channel (fixed-stride f32 array +
  slot-index table, mirroring the internal `RawMeterBlock`) removes JSON from
  the hot path entirely. Registered as D-04.
* **Canvas/WebGPU for anything per-pixel:** meters, waveforms, piano roll,
  playlist clip bodies are canvas layers; DOM is chrome only. Waveform tiles
  upload as typed arrays (already the design). Piano roll at 10⁶ notes uses
  the same LOD idea: below a zoom threshold, render from backend-supplied
  density tiles instead of individual notes.
* **One rAF loop, one store commit per frame:** all Channel consumers write
  into ring/latest-value buffers; a single rAF tick applies them to reactive
  state. Svelte 5 fine-grained reactivity ($state on per-track objects) keeps
  invalidation local — never a whole-project store object replaced per frame.
* **Frontend never blocks on invoke in a frame path.** Anything needed during
  scroll/zoom must be either already-pushed state or an async tile fetch with
  a stale-while-revalidate placeholder.

### Do NOT do now

* Do NOT add new per-property setter commands beyond the frozen list — new
  mutations should be born as ops (even if temporarily wrapped in a single
  bespoke command, shape the payload as `{ops:[...]}`).
* Do NOT build UI stores that require `get_tracks`-style full refetch after
  every mutation; write stores delta-ready (apply a returned object / op) now
  so the op-subscribe migration is a transport change, not a store rewrite.
* Do NOT introduce a second event bus (e.g. broadcasting mutations via
  `app.emit` globally) — mutation fan-out is the op-log Channel's job;
  app events stay for the rare lifecycle notifications listed in §3.4.

### Migration path

1. Prototype ships the frozen surface as-is.
2. Add `ops_apply`/`ops_subscribe` (additive commands + envelope schema);
   legacy setters reimplemented as single-op wrappers → automatic journal +
   undo coverage for old callers.
3. UI stores switch to op-subscribe deltas; full-fetch commands demoted to
   cold-start/recovery.
4. Binary meter frames + subscription scoping when profiling says so.

---

## 6. Sidecar / AI subsystem

### Current state (keep)

* NDJSON line protocol over stdout with typed progress/log/done/error events
  is simple, language-agnostic, debuggable — keep it as the *worker* protocol
  regardless of how scheduling evolves.
* UUID jobs + status polling + Channel streaming + throttled app events, and
  cancel via SIGTERM-then-SIGKILL: the right lifecycle skeleton.
* Outputs under the project dir with regenerable-cache discipline.

### Scaling direction

* **Job queue → real scheduler.** Today: one tokio task per job, implicit
  unbounded concurrency. Target: a priority queue with (a) per-resource-class
  concurrency limits (GPU jobs: 1–2; CPU-heavy: NumCores/2; IO-ish: higher),
  (b) priorities (interactive request > background batch, e.g. "split stems
  for the clip the user just right-clicked" preempts a batch transcription),
  (c) persistence of queued jobs across restart (journal, §4 pattern).
  The public surface (`sidecar_*` commands, event shapes) already supports
  this — `state: "queued"` exists in the schema. Purely internal evolution.
* **GPU scheduling.** Model workers self-report resource class
  (`{"type":"hello","needs":{"gpu":true,"vramMb":4000}}` — additive NDJSON
  message). Scheduler tracks VRAM budget per device; jobs exceeding free VRAM
  queue rather than OOM. Multi-GPU later = multiple budget pools. Never let
  two Demucs jobs share one 8 GB card by accident.
* **Model management.** A `models/` registry (app-data dir, not per-project):
  manifest of {model id, task, files, size, hash, source URL, license},
  download-on-demand with resumable fetch + hash verify, LRU eviction by
  size cap. Workers receive resolved model *paths*; they never download.
  Add `sidecar_list_models` / `sidecar_download_model` as additive commands
  when this lands.
* **Worker pooling / warm models.** Model load often dominates (Demucs ~10s).
  Evolve workers from run-to-completion processes to optionally *resident*
  workers (same NDJSON, plus `{"type":"job", ...}` request lines on stdin) so
  repeat jobs reuse a warm model. Keep-alive with idle timeout; crash of a
  resident worker only fails its current job.
* Future AI features (stem-aware editing, vocal-to-MIDI via the existing
  pitch path, generative fills) all fit this same job/queue/model triangle —
  the subsystem's job is to make "add a new model-backed task" a worker
  script + a job-kind enum entry (see D-07 on the closed `kind` enum).

### Do NOT do now

* Do NOT let job submission spawn processes directly from the command handler
  — route through the (initially trivial) queue object now, so limits and
  priorities are a policy change later, not a plumbing change.
* Do NOT hardcode "a job has exactly one input file and one output dir" into
  the registry internals (the current two request types both look like that);
  the internal job record should carry an opaque params blob per kind.
* Do NOT stream raw model logs to the UI unthrottled (the schema's 10 Hz cap
  applies to `log` events too).

---

## 7. Cross-platform audio backends

### Current state (keep)

* cpal already abstracts ALSA/WASAPI/CoreAudio and keeps the engine
  backend-agnostic; device enumeration commands return a stable-looking
  `AudioDevice` shape. Right call for the prototype: no per-OS code paths in
  AURA itself.
* The RT contract (§2.1) is backend-independent — it is what makes backend
  swaps a plumbing exercise instead of an engine rewrite.

### Direction per platform

* **Linux:** cpal/ALSA today. PipeWire is the strategic target (native graph,
  per-app latency control, and it subsumes JACK for routing between apps);
  JACK support largely comes free via PipeWire's JACK shim, but a native
  `libjack` backend is worth having for pro users pinning classic JACK —
  both fit behind the backend trait. Expect *device id instability* (D-01) to
  bite hardest here (ALSA names shift with hotplug order).
* **Windows:** cpal/WASAPI (shared, then exclusive mode for low latency).
  **ASIO is non-negotiable for the pro audience** and is cpal's weakest area —
  plan a dedicated ASIO backend (asio-sys or direct) with the SDK-licensing
  packaging implications (user-provided driver, no SDK redistribution issues
  since ASIO SDK licensing changed, but confirm at implementation time).
* **macOS:** cpal/CoreAudio is genuinely good; add aggregate-device awareness
  and AVAudioSession-style route-change handling. Later: AUv3 hosting ties in
  via the plugin layer, not the device layer.

### The backend abstraction to converge on

Define (inside the audio module — no IPC change) an `AudioBackend` trait:
enumerate devices (with **stable ids** — see D-01: id = backend-qualified
persistent identifier, not display name), open duplex stream with requested
{rate, buffer size, channel count} + negotiated actuals, expose xrun and
latency (input+output round-trip) reporting, and hotplug notifications.
cpal is then *one implementation*, not the abstraction itself — this is the
key move that lets ASIO/PipeWire-native/JACK slot in per-OS without touching
the graph engine, and it can happen entirely inside Agent 2's module.

Device selection persistence: remember the user's device by stable id +
fallback-to-default policy; a missing device at startup must degrade to
default with a UI notice, never a refusal to open the project.

### Do NOT do now

* Do NOT persist the current `AudioDevice.id` (cpal display name) inside
  project.json — device choice is a *machine-local preference* (app config),
  never project data, or projects stop being portable. (Nothing does this
  today; keep it that way.)
* Do NOT assume the output stream defines "the" sample rate in engine code —
  input/output devices may disagree (WASAPI shared mode!); the engine has a
  project rate and resamples at the device boundary.
* Do NOT scatter `#[cfg(target_os)]` outside the backend module.

---

## 8. Debt register

Frozen decisions that will need managed evolution. **None are changed now**;
"Fix" describes the planned additive/versioned path. Severity: how expensive
this becomes if unaddressed by the time its feature area ships.

| ID | Where (frozen artifact) | Decision | Why it blocks scaling | Severity | Proposed fix (additive/versioned) |
|----|---|---|---|---|---|
| D-01 | `audio-device.schema.json` (`id` = "currently the cpal device name") | Device identity = display name | Names collide (two identical interfaces), change with hotplug order/locale ⇒ persisted device prefs break, ASIO/PipeWire backends can't be addressed reliably | **High** (before device-preference persistence) | Keep field, change *contents* to backend-qualified stable id (`"wasapi:{gu id}"`, `"pipewire:node.name"`); schema already calls it opaque, so this is semantics-compatible. Add optional `backend` field. |
| D-02 | `project.schema.json`, `clip/transport` schemas | All timeline positions in samples; single global `tempoBpm`/`timeSignature` | No tempo/signature changes; MIDI patterns need musical time; sample-locked clips can't follow tempo | **High** (blocks MIDI/patterns) | **PAID DOWN (phase 2, zone C + architect merge)**: `midi::TempoMap` (tick↔sample bijection), `project-v2.schema.json` (`ppq`, `tempoMap`, tick-based `midiClips`, AMEV event chunks), v1→v2 migration (`project.json.v1.bak`), and the engine integration are all live — midi clips render into the RCU graph via control-thread pre-roll (PolySynth fallback / sampler when `instrumentId` is bound), plus `.mid` import/export. Audio clips stay sample-anchored by design. **Phase 3: live instrument nodes SHIPPED (ARCHITECTURE §15.1) — the control-side pre-render is retired; PolySynth/SamplerNode render block-live inside the RCU graph.** Remaining (tracked, not debt): time-signature map. |
| D-03 | Frozen command list (§3.3) | One invoke per mutation, full-object responses, no batch/atomicity/revision | Knob-drag rates, multi-object gestures, multi-window sync, undo journaling all need atomic ordered batches | **High** (before undo & plugin params) | Additive `ops_apply`/`ops_subscribe` + `op-envelope.schema.json` (DRAFT included); frozen setters become single-op wrappers. **Partial (phase 2): `set_track_mix` ships batch-shaped and the frozen `set_track_*` setters are now single-change wrappers over it (control::ops); the remaining half — the full op-log protocol — has its design settled in CORE-REDESIGN-ROUND-2 §4 (ACCEPTED 2026-08-13, ADR 0003); implementation remains future work.** |
| D-04 | `meter-frame.schema.json` + `subscribe_meters` | JSON meter frames, always all tracks, fixed ~60 Hz | 500 tracks ⇒ MB/s JSON + UI parse/GC in the frame path | Medium (survivable to ~100 tracks) | Additive optional args `{trackIds, maxHz}` on subscribe; later binary frame variant negotiated via a `format` arg — same command name, same Channel. |
| D-05 | ARCHITECTURE §2.3 `RawMeterBlock { [f32; MAX_TRACKS*2] }` | Compile-time track cap in the RT meter element | Hard ceiling on track count; big constant wastes queue space | Medium | Chunked meter blocks (fixed-size element carrying `{firstSlot, count, values[N]}`), multiple pushes per callback; internal only, no IPC change. |
| D-06 | All `*.schema.json` | `additionalProperties: false` everywhere | Older readers hard-reject files/payloads with newer additive fields ⇒ every addition is de facto breaking | Medium (must be settled before v2 fields ship) | Declare schemas as *emitter* contracts; readers (serde) ignore unknown fields. **SETTLED (phase 2): all new schemas (`project-v2`, `midi-clip`, `sidecar-job-v2`, `mcp-policy`, `sampler-instrument`) ship WITHOUT `additionalProperties:false`; v1 schemas unchanged (frozen).** |
| D-07 | `sidecar-job.schema.json` `status.kind`/results `oneOf` | Closed enums `["stemSplit","transcribe"]` | Every new AI task is a schema break for validators | Medium | **PAID DOWN (phase 2): `sidecar_run_job` takes an open `kind` string + opaque `params` blob (`sidecar-job-v2.schema.json`); observers must treat unknown kinds generically (progress+log only). v1 kinds keep their dedicated commands.** |
| D-08 | `clip.schema.json` `sourceChannels` max 2 | Stereo ceiling on sources | Surround stems, multi-channel field recordings, multi-out instrument freezes | Low (prototype scope fine) | v2 raises max; engine buffers channel-counted from the start (§1) so only the schema bound moves. |
| D-09 | `recording-command`/`clip` model | One clip per take per track; no take lanes/comping structure | Loop-recording comping (table stakes in mature DAWs) needs take groups | Low now | v2: optional `takeGroupId` + `laneIndex` on clip (additive fields); recording pipeline unchanged. |
| D-10 | `transport-state.schema.json` | Single loop region; `state` enum excludes future modes (e.g. pattern-mode transport, count-in) | FL pattern/song mode toggle and punch regions don't fit | Low | v2: additive optional fields (`mode: "song"|"pattern"`, punch region); enum growth is reader-tolerant once D-06 is settled. |
| D-11 | `plugins::scan` (phase 3) | CLAP descriptor scan `dlopen`s bundles; plugin DSP runs in-process | Plugin scanning is the #1 DAW crash source; one bad `*.clap` or misbehaving plugin kills the session | **High** (runtime half, before shipping plugin hosting broadly) | **HALF PAID (phase-3 zones + merge): the SCAN half is done** — `plugins::scan_worker` re-executes the AURA binary with `AURA_SCAN_WORKER=1` (guard at the top of `main.rs`), reads CLAP descriptors in that sacrificial subprocess, speaks NDJSON on stdout, and dies on bad bundles without taking AURA down. **OPEN: runtime isolation** — plugin *processing* still runs in-process; the planned per-instance out-of-process bridge behind `LiveNodeCell` (shared-memory audio ring + event pipe, yabridge-style; opt-in then default-on for unscanned/flaky plugins) remains the roadmap item. LV2 *discovery* was always safe (lilv reads metadata only). |
| D-12 | `Cargo.toml` (phase 3) | LV2 host links **system** `liblilv` via `livi` (Ubuntu: `liblilv-dev`) | Build breaks on machines without lilv headers; packaging must carry the dep | Medium (packaging) | Packaging: `liblilv-dev` is documented in README prerequisites (runtime `liblilv-0-0` comes with desktop plugin installs); consider vendoring lilv (Carla-style) if it bites. **The state half is PAID (phase 3, zone P4 + merge)**: raw-lilv `state:interface` on livi handles ships in `plugins::state` (Zyn patches round-trip through project save/open); param snapshots remain the fallback for stateless-API plugins. |

Note on D-03/D-06 interplay: the op-envelope draft below is written with an
open (`additionalProperties: true`) op payload precisely to avoid re-creating
D-06 in the new protocol.

---

## 9. Sequencing the work (what unblocks what)

```
prototype milestone (frozen surface, as-is)
   │
   ├─► compiled-schedule renderer (§1.1)  ──► buses/sends ──► PDC ──► multicore
   │
   ├─► op-log protocol (D-03) ──► undo/redo ──► journal/autosave ──► multi-window
   │                                   │
   ├─► project v2 (D-02, D-06) ────────┴─► patterns/piano roll ──► event chunks
   │
   ├─► AudioProcessor internal FX ──► CLAP host ──► bridge ──► VST3
   │
   └─► backend trait (D-01) ──► PipeWire native / ASIO
```

Only two items justify touching code *direction* (not the frozen surface)
before the prototype milestone, both zero-cost if done from the start and
expensive later: (1) render loop written as a schedule executor over an
immutable swapped snapshot — already mandated by ARCHITECTURE §2.3, just
don't cheat on it; (2) `set_track_*` handlers routing to parameter changes
rather than any full-state rebuild. Everything else is genuinely deferrable.
