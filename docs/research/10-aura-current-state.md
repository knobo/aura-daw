# AURA — Current State (research dossier 10)

> **Why this document exists.** Every other dossier in `docs/research/`
> measures a proposal against something. This is the something. It is a map of
> what the code *actually does today*, written so that a later reader can tell
> the difference between a design we chose, a design we inherited, and a defect
> nobody has noticed yet. Where the code disagrees with `ARCHITECTURE.md`, this
> document records the code.
>
> **Date:** 2026-08-13.
> **Verified against:** commit `f632580` ("Document the transport shortcuts and
> auto-stop in the README") — every `file:line` below was re-checked at that
> SHA.
> **Provenance:** five parallel exploration agents (RT engine; control plane /
> IPC / MCP; project model & persistence; frontend; documented architecture &
> debt), consolidated and re-verified by hand.

> **Correction note.** Several exploration agents read the tree *before* it was
> rebased onto current HEAD, so their reports predate the five most recent
> commits and their line numbers run ~28 lines low in `audio/rt.rs`. Two of
> their claimed absences are wrong at `f632580`:
> * **`ARCHITECTURE.md` §2.6 exists** — "Timeline boundaries: the engine
>   notices, the control plane decides", including `transport::crossing`,
>   `SharedRt::song_end`, the `engine_evt` ring and the park handshake
>   (`ARCHITECTURE.md:173`).
> * **Frontend test infrastructure exists** — `vitest.config.ts`,
>   `src/lib/utils/timeline-nav.ts` + `timeline-nav.test.ts`, and
>   `src/lib/state/generation.adopt.test.ts`, with `npm test` wired to
>   `vitest run`.
> Everything else in their reports survived re-verification.

---

## 1. The real-time audio engine

### 1.1 What runs in the callback

Two independent cpal callbacks exist: the main output/input pair, and a
completely separate **sampler preview** stream (`audio/sampler_preview.rs`)
with its own RCU node swap, its own node, and no connection to the transport
or the mix graph.

The output chain, in full:

```
cpal output stream                                  engine.rs:481 (stream build)
  └─ closure |data: &mut [f32]| cb.render(data)
     └─ OutputCb::render                            engine.rs:204
        ├─ RCU adopt: while retire_tx.slots() > 0 { graph_rx.pop() -> into_box(); push(old) }
        │                                           engine.rs:207-219
        ├─ park handshake (only while stopped, swap against NO_PARK)
        │                                           engine.rs:225-231
        ├─ read atomics: playing / position / loop_spec
        │                                           engine.rs:232-234
        ├─ discontinuity = playing && (!was_playing || base != next_pos)
        │                                           engine.rs:235
        ├─ mixer::render(graph, params, base, lp, out, channels, rate, disc)
        │                                           engine.rs:239-248  →  mixer.rs:210
        │    for each RtTrack:
        │      ├─ read gain/pan/flags atomics by slot
        │      ├─ clips: per-frame frame_pos() → clip_sample() → *gain*pan → mix_out(+=)
        │      ├─ render_live(...)                  mixer.rs:121
        │      │    ├─ unsafe live.node.rt_mut()    mixer.rs:137-ish
        │      │    ├─ discontinuity → all_notes_off()
        │      │    ├─ split block into runs ≤ MAX_LIVE_BLOCK and at loop end
        │      │    ├─ scratch[..run*2].fill(0.0)   mixer.rs:158-162
        │      │    ├─ binary-search event slice → node.queue_event(BlockNoteEvent)
        │      │    ├─ node.set_block_context(pos, run_discontinuity)   mixer.rs:177
        │      │    ├─ node.process(&mut ProcessBlock)                  mixer.rs:179
        │      │    └─ scale by gain*pan, mute gate, meter fold, mix_out(+=)
        │      └─ blk.set_slot(tr.slot, ...)         mixer.rs:273
        │    then master meters by RE-SCANNING `out`  mixer.rs:281-284
        ├─ meter_tx.push(RawMeterBlock)  (failure ⇒ xruns++)  engine.rs:249-252
        ├─ transport::crossing(base, frames, lp, song_end)
        │     → evt_tx.push(RtEvent::ReachedEnd { at })       engine.rs:263-266
        └─ transport::advance(base, frames, lp) → shared.position.store
                                                     engine.rs:267-269
```

Not playing, or no graph: `out.fill(0.0)` (`engine.rs:253`).

Everything on the RT thread is atomic loads/stores, `rtrb` push/pop, one
`Box::from_raw`/`into_raw` pointer move, fixed-size stack POD, and node
`process` calls. No allocation, no locks, no `log::`, no `String`, no
`HashMap`.

### 1.2 There is no graph

`RtGraph` is a flat `Vec<RtTrack>` summed directly into the cpal output
buffer. There is **no master node, no bus, no send, no insert slot, no edge,
no port**. `audio/rt.rs:249`:

```rust
#[derive(Default)]
pub struct RtGraph {
    pub tracks: Vec<RtTrack>,
    /// Preallocated stereo scratch for live-node rendering
    /// (`MAX_LIVE_BLOCK * 2` once any track is live; empty otherwise).
    /// Allocated at BUILD time on the control thread — never on the RT path.
    pub scratch: Vec<f32>,
}
```

`audio/rt.rs:233`:

```rust
pub struct RtTrack {
    /// Index into `ParamTable`.
    pub slot: usize,
    pub clips: Vec<RtClip>,
    /// Live instrument (midi tracks): PolySynth / SamplerNode / plugin node.
    pub live: Option<LiveSource>,
}
```

A track holds clips and **at most one** instrument node. Topology is hardcoded
inside `mixer::render` (`mixer.rs:210`): `(clips ∪ live) → per-track
gain/pan/mute → += into interleaved out`. "Master" exists only as (a) the
summed `out` buffer and (b) a meter re-scan of it (`mixer.rs:281-284`).

`mix_out` (`mixer.rs:104`) writes channels 0 and 1 only — everything above
stereo is silent, despite `RtClipData` being channel-counted.

`ARCHITECTURE.md` §10 constraint 1 requires the render loop to be "a schedule
executor over an immutable, atomically swapped graph snapshot **even while the
'graph' is just clip players → track strips → master**". Half of that is
honoured: the snapshot and the swap are real; the schedule executor is not —
`mixer::render` is a hand-written two-level loop.

Offline export builds the *same* structure via `offline::build_graph`
(`audio/offline.rs`) with a private `LiveNodeRegistry`, then runs the real
`mixer::render` in chunks. The two build paths duplicate logic by hand and must
be kept mirror-identical or export and playback diverge.

### 1.3 The RCU handoff

Two tiers, explicitly separated (`ARCHITECTURE.md` §10 rules 1–2).

**Tier A — structural: ownership transfer through rtrb SPSC queues.** Not an
`ArcSwap`, not a triple buffer. The element is `GraphPtr` (`audio/rt.rs:278`):

```rust
pub struct GraphPtr(*mut RtGraph);

impl GraphPtr {
    pub fn new(graph: Box<RtGraph>) -> Self { Self(Box::into_raw(graph)) }

    /// Take ownership of the pointee. Never deallocates (safe on RT).
    pub fn into_box(self) -> Box<RtGraph> {
        let b = unsafe { Box::from_raw(self.0) };
        std::mem::forget(self);
        b
    }
}

impl Drop for GraphPtr {
    fn drop(&mut self) { drop(unsafe { Box::from_raw(self.0) }); }
}

unsafe impl Send for GraphPtr {}
```

* **Allocation** is control-thread (`Control::rebuild`, `engine.rs:534`).
* **Free** is control-thread — `drain_retired` (`engine.rs:645`), the eager
  drain at rebuild start, and `OutputBundle::Drop`. `Drop` on `GraphPtr` also
  frees queue leftovers on teardown, so nothing leaks on device switch.
* **The ordering guarantee is one line**: the callback only pops a new graph
  *if the retire queue has a free slot* — `while self.retire_tx.slots() > 0`
  (`engine.rs:207`). The old graph is therefore always handed back, and the RT
  thread provably never deallocates. `into_box` defusing `Drop` is what makes
  that a structural property rather than a convention.

Queue capacities (`engine.rs:481` and around): graph 8, retire 8, meters 64,
`engine_evt` 64.

**Tier B — continuous params: plain atomics, no queue.** `ParamTable`
(`audio/rt.rs:94`) holds `gain`, `pan`, `flags` as `[AtomicU32; MAX_TRACKS]`
plus `any_solo`, all accessed `Ordering::Relaxed` from both sides. `SharedRt`
(`audio/rt.rs:26`) holds position, playing, recording, sample_rate, the three
loop atomics, `song_end`, `stop_at_end`, `park`, and `xruns`.

`MAX_TRACKS = 64` (`audio/types.rs:13`), chosen so the meter presence mask fits
a `u64`.

**The park handshake** (`engine.rs:225-231`, `ARCHITECTURE.md` §2.6) is the one
piece of genuinely new mechanism at HEAD. The control thread writes *where* to
`SharedRt::park` **before** clearing `playing`; the callback applies it with a
`swap` against `NO_PARK` so it happens exactly once, and only while stopped.
This makes the callback the last writer of the playhead, closing the race where
a callback that read `playing == true` before a stop writes its advanced
position afterwards. It is mechanism, not policy: the callback carries out a
position, it never chooses one.

### 1.4 The `LiveNodeCell` seam

`audio/rt.rs:205` — the node-state-survives-RCU-swap primitive, and the thing
that lets a *mutable* node (voice pool, plugin instance) live inside an
otherwise immutable snapshot:

```rust
pub struct LiveNodeCell(UnsafeCell<Box<dyn LiveInstrument>>);

// SAFETY: see the contract above — access is exclusive by construction
// (single active snapshot + single RT thread), not by type-system proof.
unsafe impl Send for LiveNodeCell {}
unsafe impl Sync for LiveNodeCell {}

impl LiveNodeCell {
    pub fn new(node: Box<dyn LiveInstrument>) -> Arc<Self> { ... }

    /// RT-thread access during render. See the safety contract on the type.
    pub unsafe fn rt_mut(&self) -> &mut dyn LiveInstrument { &mut **self.0.get() }
}
```

The soundness argument is written out at `audio/rt.rs:190-205` and is
**hand-proved, not type-level**: exactly one snapshot is rendered at a time by
exactly one RT thread; the old snapshot is retired in the same callback that
adopts its successor; the control thread only touches a node before its first
snapshot is published; deallocation happens control-side when the last `Arc`
drops. Two `RtTrack`s referencing the same cell in one snapshot, or two
concurrently-rendered snapshots, would be undefined behaviour.

Control-side ownership lives in `LiveNodeRegistry` (`midi/playback.rs`), keyed
by strings like `"synth@48000"`, `"sampler:<id>@48000"`,
`"plugin:<instanceId>@48000"`. Identical key ⇒ the cell is reused, so voices
and plugin state survive rebuilds; a key change replaces the node.

The trait behind it (`audio/dsp.rs:25`, `:50`):

```rust
pub trait AudioProcessor: Send {
    fn prepare(&mut self, sample_rate: u32, max_block: usize);
    fn process(&mut self, io: &mut ProcessBlock<'_>);   // in-place, RT-safe, ADDS
    fn reset(&mut self);
}

pub trait LiveInstrument: AudioProcessor {
    fn queue_event(&mut self, ev: crate::midi::synth::BlockNoteEvent) -> bool;
    fn all_notes_off(&mut self);
    fn set_block_context(&mut self, _base_pos: u64, _discontinuity: bool) {}
}
```

Implementors: `PolySynth`, `SamplerNode`, `StubPluginNode` (renders silence),
`Lv2Node`, `ClapNode`, `GainAutomatedNode`.

**`AudioProcessor` has no `latency_samples()`.** `ARCHITECTURE.md` §10 rule 5
requires every processor to report it so PDC can be a graph-compiler feature;
only `TimeStretcher` has it (`audio/dsp.rs:66`), and `TimeStretcher` is not in
the production graph. PDC has no reporting surface at all.

### 1.5 Gain, pan, mute, solo — and the absence of smoothing

* **Storage:** `ParamTable` atomics keyed by dense slot (`audio/rt.rs:94`),
  mirrored in `TrackState` (`audio/types.rs:103`).
* **Write path:** `set_track_gain`/`pan`/… (`audio/mod.rs:398-458`) →
  `single_mix_change` → `ControlPlane::set_track_mix` (`control/mod.rs:273`) →
  `ops::apply_track_mix` (`control/ops.rs:47`), which writes the atomics
  directly. **No `Rebuild` is sent for mix changes** — batch-shaped from birth,
  knob-rate safe.
* **Read path:** once per buffer per track, `Relaxed`, then applied as a
  **constant scalar for the whole callback block**.
* **Smoothing: none.** No ramp, no one-pole, no per-sample interpolation on
  gain, pan or mute anywhere in the codebase. A fader move steps the gain at
  the next buffer boundary — an audible zipper on large steps.
  `ARCHITECTURE.md` §10 rule 2 and `SCALABILITY.md` §1 both say "smoothed in
  the node"; that half is not implemented.

The only sample-accurate ramping that exists is `GainAutomatedNode`
(§1.9), which is not wired into anything.

Laws: `db_to_linear` (`mixer.rs:26`, −160 dB ⇒ 0), constant-power `pan_gains`
(`mixer.rs:37`), `audible(muted, soloed, any_solo)` (`mixer.rs:45`, mute wins
over own solo). Clip gain and linear fades fold per-sample in `clip_sample`
(`mixer.rs:53`).

### 1.6 Plugins in the audio path

**Only as track instruments — never as inserts.** A plugin can occupy
`RtTrack.live` and nothing else. There is no effect slot on any track and no
master chain; `PluginRegistry::instantiate` (`plugins/mod.rs`) explicitly
rejects effects with *"is an effect; v1 hosts instruments on midi tracks
only"*.

**The RT thread calls plugin DSP directly, in-process** — this is the open half
of debt D-11:

* `ClapNode::process` (`plugins/clap_host.rs`) drains an `rtrb` param ring into
  a preallocated `EventBuffer`, wraps preallocated deinterleaved buffers sized
  `MAX_LIVE_BLOCK` at activation, calls `started.process(...)`, then **adds**
  the main out port into the interleaved scratch. `steady_time` is passed;
  **the CLAP transport event is passed as `None`** — hosted plugins get no
  tempo, no bar/beat, no PPQ position.
* `Lv2Node::process` (`plugins/lv2_host.rs`) pops `ParamChange` from an `rtrb`
  ring (cap 1024), builds port connections over preallocated buffers, calls
  `unsafe instance.run(...)`, adds the first stereo pair into the scratch.
* Both clamp `frames` to `MAX_LIVE_BLOCK`; errors are counted, not logged, on
  the RT path.
* Param traffic is control → node via wait-free rings, applied **at block start
  (offset 0)** — not sample-accurate.
* Instantiation, activation, state and GUI all happen on ONE dedicated
  `aura-plugin-host` thread (`plugins/host.rs`).

### 1.7 Sampler and MIDI scheduling

Both are `LiveInstrument` implementations occupying the *same* `RtTrack.live`
slot; the selection ladder is `plugin: → sampler instrument → PolySynth`
(`midi/playback.rs`), with an unresolvable reference falling back to PolySynth
with a warning so MIDI is never silently muted.

Scheduling is entirely control-side. `MidiClip` notes (ticks) →
`schedule::clip_events` via `TempoMap` → `Vec<AbsNoteEvent { sample, key,
velocity }>` (`midi/schedule.rs`), merged and sorted per track, `Arc`'d into the
snapshot as `LiveSource.events`. **Ticks never cross the RT boundary**
(`ARCHITECTURE.md` §13). On the RT thread it is a `partition_point` binary
search per run, converted to `BlockNoteEvent { offset, key, velocity }`.

Ordering inside one callback (`mixer.rs:225-284`), strictly sequential and
single-threaded:

1. tracks in `RtGraph.tracks` order (clip tracks as built by `rebuild`, then
   midi/live tracks appended by `append_midi_tracks`);
2. per track: **all clips first**, **then the live node**;
3. live rendering splits the block into runs at `MAX_LIVE_BLOCK` (4096,
   `audio/rt.rs:184`) and at loop-end boundaries;
4. per run: zero scratch → queue events → `set_block_context` → `process`
   (node ADDs) → wrap handling → scale/gate/meter/mix;
5. after all tracks, master meters re-scan `out`.

`SamplerNode::process` renders **segments between event offsets**, so note
starts are genuinely sample-accurate. `PolySynth::process` is per-frame with an
in-place sort of the event array; 32 voices, sine + linear AR.

### 1.8 Time on the RT thread

Pure sample domain. `SharedRt.position` is an `AtomicU64` of engine samples;
per-frame positions come from `transport::frame_pos` / `advance`
(`audio/transport.rs`), saturating add with loop wrap.

**Musical time does not exist on the RT thread.** No tick, no bar, no beat, no
tempo, no PPQ, no BPM in `SharedRt` or `RtGraph`. `TempoMap`
(`midi/tempo.rs`) is piecewise-linear with a precomputed prefix table and is
control-thread only.

Consequences for any future graph: no tempo-synced LFO or delay, no PPQ
position for plugins, no musical quantisation of anything the engine decides.

`LoopSpec { enabled, start, end }` (`audio/transport.rs`) is snapshotted from
three atomics per callback. Discontinuity (`engine.rs:235`) triggers
`all_notes_off` — release, not kill — on every live node; loop wraps inside a
block re-raise it per run.

### 1.9 Buffers

There is **no buffer pool and no per-node buffer ownership**.

| Buffer | Where | Lifetime / size |
|---|---|---|
| cpal `out` (interleaved) | given by cpal | the shared **accumulator**; every track and node `+=` into it |
| `RtGraph.scratch` | `rt.rs:249` | **ONE** stereo buffer, `MAX_LIVE_BLOCK * 2 = 8192` f32, shared by *all* live tracks, zeroed per run |
| `RtClipData.data` | `rt.rs:150` | immutable decoded+resampled clip, `Arc`-shared across snapshots, cached by clip id |
| plugin port buffers | clap/lv2 host | deinterleaved, per node, allocated at activation |
| node event buffers | synth/sampler/clap/lv2 | fixed capacity, overflow drops |
| recording rings | `engine.rs` | `rate * rec_ch * 2 s` per armed track |
| meter blocks | `audio/meters.rs` | `RawMeterBlock` POD, `[[f32;2];64]` ×2 + mask (debt D-05) |

Because scratch is single and shared, live-node rendering is inherently
serialised; `mixer::render` takes `&mut RtGraph` purely to reach it.

### 1.10 The RT rules, and what enforces them

**Stated:** `ARCHITECTURE.md` §2.1 (never allocate, never lock, never syscall
including logging, no panics, no unbounded loops), §2.3 (structural change =
build off-thread + single pointer swap; retired graph returned for off-thread
deallocation; POD-only queue elements), §10 rules 1–6, `CONTRIBUTING.md` §1
("reviewers will hold the line here").

**Enforced by:** convention and code review. There is **no** static or dynamic
enforcement — no `assert_no_alloc`, no allocator hook, no clippy lint, no
`[[clang::nonblocking]]` equivalent, and no CI workflow at all
(`.github/workflows` does not exist).

The structural safety nets that *are* mechanical:

* `GraphPtr::into_box` defusing `Drop` so the RT path provably cannot
  deallocate (`rt.rs:286`), with a test that queue teardown still frees.
* The `retire_tx.slots() > 0` precondition before adopting (`engine.rs:207`).
* Behavioural headless tests through the *real* `mixer::render` — gain, pan,
  solo, loop, live nodes, discontinuity, loop-wrap voice accumulation
  (`mixer.rs` tests), midi→live track and registry reuse (`midi/playback.rs`
  tests), automation ramps (`plugins/automation.rs` tests), meter pump and
  transport (`engine.rs` tests).

Known deliberate deviations: `xruns` conflates real capture underruns with
meter-queue overflow (`engine.rs:251`); rule 5 (`latency_samples()` on every
processor) is unimplemented; rule 1 (render loop *as* a schedule executor) is
unimplemented.

### 1.11 Automation: built vs. stubbed

**Built** (`plugins/automation.rs`, ~1000 lines):

* Data model: `AutomationPoint { tick, value }` (`:70`), `AutomationLane { id,
  target_node, param_id, points }` (`:78`) where `target_node` is a plugin
  instance uuid or `"track:<trackId>"`, `param_id` a flat `u32`;
  `AutomationStore` (`:93`) with app-global registration.
* Persistence: project v2 `automation: [{id, targetNode, paramId, pointsRef}]`
  plus AMEV binary chunks under `<project>/automation/<uuid>.bin`
  (`AUTOMATION_DIR`, `:60`), path-traversal-safe reads, PPQ rescale, stale-chunk
  GC, corrupt-chunk degradation to an empty lane.
* Compilation: `AbsParamEvent { sample, value }` (`:370`) and
  `compile_lane(lane, &TempoMap)` (`:378`), control-thread only.
* Ramp contract: `value_at` (`:393`) and the RT-form `segment_value` (`:405`) —
  linear between breakpoints, hold outside.
* RT wrapper: `GainAutomatedNode` (`:434`) — one binary search per block plus
  O(1) per frame, multiplying the inner node's contribution per sample.
  Genuinely sample-accurate.
* The seam that makes it position-true:
  `LiveInstrument::set_block_context(base_pos, discontinuity)`
  (`audio/dsp.rs:53`), called by `render_live` before **every** contiguous run
  (`mixer.rs:177`), re-seeding the cursor so ramps survive seeks and loop wraps.
* Commands `automation_get` / `automation_set`, registered in `lib.rs`.
* Five tests, including "gain automation survives seek and loop wrap through
  `render_live`".

**Not built:**

* **Nothing attaches lanes to nodes in production.** `GainAutomatedNode`
  appears nowhere outside `automation.rs` and its tests — the only other
  mention in the whole tree is a doc comment at `audio/dsp.rs:47`. The module's
  own header says so (`automation.rs:40`): *"Production graph rebuilds do not
  yet attach lanes to nodes."*
* Only "output gain of a live instrument node" is expressible. `param_id` is
  carried but no path routes an `AbsParamEvent` into a plugin's param ring.
* No automation of track fader/pan/mute (`ParamTable`), no clip params, no
  read-back, no write modes, no recording.

---

## 2. The control plane, IPC and MCP

### 2.1 What the seam is

`src-tauri/src/control/` is the seam. `ARCHITECTURE.md` §11: *"AURA now has TWO
front doors: the Tauri IPC surface and the embedded MCP server. Both drive the
app through ONE object."*

Two layers:

**`control::ops`** — pure, tauri-free functions over `Store`/`ParamTable`,
described as "the single implementation of each mutation". It has **exactly
three functions**:

```rust
pub fn apply_track_mix(store, params, changes) -> Result<Vec<TrackState>, String>  // ops.rs:47
pub fn add_track(store, params, name, kind)   -> Result<TrackState, String>        // ops.rs:99
pub fn transport_snapshot(store, shared)      -> TransportState                    // ops.rs:132
```

**`ControlPlane`** (`control/mod.rs:96`) — the managed facade holding the engine
handles: `store: Arc<Mutex<Store>>`, `shared: Arc<SharedRt>`, `params:
Arc<ParamTable>`, `engine: EngineHandle`, `jobs: Arc<JobManager>`, `midi:
Arc<Mutex<MidiStore>>`, `latest_meters`, and an injected
`emit: EventEmitter = Box<dyn Fn(&str, Value) + Send + Sync>` (`:92`) so app
events fire identically no matter which front door caused the change.

Public surface (`control/mod.rs`): `project_state` `:144`, `transport_state`
`:159`, `read_meters` `:165`, `transport` `:171`, `start_recording` `:230`,
`stop_recording` `:241`, `add_track` `:252`, `set_track_mix` `:273`,
`create_project` `:315`, `import_audio_clip` `:342`, `seed_demo_project` `:358`,
`run_sidecar_job` `:466`, `app_event_sink` `:483`, `job_status` `:500`,
`list_jobs` `:507` — plus extension `impl` blocks in `control/import.rs`,
`control/hum.rs`, `control/export.rs`, `control/loopjam.rs`.

### 2.2 The one op-shaped type

`control/ops.rs:22`, and its doc comment is the clearest statement of intent in
the codebase:

```rust
/// One batched mix change (op-log-shaped per debt D-03: a UI gesture or MCP
/// tool call carries MANY of these in one request). `None` = leave unchanged.
pub struct TrackMixChange {
    pub track_id: String,
    pub gain_db: Option<f64>,
    pub pan: Option<f64>,
    pub muted: Option<bool>,
    pub soloed: Option<bool>,
    pub armed: Option<bool>,
}
```

`apply_track_mix` validates **every** track id before writing anything, so one
bad id fails the whole batch — tested by
`batched_mix_is_atomic_on_unknown_track`. That is the only transaction-like
construct in the codebase.

**What "`set_track_mix` ships batch-shaped" actually means** — exactly three
things, and nothing more:

1. the payload is a batch of per-field-optional changes (the *shape* of
   `applyBatch.ops[]`, minus `kind`/`protocol`/`baseRev`/`label`);
2. the batch is atomic (validate-all-then-apply);
3. the legacy setters are already the thin single-op wrappers migration step 2
   describes — `set_track_gain` and friends build a 1-element vector.

It does **not** have `protocol`, `baseRev`, `rev`, `label`, `kind`,
`transient`, `origin`, inverses, a commit stream, or a journal.

The same "born batch-shaped" discipline was applied ad hoc, without an
envelope, to `midi_set_notes`, `set_tempo_map`, `plugin_set_param` and
`automation_set`.

### 2.3 What bypasses the control plane

`lib.rs` registers **66** commands *(CORRECTED 2026-08-13: this line
originally said 57, which matches no commit; a fact-check against HEAD
counted 66 `#[tauri::command]` functions, all registered. Note also that a
naive grep for the attribute string returns 74 because 8 hits are
doc-comment mentions — both wrong numbers have now appeared in project
documents; the true count is 66)*; MCP exposes 10 tools. Transport, mix, add-track,
create-project, import, recording and jobs are fully funnelled and have MCP
parity. Everything else grew around the seam:

1. **`audio::remove_track`** (`audio/mod.rs:369`) — takes `State<AudioState>`,
   locks the store directly, retains tracks/clips, frees the slot, sends
   `Rebuild` itself. The mirror image of `add_track`, which *does* go through
   `ops::add_track`. There is no `ops::remove_track` and no MCP tool. This is
   the single most glaring asymmetry.
2. **`audio::set_track_instrument`** (`audio/mod.rs:519`) — direct store
   mutation plus `Rebuild`. Structural, persisted, no event.
3. **`audio::open_project`** (`audio/mod.rs:573`) — replaces the *entire* store
   and emits `project://changed` via a raw `app.emit` (`:628`) rather than the
   injected emitter.
4. **`audio::save_project`** (`audio/mod.rs:633`) — reads store, saves, raw
   `app.emit` (`:645`).
5. **All six MIDI commands** (`midi/mod.rs`) — `set_tempo_map`, `midi_add_clip`,
   `midi_set_notes`, `midi_get_clips`, `midi_import_file`, `midi_export_file`.
   They reach the store via `audio.control_parts()`, write
   `store.transport.tempo_bpm` directly, and `midi_import_file` calls
   `ops::add_track` with a store it locked itself. **MCP has no MIDI tools at
   all** — an agent can read midi clips but cannot write them.
6. **All plugin commands** (`plugins/mod.rs`, `plugins/patches.rs`) — mutate the
   registry and **auto-persist into `project.json`** via
   `state::persist_after_mutation`, i.e. they write the project file behind the
   control plane's back.
7. **Automation** (`plugins/automation.rs`) — own store, own persistence.
8. **Sampler** (`audio/mod.rs:467`, `:492`) — mutate the app-global
   `SamplerBank`; `sampler_preview_note` makes sound directly.
9. **`sidecar_split_stems` / `sidecar_transcribe`** — talk to the `JobManager`
   directly, missing the post-job import wrapping `run_sidecar_job` adds.
10. **Device selection** (`audio/mod.rs:260`, `:271`) — direct `EngineHandle`
    requests.

And three **additional writer threads**, which any command/undo layer must
treat as mutation sources rather than sinks:

* **The engine control thread** — `stop_recording` extends `store.clips`, sets
  transport state and calls `project::save`; `ensure_project` silently creates
  and adopts a project and emits `project://changed`.
* **The LoopJam watcher** (`control/loopjam.rs`) — on a wall-clock-triggered
  thread it locks the store, replaces region clips, sends `Rebuild`, saves and
  emits. No user gesture behind it.
* **Sidecar completion sinks** (`control/import.rs`, `control/hum.rs`,
  `control/mod.rs`) — `wrap_sink_with_import`, `wrap_sink_with_stem_import`,
  `wrap_sink_with_hum_apply`, `wrap_sink_with_instrument_register` all mutate
  store and midi from job-completion threads.

Plus the frontend-only mutations that never reach the backend at all (§4.2).

### 2.4 How state flows back to the UI

Three mechanisms (`ARCHITECTURE.md` §3.1): `invoke` JSON for control,
`Channel<T>` for streams, raw `Response` bytes for bulk.

App events (frozen, `ARCHITECTURE.md` §3.4; TS map in
`src/lib/types/ipc.ts`): `transport://state`, `recording://state`,
`sidecar://progress|done|error`, `project://changed`,
`mcp://confirm-requested`, `export://progress|done|error`, `loopjam://state`.

Channels: `subscribe_meters` (~60 Hz, all tracks per frame). Bulk: waveform
tiles as AWTF over `tauri::ipc::Response`.

**No event is emitted for `add_track`, `set_track_mix`, `remove_track`,
`set_track_instrument`, or any midi/plugin/automation mutation.** When MCP (or
another window) mutates, the UI does not learn about it. The workaround is
explicit refetching — `project.reload()` scattered across `jobs`,
`generation`, `loopjam`, `hum` and `dev-drive`, and in `mcp.svelte.ts` a
`setTimeout(..., 400)` reload after an approved tool call with the comment
*"the approved tool may have changed anything"*. MCP status itself is polled
every 5 s.

### 2.5 The D-03 op-log design intent

Quoted from `SCALABILITY.md` §5:

> The frozen surface is one-invoke-per-mutation … at FL scale it fails on three
> axes: **rate** (knob drag = 200 invokes/s), **atomicity** (multi-object
> edits: "delete 50 clips" must be one undo entry and one engine swap), and
> **sync** (two windows / script API / collaborative future need a single
> ordered mutation stream).
>
> * `ops_apply(batch)` — one invoke carrying `{baseRev, ops: [...]}`; applied
>   atomically by the control thread; returns `{rev, results}`.
> * `ops_subscribe(channel, sinceRev)` — Channel streaming committed batches …
>   the UI applies deltas to its local mirror instead of re-fetching.
> * Monotonic `rev` (u64) per project session gives: optimistic UI, delta-sync
>   after reconnect, undo (inverse batches), journal (§4) — **one concept, four
>   features.**

And the undo intent (`SCALABILITY.md` §4):

> * **Command pattern at the op level** … each op carries its inverse or enough
>   state to derive it (`SetTrackGain{track, old, new}`); undo stack = list of
>   op batches (one batch per user gesture — a whole knob drag is one entry).
> * Undo history is per-project, survives save (persisted as journal segments),
>   and is **bounded by size, not count**.
> * Engine interaction: undo/redo *replays ops* through the normal control-plane
>   path … **The RT thread has no concept of undo.**

The envelope is fully specified as a DRAFT in
`docs/ipc-schemas/op-envelope.schema.json` — `rev`, `baseRev`, `ops[].kind`
(pattern `^[a-z][a-zA-Z0-9]*\.[a-z][a-zA-Z0-9]*$`), `target`, `transient`,
`label`, `origin`, with `additionalProperties: true` on op payloads
deliberately, to avoid re-creating D-06 in the new protocol. Grepping the tree
for `ops_apply|ops_subscribe|applyBatch|committedBatch` returns only the docs
and the schema. **Zero implementations.**

### 2.6 MCP registry and policy

The registry is three `const` string tables (`mcp/tools.rs:21-48`): ten tool
name constants, `ALL_TOOLS`, and `READ_ONLY_TOOLS = [get_project_state,
read_meters, get_job_status]`. Dispatch is rmcp's `#[tool_router]` macro over
`impl AuraMcpHandler` (`mcp/handler.rs`), with `inputSchema` derived from
`schemars::JsonSchema` on **hand-copied mirror structs** — a third copy of each
shape, because the `control` types live in frozen files that do not derive
`JsonSchema`.

The policy gate (`mcp/policy.rs:72`) runs before *every* tool body:

```rust
pub enum PolicyMode { ReadOnly, ConfirmDestructive, Full }   // :16
pub enum ToolRule   { Allow, Confirm, Deny }                  // :29
pub enum Decision   { Allow, Confirm, Deny }                  // :37
```

Unknown tool ⇒ `Deny`. Per-tool overrides win. Otherwise: read-only ⇒ allow;
destructive under `ReadOnly` ⇒ deny, under `ConfirmDestructive` (the default) ⇒
park the call, emit `mcp://confirm-requested`, await a `tokio::oneshot` with a
60 s timeout where timeout ⇒ deny; under `Full` ⇒ allow.

Adding a tool means editing three places (`tools.rs`, `handler.rs`,
`ControlPlane`) plus the docs; one test
(`handshake_lists_all_tools_with_hints`) enforces that `tools/list` matches
`ALL_TOOLS` and that hints and input schemas are present. That test is the only
registry/handler consistency check.

Security (`ARCHITECTURE.md` §12.3, MANDATORY): 127.0.0.1 only, 256-bit bearer
token regenerated per launch and constant-time compared, written 0600, Origin
validation before dispatch, policy layer. Transport is a hand-rolled HTTP/1.1
implementation (`mcp/http1.rs`) because the frozen `Cargo.toml` has no
hyper/axum.

### 2.7 There is no undo, redo, history or journal

Grepping `src-tauri/src` for `undo|redo|history|journal|revision` returns two
incidental hits: a doc comment in `midi/mod.rs` ("the returned clip is the
undo-friendly full value") and a `.rev()` on an iterator. The frontend returns
zero.

This is **enforced policy, not an oversight**:

* `ARCHITECTURE.md` §10 rule 9: *"Do not implement undo/redo ad hoc (UI-local or
  engine-local). Undo arrives with the batched op-log protocol… the prototype
  ships without undo rather than with a throwaway one."*
* `SCALABILITY.md` §4: *"Do NOT implement 'undo' ad hoc in the Svelte stores
  (UI-local undo diverges from engine state the first time two windows or a
  script touch the project)."*
* `CONTRIBUTING.md`: *"Undo/redo arrives with the op-log protocol (D-03). Don't
  build a local one."*

Adjacent facts a designer will meet: **autosave-on-mutation exists and is
unconditional** — `project::save` fires after record-stop, after import, after
loopjam apply, after every plugin instantiate/remove/set_param, after every midi
mutation, after every automation mutation. So `project.json` is already
overwritten before an undo could run, and there is no `.bak` except the v1→v2
migration one.

### 2.8 Ownership and locking

One big struct behind a `parking_lot::Mutex`, plus RT atomics, plus one actor
thread. `audio/types.rs:197`:

```rust
pub struct Store {
    pub transport: TransportState,
    pub tracks: Vec<TrackState>,
    pub clips: Vec<Clip>,
    pub project_dir: Option<PathBuf>,
    pub project_name: Option<String>,
    pub created_at: Option<String>,
    /// track id -> RT parameter slot (0..MAX_TRACKS).
    pub slots: HashMap<String, usize>,
    slot_used: [bool; MAX_TRACKS],
}
```

Owner of record is `AudioState` (`audio/mod.rs`), whose `control_parts()` hands
out clones of three `Arc`s — which is how `ControlPlane` *and also* `midi`,
`plugins` and `automation` all get at the store.

**Five separate locks, none coordinated:**

1. `Arc<Mutex<Store>>` — tracks, clips, transport mirror, project dir, slots.
2. `Arc<Mutex<MidiStore>>` — ppq, tempo map, midi clips; also published to a
   process-global registry.
3. `Arc<Mutex<PluginRegistry>>` + `Arc<Mutex<AutomationStore>>` — also
   process-global.
4. `Arc<Mutex<SamplerBank>>` — process-global.
5. `McpShared` — policy, pending confirmations, waiters.

There is **no single "project state" lock**; a batch spanning tracks + midi +
plugins cannot be made atomic today. The lock ordering convention is store →
midi, documented in comments (`midi/mod.rs`, `control/mod.rs`) and enforced by
nothing.

The actor is the engine control thread (`aura-engine-control`,
`audio/engine.rs:370`), driven by a `crossbeam_channel` of `ControlMsg`
(`engine.rs:75`): `Subscribe`, `Rebuild`, `SelectOutput`, `SelectInput`,
`StartRecording`, `StopRecording`, `Shutdown`. It holds an `Arc<Mutex<Store>>`
clone and writes to it.

### 2.9 How IPC is typed

**Hand-written on both sides, JSON Schema as the nominal source of truth,
nothing generated and nothing enforced.**

* Schemas: `docs/ipc-schemas/*.json` (18 files), declared source of truth
  (`ARCHITECTURE.md` §3.3).
* Rust: plain serde structs with `#[serde(rename_all = "camelCase")]` and a doc
  comment naming the schema (`/// Mirrors docs/ipc-schemas/…`).
* TypeScript: `src/lib/types/ipc.ts`, 584 lines, header *"Hand-written
  TypeScript mirrors of docs/ipc-schemas/*.schema.json."*
* `src/lib/tauri.ts` (565 lines) is a hand-written `Backend` interface plus a
  `TauriBackend` with literal command-name strings; `DemoBackend`
  (`src/lib/demo.ts`, 2454 lines) is a second full implementation.

No JSON-Schema validation test exists in Rust or TS. No `ts-rs`, `specta` or
`typeshare`. No test asserts that `lib.rs`'s `generate_handler!` list matches
`Backend`'s methods. Consequences already visible in-tree: `set_track_mix`
exists in Rust and MCP but is **absent from `Backend`**, so the UI still fires
one invoke per property; and `ImportSplitReply` in Rust does not match its TS
declaration.

---

## 3. The project model and persistence

### 3.1 Where everything lives

| Concern | File |
|---|---|
| v1 typed model | `src-tauri/src/audio/types.rs` |
| v1 read/write/validate/atomic-save | `src-tauri/src/audio/project.rs` |
| v2 MIDI model (tick domain) | `src-tauri/src/midi/types.rs` |
| v2 read-modify-write of `project.json` + AMEV chunks | `src-tauri/src/midi/persist.rs` |
| AMEV binary chunk container | `src-tauri/src/midi/events.rs` |
| Plugin instance rows + APST blobs | `src-tauri/src/plugins/state.rs` |
| Automation lanes + point chunks | `src-tauri/src/plugins/automation.rs` |

**There are five independent writers into one `project.json`** *(CORRECTED
2026-08-13: five writing subsystems, but only **two** distinct atomic
tmp+fsync+rename implementations for project.json — `audio/project.rs:110`
and `midi/persist.rs:258`; plugins and automation both route through
`midi::persist::update_project_v2`. The consequence — fields added outside
`update_project_v2`'s awareness are silently dropped — stands unchanged)*.
That is the single most important architectural fact about the format.

### 3.2 The schema

`Project` (`audio/types.rs:173`) carries `schema_version`, `name`, optional
`path`/`created_at`/`modified_at`, `sample_rate`, `tempo_bpm`, optional
`time_signature`, `tracks`, `clips`, optional `transport`.

`TrackState` (`audio/types.rs:103`) is the entire track model — ten scalar
fields:

```rust
pub struct TrackState {
    pub id: String,
    pub name: String,
    /// "audio" (future: "midi", "bus")
    pub kind: String,
    pub gain_db: f64,      // -inf encoded as -160.0
    pub pan: f64,          // -1.0 .. 1.0
    pub muted: bool,
    pub soloed: bool,
    pub armed: bool,
    pub color: String,     // "#rrggbb"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument_id: Option<String>,
}
```

No parent, no ports, no fx chain, no routing, no lanes, no height, no freeze.

`Clip` (`audio/types.rs:148`) is sample-domain only: id, track_id, name,
source_path (relative, POSIX), source_channels/sample_rate/length_samples,
timeline_start_samples, offset_samples, length_samples, gain_db,
fade_in_samples, fade_out_samples. No `contentRef` indirection, no take group,
no clip kind, no loop or warp fields.

`MidiNote` (`midi/types.rs:31`) is tick, length_ticks, key, velocity, channel.
`MidiClip` (`midi/types.rs:53`) is id, track_id, name, timeline_start_ticks,
length_ticks, notes. `DEFAULT_PPQ = 960` (`midi/types.rs:16`).

Persisted-only rows with **no Rust "Project v2" struct at all**:
`PersistedPluginInstance` (`plugins/state.rs`), `PersistedLane`
(`plugins/automation.rs:203`), `PersistedClip` (`midi/persist.rs`).

On-disk v2 therefore has five top-level keys the typed struct knows nothing
about: `ppq`, `tempoMap`, `midiClips`, `plugins`, `automation`.

Directory layout (`ARCHITECTURE.md` §7): `project.json` (+ `.tmp`, `.v1.bak`,
`.corrupt.bak`), `audio/<clipId>.wav`, `stems/`, `events/<uuid>.bin`,
`automation/<uuid>.bin`, `plugins/<instanceId>.state`, `cache/`.

Binary containers: **AMEV** (`midi/events.rs`) — magic, version, **`columnMask`**,
ppq, count, then N×16-byte records `[tick u32][duration u32][kind u8][key u8]
[vel u8][chan u8][value f32]`; column-mask growth is the designed extension
path, and `decode_notes` currently ignores it. **APST** (`plugins/state.rs`) —
magic, version, kind (1 = CLAP opaque stream, 2 = LV2 property list), uid,
data.

### 3.3 Track types

Track type is a **plain `String` on one flat `Vec<TrackState>`**. No enum, no
flags, no separate vectors.

The schema enum is `["audio","midi","bus"]`
(`docs/ipc-schemas/track-state.schema.json`), but the validation whitelist
**rejects `"bus"`** (`control/ops.rs:106`):

```rust
if !matches!(kind.as_str(), "audio" | "midi") {
    return Err(format!("unsupported track kind: {kind}"));
}
```

with a test asserting `add_track(..., Some("bus")).is_err()`. Behavioural
dispatch on `kind` is scattered string comparison
(`audio/mod.rs`, `midi/mod.rs`, `control/import.rs`).

Hard cap `MAX_TRACKS = 64` (`audio/types.rs:13`), enforced by a `[bool; 64]`
slot bitmap (`:207`) *and* by `project::validate` (`audio/project.rs:125`) *and*
baked into `RawMeterBlock` (debt D-05).

### 3.4 Versioning, and the absent migration chain

The version field is `schema_version: u32`. The reader gate is a hardcoded
range (`audio/project.rs:161`):

```rust
if !(1..=2).contains(&project.schema_version) {
    return Err(format!("unsupported project schemaVersion {}", project.schema_version));
}
```

v1 → v2 differences: `const 1` → `const 2`; `additionalProperties: false` →
absent (D-06 settled); single `tempoBpm` → `ppq` + `tempoMap[]` with `tempoBpm`
retained as `tempoMap[0].bpm`; `midiClips[]` and `instruments[]` added; and —
notably — v2 **degrades `tracks`/`clips` items to bare `{"type":"object"}`, so
v2 no longer constrains track or clip shape at all**.

`SCALABILITY.md` §4 specifies "one migration module per step, pure
`Value → Value` functions, unit-tested against fixture projects". What exists
instead:

1. `midi::persist::v1_migration_defaults` — three lines, applied **in memory
   only**, never rewriting the file.
2. The file is upgraded **lazily and implicitly** by whichever v2 writer touches
   it first, by blindly inserting `schemaVersion: 2`.
3. A verbatim `project.json.v1.bak` is written once, on the first upgrade only.
4. Downgrade protection is an **overlay hack** in `audio::project::save`
   (`audio/project.rs:67`): serialize the typed v1 struct, read the existing
   file, and if it is already `schemaVersion >= 2`, merge the v1 keys *onto* the
   existing object and patch `tempoMap[0].bpm`. An unparseable file is preserved
   as `project.json.corrupt.bak`.

Tested: the seam (v1 loads as `None`; typed v1 save preserves v2 fields;
interleaved v2 writers; corrupt-file backup). **Not tested and not present:** a
fixture corpus, a migration-chain module, forward-compat (a v3 file in a v2
app), or any coverage of `plugins[]` / `automation[]` migration.

### 3.5 IDs

| Object | Generator | Stable across save/load? |
|---|---|---|
| Track | `Uuid::new_v4()` (`control/ops.rs`) | yes |
| Audio clip | v4 (import, recording, hum, loopjam) | yes |
| MIDI clip | v4 (`midi/mod.rs`, `midi/midifile.rs`) | yes |
| **Note** | **none** | **no — identity is the index in the chunk** |
| Plugin instance | v4 (`plugins/mod.rs`) | yes |
| Automation lane | v4 on upsert | yes |
| Sampler instrument | v4 **per load** | **no — regenerated every session** |
| Chunk file | v4 per write (content immutable once written) | n/a |
| Plugin *type* uid | `clap:<bundle>#<id>` / `lv2:<uri>` | yes |
| RT slot | dense `0..64`, **reused after free** | no, by design |

Uniqueness is enforced **only for tracks, only on open**
(`audio/project.rs:125`). There is no check for duplicate clip ids, duplicate
midi-clip ids, duplicate plugin ids, or dangling `trackId` references — a
dangling `trackId` silently produces an invisible, unplayable clip.

### 3.6 Assets and portability

* **Audio** is always copied into the project and referenced **relative** —
  `audio/<clipId>.wav`. Non-WAV sources are decoded and rewritten as f32 WAV.
* **Event/point/state chunks** are relative and traversal-hardened at read time
  (`eventsRef` must match `events/<name>.bin` with no `/` or `..`; tested by
  `malicious_events_ref_is_rejected`).
* **`path`** is the one absolute path written, rewritten on every load.
* **Sampler instruments are NOT portable.** `instruments[]` and
  `<project>/instruments/<id>/*.sfz` are specified in `project-v2.schema.json`,
  `sampler-instrument.schema.json` and `ARCHITECTURE.md` §14 — and
  `grep '"instruments"' src-tauri/src` returns **nothing**. No writer, no
  reader, no copy step. A track with a sampler `instrumentId` resolves to
  nothing after restart and silently falls back to PolySynth.

Move or copy a project: audio, MIDI, automation, plugin blobs and their refs all
survive; `path` self-heals; `cache/` is regenerable. Broken: sampler instrument
bindings (always, even without moving), and plugin availability when a
`clap:<bundle-path>#…` uid points at a different path on the target machine.

### 3.7 Plugin state persistence

Save (`plugins/state.rs`) fires on **every** plugin mutation via
`persist_after_mutation`, not on `save_project`. Priority ladder, designed never
to lose data:

1. live host state through `FormatStateBridge` (CLAP → `save_state`, opaque
   stream; LV2 → raw-lilv `state:interface` on a main-thread shadow instance) —
   only when `with_host_state`, so a knob drag never round-trips the plugin main
   thread;
2. else an existing on-disk blob is kept verbatim and stays referenced;
3. else a `pending_state` blob restored earlier is written back.

The param snapshot is written into the row unconditionally. Unreferenced
`*.state` files are GC'd after a successful write.

Restore happens **inside the MIDI loader** — `midi::persist::load_from_project`
calls `plugins::state::adopt_open_project` and
`plugins::automation::adopt_open_project` before it even checks the version.
Everything comes back as `status: "stub"` with params mirrored and the blob
parked in `pending_state`; `reactivate_restored_with` instantiates through the
format host, re-applies the clamped snapshot, then offers the blob.

Fallbacks, all non-fatal and all preserving: plugin missing on this machine ⇒
stays `"stub"`, renders silence, row + params + blob preserved forever;
corrupt blob ⇒ logged, left on disk, instance restores without state; malformed
row ⇒ skipped per-row; blob uid ≠ row uid ⇒ warn but keep; `plugins` key absent
⇒ registry untouched; unresolvable `plugin:` ref at graph build ⇒ PolySynth.

`PersistedPluginInstance` has **no `track_id`**; the binding lives one-way in
`TrackState.instrumentId`.

### 3.8 Schema drift

Nothing enforces schema/struct agreement. No JSON-Schema validator crate, no CI,
and the four "enforcement" tests are hand-written field-name assertions that
never read the schema files. `CONTRIBUTING.md` is explicit: *"keep the Rust/TS
types in sync **by hand**"*.

Concrete drift found:

1. **`instrumentId` violates a frozen schema** — `track-state.schema.json` has
   `additionalProperties: false` and no `instrumentId`; `TrackState` emits it.
   A strict v1 validator rejects every AURA-written track with a bound
   instrument.
2. **`plugins[]` is in no project schema.** Written by `plugins/state.rs`;
   `project-v2.schema.json` never mentions it.
3. **`automation[]` is in no schema file at all.** Its only spec is a jsonc
   sketch in `SCALABILITY.md` §3 and the module doc.
4. **`persistedInstance` lacks `name` and `format`**, which the Rust struct
   writes.
5. **`instruments[]` is specified but never written** (§3.6) — drift in the
   other direction.
6. **`project.schema.json` no longer describes any real file**: `const 1` +
   `additionalProperties: false`, while saved projects are v2 with five extra
   top-level keys.
7. **`Project.schemaVersion` is a lie on one path**: `from_store`
   (`audio/project.rs:179`) hardcodes `schema_version: 1`, so the
   `project://changed` payload after save/record/import says `1` while the file
   says `2`. The TS type declares `schemaVersion: 1` — wrong for opened
   projects.
8. **`timeSignature` is silently clobbered on every save.** `Store` has no
   time-signature field, and both `create` (`:47`) and `from_store` (`:189`)
   hardcode `Some((4, 4))`; the save overlay copies that onto the existing
   object. **A `[3,4]` project becomes `[4,4]` on the next autosave.**
9. `midi-clip.schema.json` declares a top-level `oneOf [clip, persistedClip]`
   whose branches overlap structurally, so strict `oneOf` validation fails on a
   `notes`-less clip.

### 3.9 Autosave, backup, crash recovery

**No autosave timer, no journal, no crash recovery.** `SCALABILITY.md` §4
specifies `<project>/.aura/journal.ndjson` plus a 2–5 min autosave snapshot;
grepping for `autosave|journal` finds only the doc text.

What exists is **write-on-mutation** by four writers (§3.1), producing the
autosave-before-undo-could-run problem noted in §2.7.

Backups are incidental, one-shot and never rotated: `project.json.v1.bak` once
at first upgrade, `project.json.corrupt.bak` when the existing file won't parse.

Durability is real per write (tmp + `sync_all` + rename in all three writers),
but the *directory* is never fsynced, and chunk writes are plain `fs::write`
with no fsync — so a crash between a chunk write and the manifest rename leaves
a referenced-but-unflushed chunk, which degrades to an empty clip or lane on
load, by design.

Recording is independently crash-safe (WAV streamed by the disk writer), but
orphan-take recovery is not implemented.

### 3.10 What blocks each extension

**(a) Nested track groups / folders — hard block, four layers.**
`Vec<TrackState>` is flat with **no `parentId`, no children, no order field**;
display order *is* vector order. `"bus"` — the only reserved container kind — is
actively rejected (§3.3). `MAX_TRACKS = 64` is shared with folders and baked
into the meter block. And `mixer::render` sums straight into `out` with no
intermediate bus buffers, so folder gain/mute/solo would have to be pre-flattened
into per-track params — which breaks the moment a folder hosts an effect.
`Store::any_solo()` is a flat `any()`, so hierarchical solo has nowhere to live.

**(b) Per-track effect chains with arbitrary routing — hard block.**
`RtTrack` has **one** optional instrument node, **zero** processors and **no**
input ports; `RtGraph` is a `Vec` plus a scratch buffer, not a node/edge graph.
Nothing in the persisted model can express a chain: `TrackState` has a single
`instrument_id`, and `PersistedPluginInstance` has no `trackId`, no slot index,
no ordering and no bypass flag — two effects on one track cannot be ordered.
`plugins::live_node_for` returns `Box<dyn LiveInstrument>`, the *instrument*
seam, not an insert seam. And `AudioProcessor` has no `latency_samples()`, so
PDC has no reporting surface (§1.4) — which matters because `ARCHITECTURE.md`
§2 warns PDC must land *before* sends ship.

**(c) Automation on arbitrary parameters — closest to ready; three gaps.**
The data model is already generic and tick-domain. But: lanes are never attached
during graph rebuild (§1.11); `targetNode` cannot address anything that is not a
track's live instrument or a plugin instance, because faders, pans, clip gains,
sends and buses **are not nodes** — they are `ParamTable` slots indexed by an
ephemeral RT slot; there is no param-descriptor persistence, so on restore
ranges are unknown and stub `ParamInfo` fabricates them; and lanes are
project-global with no lane→track association, so deleting a track leaks its
automation lanes and its plugin instances.

**(d) Chord/scale track — no home in the model.**
`kind` is whitelisted to `audio|midi` at creation and every consumer branches on
those two strings. A chord track carries *non-note musical events* (symbol,
root, quality, inversion, scale), and the only event container is AMEV's fixed
16-byte record — a chord symbol does not fit, so it needs a new `kind` byte plus
a new column block via `columnMask`, which is the designed path and is
unimplemented. And there is no global musical-context object at all: one
`timeSignature`, hardcoded `[4,4]` and clobbered on save (§3.8), no key, no
scale, no signature *map*.

**(e) Markers / arrangement sections — no container.**
`Project` has no `markers`, `regions` or `sections`; the only timeline-global
object is the single sample-domain loop region inside `TransportState` (debt
D-10). A new top-level array needs a **fifth** writer, and the save overlay only
preserves unknown keys when the file is already v2 — so a marker writer must go
through `update_project_v2` or its data is dropped by the next typed v1-path
save, a rule discoverable only from that function's doc comment.

**Cross-cutting:** `project::validate` is fifteen lines and checks two things,
so every new cross-reference is unvalidated on load; and each subsystem GCs *its
own* directory against *its own* in-memory store, so a new object type sharing a
directory will have its chunks silently deleted.

---

## 4. The frontend

~14.4k lines. Largest files: `demo.ts` 2454, `PianoRoll.svelte` 1007,
`Timeline.svelte` 755, `types/ipc.ts` 584, `tauri.ts` 565.

There is **no automation component and no audio editor** — the "generalised
arranger" has two concrete instances (timeline lanes, piano roll) and one
half-instance (the velocity lane inside PianoRoll).

### 4.1 State modules

Sixteen modules under `src/lib/state/` *(CORRECTED 2026-08-13: prose said
fifteen; the table below and the directory both count 16)*. **All are
process-wide singletons** —
a class with `$state` fields instantiated once at module scope
(`export const project = new ProjectStore()`). No context, no provider, no
factory, so a second timeline or piano roll is structurally impossible.

| Module | Owns |
|---|---|
| `project.svelte.ts` (183) | name, sampleRate, tempoBpm, timeSignature, tracks, audio clips, selectedClipId, projectDir; derived samplesPerBeat/Bar |
| `view.svelte.ts` (62) | timeline viewport only: `spp`, `viewStart`, `width`, `snap`, `follow` + `xOf`/`samplesAt`/`zoomAt`/`snapSamples` |
| `transport.svelte.ts` (163) | transport snapshot + wall-clock playhead extrapolation, loop region, drift re-anchor |
| `midi.svelte.ts` (175) | ppq, tempo map, MidiClips, openClipId, region, flash; **tick↔sample bijection** |
| `meters.svelte.ts` (45) | **deliberately non-reactive** meter bus, read imperatively per frame |
| `jobs` (270), `generation` (308), `hum` (158), `plugins` (245), `zynpatches` (152), `instruments` (69), `exporter` (122), `loopjam` (80), `mcp` (123) | feature state |
| `toasts` (50), `ui` (23) | leaves |

The dependency graph is acyclic and shallow.

### 4.2 Three mutation patterns, and no unifying layer

**(a) Optimistic local + fire-and-forget push.** `project.setGain` patches the
local track then `await backend.setTrackGain(...)` with the return value
discarded. No rollback on failure.

**(b) Await-and-adopt.** `transport.play/pause/seek/setLoop`; `midi.setNotes`
patches then overwrites with the returned clip; `plugins.setParam` is optimistic
and rAF-batched into one `plugin_set_param` per frame.

**(c) Local-only, never persisted.** **Clip moves.** `project.moveClip`
(`src/lib/state/project.svelte.ts:165`) and `midi.moveClip`
(`src/lib/state/midi.svelte.ts:145`) mutate the array and stop. The file header
says why (`project.svelte.ts:4`): *"no clip-move command exists in the frozen
surface"*.

**(d) Echo / re-pull.** `project://changed`, `recording://state`,
`loopjam://state`, `export://*`, `mcp://confirm-requested`, plus a blunt
`project.reload()` after imports, generations, loop-jam applies and MCP
approvals.

**(c) and (d) are incompatible.** Any `project.reload()` replaces `clips`
wholesale and silently discards every un-persisted clip drag. There is no
command object, no dirty tracking, and no undo anywhere. The only chokepoint is
the `Backend` interface — a flat ~70-method RPC facade, which is *transport, not
intent*. Agent mutations never traverse the frontend at all.

### 4.3 Timeline.svelte

755 lines, of which only ~354 are script (278 are CSS). *Medium*-god: it holds
every arrangement-level concern except clip drag — viewport width via
`ResizeObserver`, ruler canvas painting with adaptive bar stepping, lane grid
painting, the playhead rAF loop **and the only auto-follow paging in the app**,
wheel zoom/scroll, ruler scrub/seek, the loop-region create-drag and two pin
drags, loop-jam visual state, track creation, demo seeding, and MIDI lane
double-click to create a clip.

Structural smells for generalisation: track height `88` hardcoded in the paint
loop while `app.css` also defines `--track-height: 88px`; per-clip-type `{#each}`
blocks, so adding automation lanes means editing Timeline; and
`{#if backend.seedDemoProject}` leaking demo-mode branching into layout.

### 4.4 Four coordinate systems

1. **samples ↔ CSS px** — `view.svelte.ts`, `xOf(s) = (s - viewStart)/spp`.
2. **samples ↔ bars/beats** — `project.samplesPerBeat/Bar`, with Timeline
   recomputing bar iteration inline **twice** and `utils/format.ts` computing
   bars/beats/16ths a **third** time for the transport readout.
3. **ticks ↔ samples** — `midi.ticksToSamples/samplesToTicks`, piecewise over
   the tempo map, mirroring Rust `TempoMap`. The only tempo-correct conversion.
4. **ticks ↔ px (piano roll)** — entirely component-local: `tpp`, `scrollTick`,
   `topKey` are plain `$state` locals with closures. **Not in any store**, so
   the piano-roll viewport is destroyed when the clip closes, cannot be synced
   to the timeline, cannot be driven by an agent, and cannot be tested.

`MidiClipView` chains 3 and 1 to place a MIDI clip on the timeline. Note the
asymmetry: audio clips are sample-native, MIDI clips tick-native, the timeline
sample-native — so a MIDI clip drag does px→samples→**snap in samples**→ticks,
snapping in the wrong domain.

Zoom math is duplicated verbatim between `view.zoomAt` and PianoRoll's local
anchor-preserving zoom.

### 4.5 Three disjoint selection models

* `project.selectedClipId: string | null` — single audio clip.
* `midi.selectedClipId` + `openClipId` + `region` — single MIDI clip plus an
  editor-open clip plus a tick region.
* `PianoRoll.selection: Set<number>` — **indices into a component-local
  `working: MidiNote[]` array**. Index-based, so it silently aliases after any
  filter or splice; invisible outside the component; wiped when the clip
  changes.

No shared "selected objects" concept, no multi-select on the timeline, no track
selection. Mutual exclusion is hand-wired in one direction only — `MidiClipView`
clears the audio selection, `ClipView` does **not** do the inverse, so both can
appear selected at once.

### 4.6 Seven hand-rolled drag implementations

Every gesture re-implements `pointerdown/move/up` + `setPointerCapture` (in a
try/catch for synthetic pointers) + a 2 px threshold + `altKey` snap bypass:
`ClipView` clip drag; `MidiClipView` clip drag (whose comment says it *"mirrors
ClipView"*); Timeline ruler scrub; Timeline loop create-drag; Timeline loop pin
drag; PianoRoll grid (move/resize/marquee/draw); PianoRoll velocity lane. Plus
piano-key audition.

Only PianoRoll has a typed gesture union and a draft/commit discipline (a
`working` array committed as one `midi_set_notes` per gesture, per the D-03
batching rule). Clip drags have no draft/commit at all. Keyboard nudge is
duplicated too — ±one beat in samples vs ±one ppq in ticks.

### 4.7 Two snapping systems

* **Timeline:** `view.snapSamples`, global `view.snap` toggle, resolution
  *implicit and zoom-adaptive* (quarter-beat when a beat exceeds 80 px, else
  beat).
* **Piano roll:** `snapTick(t, floor)` against `gridTicks = ppq/gridDiv`, with an
  *explicit* user choice 1/4…1/32. It ignores `view.snap` entirely — there is no
  way to disable snap in the roll except holding Alt.
* A third rule: Timeline snaps new MIDI clips to whole bars.

No shared grid/quantize concept, no triplets or dotted, no swing, no per-lane
override.

### 4.8 Rendering

A painter abstraction exists **only for waveforms** — `render/painter.ts`
defines `WaveformPainter`, `createPainter` probes WebGPU once and falls back to
canvas2d, and one painter instance is created **per clip canvas**. Tiles
(`render/tiles.ts`) parse AWTF, LRU-cache promises keyed `clipId/lod/tileIndex`
(cap 512), and assemble per-pixel min/max columns. `evictClip` exists and **is
never called**.

Everything else is bespoke canvas2d inside components: timeline ruler, lane
grid, piano-roll grid/notes/marquee, piano keys, velocity lane, flash overlays,
MIDI mini-preview, VU meters. DPR-setup boilerplate is copy-pasted at 6+ sites;
hex→rgba at 4.

Repaint is two-tier by explicit convention: `$effect` blocks for state-driven
paints (several using `void x, y, z` purely to register dependencies), and raw
`requestAnimationFrame` loops for the hot path that never read runes and write
DOM directly.

**60 fps is by convention, not architecture.** There are **nine independent rAF
loops** (playhead ×2, timecode, meters, note flash, MIDI clip pulse, …) **plus
one per visible MIDI clip** — no shared ticker, no frame budget, no priority.
`ClipView.scheduleDraw` has a `drawToken` to drop stale async tile results but no
debounce, so every zoom/scroll frame re-issues `buildPeakColumns` per visible
clip. Culling is a single visibility guard; there is no lane virtualization, and
there is one DOM div + canvas + painter per clip.

### 4.9 The demo backend

`DemoBackend implements Backend` (2454 lines), selected at module load by
sniffing `__TAURI_INTERNALS__`. Nothing else in the app knows which is live.

It fakes a transport clock anchored on `performance.now()` **with the engine's
exact loop-wrap rule**, 60 Hz meter frames derived from the *same* `envelope()`
function that generates waveform tiles (so meters and drawn waveform agree),
bit-identical AWTF tiles with deliberate 2–10 ms latency injection, real
WebAudio playback, a demo song, instruments, plugin descriptors, the Zyn bank,
staged sidecar jobs, hum-to-song, export, loop-jam, and a *scripted* MCP
confirmation sequence.

Escape hatches leak into the interface as optional methods
(`seedDemoProject?`, `onFileDrop?`, `hintClipCharacter?`, `registerClip?`) and
components branch on them.

**Implication: a complete, deterministic, seeded fake engine already exists, and
it is unused for testing.** Coordinate math, snapping, gesture reducers and
repaint counts could all be covered in jsdom against `DemoBackend` with zero
Rust — provided that logic moves out of `.svelte` files into plain `.ts`. That
is exactly the pattern `src/lib/utils/timeline-nav.ts` establishes at HEAD:
pure functions, no transport, no project, no DOM, tested directly.

### 4.10 Where Timeline/PianoRoll duplication is worst

Ranked by what an abstraction would recover:

1. **Viewport + zoom/scroll** — same anchor-preserving zoom, same shift/deltaX
   scroll branch, one in samples/px and one in ticks/px; PianoRoll's is
   component-local so it cannot be shared, tested or persisted.
2. **Clip drag** — `ClipView` vs `MidiClipView`, near line-for-line clones.
3. **Grid painting** — identical loop structure and "hide subdivisions below N
   px" heuristic, over samples vs ticks.
4. **Playhead rAF overlay** — same pattern; Timeline additionally owns
   auto-follow, which the roll lacks, so the roll silently loses the playhead
   when scrolled.
5. **Snapping** — incompatible policies (§4.7).
6. **Canvas DPR setup** — 6 copies.
7. **hex→rgba** — 4 copies.
8. **Note-onset flash decay** — same magic constant and same `exp(-ageSec*k)`
   law, computed independently in two places.
9. **Bars/beats formatting** — three implementations.

---

## 5. The documented architecture and its debt

### 5.1 The document map

| Path | Role |
|---|---|
| `docs/README.md` | 12-line reading map |
| `docs/ARCHITECTURE.md` | the system as built; 723 lines, §0–§15; normative |
| `docs/SCALABILITY.md` | prototype→FL-class roadmap per axis, ending in the debt register; 676 lines |
| `docs/PHASE2-PLAN.md`, `docs/PHASE3-PLAN.md` | executed build-round plans; process history |
| `docs/mcp-usage.md` | operator-facing MCP guide |
| `docs/synth-compatibility.md` | empirical per-synth verdicts from the acceptance harness |
| `docs/ipc-schemas/` | 18 JSON Schemas, the wire source of truth |
| `CONTRIBUTING.md` | the rules distilled for humans |

**House conventions.** Sections `## N. Title`, subsections `### N.M Title`;
module-scoped sections append the owning path and the round
(`## 14. Sampler instrument (…) — phase 2, zone D`). ASCII box diagrams for
topology; tables for anything enumerable. Bold ALL-CAPS status words inline:
`FROZEN`, `MANDATORY`, `READ FIRST`, `AS BUILT`, `(implemented)`. Every rule
carries its reason inline, usually with a concrete number ("at 48 kHz with a
128-frame buffer it has ~2.6 ms"; "a knob drag is 200+ updates/s"). Honesty
markers are a deliberate device: *"Isolation (honest v1 risk)"*, *"Honest status
summary:"*, *"we have not run this model for real for that reason"*. Prose is
hard-wrapped at ~76 columns.

`SCALABILITY.md` declares its own per-axis template up front: **Current state
(keep)** / **Do NOT do now** / **Migration path**, with the migration path as a
numbered list whose done steps are annotated in bold inline.

**The debt-item template**, exactly. Items are `D-NN`, monotonically assigned,
never renumbered, never deleted, in one table under `## 8. Debt register`:

```markdown
| ID | Where (frozen artifact) | Decision | Why it blocks scaling | Severity | Proposed fix (additive/versioned) |
|----|---|---|---|---|---|
| D-05 | ARCHITECTURE §2.3 `RawMeterBlock { [f32; MAX_TRACKS*2] }` | Compile-time track cap in the RT meter element | Hard ceiling on track count; big constant wastes queue space | Medium | Chunked meter blocks (fixed-size element carrying `{firstSlot, count, values[N]}`), multiple pushes per callback; internal only, no IPC change. |
```

Severity vocabulary: `**High** (before device-preference persistence)`,
`Medium (survivable to ~100 tracks)`, `Low (prototype scope fine)` — High is
bolded, and the parenthetical names the *trigger event*, not a date. **Status is
recorded by editing the "Proposed fix" cell in place, prepending a bolded
verdict** (`**PAID DOWN (phase 2, zone C + architect merge)**`, `**Partial
(phase 2)**`, `**HALF PAID … OPEN: runtime isolation**`) — never by moving or
deleting the row, and always mirrored by an inline annotation in the body
section that owns it.

### 5.2 The stated principles

**Ownership (§0).** Four agents, strict exclusive write access, and a FROZEN
file list nobody edits (`lib.rs`, `Cargo.toml`, `tauri.conf.json`,
`capabilities/default.json`, `build.rs`, `package.json`, `vite.config.ts`,
`svelte.config.js`, `tsconfig.json`, `index.html`). Later rounds added
`docs/` and the phase-3 zones. *"Do not edit frozen files 'just this once'."*
Command names and event names are **the frozen API surface**; signatures may
evolve inside the owning module, names may not.

**The RT contract (§2.1).** Never allocate; never lock; never syscall (*"no
channel `send` that can wake a thread via futex"*); no panics, no unbounded
loops — each with its reason stated. §2.3 adds the RCU law with **no
exceptions, not even single-track edits** (§10.1).

**§10 rules 2–3.** Mix setters route to **parameter changes** *"(preallocated
param ring or atomics, smoothed in the node) — never to a graph rebuild"*; on
the RT thread objects are **dense slot indices**, and *"UUID strings, `String`,
`HashMap`, and `log::` never cross onto the RT thread"*.

**Schema discipline.** Schemas are *emitter* contracts; readers must tolerate
and preserve unknown fields (*"never `deny_unknown_fields`"*). New schemas must
not set `additionalProperties: false`. Newer-major file + older app ⇒ refuse
with a clear message, *"never best-effort-parse someone's session"*.
**Any new mutation surface must be batch-shaped `{ops: [...]}`** (D-03).

**Cadence.** *"Never send per-audio-buffer messages to the UI"* — labelled an
iron law. And: *"Do NOT introduce a second event bus … mutation fan-out is the
op-log's job."*

**Storage rule of thumb:** *"if it can exceed 10k elements, it gets a chunk file
and a `...Ref` in the manifest."*

**The control-plane rule (§11):** *"MCP tool handlers call `ControlPlane`
methods only. Never Tauri commands, never module internals."*

**No ad-hoc undo** (§10.9, `CONTRIBUTING.md` rule 4) — §2.7 above.

**Frontend (§10.11–13):** stores must be **delta-ready** (apply the returned
object to a keyed store rather than refetching), and lists that scale with
project size must be **virtualized from the start**.

### 5.3 The debt register, D-01 … D-12

| ID | Title | Status | Summary |
|---|---|---|---|
| D-01 | Device identity = cpal display name | **OPEN** (High) | Names collide and shift with hotplug/locale; fix = backend-qualified stable id + optional `backend` field |
| D-02 | Sample-only timeline, single global `tempoBpm` | **PAID DOWN**; time-signature map tracked | `midi::TempoMap` + project v2 + live instrument nodes shipped |
| D-03 | One invoke per mutation, no batch/atomicity/revision | **PARTIAL** (High) | Op-log is the fix; only `set_track_mix` is batch-shaped. **Blocks undo.** |
| D-04 | JSON meter frames, all tracks, fixed 60 Hz | **OPEN** (Medium) | Additive `{trackIds, maxHz}`, later a binary frame variant |
| D-05 | `RawMeterBlock` compile-time track cap | **OPEN** (Medium) | Chunked meter blocks; internal only |
| D-06 | `additionalProperties: false` everywhere | **SETTLED** (phase 2) | Schemas are emitter contracts; new schemas ship without it |
| D-07 | Closed `kind` enums in sidecar jobs | **PAID DOWN** (phase 2) | Open `kind` string + opaque params |
| D-08 | `sourceChannels` max 2 | **OPEN** (Low) | Engine buffers already channel-counted; only the schema bound moves |
| D-09 | One clip per take; no take lanes/comping | **OPEN** (Low now) | Additive `takeGroupId` + `laneIndex` |
| D-10 | Single loop region; closed `state` enum | **OPEN** (Low) | Additive `mode: "song"\|"pattern"` + punch region |
| D-11 | CLAP scan `dlopen`s bundles; plugin DSP in-process | **HALF PAID** (High; runtime half OPEN) | Sacrificial scan worker shipped; **per-instance out-of-process bridge behind `LiveNodeCell` is the roadmap item** |
| D-12 | LV2 host links system `liblilv` | **PARTIAL** — state half PAID, packaging open | Documented prerequisite; vendoring considered |

Relevance map for the work ahead:

| Concern | Binding debts |
|---|---|
| Routing graph (buses, sends, sidechain, inserts) | D-05 (slot cap in the meter element), D-08 (stereo ceiling), D-03 (born batch-shaped), plus the PDC-before-sends ordering rule |
| Automation | D-02 (ticks, mandatory), D-03 (knob-drag batching), D-06, plus `set_block_context` and `plugins/automation.rs` |
| Undo/redo | **D-03 is the gate.** §10.9 and `CONTRIBUTING.md` rule 4 forbid an ad-hoc undo; journal/autosave is the same design |
| Plugin isolation | **D-11 open half** — highest-severity open item; `LiveNodeCell` is the named insertion point. D-12 for packaging |
| Track types | D-02, D-08, D-09, D-10; `"bus"` and `"midi"` already reserved in the v1 schema |
| UI scaling | D-04, D-05, D-03 (op-subscribe deltas), §10.11–12 |
| Project format | D-02, D-06, D-08, D-09, D-10, D-12, plus the v2 additive rules of §3/§4 |

### 5.4 Every seam the docs promise

1. **RCU prepare-then-swap** (§2.3, §10.1) — *"generalizes unchanged from 'list
   of tracks' to 'arbitrary graph'"*; node state that survives a swap is *"keyed
   by stable node ID and **moved, not copied**"*. → `audio/rt.rs`,
   `audio/engine.rs`, `audio/mixer.rs:210`.
2. **`AudioProcessor` / `TimeStretcher` node abstraction** (§8) — allocation in
   `prepare` on the control thread; every processor must report
   `latency_samples()` *"so plugin-delay compensation can be a graph-compiler
   feature, not a per-node retrofit"* (§10.5). → `audio/dsp.rs`. **The latency
   half is unimplemented.**
3. **`LiveInstrument` + `LiveNodeCell` + `RtGraph.scratch`** (§15.1) — the
   marquee seam; *"a plugin instance must never be re-instantiated per
   rebuild"*. → `audio/dsp.rs:50`, `audio/rt.rs:205`, `midi/playback.rs`.
4. **The discontinuity contract** (§15.1) — release, not kill; loop wraps split
   the run. → `audio/engine.rs:235`, `audio/mixer.rs:121`.
5. **The automation ramp seam — `set_block_context`** (§15.1) — *"Lane→node
   attachment during graph rebuild is the next automation round."* →
   `audio/dsp.rs:53`, `audio/mixer.rs:177`, `plugins/automation.rs`.
6. **The RT boundary seam** (§2.6) — `transport::crossing`, `SharedRt::song_end`,
   `RtEvent` over `engine_evt`, and the park handshake; *"New 'the engine saw
   something' features become new `RtEvent` variants, not new branches in
   `OutputCb::render`."* Explicitly **not** for automation: *"dense per-sample
   curves belong in the RCU graph snapshot next to the clips, not in atomics."*
   → `audio/transport.rs`, `audio/rt.rs:23`/`:42`/`:57`, `audio/engine.rs:204`.
7. **The op-log seam** (D-03, §5) — the biggest *unbuilt* seam.
   → `docs/ipc-schemas/op-envelope.schema.json` only.
8. **The batch-shaped `set_track_mix`** (§11) — *"the shape any new mutation
   surface must copy"*: `Option<T>` per field, whole-batch validation before any
   write. → `control/ops.rs:22`, `:47`.
9. **The control plane as the single seam** (§11) — → `control/`.
10. **`plugins::live_node_for`** (PHASE3 contract 3) — the engine-facing
    resolution hook, signature frozen. → `plugins/mod.rs`.
11. **The shared plugin main thread** (§15.3) — ONE `aura-plugin-host` thread
    serving both formats. → `plugins/host.rs`.
12. **The out-of-process bridge insertion point** (D-11) — *"The `LiveNodeCell`
    indirection is the bridge's insertion point."* Crash semantics
    pre-specified: node emits silence + UI badge, never a lost project; the
    `"crashed"` status is already reserved. → `audio/rt.rs:205`,
    `plugins/host.rs` (`StubPluginNode` is the silent-node precedent).
13. **The sacrificial-subprocess pattern** (D-11 scan half) — re-exec guarded at
    the top of `main.rs`, NDJSON on stdout. → `src-tauri/src/main.rs`,
    `plugins/scan_worker.rs`.
14. **The plugin state seam** (`FormatStateBridge`) — chunked opaque blobs,
    never inline JSON; *"nothing is ever dropped"*. → `plugins/state.rs`.
15. **Additive track-instrument binding** — `"plugin:<instanceId>"`, unknown refs
    fall back to PolySynth. → `audio/mod.rs:519`.
16. **Ticks ↔ samples bijection** (§13) — *"the RT thread never sees ticks"*;
    AMEV column-bitmap headers so old readers skip unknown columns. →
    `midi/tempo.rs`, `midi/schedule.rs`, `midi/events.rs`.
17. **Meter seams** (D-04/D-05) — same command name, additive options, later a
    binary frame. → `audio/meters.rs`.
18. **Sidecar job seams** (§6, D-07) — a queue object rather than spawning in
    command handlers; opaque per-kind params. → `sidecars/jobs.rs`.
19. **The audio backend seam** (D-01, §7) — an `AudioBackend` trait with stable
    device ids, negotiated actuals, xrun/latency and hotplug; *"cpal is then
    one implementation, not the abstraction itself"*. → **specified, not built**;
    cpal usage is concentrated in `audio/engine.rs`.
20. **The pattern/placement data model** (§3) — `Pattern` as a reusable
    tick-domain event container vs `Clip` generalised to a *placement*, *"which
    makes the pattern workflow a usage style of one data model rather than a
    second engine"*; SoA copy-on-write chunk ropes with per-chunk summaries as
    an implicit interval tree. → **specified, not built**; `patternId` is
    reserved in `midi-clip.schema.json` and unimplemented.
21. **The offline render path** — `audio/offline.rs`, owning its own nodes so
    the `LiveNodeCell` contract holds. This is what makes acceptance tests
    headless and what export rides on.

### 5.5 The phase plans

**Phase 2** — four parallel zones (A MCP, B GenMusic + import, C MIDI/AMT,
D Sampler). `BlockNoteEvent` was frozen as the only C↔D contract. **All frontend
work was explicitly deferred** ("`/src` stays frozen this round"), with the
future partition pre-designed.

**Phase 3** — plugin hosting (P1 CLAP, P2 LV2 + the gating Zyn acceptance test,
P3 plugin UI, P4 state & automation groundwork), with a `§0. What the architect
round already built (do not rebuild)` section and eight numbered cross-zone
contracts.

**Explicitly deferred by PHASE3-PLAN §6:** MCP tools for plugins;
**out-of-process plugin processing (D-11 second half)**; VST3; **plugin effects
on audio tracks (needs insert chains)**; X11 UI embedding; **PDC**;
time-signature map (D-02 remainder); **op-log protocol (D-03 remainder)**. Plus
§15.1's *"Lane→node attachment during graph rebuild is the next automation
round."*

After phase 3, an unplanned **wave 1.5** shipped (hum-to-song, export,
stem-split import, loop-jam, Zyn patches) as additive commands with emitter
contracts documented in the Rust module rather than in new schema files.

The roadmap (root `README.md`, distilled from the register): out-of-process
plugin hosting; undo/redo via the op-log; automation UI; plugin GUI windows;
PDC before sends; VST3 later.

`SCALABILITY.md` §9's dependency graph is the canonical "what unblocks what":

```
prototype milestone (frozen surface, as-is)
   ├─► compiled-schedule renderer (§1.1)  ──► buses/sends ──► PDC ──► multicore
   ├─► op-log protocol (D-03) ──► undo/redo ──► journal/autosave ──► multi-window
   ├─► project v2 (D-02, D-06) ────────┴─► patterns/piano roll ──► event chunks
   ├─► AudioProcessor internal FX ──► CLAP host ──► bridge ──► VST3
   └─► backend trait (D-01) ──► PipeWire native / ASIO
```

### 5.6 Testing doctrine

The green test count is a doc-level invariant: each plan opens by quoting it
("**78 passing**", "**180 lib tests passing**") and makes it every zone's
definition of done. Today `README.md` and `CONTRIBUTING.md` say **259**
*(CORRECTED 2026-08-13: they disagree — `CONTRIBUTING.md:28` says 259 but
`README.md:388` says 277 backend + 17 frontend; the drift is itself an
instance of the doc-sync failure this section catalogues)*.

The verification gate is a fenced shell block, identical in shape per phase:

```sh
cd src-tauri && cargo check && cargo test     # existing + yours, all green
cd .. && npm run build && npx svelte-check    # green
cargo test lv2 -- --nocapture                 # Zyn acceptance not skipped
```

**The pattern to follow for a new subsystem**, assembled from the doctrine:

1. **Name ONE gating acceptance test** in the plan doc, phrased as a contract
   with an observable assertion and a clean-skip condition — the "Zyn shape":
   construct real state → drive it through the **production** path
   (`mixer::render` / `ControlPlane`, never a test-only path) → assert a
   *physical* property (non-silence, f0 = 440 Hz, exact sample count) → and
   **assert you did not silently hit the fallback** (the real test checks the
   node key is the plugin, because *"a PolySynth fallback would be keyed
   `synth@48000` — and would fake the pitch"*).
2. Make it **headless**, through `audio::offline::render` / `mixer::render` /
   `control::ops` — no audio device, no WebView.
3. **Pure unit tests** for the tauri-free core, plus a **round-trip test for
   every persisted artifact** (save → open → equal) *and* a corrupt-input
   degradation test (the automation module's rule: *"corrupt chunks degrade to
   an empty lane instead of failing the open"*).
4. **Gate anything needing external software**, print the reason on skip, and
   document the apt line in the README.
5. **Subprocess-isolate anything that can segfault**, so a crash is a recorded
   verdict rather than a dead suite (`AURA_SYNTH_PROBE`).
6. **Simulate mode is a first-class test dependency** — `AURA_SIDECAR_SIMULATE=1`
   must keep working, and its output must be deterministic *and valid*.
7. Quote the new green count in the plan doc and require it in the gate.
8. `CONTRIBUTING.md`: *"Bug fixes come with the regression test that would have
   caught them."*

Frontend testing at HEAD is `vitest` in `node` environment (no DOM), including
only `src/**/*.test.ts`, with the svelte plugin present to compile runes in
`*.svelte.ts` store modules. Two test files exist:
`src/lib/utils/timeline-nav.test.ts` and
`src/lib/state/generation.adopt.test.ts`.

### 5.7 How documentation is kept in sync

`docs/` is exclusively the architect's and is on the FROZEN list for every build
round; contributors *request* doc and schema changes in their final report.

**Schemas are the source of truth, and adding one is a documented three-step:**
add `docs/ipc-schemas/<name>.schema.json` (draft-07, emitter contract, no
`additionalProperties: false`), **keep the Rust/TS types in sync by hand**, and
validate in a round-trip test. The sync mechanism in code is a doc-comment
convention (`/// Mirrors docs/ipc-schemas/track-state.schema.json`). There is no
codegen and no CI schema linter.

**Frozen-surface changes require a doc edit by construction**, because the frozen
list, the command roster (§3.3) and the event table (§3.4) live in
`ARCHITECTURE.md` and only the architect may edit them. New commands are
appended under a round-labelled sub-block ("**Phase-3 additions**",
"**Wave-1.5 additions**").

**Debt is updated in place, never removed** (§5.1). Docs cross-link by section
number in both directions, and code comments cite doc sections back
(`plugins/automation.rs`: *"ticks never cross onto the RT thread (ARCHITECTURE
§13/§15.1)"*; `main.rs`: *"(PHASE3-PLAN contract 8)"*).

Verified claims are dated and machine-scoped: *"decided, verified on this
machine"*, *"Note (2026 crate reality, verified locally)"*, *"wave 1C
(2026-08-12)"*. Unverified things are marked as such.

---

## 6. The traps

Every concrete defect and mis-design this analysis found in the current code,
consolidated. Ordered by how expensive it is to leave in place.

| # | Trap | Where | Consequence |
|---|---|---|---|
| 1 | **`MidiNote` has no id** — identity is the index in a sorted array | `midi/types.rs:31`, AMEV record `midi/events.rs` | Positional addressing. Delete a note, undo, and a *different* note moves. Also blocks per-note expression, MPE and any per-voice modulation. Adding a `note_id` column costs one `columnMask` bit now and a migration of every event chunk ever written later. |
| 2 | **RT slots are reused after free** — with a test asserting it | `audio/types.rs:237`, `Store::free_slot` | Delete track A (frees slot 3) → add track C (takes slot 3) → undo the delete → A cannot have its slot back. Slots must be derived state reassigned wholesale on rebuild, not identity. |
| 3 | **`timeSignature` is silently clobbered to `[4,4]` on every save** | `audio/project.rs:47`, `:189`, save overlay `:67` | A `[3,4]` project becomes `[4,4]` on the next autosave. `Store` has no field to hold it. Silent user-data loss, today. |
| 4 | **Audio assets are named by clip id, and the decode cache is keyed by clip id** | `audio/<clipId>.wav`; `Control::cache` in `audio/engine.rs` | Couples asset naming to *instance* identity. *(CORRECTED 2026-08-13: the original "would serve the wrong audio" consequence was wrong — each cache entry decodes from its own clip's `source_path`, so two clips sharing a source get duplicate **correct** decodes. The real defects: 2× decode memory per shared source; asset-GC coupling — deleting one clip can orphan or delete a file another clip references; and **staleness** — the cache never invalidates when a clip's `source_path` changes under an unchanged clip id, `engine.rs:599-623`. Source-keying remains the right fix, for these reasons.)* |
| 5 | **`remove_track` bypasses the control plane** while `add_track` does not | `audio/mod.rs:369` vs `control/ops.rs:99` | The mirror-image asymmetry. No `ops::remove_track`, no MCP tool, no event, no validation, no possibility of an inverse. |
| 6 | **Deleting a track leaks its automation lanes and plugin instances** | `audio/mod.rs:369`; `plugins/automation.rs` lanes are project-global | Lanes have no track association, so nothing collects them. Orphaned rows accumulate in `project.json` forever. |
| 7 | **Five uncoordinated locks; no single project-state lock** | `Store`, `MidiStore`, `PluginRegistry`, `AutomationStore`, `SamplerBank` | A batch spanning tracks + midi + plugins cannot be made atomic. Lock ordering (store → midi) is a comment, enforced by nothing. |
| 8 | **Three additional writer threads mutate the store** | engine control thread (`engine.rs`), LoopJam watcher (`control/loopjam.rs`), sidecar completion sinks (`control/import.rs`, `hum.rs`) | Any command/undo layer must treat these as mutation *sources*. Two of them (LoopJam, sinks) fire with no user gesture behind them. |
| 9 | **Clip moves are never persisted, and `project.reload()` discards them** | `src/lib/state/project.svelte.ts:165`, `midi.svelte.ts:145` | The timeline the user sees is partly not in `Store`. Any reload — after import, generation, loop-jam or an MCP approval — silently reverts every drag. |
| 10 | **No event is emitted for structural mutation** | `control/mod.rs` emits only transport/project-changed; `add_track`, `set_track_mix`, `remove_track`, midi, plugins, automation emit nothing | MCP mutations are invisible to the UI. The workaround is a `setTimeout(..., 400)` blind reload with the comment *"the approved tool may have changed anything"* (`mcp.svelte.ts`). |
| 11 | **`GainAutomatedNode` is proven by five tests and wired into nothing** | `plugins/automation.rs:434`; only other mention is a doc comment at `audio/dsp.rs:47` | Sample-accurate automation exists and is unreachable. `automation.rs:40` says so in the module header. |
| 12 | **No parameter smoothing anywhere** | `mixer.rs` applies `ParamTable` values as a constant per block | Audible zipper on any large fader move. §10 rule 2 says "smoothed in the node"; that half was never built. |
| 13 | **`AudioProcessor` has no `latency_samples()`** | `audio/dsp.rs:25` vs §10 rule 5 | PDC has no reporting surface. §2 warns PDC must land *before* sends ship, because retrofitting it audibly changes users' existing mixes. |
| 14 | **Plugins get no transport** — CLAP transport event is passed as `None` | `plugins/clap_host.rs` | Hosted plugins have no tempo, no bar/beat, no PPQ. Tempo-synced delays and LFOs cannot work. |
| 15 | **Musical time does not exist on the RT thread** | `audio/rt.rs:26` `SharedRt` | No tempo-synced anything the engine itself decides; no PPQ to hand a plugin (see 14). |
| 16 | **One shared stereo scratch buffer for all live nodes** | `audio/rt.rs:249` | Live-node rendering is inherently serialised, and nothing can have more than one input or feed a bus. Must become an indexed, channel-counted pool before any graph work. |
| 17 | **Multichannel is silently dropped** — `mix_out` writes channels 0/1 only | `audio/mixer.rs:104` | `RtClipData` is channel-counted, the mix path is not. Anything above stereo is silence. |
| 18 | **`"bus"` is reserved in the schema and actively rejected in code** | `control/ops.rs:106` | The one container kind that exists on paper cannot be created. First blocker for groups, folders and sends. |
| 19 | **Nothing enforces the RT rules mechanically** | no `assert_no_alloc`, no lint, no CI (`.github/workflows` does not exist) | The four non-negotiable rules are prose. `rtsan-standalone` and `assert_no_alloc` both exist and are unused. |
| 20 | **`xruns` conflates capture underruns with meter-queue overflow** | `audio/engine.rs:251` | The one health metric the engine exposes is ambiguous. |
| 21 | **Five independent writers to `project.json`, each with its own directory GC** | `audio/project.rs`, `midi/persist.rs`, `plugins/state.rs`, `plugins/automation.rs` | A new object type sharing a directory will have its chunks silently deleted by another subsystem's GC. New top-level keys are dropped unless written through `update_project_v2` — a rule discoverable only from a doc comment. |
| 22 | **Autosave-on-mutation is unconditional and there is no `.bak`** | `project::save` after record-stop, import, loopjam, every plugin and midi and automation mutation | The file is already overwritten before an undo could run. Any history layer must reckon with this first. |
| 23 | **No migration chain; upgrade is lazy and implicit** | `midi/persist.rs` blindly inserts `schemaVersion: 2` | `SCALABILITY.md` §4 specifies pure `Value → Value` migration modules tested against fixtures. None exist. No forward-compat test either. |
| 24 | **`project::validate` is 15 lines and checks two things** | `audio/project.rs:125` | No referential integrity on load. A dangling `clip.trackId` produces an invisible, unplayable clip with no error. |
| 25 | **`Project.schemaVersion` is wrong on the `from_store` path** | `audio/project.rs:179` hardcodes `1` | The `project://changed` payload says `1` while the file says `2`. The TS type declares `1` — wrong for every opened project. |
| 26 | **`instrumentId` violates its own frozen schema** | `TrackState` emits it; `track-state.schema.json` has `additionalProperties: false` and no such field | A strict v1 validator rejects every AURA-written track with a bound instrument. |
| 27 | **`plugins[]` and `automation[]` are written into `project.json` and appear in no schema** | `plugins/state.rs`, `plugins/automation.rs` | Two persisted top-level arrays with no wire contract at all. |
| 28 | **`instruments[]` is specified in three places and never written** | `project-v2.schema.json`, `sampler-instrument.schema.json`, §14 | Sampler instrument bindings do not survive a restart — the track silently falls back to PolySynth. |
| 29 | **Sampler instrument ids are regenerated per load** | `audio/sampler_engine.rs` | Even without (28), the id in `TrackState.instrumentId` cannot match after a restart. |
| 30 | **AMEV `columnMask` is written and ignored on read** | `midi/events.rs` `decode_notes` | The designed forward-compatibility mechanism is not actually exercised, so it is unverified. |
| 31 | **`midi-clip.schema.json`'s top-level `oneOf` cannot validate** | branches overlap structurally | A `notes`-less clip matches both, so strict validation fails. |
| 32 | **`set_track_mix` exists in Rust and MCP but not in the frontend `Backend`** | `src/lib/tauri.ts` | The UI still fires one invoke per property; the batch path has no UI caller. |
| 33 | **`ImportSplitReply` differs between Rust and TS** | `control/import.rs` vs `src/lib/types/ipc.ts` | Live drift in a hand-maintained mirror, with nothing to catch it. |
| 34 | **Nothing enforces IPC type sync** | no codegen, no schema test, no handler-list test | Traps 25–27 and 32–33 are all instances of the same missing check. |
| 35 | **Three disjoint selection models, one of them index-based** | `PianoRoll.selection: Set<number>` into a component-local array | Silently aliases after any filter or splice. No shared "selected objects" concept, which is a prerequisite for any command layer (commands need targets). |
| 36 | **Seven hand-rolled drag implementations; only one has draft/commit** | §4.6 | Clip drags have no batching discipline at all, which is why they were never persisted (trap 9). |
| 37 | **Two incompatible snapping systems plus a third ad-hoc rule** | `view.snapSamples`, `PianoRoll.snapTick`, Timeline's bar rounding | MIDI clip drags snap in the *sample* domain against a *tick* grid. |
| 38 | **Four coordinate systems; bars/beats computed three times** | §4.4 | The piano-roll viewport is component-local, so it cannot be tested, synced or agent-driven. |
| 39 | **Nine independent rAF loops plus one per visible MIDI clip** | §4.8 | No shared ticker, no frame budget, no priority, no lane virtualization, one painter per clip canvas. §10.12 requires virtualization "from the start". |
| 40 | **`tiles.evictClip` exists and is never called** | `src/lib/render/tiles.ts` | The tile LRU is capped at 512 promises but never scoped to live clips. |
| 41 | **The deterministic demo engine is unused for testing** | `src/lib/demo.ts`, 2454 lines | A complete seeded fake backend exists. Coordinate math, snapping and gesture reducers could be covered in jsdom today, if that logic moved out of `.svelte` files — the pattern `timeline-nav.ts` already establishes. |
| 42 | **`ARCHITECTURE.md` §0 contains a stale cross-reference** | *"the event names in §5.4"* — should be §3.4 | Minor, but it is in the section labelled READ FIRST. |

---

*Compiled 2026-08-13 against `f632580`. Companion dossiers in
`docs/research/` cover the external analysis this document is measured
against.*
