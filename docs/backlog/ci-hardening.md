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
5. **The parallel `--lib` SIGSEGV — DIAGNOSED AND FIXED (2026-08-28).**

   The symptom was real: three consecutive default-parallelism runs gave
   `signal: 11, SIGSEGV` twice and one ordinary failure, where
   `--test-threads=1` passed 1407/1407 repeatedly. **The diagnosis this
   entry used to carry was wrong on both counts**, and both wrong turns are
   worth keeping, because either one would cost the next agent a day:

   - *"A SIGSEGV is memory unsafety, not a lost race."* True, but not OUR
     memory. The faulting thread is named `alsa-pipewire` and its whole
     stack is third-party — `libpipewire-0.3.so.0`, called from
     `libpipewire-module-protocol-native.so` via `libspa-support.so`'s
     event loop. There is no AURA frame in it.
   - *"Prime suspect is LV2 hosting."* No. The crash reproduces 3 runs out
     of 4 with **every `plugins::` test skipped**. `lilv`/`livi` are not
     thread-safe, but they are not this.

   **What it actually was.** `WorkerCommand::current_exe()` re-executes the
   current binary and relies on the `AURA_SCAN_WORKER` guard at the top of
   `main` to route the child into `worker_main`. A libtest binary has no
   such guard, so the child ran **the entire test suite again**. It printed
   test output where the parent expected NDJSON; the parent ignored it as
   harness noise (a deliberate part of the wire protocol), waited out the
   15 s `LINE_TIMEOUT`, killed it, respawned, and waited again. Meanwhile
   the recursive suite opened every audio device its engine tests asked
   for.

   Two lib tests reached that path, both calling
   `scan_clap_subprocess(&clap_search_paths())` directly:

   - `control::tests::plugin_add_of_an_insert_rehosts_as_effect`
   - `control::tests::reactivate_restored_hosts_an_insert_as_effect`

   Each cost **29.7 s** of pure timeout and drove the PipeWire **daemon**
   from 164 to 578 open file descriptors. `scan_worker`'s own tests never
   hit it: they ask for `test_worker_command()` explicitly, whose doc
   comment had described this exact hazard since it was written.

   From there the chain is other people's software failing honestly under
   resource exhaustion:

   1. `pipewire` and `wireplumber` run under systemd's
      `LimitNOFILESoft=1024` (hard limit 1048576) — check with
      `grep 'open files' /proc/$(pgrep -x pipewire)/limits`.
   2. The two recursive suites plus the parallel run's own ~25 live engines
      pin the daemon at **exactly 1024** fds. Measured by sampling
      `/proc/<pipewire>/fd` during a run: 441 → 842 → 1021 → 1024, then the
      crash.
   3. At that point the daemon cannot finish a client handshake. The user
      journal says so: `mod.access: … flatpak check failed: Too many open
      files` and `mod.client-node: … unknown peer … fd:1018`. **wireplumber
      then dies** — `wireplumber.service: Main process exited, code=dumped,
      status=11/SEGV`. This is why the owner had to restart pipewire by
      hand mid-session.
   4. Our test process segfaults inside `libpipewire` handling that broken
      connection.

   **The fix (this PR).** Under `cfg(test)`,
   `WorkerCommand::current_exe()` returns the hidden-entry command instead
   of a bare re-exec. That closes the class rather than the two call sites:
   any test reaching a production scan path now gets a real sacrificial
   worker, and `the_lib_test_worker_command_never_re_executes_the_suite`
   asserts the routing so it cannot come back.

   Result, measured on the same machine in the same sitting:

   | | before | after |
   |---|---|---|
   | full `--lib`, default parallelism | SIGSEGV in 2 of 3 runs | **0 of 5** |
   | daemon fd peak, full run | 1024 (its limit) | 578 |
   | `control::` alone, fd peak | 1024 | 469 |
   | the two offending tests | 29.7 s each, +414 daemon fds | 0.33 s each, +0 |

   **What is still open, and it is not the same bug:**

   - **18 tests still fail under default parallelism** — the same 18 names
     before and after the fix, identical across runs. They are the
     engine-starvation family — `control::loopjam::*` reporting "audio
     engine did not respond", plus
     `audio::engine::tests::engine_pumps_meter_frames_at_60hz` and
     `mcp::server::tests::read_meters_hears_the_headless_engine` — and each
     passes in isolation. That is item 4's territory, not a crash.
     `--test-threads=1` is still what CI should use.
   - **The headroom is ~2.4×, not comfortable.** A clean parallel run now
     costs the daemon ~300–420 fds on top of whatever else the desktop is
     playing, against a 1024 soft limit. A box with a bigger `nproc` or a
     busier audio server can still reach it. The honest fix if it recurs is
     to bound how many engine tests hold a device at once, not to raise the
     limit.
   - **The integration-test half of the same hole.**
     `tests/plugin_load_profile.rs` calls `plugins::scan::scan_all()` twice, and an
     integration test binary links the lib WITHOUT `cfg(test)` — so it
     still gets a bare re-exec and will run its own suite recursively.
     Both call sites are gated (`AURA_PROFILE_PLUGINS` /
     `AURA_PROFILE_MAX_US`), so a plain run never reaches them, but
     `scripts/perf-check.sh --run full` does. Fixing it needs a hidden
     worker entry in that binary plus a way to install it (a
     `set_worker_command` override), which is a separate change from this
     one.
   - **Orphaned PipeWire clients survive a signal death.** A process killed
     by SIGSEGV never runs `Drop`, so its streams are left standing; the
     daemon's idle fd baseline crept 105 → 164 over a session of
     reproducing this. Same mechanism as the leftover `zynaddsubfx-ext-gui`
     windows below. Recovery is
     `systemctl --user restart wireplumber pipewire-pulse pipewire`.

   **Still worth doing, unchanged by this fix:** make the GUI child die
   with its parent regardless of how the parent dies — `PR_SET_PDEATHSIG`
   via `CommandExt::pre_exec` on the `zynaddsubfx-ext-gui` spawn
   (`plugins/lv2_ui.rs`). `Drop` cannot help under SIGKILL. Needs `libc` as
   a direct dependency of `src-tauri`, whose `Cargo.toml` is **FROZEN** —
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
