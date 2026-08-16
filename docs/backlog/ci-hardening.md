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
