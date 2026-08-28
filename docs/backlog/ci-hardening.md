# CI hardening — plugin-gated and real-model tests

**Status:** backlog. First version of `.github/workflows/tests.yml` runs
`cargo test` and the frontend suite (vitest, svelte-check, build) on every
PR, but two categories of tests are currently skipped rather than exercised:

1. **Plugin-gated tests** (Zyn acceptance + state round-trip, CLAP lifecycle,
   LV2 params). These need `zynaddsubfx-lv2`, `dpf-plugins-clap`/
   `dpf-plugins-lv2`, `mda-lv2` installed (see README quickstart). The CI
   `rust` job does not install them, so these tests skip cleanly (by design —
   see CONTRIBUTING.md) instead of actually running.
2. **Real-model integration tests** (`src-tauri/tests/real_models.rs`). These
   need `AURA_REAL_MODELS=1` plus a real `.venv-sidecars` Python stack
   (Demucs/ACE-Step etc.) and are not run in CI at all.
3. **Two MIDI hardware tests** are explicitly `--skip`ped in the `rust` job's
   `cargo test` invocation:
   `midi_input::tests::select_nonexistent_port_is_graceful_err` and
   `midi_out::tests::opening_a_nonexistent_output_port_is_a_graceful_error`.
   Both open a real ALSA sequencer client (`midir::MidiInput::new` /
   `MidiOutput::new`) *before* looking at ports, and that call itself fails
   on the GitHub-hosted `ubuntu-24.04` runner — there's no ALSA sequencer
   subsystem (`/dev/snd/seq`) available — with "MIDI support could not be
   initialized" instead of the "port not found" the tests expect. Confirmed
   locally: these pass fine on a machine with a real `snd_seq` kernel module
   loaded. Not a code bug — a missing OS-level dependency in the runner.
4. **`cargo test` runs single-threaded** (`--test-threads=1`) in CI. Several
   `midi_out::tests::*` tests spawn real background threads (note-off /
   shutdown machinery) and race under default parallel execution — confirmed
   locally too: three consecutive full `cargo test --lib` runs (default
   parallelism) each failed a *different* `midi_out` test. This is a
   pre-existing test-isolation/flakiness issue in the test suite, independent
   of CI; forcing `--test-threads=1` makes CI deterministic without touching
   the tests or the code they exercise.
5. **The parallel `--lib` run does not just flake, it CRASHES** (found
   2026-08-28, while answering a question about leftover plugin windows).
   Three consecutive default-parallelism runs on a developer machine:

   ```
   run 1: signal: 11, SIGSEGV: invalid memory reference
   run 2: test result: FAILED. 1406 passed; 1 failed
   run 3: signal: 11, SIGSEGV
   ```

   The same suite passes 1407/1407 under `--test-threads=1`, repeatedly.
   This is a different and worse failure than item 4's races: a SIGSEGV is
   memory unsafety, not a lost race, and no assertion catches it. Prime
   suspect is LV2 hosting — `lilv`/`livi` are not thread-safe and
   `plugins::host::plugin_main()` is a single thread, while parallel tests
   register and drop LV2 instances concurrently. **Not diagnosed.**

   **It has a visible consequence.** `zynaddsubfx-ext-gui` is a separate
   PROCESS that Zyn's DPF ExternalWindow UI spawns, and the only thing that
   kills it is `OpenLv2Gui::drop` -> `hide()` -> `kill_ext()`
   (`plugins/lv2_ui.rs`). `Drop` does not run when a process dies by
   signal, so every crash after a GUI test orphans a window on the
   developer's desktop. That is the "one or two zyn windows left behind"
   people see.

   Two separable pieces of work:

   - **Diagnose the SIGSEGV.** The real bug. Start by running the lib
     suite under a debugger or `RUST_TEST_THREADS=2` bisecting the module
     set; the LV2 tests are the first thing to isolate.
   - **Make the GUI child die with its parent** regardless of how the
     parent dies: `PR_SET_PDEATHSIG` via `CommandExt::pre_exec` on the
     `zynaddsubfx-ext-gui` spawn. That fixes the orphan even under
     SIGKILL, where no amount of `Drop` discipline can. Needs `libc` as a
     direct dependency of `src-tauri`, whose `Cargo.toml` is **FROZEN** —
     ask the owner first. `libc` is already in the lockfile transitively.

## Why deferred

Getting a first green CI pipeline landed mattered more than full coverage.
Installing plugin packages and building `.venv-sidecars` in CI both add
non-trivial runtime/complexity to the workflow, so they were left out of v1.

## Next steps (when picked up)

- Add `zynaddsubfx-lv2 dpf-plugins-clap dpf-plugins-lv2 mda-lv2` to the apt
  install step in the `rust` job so the plugin-gated tests actually run
  instead of skipping.
- Decide whether real-model tests belong in the PR-blocking workflow at all
  (they're heavy) — likely a separate, manually-triggered or nightly workflow
  that sets up `.venv-sidecars` and runs `AURA_REAL_MODELS=1 cargo test
  --test real_models`.
- Get a real ALSA sequencer device into the CI runner (e.g. `sudo modprobe
  snd_seq snd_seq_dummy` — untested whether GitHub-hosted runners permit
  this) so the two `--skip`ped MIDI tests can run for real, then drop the
  `--skip` flags.
- Root-cause the `midi_out::tests::*` thread race so `--test-threads=1` can
  be dropped and the suite runs at full parallelism again in CI.
