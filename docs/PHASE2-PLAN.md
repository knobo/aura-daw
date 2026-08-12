# AURA — Phase 2 Plan: parallel-agent zones & frozen surface

Status: authoritative for the phase-2 build round. Owned by the architect.
Companion to `ARCHITECTURE.md` (esp. §§10–14) and `SCALABILITY.md`.

Four features are built IN PARALLEL by four agents. This file defines who
owns which directories, what is frozen, the exact surface each agent codes
against, and the cross-zone contracts that were fixed NOW so nobody blocks
on anybody.

The scaffold is green: `cargo check` clean, `cargo test` = **78 passing**
(60 round-1 + 18 phase-2 scaffold tests), `npm run build` + `svelte-check`
clean. Keep it that way: every zone's definition of done includes all 78
existing tests still passing.

---

## 1. Zones

| Zone | Feature | Owns (exclusive write access) |
|------|---------|-------------------------------|
| **A — MCP** | Embedded MCP server | `src-tauri/src/mcp/` (all of it) |
| **B — GenMusic** | ACE-Step + ElevenLabs sidecars, audio import | `src-tauri/src/sidecars/`, `/sidecars/*.py`, `src-tauri/src/control/import.rs` (new file; see §3.B) |
| **C — MIDI/AMT** | Tick timeline, MIDI model, synth, AMT infilling | `src-tauri/src/midi/` (all of it), `/sidecars/amt_infill_worker.py` |
| **D — Sampler** | SFZ sampler + Stable Audio instrument builder | `src-tauri/src/audio/sampler.rs` (+ new `audio/sampler_*.rs` submodules), `/sidecars/stable_audio_sfz_worker.py` |

Shared-but-guarded (architect arbitrates conflicts; edit ONLY as itemized in
your brief below):

* `src-tauri/src/audio/` outside `sampler*` — zones C and D each get a
  *named integration point* (§3) instead of free rein. Anything else in
  `audio/` requires an architect request.
* `src-tauri/src/control/mod.rs` + `control/ops.rs` — architect-owned.
  Zones may ADD methods to `ControlPlane` only when their brief says so;
  never change existing method signatures.

**FROZEN files — nobody edits (request via report instead):**
`src-tauri/src/lib.rs`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`,
`src-tauri/capabilities/default.json`, `src-tauri/build.rs`, `package.json`,
`vite.config.ts`, `svelte.config.js`, `tsconfig.json`, `index.html`,
everything in `docs/` (including this file and all schemas), and **all of
`/src` (frontend)** — no UI work this round (§5).

Rules of engagement (unchanged from round 1):

* Command/tool/event NAMES are frozen; signatures/bodies evolve inside the
  owning module.
* Needed crates are already in `Cargo.toml` (`rmcp` 3.1 with
  `transport-streamable-http-server`, `midly` 0.5, `rand` 0.9). Anything
  else: request it.
* All 78 tests green + `cargo check` warning-free-ish before you report.
  Add tests for what you build (job-manager style: tauri-free, fake
  processes).

## 2. The frozen phase-2 surface

### 2.1 New Tauri commands (registered in lib.rs, stubs compile today)

| Command | Zone | Status in scaffold |
|---|---|---|
| `get_project_state` | architect (done) | real |
| `set_track_mix` (batched `changes: [TrackMixChange]`) | architect (done) | real |
| `import_audio_clip` | **B** | stub → implement |
| `sidecar_run_job` (open `kind` + `params`) | **B** | real dispatch, simulate-only workers |
| `set_tempo_map`, `midi_add_clip`, `midi_set_notes`, `midi_get_clips` | **C** | real in-memory; persistence/engine missing |
| `sampler_load_instrument`, `sampler_list_instruments` | **D** | real (parser + bank) |
| `sampler_preview_note` | **D** | stub → implement |
| `mcp_get_status`, `mcp_set_policy` | **A** | real |
| `mcp_confirm_pending` | **A** | registry stub → wire to parked calls |

### 2.2 MCP tools (zone A; names in `mcp/tools.rs`)

Read-only: `get_project_state`, `read_meters`, `get_job_status`.
Destructive: `create_project`, `add_track`, `set_track_mix`,
`transport_control`, `record_take`, `import_audio_clip`, `run_sidecar_job`.
Each maps 1:1 onto an existing `ControlPlane` method (table in `tools.rs`).

### 2.3 New job kinds (open-kind wire strings, `sidecar-job-v2.schema.json`)

`aceStepGenerate`, `aceStepRepaint`, `aceStepAudio2Audio` (B);
`elevenLabsMusic` (B); `amtInfill` (C); `stableAudioSfz` (D).

### 2.4 New app event

`mcp://confirm-requested` (payload:
`mcp-policy.schema.json#/$defs/pendingConfirmation`).

### 2.5 Schemas (source of truth, architect-owned)

`project-v2.schema.json`, `midi-clip.schema.json`,
`sidecar-job-v2.schema.json`, `mcp-policy.schema.json`,
`sampler-instrument.schema.json`. All are emitter contracts WITHOUT
`additionalProperties:false` (D-06 settled); Rust readers ignore unknown
fields (serde default — never `deny_unknown_fields`).

## 3. Per-zone briefs

### Zone A — MCP server (`src-tauri/src/mcp/`)

Goal: an MCP agent (Claude etc.) can inspect, edit, mix, record and generate
in AURA over streamable HTTP, gated by policy + UI confirmation.

* Implement `mcp/server.rs::start`: rmcp streamable-HTTP server on
  `127.0.0.1:<policy.port>` (default 41717), started from `mcp::init`,
  `running` flag maintained. Tokio runtime: Tauri's (`tauri::async_runtime`).
* Tool handlers: exactly the §2.2 roster, params/results mirroring the
  corresponding command schemas; call `ControlPlane` methods ONLY (the Arc
  is `app.state::<Arc<ControlPlane>>()` — take a clone at startup). Never
  invoke Tauri commands, never touch `audio::*` internals.
* Security (ARCHITECTURE §12.3 — MANDATORY): loopback bind; bearer token
  from `McpShared.token` via `auth::token_matches` (constant-time);
  `auth::origin_allowed` on EVERY request before dispatch; policy
  `decide()` before every tool run; `Confirm` ⇒ create `PendingConfirmation`,
  emit `mcp://confirm-requested`, park the call (tokio oneshot) until
  `mcp_confirm_pending` or a 60 s timeout ⇒ deny. Token delivery: log the
  fingerprint only; write the full token 0600 to the app-data dir
  (`dirs::data_dir()/aura/mcp-token`) for local clients.
* For `run_sidecar_job`, pass a sink that only feeds the job registry (job
  events are polled via `get_job_status`; no channel fan-out needed).
* Tests to add: policy gate on every destructive tool, origin/token rejection
  paths (rmcp server can be exercised with plain HTTP requests), pending
  confirmation timeout. Keep `mcp/` tauri-light so tests run headless.

### Zone B — Generative music sidecars + audio import

Goal: text→music (local ACE-Step + cloud ElevenLabs), repaint and
audio2audio, results landing as clips on the timeline.

* Replace the simulate-only bodies of `/sidecars/ace_step_worker.py` and
  `/sidecars/elevenlabs_music_worker.py` with real implementations
  (`--simulate` MUST keep working — CI/dev has no model stack). Params /
  result shapes are frozen in `sidecar-job-v2.schema.json`; NDJSON protocol
  unchanged; ElevenLabs key from `ELEVENLABS_API_KEY` env only (never in
  params, never in logs).
* Implement `ControlPlane::import_audio_clip` in NEW file
  `src-tauri/src/control/import.rs` (mod wiring: ask architect or use
  `include!`-free `pub mod import;` addition to `control/mod.rs` — that one
  line is pre-approved). Behavior: validate wav/flac, copy into
  `<project>/audio/<clipId>.wav`, probe channels/rate/length (hound), append
  to `Store.clips` (sample-based placement, `at_samples` or 0), build the
  waveform pyramid (`audio::waveform`), send `ControlMsg::Rebuild`, return
  the `Clip`. Auto-create a track when `track_id` is None. This unblocks
  BOTH the MCP tool and the "generate → hear it" loop.
* Wire a post-job convenience: a `done` result with `outputPath` +
  `importToTrackId` param (optional, additive) triggers import through the
  same `ControlPlane` method (control-plane call, not a Tauri command).
* Keep the 11 job-manager tests green; extend with: unknown-kind rejection
  (exists), params-json forwarding, one simulate-mode end-to-end per worker.
* Job manager evolution allowed (queue/scheduler per SCALABILITY §6) but not
  required this round; concurrency stays semaphore-limited.

### Zone C — MIDI + AMT infilling (`src-tauri/src/midi/`)

Goal: MIDI tracks with tick-based clips that PLAY audibly, tempo map wired
into the engine, and AMT infilling filling selected regions.

* Persistence: project v2 (`project-v2.schema.json`) — extend save/load.
  The v1 reader/writer lives in `audio/project.rs` (guarded); your named
  integration point is: add v2 fields via a `serde_json::Value` round-trip
  or request architect merge — do NOT restructure `project.rs`. Migration
  v1→v2 mechanical (one-entry tempo map from `tempoBpm`); write
  `project.json.v1.bak` before first v2 save (SCALABILITY §4). Notes go to
  AMEV chunks (`midi/events.rs` — format frozen, tested).
* Engine audibility: implement `midi/synth.rs::PolySynth` as an
  `AudioProcessor` and the pre-roll path: control thread converts ticks →
  sample-offset `BlockNoteEvent`s via `TempoMap` (never on the RT thread),
  feeds them through the graph-snapshot rebuild. Your named integration
  point in `audio/`: `engine.rs`'s graph-build step gains a hook for
  midi-track sources — coordinate the exact seam with the architect if it
  exceeds ~30 lines; the RCU swap discipline (§10.1–10.3) is non-negotiable.
  If a midi track has an `instrumentId` and zone D's sampler node is ready,
  prefer it; `PolySynth` is the always-available fallback.
* AMT: real `/sidecars/amt_infill_worker.py` (anticipation library),
  driven via `sidecar_run_job` with `amtInfillParams` (context notes +
  region ticks — schema frozen). Apply results through `midi_set_notes`
  semantics (whole-clip replace) — merge logic lives Rust-side, not in the
  worker. `--simulate` keeps producing the deterministic arpeggio.
* `midly` is available for .mid import/export if you get to it (optional).
* Tests: tempo-map ↔ engine conversion at block boundaries, AMEV
  persistence round-trip through project save/load, synth block rendering
  (pure, no device).

### Zone D — Sampler + instrument builder

Goal: SFZ-subset instruments load, play on midi tracks, and can be
generated from a text prompt.

* The SFZ SUBSET v1 parser is DONE and tested (`audio/sampler.rs`); the
  opcode table there is the frozen contract — extending the subset needs an
  architect request (it is also the generator's output contract).
* Implement the playback half: sample loading (hound; preload whole files
  v1), `SamplerNode` implementing `AudioProcessor` (§14 contract: RCU
  snapshot, 64-voice preallocated pool, oldest stealing, linear-interp
  pitch, ampeg attack/release), `sampler_preview_note` (audition through
  the master bus without a project), and the midi-track binding
  (`instrumentId` on `TrackState` — ADDITIVE optional field; coordinate the
  one-line `TrackState` addition with the architect; readers tolerate its
  absence). New submodules as `audio/sampler_voice.rs` etc. are yours.
* Coordinate with zone C ONLY through the frozen `BlockNoteEvent` contract
  (`midi/synth.rs`) — do not import zone C internals.
* Implement `/sidecars/stable_audio_sfz_worker.py` for real: generate one
  sample per requested key/layer, trim/normalize, loop-detect, write
  `<outputDir>/<name>.sfz` (SUBSET v1 ONLY) + `samples/*.wav` +
  result per `sfzInstrumentResult`. `--simulate` writes a tiny valid .sfz
  with silent wavs so the load path is testable end-to-end without models.
* Tests: voice allocation/stealing (pure), pitch-ratio math, region lookup
  by key/velocity, end-to-end "simulate-generated .sfz loads and reports
  correct key range".

## 4. Dependency notes (what was frozen NOW so zones stay parallel)

* **D depends on nothing from B/C to start**: the SFZ subset + parser exist;
  `BlockNoteEvent` is frozen. C's synth and D's sampler implement the same
  event contract independently.
* **B's instrument-builder ↔ D's sampler**: bound by SFZ SUBSET v1 +
  `sfzInstrumentResult` (schema). D owns the generator worker; B only ever
  touches audio-generation workers.
* **A depends on `ControlPlane` only** — fully scaffolded; A never waits for
  B/C/D. Tools whose backing is stubbed (`import_audio_clip`) return the
  stub error until B lands; that is acceptable mid-round.
* **C ↔ engine**: the only cross-zone code seam this round (midi sources in
  the graph build). Flag it early in your reports; architect merges.
* Time units: ticks everywhere musical (D-02 rules in ARCHITECTURE §13);
  `TempoMap` is the only converter; sample-anchored audio unchanged.
* Nobody touches v1 schemas, `project.json` v1 writing stays valid until C's
  migration lands.

## 5. Frontend partition (LATER round — no UI work now)

`/src` stays frozen this round. When the UI round starts, the planned
partition is:

* `src/lib/state/` — one keyed, delta-ready store per domain: `tracks`,
  `clips`, `midi` (clips+tempo), `jobs`, `mcp` (status+pending confirms),
  `instruments`. Stores apply returned objects/ops (ARCHITECTURE §10.11);
  `get_project_state` is cold-start only.
* `src/lib/components/mcp/` — MCP status panel + confirm dialog
  (`mcp://confirm-requested` listener).
* `src/lib/components/pianoroll/` — canvas piano roll over `midi_*`
  commands (virtualized; density tiles later).
* `src/lib/components/generate/` — prompt UIs over `sidecar_run_job` +
  job progress; instrument builder UI over the same.
* `src/lib/types/` — TS mirrors are generated/hand-written per schema file;
  none were added this round (zero-risk rule: backend-only round).

## 6. Verification gate (every zone, before reporting)

```
cd src-tauri && cargo check && cargo test        # 78 + your new tests, all green
cd .. && npm run build && npx svelte-check       # must stay green (untouched)
AURA_SIDECAR_SIMULATE=1 cargo tauri dev          # app boots, no panic in your init path
```
