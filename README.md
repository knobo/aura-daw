# AURA

AURA is a digital audio workstation (DAW) prototype built with **Tauri v2** and **Svelte 5**.
The long-term ambition is an FL Studio-class DAW; this repository is the first working
vertical slice: a native Rust audio engine, a reactive Svelte UI, and an AI sidecar layer
for stem separation and transcription.

## Features

- **Rust audio engine** (`src-tauri/src/audio/`) — cpal-based playback/transport, lock-free
  real-time mixer, per-track gain/pan, sample-accurate transport, and a **loop region**
  (draggable pins in the timeline ruler + LOOP toggle; persisted with the project, voices
  release cleanly on every wrap).
- **Recording** — multitrack input capture straight to WAV.
- **Meters** — peak/RMS metering streamed to the UI at 60 Hz.
- **Waveform renderer** — tiled min/max waveform pyramid rendered on canvas in the frontend.
- **AI stem separation** — Demucs Python sidecar (`sidecars/demucs_split.py`) splits a track
  into vocals / drums / bass / other, with streamed JSON progress.
- **AI transcription** — whisper.cpp sidecar (`sidecars/whisper_transcribe.py`) transcribes
  audio to text.
- **Job runner** (`src-tauri/src/sidecars/`) — queued, cancellable sidecar jobs with a
  line-delimited JSON protocol, concurrency limits, and structured errors.
- **MCP control** (`src-tauri/src/mcp/`) — an embedded MCP streamable-HTTP server on
  `127.0.0.1:41717` lets agents (Claude Code, Claude Desktop) inspect, mix, record and
  generate in the running session — token-authenticated, policy-gated with per-call
  confirm-in-UI by default. See **[docs/mcp-usage.md](docs/mcp-usage.md)**.
- **Generative music** — text→music via local **ACE-Step** (generate / repaint /
  audio2audio, `sidecars/ace_step_worker.py`) and cloud **ElevenLabs Music**
  (`sidecars/elevenlabs_music_worker.py`); results can auto-import straight onto the
  timeline (`importToTrackId` job param).
- **MIDI + AMT infilling** (`src-tauri/src/midi/`) — tick timeline with a tempo map,
  MIDI clips audible through a built-in synth, `.mid` import/export
  (`midi_import_file` / `midi_export_file`), and region infilling with the
  **Anticipatory Music Transformer** (`sidecars/amt_infill_worker.py`).
- **SFZ sampler + AI instrument builder** (`src-tauri/src/audio/sampler*.rs`) — loads
  SFZ-subset instruments (64-voice, RT-safe), plays them on MIDI tracks
  (`set_track_instrument`) and auditions without a project (`sampler_preview_note`);
  **Stable Audio Open** generates whole instruments from a text prompt
  (`sidecars/stable_audio_sfz_worker.py`) and auto-registers them on completion.
- **Plugin hosting — CLAP + LV2 instruments** (`src-tauri/src/plugins/`) — real in-graph
  hosting of third-party instruments on MIDI tracks (**ZynAddSubFX verified end-to-end**:
  headless render is pitch-checked in the test suite). One shared plugin main thread
  serves both formats; CLAP bundles are scanned by a **sacrificial subprocess** so a bad
  `.clap` can't take AURA down. Ships a plugin browser + generic parameter panel in the
  UI, **plugin state persistence** (opaque patch blobs — Zyn patches — saved with the
  project and restored on open, with param-snapshot fallback), and **automation
  groundwork** (`automation_get`/`automation_set` lanes persisted per project, plus the
  sample-accurate ramp seam in the render path).

## Prerequisites

- **Rust** (stable toolchain, via [rustup](https://rustup.rs))
- **Node.js 18+** and npm
- **Linux system packages** (for building Tauri on other machines):

  ```sh
  sudo apt install libasound2-dev libwebkit2gtk-4.1-dev build-essential \
    curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
  ```

- **liblilv-dev** — required to build the LV2 plugin host (`livi` links system lilv):

  ```sh
  sudo apt install liblilv-dev
  ```

- **Optional — plugins for the gated plugin tests / to actually play** (each test skips
  cleanly when its plugin is absent):
  - `sudo apt install zynaddsubfx-lv2` (universe) — the ZynAddSubFX instrument; enables
    the LV2 acceptance + state round-trip tests.
  - `sudo apt install dpf-plugins-clap dpf-plugins-lv2` — small open-source test
    instruments/effects (Kars, Nekobi, …); enables the CLAP lifecycle tests.
  - `sudo apt install mda-lv2` — mda EPiano etc., used by the LV2 param tests.

- **Optional — real AI sidecars** (each worker degrades gracefully when its stack is
  missing):
  - `pip install demucs` — stem separation
  - `whisper-cli` (whisper.cpp) on your `PATH` plus a GGML model — transcription
  - `pip install git+https://github.com/ace-step/ACE-Step.git` — ACE-Step music
    generation (checkpoints download on first use; CUDA GPU with ≥ 8 GB VRAM
    recommended)
  - `export ELEVENLABS_API_KEY=...` — ElevenLabs Music (cloud; no local model)
  - `pip install anticipation torch transformers mido` — AMT MIDI infilling (CPU works)
  - `pip install stable-audio-tools torch torchaudio einops` — Stable Audio Open
    instrument builder (model `stabilityai/stable-audio-open-small`; GPU optional)
  - **Or skip all of it and set `AURA_SIDECAR_SIMULATE=1`** for a fully offline demo:
    every worker (generation, infilling, instrument building included) produces
    deterministic placeholder output, so the whole generate → import → hear-it loop
    works with zero model stacks.

## Running

```sh
npm install
npm run tauri dev      # or: npx tauri dev
```

Pure-browser demo mode (no Rust build, mock engine data):

```sh
npx vite
```

## Tests

```sh
cd src-tauri && cargo test   # engine, MIDI, sampler, plugins, MCP, sidecars (~218 tests)
npx svelte-check             # frontend type checking
npm run build                # production frontend build
```

Plugin-dependent tests (Zyn acceptance, CLAP lifecycle, state round-trips) run for real
when the optional plugins above are installed and skip cleanly otherwise.

Note: the vite dev server uses port **1420** (`vite.config.ts`, `strictPort`). If 1420 is
taken, you can run on another port without editing anything:

```sh
npx tauri dev --config '{"build":{"devUrl":"http://localhost:1430","beforeDevCommand":"npm run dev -- --port 1430 --strictPort"}}'
```

## Repository layout

| Path                      | Contents                                              |
| ------------------------- | ----------------------------------------------------- |
| `src/`                    | Svelte 5 frontend (components, state, canvas render)  |
| `src/lib/tauri.ts`        | Typed IPC bridge to the Rust backend                  |
| `src-tauri/src/audio/`    | Audio engine: transport, mixer, recorder, meters, waveform, project persistence, SFZ sampler |
| `src-tauri/src/control/`  | Shared control plane — the one seam Tauri commands AND MCP tools drive |
| `src-tauri/src/midi/`     | Tick timeline, tempo map, MIDI clips/synth, .mid interop, AMT infill |
| `src-tauri/src/plugins/`  | CLAP/LV2 plugin hosting: shared plugin main thread, scan worker, state persistence, automation groundwork |
| `src-tauri/src/mcp/`      | Embedded MCP server: tools, policy, auth, HTTP layer  |
| `src-tauri/src/sidecars/` | Sidecar job runner (spawn, queue, cancel, JSON protocol) |
| `sidecars/`               | Python sidecars: Demucs, Whisper, ACE-Step, ElevenLabs, AMT infill, Stable Audio SFZ |
| `docs/`                   | Architecture and scaling docs, IPC schemas, MCP usage |

See **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** for the system design and
**[docs/SCALABILITY.md](docs/SCALABILITY.md)** for the path toward a full DAW.

## Sidecar environment variables

| Variable                  | Effect                                                        |
| ------------------------- | ------------------------------------------------------------- |
| `AURA_SIDECAR_DIR`        | Directory containing the sidecar scripts (default: `sidecars/`) |
| `AURA_SIDECAR_MAX_JOBS`   | Max concurrent sidecar jobs (excess jobs queue)               |
| `AURA_SIDECAR_SIMULATE`   | `1` = force simulate mode for ALL workers: deterministic placeholder output, no model stacks needed |
| `WHISPER_CPP_BIN`         | Path to the whisper.cpp binary (default: `whisper-cli` on `PATH`) |
| `WHISPER_MODEL_DIR`       | Directory searched for GGML Whisper models                    |
| `ELEVENLABS_API_KEY`      | ElevenLabs Music API key (required for real `elevenLabsMusic` runs; never logged) |
| `ELEVENLABS_BASE_URL`     | Override the ElevenLabs API base URL (testing)                |
| `ACESTEP_CHECKPOINT_DIR`  | Existing ACE-Step checkpoint directory (skips first-use download) |
| `ACESTEP_DTYPE`           | ACE-Step inference dtype (default `bfloat16`)                 |
| `ACESTEP_INFER_STEPS`     | ACE-Step diffusion step count override                        |
| `AURA_AMT_MODEL`          | Hugging Face model id for AMT infilling (default `stanford-crfm/music-medium-800k`) |
