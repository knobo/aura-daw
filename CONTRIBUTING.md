# Contributing to AURA

Thanks for looking under the hood. AURA is unusual: the code was written by
orchestrated AI agents, and the *boundaries* that made that work — a frozen
IPC surface, schema-defined contracts, strict module ownership — are also what
make it a pleasant codebase for humans. Please keep them intact.

## Dev setup

Prerequisites and system packages are in the [README](README.md#quickstart).
Short version:

```sh
scripts/setup.sh --all   # apt packages, Node deps, Rust check, plugins, Python
npm run tauri dev        # full app (vite on port 1420, strictPort)
npx vite                 # browser-only demo mode (mock engine, no Rust build)
```

`scripts/setup.sh` is idempotent and installs only what's absent; `--check`
reports without touching anything. Drop `--all` for just the build essentials.

Use Node 18–24 (`.nvmrc` pins 24). On Node 25 the app builds and runs, but
`npm test` fails: Node 25's built-in `localStorage` global shadows jsdom's.

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
cd src-tauri && cargo test
npm test
npx svelte-check             # frontend types
npm run build                # production frontend build must stay green
```

**"audio engine did not respond" is usually the environment, not your
change.** The engine opens a stream and publishes a sample rate — so it
appears to have started — and then no callback arrives: the transport stays
at 0, no meter frames appear, and every `control::loopjam` test reports
`"audio engine did not respond"`. Two different causes produce that same
line, and the timeouts are the runtime either way:

- **A Bluetooth default sink.** Check `pactl get-default-sink` before
  filing an engine, transport, loopjam or meter failure as a regression.
- **Parallel load.** The same ~18 tests fail deterministically under
  default parallelism and pass in isolation — engines starved of callbacks
  while ~25 of them hold a device at once
  (`backlog/ci-hardening.md` item 4). `--test-threads=1` is the answer,
  not a sink change.

To pin a sink, override the ALSA PCM. **`PULSE_SINK` does not work here**:
ALSA's `default` resolves to the *pipewire* plugin on this box, not the
pulse one, so the pulse client variable is never consulted (verified
2026-08-28 — see [`docs/TRAPS.md`](docs/TRAPS.md)).

```sh
cat > /tmp/asound-pin.conf <<'EOF'
</usr/share/alsa/alsa.conf>
pcm.!default { type pipewire playback_node "REPLACE_WITH_A_SINK_NAME" }
EOF
# `pactl list short sinks | grep alsa_output` names the candidates.
ALSA_CONFIG_PATH=/tmp/asound-pin.conf \
  cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
```

Run the local suite under a virtual display and single-threaded:

```sh
xvfb-run -a cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
```

Zyn's LV2 UI spawns `zynaddsubfx-ext-gui` as a separate process, so the GUI
tests open real windows; `xvfb-run` sends them to a display that dies with
the run. `--test-threads=1` is what CI uses. Unsetting `DISPLAY` instead
makes the three GUI tests pass without asserting anything — see
[`docs/TRAPS.md`](docs/TRAPS.md).

The parallel SIGSEGV that used to make `--test-threads=1` mandatory is
**fixed** (2026-08-28): a lib test's plugin scan was re-executing the whole
suite as its own subprocess worker, and the resulting flood of audio streams
exhausted the PipeWire daemon's file descriptors until wireplumber died and
took our client's connection with it. Zero signal deaths in twelve
default-parallelism runs, against three in four on the parent commit.

**Do not read that as "parallel runs are fine now."** They still fail 1–18
tests depending on how loaded the audio server is (item 4's engine
starvation), and about half of them still ABORT — `X Error of failed
request: BadWindow`, exit 1, no test summary — because Xlib's default error
handler calls `exit(1)` when we restack an already-closed plugin window
(item 5's open list). Single-threaded remains the gate.
`backlog/ci-hardening.md` items 4–5 have the measurements.

Counts, measured 2026-08-28 on `fix/parallel-test-sigsegv`: **1459 backend**
(1409 lib + 50 integration, 2 `#[ignore]`d plugin repros, 16 plugin-gated
tests skipped politely) — 64 s for the lib half plus 70 s for the
integration half, the latter including the link of its eleven binaries —
and **1383 frontend** across 126 files in ~5 s.

The lib half needs `-- --test-threads=1` to be measured at all: the
`midi_out::tests` race below flakes 2–5 names out of a parallel run, and
a failed suite prints no count.

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
  params, LV2 port properties) need `zynaddsubfx-lv2`,
  `dpf-plugins-clap`/`dpf-plugins-lv2`, `mda-lv2` and `zam-plugins`
  installed via apt. A skipped plugin test prints why. `zam-plugins` is
  there for ZamVerb specifically: its "Room" port is the only installed
  example of an `lv2:integer` port that also declares
  `pprops:expensive` + `kx:NonAutomatable`, which is what
  `lv2_host::tests::expensive_integer_port_is_reported_stepped_and_non_automatable`
  pins.
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
