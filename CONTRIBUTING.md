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

## Running the tests

```sh
cd src-tauri && cargo test   # 501 tests (counted 2026-08-14)
npx svelte-check             # frontend types
npm run build                # production frontend build must stay green
```

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
