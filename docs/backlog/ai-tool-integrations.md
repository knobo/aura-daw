# AI tool integrations — grounded plan (2026-08-15)

**Status:** planning. Not a new architecture. An inventory of what
already ships, what the four named tools actually are, and the thinnest
path to add the one that is still missing.

This exists because a generic "design AI integrations from scratch"
prompt would rebuild Phase 1 of the sidecar stack. That stack landed in
Plan E Task 11 and phase-2 generation. Do not open a parallel job
queue, a second IPC, or an ONNX-in-Rust inference path.

## 1. What already exists (do not rebuild)

```
Svelte (Generate dock / clip overlay / MCP run_sidecar_job)
        │  sidecar_run_job(kind, params)     [additive, open kind string]
        ▼
Rust job manager   src-tauri/src/sidecars/jobs.rs
        │  spawn python3 worker, NDJSON on stdout
        │  progress / log / done / error
        │  NEVER on the audio callback
        ▼
sidecars/*.py      --params-json '{...}' [--simulate]
        │  writes a WAV / MIDI / SFZ to a path
        ▼
Control plane      import_audio_clip / MidiClipAdd / sampler_load
                   (one transaction, undoable, Actor::Agent if MCP)
```

Payloads are **paths**, not buffers. The worker writes a file; Rust
imports it. That is the zero-copy / no-leak contract. Do not pass
PCM or MIDI blobs through JSON.

Landed kinds (`JobKind` in `sidecars/types.rs`):

| Kind | Worker | What it does |
|---|---|---|
| `stemSplit` | `demucs_split.py` | 4-stem Demucs |
| `transcribe` | `whisper_transcribe.py` | lyrics / speech, optional pitch |
| `humToMidi` | `hum_to_midi.py` | monophonic hummed melody → MIDI |
| `amtInfill` | `amt_infill_worker.py` | piano-roll region infill |
| `aceStepGenerate` | `ace_step_worker.py` | text → WAV |
| `aceStepRepaint` | same | time-window inpaint |
| `aceStepAudio2Audio` | same | restyle |
| `elevenLabsMusic` | `elevenlabs_music_worker.py` | cloud song draft |
| `stableAudioSfz` | `stable_audio_sfz_worker.py` | multisample → SFZ |

Simulate mode (`AURA_SIDECAR_SIMULATE=1` / `--simulate`) is mandatory
for every new worker. The job overlay, MCP `run_sidecar_job`, and
`importToTrackId` already exist. A new model is a new `JobKind` +
worker + schema `$defs` + a thin UI affordance.

RT rule (unchanged): inferencing is a sidecar process. The engine
hears the result only after import/rebuild.

## 2. The four named tools, honestly

### MuScriptor (Kyutai + Mirelo, 2026-07)

Real. Open. Multi-instrument transcription of a mixed recording into
per-part MIDI, no prior stem labels required. Repo:
`github.com/muscriptor/muscriptor`. This is the one capability AURA
does **not** have. `humToMidi` is monophonic hummed input; AMT is
infill of an existing clip; Whisper is lyrics. None of them turn a
mixed pop/metal/jazz take into a stack of MIDI clips.

**DAW wedge vs FL:** FL's Newtone/Edison is single-source pitch
editing. Multi-instrument "drop a song, get a piano-roll arrangement"
is a real differentiator if the output is editable MIDI on existing
tracks, one undo.

**VRAM:** 1.3B-class; treat as GPU-optional with a smaller checkpoint
fallback. Always ship `--simulate` that emits a deterministic
multi-clip MIDI fixture.

### Foundation-1 (RoyalCities, Stable Audio Open)

Real. Tempo/key/bar-structured **loops and samples**, not full songs.
HF: `RoyalCities/Foundation-1`. Close cousin of the already-landed
`stableAudioSfz` worker. Value is BPM-locked loops that drop on the
timeline, not a new pipeline.

Do this as a new kind `foundationLoop` on the existing generate dock
+ library PRESETS/SAMPLES, conditioned on the project's tempo map
(section table → BPM at the playhead). Do not stand up ComfyUI.

### MiniMax Music 3

Real open weights (`MiniMaxAI/MiniMax-Music3`): 8B planner + 0.6B
local + flow VAE, up-to-5-minute songs. Local VRAM budget is in the
"dedicated 24 GB card" class. AURA already covers the *product* with
`aceStepGenerate` (local) and `elevenLabsMusic` (cloud). MiniMax is a
quality/length upgrade of those kinds, not a new UX.

Defer until ACE-Step 1.5 is the default local generate path and
someone with a big GPU asks. If it lands, it is `minimaxGenerate`
with the same params shape as `aceStepGenerate` (prompt, lyrics,
duration, seed, outputPath).

### ACE-Step 1.0 / ACE-Step 1.5

1.0 is already integrated (generate / repaint / audio2audio) via
`ace-step/ACE-Step` and `ACEStepPipeline`. 1.5 XL is **not** that
stack and **not** a `ACESTEP_CHECKPOINT_DIR` swap: 1.5 is a separate
package (`ace-step/ACE-Step-1.5`, LM planner + DiT, turbo 8-step /
sft 50-step). XL is a 4B DiT variant of *that* stack (≥12 GB with
offload). The unfinished half on 1.0 is the inpaint UX (region →
`aceStepRepaint`). Finish the UX before a package bump.

## 3. What we will not do

- A second job queue, gRPC gateway, or ComfyUI as a required runtime.
- ONNX/ort inside the `aura` RT process. Models stay out-of-process.
- Passing PCM/MIDI as JSON arrays (the I-5 lesson, just for audio).
- New op kinds for "AI did this". Import/clip-add/midi-clip-add in
  one `Actor::Agent` (or User) transaction. Undo is free.
- Blocking the plugin-main or audio callback on inferencing.
- Building MiniMax or Foundation-1 before MuScriptor and the
  ACE-Step *UX* are heard.

## 4. Implementation sequence

Metronome/count-in (#38) and plugin-param curves (#39) have landed.
`next-prompt.md` still owes `gesture_end(id)`, multi-clip delete, and
Plan F (history storage). Prefer those first; they no longer
hard-block Slice 0/1.

When we pick this track up:

### Slice 0 — no new models (days)

- Timeline region → `aceStepRepaint` (start/end from selection,
  existing kind). Hear it.
- Clip overlay (same chrome as `SPLIT STEMS` on the selected clip):
  add "Transcribe MIDI…" pointing at the *new* kind below, disabled
  until the worker is in.
- Document VRAM/simulate in `docs/mcp-usage.md` / Generate dock
  empty-state.

### Slice 1 — `museTranscribe` (the actual new kind)

**Worker:** `sidecars/muse_transcribe.py` (same NDJSON, `--simulate`).

**Params (schema `$defs`):**

```json
{
  "inputPath": "…/clip.wav",
  "outputDir": "…/project/midi-extract/",
  "maxParts": 8,
  "tempoBpm": 120,
  "ppq": 960,
  "timelineStartTicks": 0
}
```

Worker (simulate and real) must rescale emitted SMF to that map.
Do not infer a private tempo and hope import fixes it.

**Result:** `{ "kind": "museTranscribe", "parts": [
  { "name": "drums", "midiPath": "…/drums.mid" },
  { "name": "bass",  "midiPath": "…/bass.mid" }
] }`.

**Control plane:** a dedicated clip-id command, mirror
`split_stems_for_clip`: `transcribe_midi_for_clip(clip_id)`. Backend
resolves `project_dir + clip.source_path`. A new sink wrapper parses
`parts`, `import_smf`s outside the lock, and commits one
`TrackAdd`+`MidiClipAdd` batch at the source clip's timeline start
*without* `TempoSet` (extracted MIDI is already on the project map).
Source audio clip stays. One undo pops the whole extract.

Do **not** reuse `importToTrackId` / `wrap_sink_with_import`: that
path only reads `result.outputPath` and will ignore `parts[].midiPath`.
N calls to `midi_import_file` would be N undos and can `TempoSet`
the project from SMF tempo events.

**Frontend:** clip overlay + scan overlay (reuse stem-split job
chrome). Piano roll opens the first landed clip. Frontend only
re-pulls after the job; it does not invoke import.

**Tests:** simulate worker golden MIDI rescaled to the passed tempo
map; control-plane test that one commit produces N midi clips,
inverts cleanly, and does not `TempoSet`; frontend store test that a
finished `parts` result triggers one project reload, not an import
invoke.

### Slice 2 — `foundationLoop`

Params: prompt, bars, seed, outputPath. Worker reads BPM from
params (frontend ships `project.tempoBpm` / playhead section).
Import as one audio clip at the playhead, length snapped to bars.
Library PRESETS can list recent loops. Simulate = metronomic WAV.

### Slice 3 — ACE-Step 1.5 (worker/package bump)

New install target and pipeline call (`ace-step/ACE-Step-1.5`).
Keep 1.0 as the fallback package (or an env that selects which
worker/package to spawn). 1.5 XL weights will not boot through the
current `ACEStepPipeline` constructor.

### Slice 4 — MiniMax (optional)

Same shape as `aceStepGenerate`. Gate on a detected GPU memory
class; otherwise the Generate dock says "needs 24 GB" and offers
ElevenLabs / ACE-Step.

## 5. Data model (additive only)

```
JobKind += MuseTranscribe | FoundationLoop | MinimaxGenerate
```

No new `AiTask` trait. No new `TranscriptionJob` type in the
engine. The sidecar job **is** the task. The document sees ordinary
clips.

Tempo: workers never guess BPM. The frontend/control plane passes
`tempoBpm` / `ppq` / region ticks. Thin renderer (ADR 0006).

Takes: out of scope here. That is Plan F + the Track C "takes"
deferral. AI output is a new clip (or a new take when that exists),
never a silent overwrite of the source.

## 6. VRAM / errors

Existing pattern, reuse it:

- `--simulate` always works.
- Missing python deps → worker `error` line, UI toast, job overlay.
- Optional env: `AURA_SIDECAR_SIMULATE=1` for the whole session.
- Per-worker offload flags (ACE-Step already has
  `ACESTEP_CPU_OFFLOAD`). MuScriptor gets the same.
- Model files live under a user cache dir (not the repo). First run
  downloads; hash-check if the vendor publishes one.

## 7. Directory touch list (slice 1)

```
sidecars/muse_transcribe.py          new worker
docs/ipc-schemas/sidecar-job-v2.schema.json   $defs
src-tauri/src/sidecars/types.rs      JobKind arm
src-tauri/src/sidecars/mod.rs        kind → worker (run_generic_job)
src-tauri/src/control/import.rs      transcribe_midi_for_clip + parts sink
src-tauri/src/control/mod.rs         run_sidecar_job auto-apply if needed
src/lib/state/generation.svelte.ts   adopt the new result shape
src/lib/components/ClipView.svelte   overlay action (same as SPLIT STEMS)
```

`JobSpec` is constructed at the dispatch site, not as a per-kind
entry in `jobs.rs` (`JobSpec` there is only the struct).
`jobs.svelte.ts` still only knows `stemSplit` | `transcribe`;
open-kind UI/MCP jobs land in `generation.svelte.ts`.

`engine.rs` untouched. `OP_FORMAT_VERSION` stays 2.

---

**Recommendation:** prefer `gesture_end(id)` and Plan F first, then
Slice 0 (ACE-Step repaint UX) and Slice 1 (MuScriptor). Foundation-1
and MiniMax wait.
