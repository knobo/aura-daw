# Contributing to AURA

Thanks for looking under the hood. AURA is unusual: the code was written by
orchestrated AI agents, and the *boundaries* that made that work — a frozen
IPC surface, schema-defined contracts, strict module ownership — are also what
make it a pleasant codebase for humans. Please keep them intact.

## Dev setup

Prerequisites and system packages are in the [README](README.md#quickstart).
Short version:

```sh
sudo apt install libasound2-dev libwebkit2gtk-4.1-dev build-essential \
  curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev \
  librsvg2-dev liblilv-dev ffmpeg
npm install
npm run tauri dev        # full app (vite on port 1420, strictPort)
npx vite                 # browser-only demo mode (mock engine, no Rust build)
```

`AURA_SIDECAR_SIMULATE=1` makes every AI worker produce deterministic
placeholder output — develop AI-adjacent features without any model stack.

### Sidecar Python on PATH

`sidecars_run_job` (and MCP's `run_sidecar_job`) spawns the **first
`python3`/`python` found on `PATH`** — see `resolve_python()` in
`src-tauri/src/sidecars/jobs.rs`. If you've set up `.venv-sidecars` (real
model stack, [README](README.md#ai-features--real-model-setup)), launch with
it prepended so sidecar jobs actually see `anticipation`/`torch`/
`transformers`/`mido`/`demucs` instead of falling back to (or erroring on)
whatever `python3` the system PATH resolves first:

```sh
PATH="$PWD/.venv-sidecars/bin:$PATH" npm run tauri dev
```

Without a real model stack, skip this and set `AURA_SIDECAR_SIMULATE=1`
instead.

### Restarting on a new `main`

The dev binary does not hot-reload Rust changes, and the MCP bearer token
rotates on every launch — so after pulling new commits, restart both:

```sh
git status                       # make sure nothing local would be discarded
git pull --ff-only origin main   # fast-forward only; stop and ask if it can't be
# stop the running `npm run tauri dev` (Ctrl-C, or kill the pid group)
PATH="$PWD/.venv-sidecars/bin:$PATH" npm run tauri dev
```

If you're driving AURA over MCP, re-run `claude mcp add` (or update
whichever client config holds the token) after the restart — the old token
stops working the moment the new one is generated. Full client setup:
[docs/mcp-usage.md](docs/mcp-usage.md).

## Checking the pitch detector

```sh
cargo run --manifest-path src-tauri/Cargo.toml --example pitch_check -- 15 A3 --tone=0.35
cargo run --manifest-path src-tauri/Cargo.toml --example pitch_check -- 15   # sing/hum/whistle instead
```

Opens the default capture device and runs the live pitch chain — the same
`PitchTap` the audio callback drives and the same `PitchWorker` the engine
spawns — printing input level, detected pitch, cents error, voiced fraction
and clarity. `--tone` plays the target out of the speakers so nothing has to
be sung, which is the only way to measure the *detector* rather than the
singer. It does not exercise the Tauri command layer.

## Running the tests

```sh
cd src-tauri && cargo test   # 1261 tests (1225 lib + 36 integration, plus 2 #[ignore]d plugin repros; counted 2026-08-18 after G1 Task 5 merged into the Composer, Plan H1)
npm test                     # 836 frontend tests (829 unit + 7 DOM-mounted; vitest; counted 2026-08-18 after the read-only version-history browser)
npx svelte-check             # frontend types
npm run build                # production frontend build must stay green
```

**A Bluetooth default output sink fails 18 engine tests, and it does not look
like an audio problem.** The engine opens a stream and publishes a sample rate
— so it appears to have started — and then no callback ever arrives: the
transport stays at 0, no meter frames appear, and every `control::loopjam`
test reports `"audio engine did not respond"`. Point the test process at a
real ALSA sink instead. This changes nothing globally, so your own audio stays
where it is:

```sh
PULSE_SINK=$(pactl list short sinks | grep alsa_output | head -1 | cut -f2) \
  cargo test -- --test-threads=1
```

Measured: 1024/1024 in 73 s that way, against 1002/1020 in 882 s over
Bluetooth — the timeouts are the runtime. Check `pactl get-default-sink`
before filing an engine, transport, loopjam or meter failure as a regression.

The engine tests are not the only casualty. A Bluetooth sink also makes the
engine open its output stream at **44 100 Hz** where an ALSA sink gives it
48 000, and the stream open commits that rate into the document
(`engine::commit_output_sample_rate`, transient). Any test that snapshots the
document twice around a live engine can catch the change in between — e.g.
`tests/pure_readers.rs`'s `library_audition_core_leaves_the_document_and_the_history_untouched`
fails with `"sampleRate": 48000` vs `44100` and reads exactly like a real
mutation bug. Same fix: give the test process a real ALSA sink.

`midi_out::tests` is **known flaky as a module**, not as individual tests:
its cases share a process-global hub and open real ALSA virtual ports, so
under a parallel run any one of them can lose the race. Three distinct names
have flaked so far; re-run the module in isolation before believing a failure
there (`cargo test --lib midi_out::tests`).

Some tests are **gated** and skip politely when their requirements are absent:

- **Plugin tests** (Zyn acceptance + state round-trip, CLAP lifecycle, LV2
  params) need `zynaddsubfx-lv2`, `dpf-plugins-clap`/`dpf-plugins-lv2`,
  `mda-lv2` installed via apt. A skipped plugin test prints why.
- **Real-model integration tests** (`src-tauri/tests/real_models.rs`) drive the
  actual Demucs/ACE-Step models:

  ```sh
  AURA_REAL_MODELS=1 AURA_REAL_PYTHON=$PWD/.venv-sidecars/bin/python \
      cargo test --test real_models -- --nocapture
  ```

- Crash-prone synth probes re-execute the test binary in a subprocess
  (`AURA_SYNTH_PROBE` guard) so a plugin segfault is a recorded verdict, not a
  dead suite.

Run `cargo test` and `npx svelte-check` before opening a PR; if you touched
plugin hosting, install the plugin packages so the gated tests actually run.

## The rules that matter

### 1. Real-time discipline (non-negotiable)

Anything that can run on the audio callback thread must not allocate, lock,
syscall, log, panic, or loop unboundedly. All shared state crosses via
preallocated lock-free rings or RCU snapshot swaps. Read
[ARCHITECTURE §2](docs/ARCHITECTURE.md) before touching `src-tauri/src/audio/`
or any render path — reviewers will hold the line here.

### 2. The frozen IPC surface

Tauri command *names*, event names, and the v1 wire schemas in
[docs/ipc-schemas/](docs/ipc-schemas/) are frozen. Evolution is **additive**:
new commands, new optional fields, v2 schemas. New schemas must NOT set
`additionalProperties: false` (they are emitter contracts; readers ignore
unknown fields — debt D-06, settled). If a change seems to require breaking a
frozen contract, open an issue first — there is usually an additive path, and
the [SCALABILITY debt register](docs/SCALABILITY.md) may already sketch it.

### 3. Ownership zones

The repo is organized so that parallel contributors (human or agent) don't
collide: `audio/`, `midi/`, `plugins/`, `mcp/`, `sidecars/`, `control/`, and
`src/` (frontend) each have a single owner per round, and cross-cutting state
goes through the **control plane** (`src-tauri/src/control/`) — the one seam
both Tauri commands and MCP tools drive. Follow the same instinct: keep changes
inside one zone, and route new UI-visible state through the control plane
rather than wiring modules to each other. History and the FROZEN-file list:
[ARCHITECTURE §0](docs/ARCHITECTURE.md).

### 4. No ad hoc undo, no per-feature protocols

Undo/redo arrives with the op-log protocol (D-03). Don't build a local one.

## How to add things

**A new AI sidecar**
1. Drop a worker script in `sidecars/` speaking the NDJSON protocol on stdout
   (progress / log / result events — copy the shape from
   `sidecars/amt_infill_worker.py`) and **implement `--simulate`** honoring
   `AURA_SIDECAR_SIMULATE` — CI and modelless machines depend on it.
2. Run it through `sidecar_run_job`: the v2 job surface takes an open `kind`
   string + opaque params (`docs/ipc-schemas/sidecar-job-v2.schema.json`), so
   no Rust enum or schema change is needed. Observers treat unknown kinds
   generically.
3. If it needs first-class treatment (dedicated command, auto-import), extend
   `src-tauri/src/sidecars/mod.rs` and the control plane, with tests.

**A new plugin format**
Goes in `src-tauri/src/plugins/` behind the existing seams: descriptor scan in
the sacrificial scan worker, instantiation on the shared plugin main thread,
RT processing behind `LiveNodeCell`. Mirror the acceptance-test pattern
(instantiate → note → headless render → assert non-silence/pitch) and the
state-persistence contract (opaque blob + param-snapshot fallback).
[ARCHITECTURE §15](docs/ARCHITECTURE.md) is the map.

**A new IPC schema**
Add `docs/ipc-schemas/<name>.schema.json` (draft-07, emitter contract, no
`additionalProperties: false`), keep the Rust/TS types in sync by hand, and
validate in a test like the existing schema round-trip tests.

## Style

- Rust: rustfmt defaults, no new warnings; comments explain *why* (the
  codebase leans on doc-comments heavily — keep that up).
- Frontend: Svelte 5 runes, TypeScript strict, Tailwind for styling;
  `npx svelte-check` stays clean.
- Tests accompany behavior. Bug fixes come with the regression test that would
  have caught them.
