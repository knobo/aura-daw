# AURA — Architecture

Tauri v2 + Svelte 5 Digital Audio Workstation prototype.
Rust real-time audio core, web-technology UI, Python/native sidecars for ML tasks.

> Long-term evolution (audio graph, plugins, MIDI/patterns, op-log IPC,
> project v2) is specified in **`docs/SCALABILITY.md`**; §10 below lists the
> constraints from it that bind prototype code TODAY.

---

## 0. Agent ownership boundaries (READ FIRST)

> **PHASE 3 (current round): ownership zones are defined in
> `docs/PHASE3-PLAN.md`** (plugin hosting; §15). Phase-2 zones lived in
> `docs/PHASE2-PLAN.md`. The round-1 table below is kept for history; the
> FROZEN-file rule still stands.

Round 1: four agents built this project in parallel. To avoid merge
conflicts, ownership was strict:

| Agent | Owns (exclusive write access) |
|-------|-------------------------------|
| **Agent 1** (architecture) | `docs/`, all FROZEN files below |
| **Agent 2** (audio engine) | `src-tauri/src/audio/` — everything in it, new submodules welcome |
| **Agent 3** (sidecars) | `src-tauri/src/sidecars/` and top-level `/sidecars/` (Python/worker scripts, models config) |
| **Agent 4** (frontend) | `src/` — everything in it (components, stores, WebGPU/canvas renderers, CSS) |

**FROZEN files — nobody edits these:**
`src-tauri/src/lib.rs`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`,
`src-tauri/capabilities/default.json`, `src-tauri/build.rs`, `package.json`,
`vite.config.ts`, `svelte.config.js`, `tsconfig.json`, `index.html`.

If you need a new crate, npm package, Tauri permission, or a new command
registered in `lib.rs`: **state the request in your final report** and Agent 1
(or the orchestrator) applies it. Do not edit frozen files "just this once".

The command names registered in `lib.rs` and the event names in §3.4 are the
**frozen API surface**. Signatures/bodies of commands may evolve inside the
owning module; names may not.

---

## 1. Topology

```
┌────────────────────────────────────────────────────────────────────────┐
│  OS audio device (ALSA/JACK/PipeWire via cpal)                         │
└───────▲───────────────────────────────▲────────────────────────────────┘
        │ output callback               │ input callback
┌───────┴───────────────────────────────┴────────────────────┐
│  AUDIO CALLBACK THREAD(S)  (real-time, cpal)               │
│  mix graph render · input capture · meter taps             │
│  NO alloc / NO locks / NO syscalls                         │
└───┬───────────────▲───────────────┬────────────────────────┘
    │ SPSC rtrb     │ SPSC rtrb     │ SPSC rtrb
    │ (meters)      │ (commands)    │ (recorded samples)
┌───▼───────────────┴───┐       ┌───▼──────────────────┐
│  ENGINE CONTROL       │       │  DISK WRITER THREAD  │
│  THREAD (std thread)  │       │  WAV/FLAC encode     │
│  batches meters,      │       │  (hound / flacenc)   │
│  owns graph edits     │       └──────────────────────┘
└───┬───────────────────┘
    │ Tauri Channel (binary/JSON)     ┌──────────────────────────────┐
    │ + app events                    │  TOKIO WORKER TASKS          │
┌───▼──────────────────────────┐      │  sidecar job runner:         │
│  TAURI MAIN / IPC            │◄─────┤  Demucs (python), Whisper.cpp│
│  #[tauri::command] handlers  │      │  waveform tile builder,      │
└───┬──────────────────────────┘      │  project I/O                 │
    │ WebView IPC (invoke / Channel / events)
┌───▼──────────────────────────────────────────────┐
│  SVELTE 5 UI (WebView)                           │
│  Canvas/WebGL2 waveforms & meters · Tailwind UI  │
└──────────────────────────────────────────────────┘
```

Three planes:

* **Real-time plane** — cpal callback threads. Communicates only through
  lock-free ringbuffers and atomics.
* **Control plane** — engine control thread + Tauri commands + tokio tasks.
  Owns all mutable project/engine state, performs allocation and I/O.
* **Presentation plane** — Svelte 5 in the WebView. Renders meters and
  waveforms on Canvas2D first. *(updated 2026-08-13, see CORE-REDESIGN-ROUND-2
  §9)* WebGPU is **not** the upgrade path on Linux: it is not coming to
  WebKitGTK (verified absent from the roadmap, 2026-08), so **WebGL2 is the
  ceiling** in our webview. Hot rendering (arranger, piano roll, meters) goes
  behind **one texture-targeting renderer interface** — WebGL2 today,
  WebGPU-capable by design for other platforms/futures — and Svelte keeps
  chrome only (menus, inspectors, dialogs). Never blocks on the backend;
  everything is push (channels/events) or cheap request/response.

## 2. Real-time rules and lock-free ringbuffers

### 2.1 The audio-callback contract

The cpal callback runs on a thread with a hard deadline: at 48 kHz with a
128-frame buffer it has ~2.6 ms. A page fault, mutex, `malloc`, or syscall can
block for longer than that, which is an audible dropout. Therefore, inside the
callback:

* **Never allocate** — `malloc` can take a lock in the allocator and fault in
  new pages. All buffers are preallocated by the control thread.
* **Never lock** — a mutex held by a preempted non-RT thread means unbounded
  priority-inversion stalls. All shared state uses atomics or wait-free SPSC
  queues.
* **Never syscall** — no file I/O, no logging, no channel `send` that can wake
  a thread via futex. (The exception is what cpal itself does to deliver
  audio.)
* **No panics / no unbounded loops** — deterministic worst-case per callback.

### 2.2 Crate choice: `rtrb` (primary), `ringbuf` (available)

Both are in `Cargo.toml`; **`rtrb` is the default choice**:

* `rtrb` is a wait-free SPSC ringbuffer designed specifically for audio
  (by the same community as PortAudio/JACK bindings). `push`/`pop` are
  lock-free and allocation-free; chunk APIs (`write_chunk_uninit`,
  `read_chunk`) move whole buffers per callback with two atomic ops.
* Strict SPSC matches our topology: every queue has exactly one producer and
  one consumer thread, which keeps reasoning trivial and the fast path minimal.
* `ringbuf` is kept as an alternative because it offers MPMC-ish flexibility,
  `HeapRb` splitting, and overwrite semantics that can be convenient for
  "latest value wins" telemetry. Use it only where rtrb's strict SPSC is
  genuinely awkward.

### 2.3 The queues

| Queue | Producer → Consumer | Element | Sizing |
|---|---|---|---|
| `meters` | audio callback → engine control | `RawMeterBlock { track_peaks: [f32; MAX_TRACKS*2], rms_acc, position }` fixed-size POD | ~16 blocks (¼ s) |
| `engine_cmd` | engine control → audio callback | `EngineCmd` enum (set gain, seek, start/stop, swap graph pointer) — POD, no heap payloads | 256 cmds |
| `engine_evt` | audio callback → engine control | `RtEvent` — POD, `Copy`, no heap payloads (`ReachedEnd { at }`; §2.6) | 64 |
| `rec_{track}` | input callback → disk writer | interleaved `f32` samples, chunked | ≥ 1 s of audio (headroom for slow disks) |

Structural changes (add track, load clip) never travel through `engine_cmd`
directly: the control thread builds the new audio graph off-thread, then sends
a single *pointer swap* command; the callback publishes the retired graph back
so the control thread can drop it (deallocation never happens on the RT
thread).

### 2.4 Meter/peak data flow

1. Audio callback folds per-track peak/RMS into a fixed POD block per buffer
   and pushes it to `meters` (wait-free; on overflow, drop the oldest — meters
   are telemetry, not data).
2. Engine control thread drains the queue, aggregates into UI-friendly
   `MeterFrame` batches.
3. At **60 Hz** (one `MeterFrame` per ~16.6 ms containing max-peak / windowed
   RMS since the last frame) it pushes the frame into the Tauri
   `Channel<MeterFrame>` obtained from the frontend's `subscribe_meters` call.
4. Svelte reads frames and paints meters on canvas in rAF. Peak-hold/decay is
   a UI concern.

### 2.5 Waveform tile streaming

Waveform rendering uses **min/max tiles** (audio "mipmaps"):

* On import/record, a tokio worker computes an LOD pyramid: LOD *n* stores
  min/max `i16` pairs per bin of `2^(8+n)` samples, cached under
  `<project>/.aura/cache/waveforms/<clipId>/lod<N>.bin`.
* The UI requests tiles by `(clipId, lod, tileIndex)` via `get_waveform_tile`,
  which returns **raw binary** (§3.2) — no JSON, no base64.
* Live recording: the disk-writer thread also folds incoming audio into
  "hot tile" updates streamed to the UI over an events channel so the waveform
  grows in near-real-time.

**Waveform tile wire format** (little-endian, matches the stub in
`audio/mod.rs`):

```
[magic  u32 = 0x41575446 "AWTF"]
[version u16 = 1] [channels u16]
[lod u32] [tileIndex u64] [bins u32]
then channels × bins × (min i16, max i16)
```

### 2.6 Timeline boundaries: the engine notices, the control plane decides

Auto-stop ("stop when the material ends") is the first feature where the
audio callback must *react to where the playhead is*. The rule that keeps
this from metastasising into policy on the RT thread:

> **The callback reports that a boundary was crossed. It never decides what
> that means.**

Three pieces, each usable on its own:

1. **`transport::crossing(base, frames, loop, point) -> Option<u64>`** — pure,
   allocation-free, unit-tested next to `advance`. Answers only *where* the
   playhead crossed `point`, honouring loop wrap (an active loop can never
   reach a point past its end). `point == 0` means "no point", the same
   degenerate-means-off convention `LoopSpec` uses. Punch-in/out and markers
   are the same call with a different point.
2. **`SharedRt::song_end`** — the boundary itself, an atomic beside
   `loop_start`/`loop_end`, recomputed by the control thread on every
   structural rebuild via `offline::song_end` (so live playback and the
   offline bounce agree on where a song ends, note-offs included).
3. **`engine_evt` (§2.3)** — the callback pushes `RtEvent::ReachedEnd { at }`
   and moves on. The control thread drains it and applies policy:
   `stop_at_end` on, no active loop, not recording. New "the engine saw
   something" features become new `RtEvent` variants, not new branches in
   `OutputCb::render`.

**The park handshake.** A control thread cannot place the playhead by itself.
A callback that read `playing == true` before the stop still writes its
advanced position afterwards, landing the playhead a buffer past the
boundary — the smaller the buffer, the likelier the interleaving. So the
control thread writes *where* to `SharedRt::park` **before** clearing
`playing`, and the callback applies it (a `swap` against `NO_PARK`, so
exactly once) the moment it sees the transport stopped. The last writer of
the playhead is then always the one that knows where it belongs. This is
mechanism, not policy: the callback carries out a position, it never chooses
one. Any explicit transport command clears a pending park — a fresh
instruction supersedes an owed one.

Automation lanes will NOT reuse this: dense per-sample curves belong in the
RCU graph snapshot next to the clips, not in atomics. Boundaries are sparse
events; automation is continuous data. Keeping them separate is the point.

## 3. IPC strategy (Tauri v2)

### 3.1 Three mechanisms, by payload class

| Class | Mechanism | Why |
|---|---|---|
| Control / CRUD (transport, track edits, project ops) | `invoke()` commands, JSON | Low rate, needs request/response semantics and typed errors |
| High-rate streams (meters, sidecar progress, live-record tiles) | **Tauri v2 `Channel<T>`** passed into a command | Channels are ordered, per-subscription, and drop cleanly when the JS side GCs — no global event-bus fan-out cost |
| Bulk binary (waveform tiles, later: clip audio for scrubbing) | Command returning **`tauri::ipc::Response`** with a `Vec<u8>` body | Tauri v2's raw IPC path skips JSON serialization; frontend receives an `ArrayBuffer` directly (`invoke(..., { ... })` returns `ArrayBuffer` when the handler responds with raw bytes) |

### 3.2 Zero-copy notes

* `tauri::ipc::Response::new(Vec<u8>)` transfers the byte payload over the
  WebView's native IPC without a JSON round-trip or base64 inflation. On the
  JS side it arrives as an `ArrayBuffer` → wrap in a typed array, upload
  straight to a GPU buffer / draw to canvas. This is the lowest-overhead path
  Tauri offers portably.
* True shared memory (memfd / `SharedArrayBuffer` bridged into the WebView) is
  **deliberately out of scope** for the prototype: WebKitGTK offers no
  supported way to map external memory into the page, so it would be
  platform-specific hackery for marginal gains at our data rates
  (meters ≈ a few kB/s; a waveform tile ≈ 4 kB). Revisit only if profiling
  shows IPC as a bottleneck (e.g. streaming raw audio for a scrub preview).
* Cadence: **meters 60 Hz batched** (one frame per subscription per tick, all
  tracks in one message), sidecar progress ≤ 10 Hz, everything else on-demand.
  Never send per-audio-buffer messages to the UI.

### 3.3 Command surface (frozen names)

Registered in `src-tauri/src/lib.rs`; implementations live in the owning
module.

**audio** — `transport_play`, `transport_stop`, `transport_seek`,
`get_transport_state`, `list_input_devices`, `list_output_devices`,
`select_input_device`, `select_output_device`, `start_recording`,
`stop_recording`, `subscribe_meters`, `get_waveform_tile`, `add_track`,
`remove_track`, `get_tracks`, `set_track_gain`, `set_track_pan`,
`set_track_mute`, `set_track_solo`, `set_track_arm`, `create_project`,
`open_project`, `save_project`.

**sidecars** — `sidecar_split_stems`, `sidecar_transcribe`,
`sidecar_job_status`, `sidecar_cancel_job`, `sidecar_list_jobs`.

**Phase-2 additions** (registered as compiling stubs/implementations; zone
ownership in `docs/PHASE2-PLAN.md`):

* control plane — `get_project_state`, `set_track_mix` (batched, D-03-shaped),
  `import_audio_clip` (stub, zone B)
* sampler (zone D) — `sampler_load_instrument`, `sampler_list_instruments`,
  `sampler_preview_note` (stub)
* midi (zone C) — `set_tempo_map`, `midi_add_clip`, `midi_set_notes`
  (batched), `midi_get_clips`
* sidecars (zone B) — `sidecar_run_job` (open kind + opaque params, D-07)
* mcp (zone A) — `mcp_get_status`, `mcp_set_policy`, `mcp_confirm_pending`

**Phase-3 additions** (plugin hosting, §15; zone ownership in
`docs/PHASE3-PLAN.md`):

* plugins — `plugin_scan`, `plugin_list`, `plugin_instantiate`,
  `plugin_remove`, `plugin_get_params`, `plugin_set_param` (batched)
* `set_track_instrument` additionally accepts `"plugin:<instanceId>"` refs
  (same command name, additive semantics)

**Wave-1.5 additions** (registered in `lib.rs`; implementations in the
owning modules):

* control plane — `hum_to_song` (hum/voice WAV → MIDI melody clip, optional
  AMT accompaniment), `export_song` / `export_job_status` /
  `export_capabilities` (offline bounce to wav/flac/mp3/ogg),
  `import_audio_clip_split_stems` (import + Demucs stem split in one step),
  `loopjam_evolve` / `loopjam_cancel` / `loopjam_status` (ACE-Step repaint
  of the loop region, applied at wrap)
* plugins — `zyn_list_patches`, `zyn_load_patch` (ZynAddSubFX bank patches)
* sidecars — job kind `humToMidi` (`JobKind::HumToMidi`, dedicated
  `hum_to_song` command; rejected by the open-kind `sidecar_run_job` path)

**Composer additions** (§16; all additive names, `control::composer`):
`harmony_get`, `harmony_set` (batch-shaped: one invoke carries the whole
document, so an editing gesture is one op and one undo), `composer_palette`,
`composer_suggest`, `composer_generate`.

**Pitch Coach additions** (§5.1; all additive names, registered next to
`subscribe_meters`):

* live — `pitch_listen_start`, `pitch_listen_stop`, `pitch_subscribe`,
  `pitch_unsubscribe`, `set_rehearse_hold`, `pitch_set_reference`
* the recorded take — `pitch_score` (score a take's curve against a MIDI
  track's melody), `pitch_track` (the stored curve on the timeline, thinned),
  `pitch_analyze_clip` ((re)build a take's curve). All three are `async` +
  `spawn_blocking`: a missing cache turns any of them into a decode plus an
  analysis, and a sync command runs on the GTK main thread the webview draws
  on (see `seed_demo_project`). The session lock is taken and released before
  the blocking half, never held across it

`pitch_unsubscribe` is not optional symmetry. `pitch_subscribe` appends a
sink and the engine only retires one when `send_batch` fails, which a live
Tauri `Channel` never does no matter how dead its JS end is — a per-mount
subscriber that only mutes its own end leaks a sink per visit.

Payload shapes: see `docs/ipc-schemas/*.schema.json` (source of truth for the
frontend; the Rust structs in the modules mirror them, `camelCase` on the
wire). New v2 schemas are *emitter* contracts without
`additionalProperties: false` (debt D-06 resolved for new surface).

### 3.4 Event names (frozen)

App-level events (`app.emit` → `listen()` in JS). High-rate data does NOT go
here — use the channels.

| Event | Payload schema | Emitted when |
|---|---|---|
| `transport://state` | `transport-state.schema.json` | transport state/position jumps (not per-sample) |
| `recording://state` | `recording-command.schema.json#/$defs/recordingState` | recording started/stopped, take finalized |
| `sidecar://progress` | `sidecar-job.schema.json#/$defs/progressEvent` | job progress (≤ 10 Hz, throttled) |
| `sidecar://done` | `sidecar-job.schema.json#/$defs/doneEvent` | job success |
| `sidecar://error` | `sidecar-job.schema.json#/$defs/errorEvent` | job failure |
| `project://changed` | `project.schema.json` | project loaded/saved/renamed |
| `mcp://confirm-requested` | `mcp-policy.schema.json#/$defs/pendingConfirmation` | (phase 2) an MCP tool call awaits user confirmation |
| `export://progress` | `{ jobId, progress, stage }` (emitter contract in `control/export.rs`; ≤ 10 Hz, stage changes always pass) | (wave 1.5) offline export/bounce advances |
| `export://done` | export result object + `jobId` (see `control/export.rs`) | (wave 1.5) export finished |
| `export://error` | `{ jobId, message }`-shaped error (see `control/export.rs`) | (wave 1.5) export failed |
| `loopjam://state` | Rust `LoopJamStatus` in `control/loopjam.rs` | (wave 1.5) loop-jam session state changes (idle/generating/ready/applied/cancelled) |
| `pitch://state` | `pitch-state.schema.json` | (§5.1) listening/rehearse-hold/reference-track/device-rate changes. Pitch FRAMES do not ride here — they are a `Channel`, batched at 60 Hz |

## 4. Threading model

| Thread | Kind | Responsibilities | RT-safe? |
|---|---|---|---|
| cpal output callback | RT (cpal-owned) | render mix graph, fold meters, consume `engine_cmd` | YES — no alloc/lock/syscall |
| cpal input callback | RT (cpal-owned) | copy input into `rec_*` SPSC, input meters | YES |
| engine control | std thread, owned by `audio` | drain meter queue, batch to 60 Hz, apply UI edits, build/swap graphs, drop retired graphs | no |
| disk writer | std thread, owned by `audio` | drain `rec_*` queues, encode WAV (hound) / FLAC (flacenc), fsync takes, fold hot waveform tiles | no |
| tokio runtime | Tauri's async runtime | sidecar process supervision, waveform LOD builds, project file I/O, anything `await` | no |
| Tauri main | main thread | window/event loop, command dispatch (commands are cheap or `async`) | no |
| WebView | UI | Svelte 5, canvas/WebGL2 painting (updated 2026-08-13, see CORE-REDESIGN-ROUND-2 §9) | n/a |

Rules: commands never block the main thread on the engine — they enqueue and
return, or are `async` and await the control thread over a oneshot. The RT
threads are the only ones with deadlines; everything else is ordinary code.

## 5. Recording pipeline

```
cpal input callback ──rtrb SPSC (≥1 s cap)──► disk writer thread
   │                                             │
   └── input meters ► meters queue               ├─► WAV via hound (32-bit float, native rate)
                                                 ├─► hot waveform tiles ► UI channel
                                                 └─► on stop: finalize header, register clip,
                                                     optional FLAC transcode (flacenc, tokio task)
```

* Record format is **WAV (f32)** for zero-cost writes during capture; FLAC is
  an *offline* transcode for archival/export (flacenc is pure Rust, no C deps).
* The SPSC is sized for ≥ 1 s of audio so a slow fsync never backpressures the
  callback; overflow increments an xrun counter surfaced in `transport://state`.
* On `stop_recording`, the writer finalizes the file under
  `<project>/audio/`, computes remaining LOD tiles, and returns the new clip
  descriptor(s) (`clip.schema.json`).

### 5.1 Input hub and pitch detection (Pitch Coach)

The microphone's lifetime is **not** the take's. An `InputHub` in
`audio/engine.rs` owns a listen-only stream, and a take on the same device
takes the analyser over and drops it; stop hands the mic back. Listening
starts on an explicit toggle or an open Pitch Coach panel, never on track arm
(ruling R6).

```
cpal input callback ──► InputCb::capture ──┬─► recorder ring (as above)
   (RT, no alloc/lock)                     │
                                           └─► PitchTap: decimate to 8 kHz
                                                    │  two rtrb rings:
                                                    │  samples + per-chunk
                                                    │  descriptors (device
                                                    │  position and rate)
                                                    ▼
                             PitchWorker (own thread, NOT RT)
                             YIN ► RMS gate ► median ► jump limiter
                                                    │
                                       ┌────────────┴────────────┐
                                       ▼                         ▼
                            60 Hz batch ► Channel      PitchFolder ► APTF
                            (live lane/tuner)          cache/pitch/<clipId>.bin
```

Load-bearing details, each of which cost a bug to find:

* **No detection runs on the callback.** The tap decimates and hands over;
  everything else is the worker's. Samples are written *before* their
  descriptor, and a chunk that does not fit anywhere is dropped **whole** —
  a half-written chunk would mistime every frame after it. A dropped chunk
  also owes the analyser a reset, because it otherwise carries most of an
  analysis frame across the hole and timestamps the splice at the new
  position.
* **Taps are gated on a shared `pitch_active` flag, not on their own
  existence.** A take carries a dormant tap whether or not anyone was
  listening when it started, because a recording stream cannot be rebuilt
  mid-take without losing audio. A dormant tap costs one relaxed atomic load
  per buffer.
* **`PitchFrame.sample` is a PROJECT sample position**, anchored to
  `shared.position` and only offset within one callback buffer — not
  device-rate. A 44.1 kHz microphone in a 48 kHz project makes any offset
  computed against the capture rate ~8 % short.
* **Rehearse-hold writes silence** into the take for the held span rather
  than skipping it, so the take stays sample-aligned; the spans come back on
  `recording://state` as `rehearseSpans`.
* **The live median is causal**, so the live track lags the centred
  (`sidecars/hum_to_midi.py`) one by the median half-kernel — 2 frames,
  20 ms. This is a property of live detection, not a bug to fix; a parity
  test asserts the two agree to 10 cents once it is accounted for.

**The stored curve.** `audio/pitch_store.rs` defines `APTF` (magic, `u16`
version, rate, hop, `first_sample`, provenance byte, frame count, then the
frames). The recorder's writer thread folds it live, the same way it folds
the waveform pyramid. Positions are **take-local**, in the take's own rate,
snapped onto a `first_sample + i * hop` grid; the header carries
`first_sample` because the analyser timestamps the *centre* of its window
(~15 ms in), and deriving positions from a bare `i * hop` would slide every
curve early by that much. Mapping onto the timeline is the reader's job:

```
timeline = clip.timeline_start_samples
         + (sample * project_rate / clip.source_sample_rate) - clip.offset_samples
```

The conversion comes **first** and the offset subtraction second:
`offset_samples` and `length_samples` index the decode cache, which
`linear_resample`s to the engine rate at load, so they are project-rate
quantities while the curve is not.

Scores are **not** persisted — only the curve. `pitch_score` recomputes, so a
report stays correct after the reference melody is edited.

## 6. Sidecar architecture (Demucs, Whisper.cpp)

* **Demucs** (stem separation) runs as `python3 sidecars/demucs_worker.py`
  (Agent 3 owns the script). **Whisper.cpp** (vocal-to-text + pitch) runs as a
  native binary wrapper, same protocol.
* Spawned via `tauri-plugin-shell` (already permitted in
  `capabilities/default.json` for `python3`/`python`/`sh`) or
  `tokio::process` from the job runner.
* **Protocol:** workers write NDJSON to stdout — one JSON object per line:
  `{"type":"progress","progress":0.42,"stage":"separating"}`,
  `{"type":"done","result":{...}}`, `{"type":"error","message":"..."}`.
  stderr is captured as log lines. The Rust job runner (a tokio task per job)
  parses lines, updates the job registry, forwards to the submit-time
  `Channel<SidecarEvent>` and re-emits throttled `sidecar://*` app events.
* Jobs are identified by UUIDv4, queryable via `sidecar_job_status` /
  `sidecar_list_jobs`, cancellable via `sidecar_cancel_job` (SIGTERM, then
  SIGKILL after grace).
* Outputs land in the project dir: stems → `<project>/stems/<clipId>/`,
  transcripts → `<project>/cache/transcripts/`.
* Packaging note: for the prototype, workers run from the repo's `/sidecars/`
  dir with a system `python3`. Bundling as true Tauri `externalBin` sidecars
  (PyInstaller-frozen) is a later packaging step; the IPC protocol is
  identical.

## 7. Project folder layout (`*.aura` directory)

```
MySong.aura/
├── project.json          # project.schema.json — tracks, clips, transport, refs
├── audio/                # recorded/imported takes: <clipId>.wav (f32) / .flac
├── stems/                # Demucs output: <sourceClipId>/{vocals,drums,bass,other}.wav
└── cache/                # regenerable — safe to delete
    ├── waveforms/<clipId>/lod<N>.bin   # min/max tile pyramids (§2.5)
    ├── pitch/<clipId>.bin              # APTF pitch curve per take (§5.1)
    └── transcripts/<clipId>.json       # Whisper output (text + timings + pitch)
```

* `project.json` is the single source of truth; everything in `cache/` is
  derived and rebuilt on demand.
* All clip references in `project.json` are **relative paths** inside the
  `.aura` dir so projects are portable.
* Saves are atomic: write `project.json.tmp`, fsync, rename.

## 8. Future DSP abstractions

The engine reserves a processing-node abstraction so effects/time-stretch slot
in without re-architecture. Placeholder trait (lives in Agent 2's module when
implemented):

```rust
/// RT-safe processor: `process` is called on the audio thread and must obey
/// the §2.1 contract. Allocation happens in `prepare` on the control thread.
pub trait AudioProcessor: Send {
    fn prepare(&mut self, sample_rate: u32, max_block: usize);
    fn process(&mut self, io: &mut ProcessBlock<'_>); // in-place, RT-safe
    fn reset(&mut self);
}

/// Future: time-stretch/pitch-shift (Rubber Band-style) behind the same
/// contract, with an offline path for cache pre-render.
pub trait TimeStretcher: AudioProcessor {
    fn set_ratio(&mut self, time_ratio: f64, pitch_ratio: f64);
    fn latency_samples(&self) -> usize;
}
```

Planned uses: varispeed monitoring, tempo-conform of imported clips (offline,
cached under `cache/`), and pitch-corrected vocal comping fed by Whisper pitch
data.

## 9. Build & environment status (as scaffolded)

* Node 24 / npm 11, cargo & rustc 1.94 present.
* ALSA dev libs present → cpal builds.
* **Missing system packages** (block `cargo check` of the Tauri stack on this
  machine): `libwebkit2gtk-4.1-dev`, `libjavascriptcoregtk-4.1-dev`,
  `libsoup-3.0-dev`. Install those and `cargo check` should pass — the
  first-party crate code compiles against the tauri 2 API.

## 10. Design constraints for scalability (binding on prototype code)

AURA's strategic target is an FL Studio-class DAW (500+ tracks, plugin
hosting, pattern-based MIDI sequencing, millions of timeline events). The full
roadmap lives in `docs/SCALABILITY.md`; known limitations of the frozen
prototype surface are catalogued there in the **Debt register** (D-01…D-10)
and must NOT be "fixed" by breaking frozen schemas or command names now.

The following constraints are cheap today and expensive to retrofit, so they
bind prototype implementation work immediately. They clarify — never
contradict — the sections above.

**Audio engine (Agent 2)**

1. Implement the render loop as a **schedule executor over an immutable,
   atomically swapped graph snapshot** even while the "graph" is just
   `clip players → track strips → master`. §2.3's prepare-then-pointer-swap
   rule has no exceptions, not even single-track edits. (SCALABILITY §1.)
2. `set_track_gain` / `set_track_pan` / mute / solo route to **parameter
   changes** (preallocated param ring or atomics, smoothed in the node) —
   never to a graph rebuild, and never by mutating the running snapshot.
3. On the RT thread, tracks/nodes are identified by **dense slot indices**
   assigned at graph-compile time; UUID strings, `String`, `HashMap`, and
   `log::` never cross onto the RT thread. UUID↔slot mapping is
   control-thread-only.
4. Audio buffers are **channel-counted** (frames × channels / planar slices),
   not hardcoded stereo pairs, even while everything is stereo (debt D-08).
5. Every future `AudioProcessor` reports `latency_samples()` (as
   `TimeStretcher` in §8 already does) so plugin-delay compensation can be a
   graph-compiler feature, not a per-node retrofit.
6. Keep all `cpal`-specific code inside one backend layer; the engine core
   talks to a device abstraction (stable device ids, negotiated stream
   params, xrun/latency reporting) so PipeWire-native/ASIO/JACK backends can
   be added per-OS later (SCALABILITY §7, debt D-01). Do not persist cpal
   display-name device ids in any project file — device choice is app config,
   not project data.

**Persistence & project data (Agent 2)**

7. `project.json` v1 is frozen: no ad-hoc extra fields. New fields go through
   the v2 design in SCALABILITY §3–§4 (tempo map + ticks, patterns, binary
   event chunks). Any *reader* of project.json must tolerate and preserve
   unknown fields (serde: ignore unknowns; never `deny_unknown_fields`) —
   the schemas' `additionalProperties: false` describes what we *emit*, not
   what we accept (debt D-06).
8. No new time-position field anywhere may be introduced in seconds; audio
   stays sample-based (as specced), and future *musical* positions use ticks
   against a tempo map (debt D-02).
9. Do not implement undo/redo ad hoc (UI-local or engine-local). Undo arrives
   with the batched op-log protocol (SCALABILITY §5, draft
   `docs/ipc-schemas/op-envelope.schema.json`); the prototype ships without
   undo rather than with a throwaway one.

**IPC & frontend (Agents 2, 4)**

10. The frozen per-command surface stays; new command *names* may be ADDED
    additively (via Agent 1/orchestrator, since `lib.rs` is frozen), and any
    new mutation surface must be batch-shaped (`{ops: [...]}`) rather than
    one-invoke-per-property (debt D-03).
11. Frontend stores must be **delta-ready**: apply the returned object/op to a
    keyed store instead of refetching `get_tracks` after every mutation, so
    the later op-subscribe migration is a transport change, not a store
    rewrite.
12. UI lists that scale with project size (tracks, mixer strips, clips) are
    virtualized from the start; no component iterates all tracks per frame.
    Per-pixel surfaces (meters, waveforms) are canvas-painted behind the one
    texture-targeting renderer interface — WebGL2 today; WebGPU is not coming
    to WebKitGTK, so it is a per-platform backend option, never the assumed
    upgrade path *(updated 2026-08-13, see CORE-REDESIGN-ROUND-2 §9)* — fed
    from one rAF-driven commit point. High-rate data never travels as app
    events (§3.4 rule restated).
13. Meter internals should not hard-cap track count beyond the single POD
    queue element (debt D-05 documents the planned chunked meter blocks);
    everything downstream of the queue sizes dynamically.

**Sidecars (Agent 3)**

14. Job submission goes through a queue/scheduler object (initially trivial
    FIFO) rather than spawning processes directly in command handlers, so
    concurrency limits, priorities, and GPU budgeting (SCALABILITY §6) are a
    policy change later. Keep the internal job record's params as an opaque
    per-kind blob; do not bake "one input file, one output dir" into the
    registry.

---

## 11. The control plane seam (`src-tauri/src/control/`) — phase 2

AURA now has TWO front doors: the Tauri IPC surface (WebView) and the
embedded MCP server (§12). Both drive the app through ONE object:

```
Tauri commands ─┐                       ┌─ Store (Mutex)      ─┐
                ├─► control::ControlPlane ─ ParamTable (atomics)├─► engine
MCP tools ──────┘        │              └─ EngineHandle       ─┘  control
                         ├──► sidecars::JobManager                thread
                         └──► midi::MidiStore
```

* `control::ops` holds the *single implementation* of each mutation
  (`apply_track_mix`, `add_track`, `transport_snapshot`, …) as pure,
  tauri-free functions over `Store`/`ParamTable`. Unit-tested standalone.
* `control::ControlPlane` is the managed facade: transport, recording,
  project ops, batched mix, job submission, and a **latest-meter cache** (a
  permanently subscribed `MeterSink`) that gives non-streaming callers (MCP)
  peak/RMS readback.
* The frozen per-command handlers in `audio/` and `sidecars/` are now thin
  wrappers over `ControlPlane` / `control::ops`. The per-property
  `set_track_*` setters delegate to the batched `set_track_mix` path — they
  are single-op wrappers exactly as SCALABILITY §5 planned (D-03 direction).
* **Rule: MCP tool handlers call `ControlPlane` methods only.** Never Tauri
  commands, never module internals. If a tool needs something ControlPlane
  lacks, the method is added to ControlPlane (architect approves), not
  reached around.
* Events: ControlPlane emits app events through an injected `EventEmitter`
  closure, so `transport://state` / `project://changed` fire identically no
  matter which front door caused the change.

## 12. Embedded MCP server (`src-tauri/src/mcp/`) — phase 2, zone A

### 12.1 Shape

rmcp (official Rust MCP SDK) **streamable-HTTP** server running inside the
Tauri process on **127.0.0.1:41717** (port configurable via policy). Started
from `mcp::init`; live since the phase-2 merge (`mcp/server.rs`: rmcp's
service core behind a deliberately small, fully tested HTTP/1.1 layer in
`mcp/http1.rs` — the frozen dependency set has no hyper/axum; a swap to
rmcp's `StreamableHttpService` tower layer remains possible later without
touching handler/policy/session code). Client setup: `docs/mcp-usage.md`.

### 12.2 Tool roster (frozen names)

`get_project_state`, `read_meters`, `get_job_status` (read-only);
`create_project`, `add_track`, `set_track_mix` (batched), `transport_control`,
`record_take`, `import_audio_clip`, `run_sidecar_job` (destructive).
Definitions + ControlPlane mapping: `src-tauri/src/mcp/tools.rs`.
`read_meters` returns the latest 60 Hz `MeterFrame` — the agent's "ears".

### 12.3 Security requirements (MANDATORY, non-configurable)

1. **Bind 127.0.0.1 only.** Never 0.0.0.0, never a LAN interface.
2. **Bearer session token**: 256-bit, regenerated at every app launch
   (`mcp/auth.rs`), constant-time compared, delivered out-of-band (UI
   reveal / 0600 file), never logged beyond an 8-char fingerprint.
3. **Origin validation** on every HTTP request: requests carrying an Origin
   header must be loopback (`127.0.0.1`/`localhost`/`[::1]`) or are rejected
   (DNS-rebinding defense). Checks run BEFORE MCP dispatch.
4. **Policy layer** (`mcp/policy.rs`, wire: `mcp-policy.schema.json`): modes
   `readOnly` / `confirmDestructive` (default) / `full`, plus per-tool
   overrides; unknown tools always denied. `Confirm` parks the call as a
   `PendingConfirmation`, surfaces `mcp://confirm-requested` to the UI, and
   resolves via the `mcp_confirm_pending` command (timeout ⇒ deny).

## 13. Musical time: ticks + tempo map (`src-tauri/src/midi/`) — phase 2, zone C

Debt **D-02 is being paid down**. Frozen rules:

* All musical positions are **integer ticks at the project PPQ**
  (default 960). Audio clips stay sample-anchored (v1 semantics).
* `midi::TempoMap` is the only tick↔sample bijection (piecewise over sorted
  `{tick, bpm}` events, first at tick 0). The control thread converts during
  pre-roll; **the RT thread never sees ticks** — it receives sample-offset
  event blocks (`midi::synth::BlockNoteEvent`).
* project.json **v2** (`project-v2.schema.json`): `ppq` + `tempoMap` +
  `midiClips` + `instruments`; `tempoBpm` kept ≡ `tempoMap[0].bpm`.
  v1→v2 migration is mechanical (one-entry map); migration chain per
  SCALABILITY §4.
* Note payloads persist as **AMEV binary chunks** (`events/<id>.bin`,
  16-byte records, format in `midi/events.rs`) — never inline JSON.
* MIDI is audible via the built-in `PolySynth` (`midi/synth.rs`) or, when
  the track's additive `instrumentId` field resolves in the sampler bank, a
  sampler instrument (§14). **Phase 3: both are LIVE in-graph instrument
  nodes** — rendered block-by-block on the RT thread behind the
  `LiveInstrument` seam (§15), fed sample-offset `BlockNoteEvent`s sliced
  from control-thread-prescheduled event lists. The phase-2 control-side
  pre-render is retired.

## 14. Sampler instrument (`src-tauri/src/audio/sampler.rs`) — phase 2, zone D

* Loads the **AURA SFZ SUBSET v1** — normative opcode table in `sampler.rs`
  (headers control/global/group/region; sample, key/lokey/hikey,
  pitch_keycenter, lovel/hivel, tune, transpose, volume, pan, offset,
  loop_mode/start/end, ampeg_attack/release, default_path). The parser ships
  and is unit-tested; the subset is the contract the Stable Audio
  instrument-builder sidecar generates against.
* Engine integration follows the existing discipline: instrument compiles to
  an immutable `SamplerNode` installed by RCU build-and-swap; preallocated
  voice pool (64) with oldest-note stealing; pitch mapping
  `2^((key−root+transpose+tune/100)/12)` with linear interpolation (v1);
  `AudioProcessor::process` under the §2.1 RT contract.
* Instruments used by a project are copied to `<project>/instruments/<id>/`
  (portable, relative `sfzRef` — `sampler-instrument.schema.json`).
* Phase 3: `SamplerNode` is installed as a LIVE per-track instrument node
  (§15) instead of pre-rendering; its `set_block_events`/`queue_event` +
  `process` contract is unchanged.

## 15. Live instrument nodes & plugin hosting (`src-tauri/src/plugins/`) — phase 3

### 15.1 The live-node seam (implemented)

The engine hosts **live instrument nodes inside the RCU graph**. This
replaces the phase-2 control-side MIDI pre-render and is the seam every
instrument — built-in or third-party — sits behind:

```
control thread (rebuild)                        RT thread (mixer::render)
────────────────────────                        ─────────────────────────
midi clips ──TempoMap──► Vec<AbsNoteEvent>      per block, per live track:
(ticks -> abs samples,   (sorted, Arc'd into      slice events [pos, pos+run)
 control side ONLY)       the snapshot)           -> BlockNoteEvent offsets
                                                  -> node.queue_event(...)
LiveNodeRegistry ───────► Arc<LiveNodeCell> ───►  node.process(&mut scratch)
(nodes keyed by track;    shared by SUCCESSIVE    -> gain/pan/mute -> mix+meters
 PolySynth / SamplerNode  snapshots: voice &
 / plugin node)           plugin state SURVIVES
                          graph swaps
```

Contracts (all enforced in code today):

* **`dsp::LiveInstrument`** (`queue_event(BlockNoteEvent) -> bool`,
  `all_notes_off()`, plus the `AudioProcessor` §2.1 rules). Implemented by
  `PolySynth`, `SamplerNode`, and `plugins::host::StubPluginNode`.
* **`rt::LiveNodeCell`** — an `UnsafeCell` node cell shared between
  successive graph snapshots. Sound because exactly one snapshot is rendered
  at a time by exactly one RT thread; the control thread constructs +
  `prepare`s a node only BEFORE its first snapshot is published, and frees
  cells only control-side. This is also precisely what plugins need: a
  plugin instance must never be re-instantiated per rebuild.
* **`rt::RtGraph.scratch`** — stereo live-render scratch preallocated at
  BUILD time (`MAX_LIVE_BLOCK` frames; larger callbacks render in chunks).
  The RT thread never allocates.
* **Discontinuities.** The output callback detects seek/stop→play
  (`base != next_pos`) and passes `discontinuity=true` into `render`; live
  nodes get `all_notes_off` (release, not kill). A loop wrap inside a block
  splits the render run and releases voices at the wrap (their note-offs
  lie beyond the loop end and would otherwise never arrive).
* **Automation seam (phase-3 merge).** `render_live` calls
  `LiveInstrument::set_block_context(base_pos, discontinuity)` (default
  no-op) before every contiguous run, delivering the run's ABSOLUTE
  timeline position + whether it breaks continuity (seek, stop→play, loop
  wrap). Automation-aware wrappers
  (`plugins::automation::GainAutomatedNode`) re-seed their compiled-ramp
  cursors from it, so ramps stay position-true across seeks and loops —
  proven by headless `mixer::render` tests. Lane→node attachment during
  graph rebuild is the next automation round.
* **Transport loop region.** `transport_set_loop` (additive command,
  control-plane `TransportAction::SetLoop`) sets `SharedRt`'s
  loop atomics AND the store mirror, so the region persists in
  `project.json`'s transport block and both advance paths (output callback
  + headless) wrap inside it. The UI's ruler pins/LOOP chip drive this
  command; MCP agents see the region through `get_project_state`.
* **Ticks never cross onto the RT thread** (§13 unchanged): scheduling is
  control-side; the RT side only slices sorted absolute-sample events.

### 15.2 Plugin formats & crates (decided, verified on this machine)

* **CLAP — `clack-host` + `clack-extensions` 0.1.x** (crates.io, CLAP 1.2
  ABI, maintained; pinned pre-1.0). Verified: entry/factory/descriptor read
  of `/usr/lib/clap/*.clap`. Zone P1 builds instantiation + processing on it.
* **LV2 — `livi` 0.7** over system `liblilv` (**build prereq:
  `apt install liblilv-dev`**). livi's `Features` provides exactly
  ZynAddSubFX's required set (urid:map, options, boundedBlockLength,
  worker:schedule) + atom-sequence MIDI. Verified: **ZynAddSubFX
  instantiates and renders audio through livi in-process on this machine**
  (headless pitch-checked acceptance test). LV2 `state:interface` is raw
  lilv on top of livi handles (`plugins::state`, zone P4 — shipped).
* **VST3 later**, per SCALABILITY §2, behind the same internal contract.

### 15.3 Discovery / lifecycle / safety (AS BUILT, phase-3 zones + merge)

* **Scan** (`plugins::scan`): LV2 via the lilv world — metadata only, no
  plugin code loaded, safe. CLAP descriptors are read by a **sacrificial
  scan subprocess** (`plugins::scan_worker`, paying D-11's scan half): the
  current binary re-executes itself with `AURA_SCAN_WORKER=1` — guarded at
  the **top of `main.rs`**, before Tauri boots — dlopens each `*.clap`
  under `~/.clap:/usr/lib/clap:/usr/local/lib/clap:$CLAP_PATH` inside that
  worker, and streams NDJSON descriptors over stdout. A crashing bundle
  kills the worker, not AURA; the in-process `scan_clap_paths` remains as
  the in-worker body (and test path). `plugin_scan` runs on a blocking
  worker thread, never the main thread.
* **Instantiation lifecycle**: ONE dedicated **plugin main thread**
  (`aura-plugin-host`, `plugins::host::plugin_main`; CLAP
  `[main-thread]`/lilv affinity) serves BOTH formats — CLAP entries +
  instances live in its `"clap"` `MainCtx` slot (`clap_host`), the
  `livi::World` + LV2 registrations in its `"lv2"` slot (`lv2_host`; the
  interim `aura-lv2-host` thread was merged away). Requests are closures
  through `PluginMainThread::run`/`post` (the `engine::start` pattern);
  per-format tickers pump CLAP `on_main_thread` callbacks and LV2 workers.
  Activation yields the RT half (CLAP `StartedPluginAudioProcessor` /
  `livi::Instance`) which moves into a `LiveInstrument` node behind the
  §15.1 seam; instances the hosts can't serve stay `"stub"` and render
  **silence** (never a misleading fallback sound).
* **Track binding**: `set_track_instrument` accepts (additively)
  `instrumentId = "plugin:<instanceId>"`; unknown refs fall back to
  PolySynth with a warning so MIDI is never silently muted by a missing
  plugin.
* **Parameters**: flat `ParamId(u32) → {name, range, value}` per instance
  (plugin-state.schema.json), `plugin_set_param` is **batch-shaped from
  birth** (D-03). Values mirror into the registry for the generic param UI
  and forward through wait-free rings to the RT nodes. Automation
  groundwork ships alongside: `automation_get`/`automation_set` (lanes in
  project v2, AMEV point chunks under `<project>/automation/`), tick→sample
  compilation, and the §15.1 `set_block_context` playback seam.
* **State (zone P4 + merge)**: chunked opaque blobs
  `<project>/plugins/<instanceId>.state` (APST container) referenced from
  project v2 (`persistedInstance.stateRef`), never inline JSON. The
  registered `state::FormatStateBridge` serializes LIVE instance state on
  the plugin main thread at save: CLAP via the state extension
  (`KIND_OPAQUE` host streams), LV2 via raw-lilv `state:interface` on livi
  handles (`KIND_LV2_PROPS` property lists — Zyn patches round-trip; the
  LV2 host keeps a lazy main-thread shadow instance for state calls and
  re-applies loaded blobs to every future RT node). On open, instances
  restore as `"stub"`, re-activate through their format host, get their
  param snapshot back, and their blob loaded — a plugin missing on this
  machine keeps row + snapshot + blob forever (nothing is ever dropped).
* **GUIs**: plugin editor windows are **plugin-owned/floating** in v1
  (CLAP gui floating; LV2 `ui:showInterface` — which is all ZynAddSubFX
  offers anyway). The Tauri WebView gets only the generic param UI; X11
  embedding (`ui:parent`/XEmbed) is a later opt-in, Wayland embedding does
  not exist yet (2026) and is explicitly out of scope.
* **Isolation (honest v1 risk)**: plugins run **in-process**; a crashing or
  RT-violating plugin can take AURA down or glitch audio. Mitigations now:
  scan-time risk documented (D-11), instruments-only roster, stub-first
  bring-up. Evolution (unchanged from SCALABILITY §2): per-instance
  out-of-process bridge behind the same node handle (shared-memory audio
  ring + event pipe, yabridge-style), default-on for unscanned/flaky
  plugins. The `LiveNodeCell` indirection is the bridge's insertion point.

### 15.4 Phase-3 command surface (registered, additive)

`plugin_scan`, `plugin_list`, `plugin_instantiate`, `plugin_remove`,
`plugin_get_params`, `plugin_set_param` (batched) — payloads in
`plugin-descriptor.schema.json` / `plugin-state.schema.json` — plus the
architect-merge additions `automation_get` / `automation_set` (lane
upserts, batch-shaped: one invoke carries a lane's full point set) and
`transport_set_loop` (loop region + enabled flag; persisted with the
project). The MCP tool roster stays frozen this round; candidate future
tools (`list_plugins`, `set_plugin_param`) are noted in PHASE3-PLAN §6,
not registered — loop and plugin state are visible to agents through
`get_project_state`/`plugin_list` equivalents on the control plane.

## 16. The Composer: a pure theory library + one harmony document — Plan H1

Product doc: `docs/backlog/composer-assistant.md`. Plan and rulings:
`docs/superpowers/plans/2026-08-17-plan-h-composer.md`.

### 16.1 Two halves, one seam

```
        ┌──────────────────────────────────────────────────────────┐
        │  src-tauri/src/theory/   PURE — no tauri, no locks,      │
        │  no I/O, no state, no thread_rng                         │
        │  tpc · scale · chord · analysis · circle · palette ·     │
        │  metre · rng · harmony · progression · voicing · bass ·  │
        │  melody · groove                                         │
        └───────────────────────▲──────────────────────────────────┘
                                │ pure calls
        ┌───────────────────────┴──────────────────────────────────┐
        │  control/composer.rs — the ONE stateful seam             │
        │  harmony_get · harmony_set · composer_palette ·          │
        │  composer_suggest · composer_generate                    │
        └──────▲──────────────────────────────────▲────────────────┘
               │ Tauri commands                   │ (H6: MCP later)
      COMPOSER panel + piano-roll tint      agents compose too
```

**The purity contract is load-bearing, not stylistic.** `theory/` may not
`use tauri`, `parking_lot`, `std::fs`, `crate::control` or `crate::audio`, and
every generator takes an explicit `seed: u64` — no `thread_rng` anywhere. That
is what keeps the theory suite fast enough to run on every save, makes a
generator testable without a project, and keeps the library reusable by a
future notation surface or a WASM build. One dependency on the rest of the
crate is deliberate: the generators emit `crate::midi::MidiNote`, the
tauri-free document note type, because a parallel note struct would buy
nothing but a conversion at every call site.

**Determinism is a contract** (ruling H-4): same seed + same params ⇒
byte-identical notes, tested per generator. A reply carries the seed that
produced it, so a take the user liked is recoverable.

### 16.2 The harmony document

`MidiStore.harmony` — `HarmonyDoc { keys: Vec<KeySpan>, chords: Vec<ChordSpan> }`,
integer ticks at the project PPQ. It lives beside the tempo and meter maps
because it is the same kind of thing: a map over musical time that every
surface reads and no track owns. Consequences:

* **No new `TrackKind`, no new lane** (H-5) — the chord ribbon is chrome.
* **One op**, `Op::HarmonySet { keys, chords }`, `Op::TempoSet`'s shape and for
  the same reason: a chord region and the key it is analysed in are ONE edit
  (H-4). It validates before mutating, computes its inverse from store truth,
  and sets **no** `effect.rebuild` — the harmony document is not scheduled and
  makes no sound of its own. `OP_FORMAT_VERSION` stays **2**.
* **Persisted additively**: the `harmony` key in `project.json`, written only
  when the document is non-empty, so a project that never used the Composer
  resaves byte-diff-free and `schemaVersion` does not move. A malformed block
  loses the harmony, not the song.
* **In the published snapshot** (`MidiSnapshot.harmony`) and in
  `get_project_state`, so a reader holding an image never re-locks.
* Wire form spells keys and chords as **strings** (`"C ionian"`, `"Cmaj7"`):
  `Tpc`'s line-of-fifths integer encoding stays an implementation detail, a
  journal line stays readable, and an enharmonic distinction survives a
  round trip (`C♭maj7` does not come back as `Bmaj7`).

### 16.3 Generation

`composer_generate` prepares OUTSIDE the transaction (the `hum.rs`
prepare-outside pattern — voice leading is a DP over the whole progression and
has no business under the session lock), then commits the whole sketch in
**one** transaction: the harmony write, the tracks it needs
(`ops::add_track_tx`), and up to four `Op::MidiClipAdd`s. One Ctrl+Z removes
the idea rather than a quarter of it (H-7/H-8); a rejected request writes
nothing.

Generated clips are **ordinary clips** (H-6): minted note ids, a content id,
the track's own lane, editable in the piano roll, undoable, `.mid`-exportable.
There is no generated-clip type and no regeneration link. The why-strings ride
back with the reply as `Annotation`s for display and are never document state.

### 16.4 Where the theory is written down

Each module's head comment carries the derivation and the citation; the
non-obvious ones are worth knowing about before touching them:

| Module | The fact it is built on |
|---|---|
| `tpc.rs` | The line of fifths: `pitch_class = (fifths * 7) mod 12`, `letter = "FCGDAEB"[(fifths+1) mod 7]`. Spelling, key signatures and transposition are arithmetic. |
| `scale.rs` | A diatonic mode is a CONTIGUOUS 7-window on that line, so a key signature is a subtraction. |
| `palette.rs` | Berklee's avoid note: a key tone a semitone ABOVE a chord tone. The seven-chord table is a test, including the `Fmaj7` row (the ♯11 must come out available). |
| `circle.rs` | A key's whole chord vocabulary is a contiguous arc; distance is relatedness; counter-clockwise is the strongest drive. |
| `metre.rs` | Lerdahl & Jackendoff's metrical tree (ONE weight function drives placement, backbeat, accents and every velocity — H-10), Bjorklund/Euclid (Toussaint 2005), Longuet-Higgins & Lee syncopation as a NUMBER. |
| `melody.rs` | Motif + development, with pitches as indices into the key's ladder so a transposed copy stays diatonic for free. |

