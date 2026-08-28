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
   such guard, so the child ran **the entire test suite again**, and the
   parent BLOCKED until it finished: `recv_timeout` is reset by every line
   the child prints, a running suite prints constantly, so the 15 s
   `LINE_TIMEOUT` never fires and there is exactly one spawn — no respawn.
   Meanwhile the recursive suite opened every audio device its engine tests
   asked for.

   **Why nothing complained.** The worker env is INHERITED, so the recursive
   suite's own `worker_entry` case became a worker and emitted real NDJSON.
   Measured directly: a bare re-exec of the lib binary with the worker env
   set prints 1464 libtest lines *and* 71 valid protocol lines, then exits
   on its own after 13.5 s. So the parent got a CORRECT descriptor list,
   slowly, having run the suite twice to get it — the two tests passed
   rather than skipped, and no log line said anything was wrong. That is why
   this survived three weeks of being looked at.

   Two lib tests reached that path, both calling
   `scan_clap_subprocess(&clap_search_paths())` directly:

   - `control::tests::plugin_add_of_an_insert_rehosts_as_effect`
   - `control::tests::reactivate_restored_hosts_an_insert_as_effect`

   The cost varies, because how far the recursive suite gets before the
   parent kills it is nondeterministic: measured on `main`, the two tests
   took **6 s, 13 s and 29.7 s** across runs and drove the PipeWire
   **daemon** from a 164-fd baseline to peaks of **335, 427 and 578**.
   `scan_worker`'s own tests never hit any of it: they ask for
   `test_worker_command()` explicitly, whose doc comment had described this
   exact hazard since it was written.

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

   Result. The A/B is four default-parallelism runs of each binary, built
   from `main` and from this branch, alternated on one machine in one
   sitting:

   | | `main` | this branch |
   |---|---|---|
   | SIGSEGV (`rc=139`) | **3 of 4** | **0 of 12** |
   | `X Error … BadWindow` abort (`rc=1`) | 0 of 4 | 2 of 4 — see below |
   | ordinary test failure | 1 of 4 | 2 of 4 |
   | `--test-threads=1` | — | 1409 passed, ~64 s |

   and the narrower measurements behind it:

   | | before | after |
   |---|---|---|
   | daemon fd peak, full parallel run | 1024 (its limit) | 412–578 |
   | `control::` alone, fd peak | 1024 | 469 |
   | the two offending tests | 6–30 s each, +171…+414 daemon fds | 0.33 s each, +0 |
   | `control::` at `--test-threads=1` | 401 passed in 34 s | 401 passed in 11 s |

   **What is still open, and none of it is the same bug:**

   - **A parallel run now aborts on `X Error of failed request: BadWindow`
     in about half the runs** (`rc=1`, no core, no signal). This is NEWLY
     REACHABLE, not newly caused: `main` never got there because it
     segfaulted first, and the A/B above is 0 of 4 on `main` against 2 of 4
     here. It lands right after the three zyn GUI tests all report `ok`, at
     a very low request serial, so the shape is a stale window id —
     `wm_stack`'s `xdotool`-discovered ids get restacked (`XRaiseWindow` /
     `XSetTransientForHint`) after another test has closed that window.
     Xlib's DEFAULT error handler calls `exit(1)`, which is why it takes the
     process with it.

     **That is a product bug, not only a test bug.** A DAW must not exit
     because a plugin editor closed a millisecond before we restacked it —
     that is unsaved work gone. The fix is an `XSetErrorHandler` that logs
     and returns 0, resolved through the same runtime `dlopen` the module
     already uses, so the frozen `Cargo.toml` is untouched. The care needed
     is that the handler is process-GLOBAL and GTK/GDK installs its own:
     save and restore the previous handler around our own calls rather than
     installing ours for the process lifetime. `--test-threads=1` never hits it, so the gate is unaffected.

   - **Tests still fail under default parallelism, and how many depends on
     how loaded the audio server is.** Under the fd pressure that preceded
     the fix it was a stable **18** — identical names before and after, so
     not caused by this change: the engine-starvation family
     (`control::loopjam::*` reporting "audio engine did not respond", plus
     `audio::engine::tests::engine_pumps_meter_frames_at_60hz` and
     `mcp::server::tests::read_meters_hears_the_headless_engine`). On a calm
     box, with the daemon peaking at 412–453 instead of 1024, the same runs
     fail **1–2**: `plugins::clap_host::tests::post_params_returns_immediately_and_the_change_still_lands` and the documented
     `midi_out::tests::*` race. Every one of them passes in isolation. That
     is item 4's territory, not a crash; `--test-threads=1` is still what
     CI should use.
   - **The headroom is ~2.4×, not comfortable.** A clean parallel run now
     costs the daemon ~300–420 fds on top of whatever else the desktop is
     playing, against a 1024 soft limit. A box with a bigger `nproc` or a
     busier audio server can still reach it. The honest fix if it recurs is
     to bound how many engine tests hold a device at once, not to raise the
     limit.
   - **The integration-test half of the same hole, which fails the OPPOSITE
     way and is worse than "recursive".** An integration binary links the
     lib WITHOUT `cfg(test)`, so `tests/plugin_load_profile.rs`'s two
     `plugins::scan::scan_all()` calls still get a bare re-exec — and that
     binary has no hidden worker case at all. Measured: the child runs its
     own seven gated tests, returns in **3.5 ms with zero protocol lines**,
     and the parent takes the `scanning == None` arm, logs "worker made no
     progress; aborting CLAP scan" and returns. **`scan_all()` silently
     loses its entire CLAP half.** If the wanted instrument or inserts exist
     only as CLAP, `perf_budget_gate` finds `inst.instruments.is_empty()`
     and emits `PERF-VERDICT: SKIP`, which `perf-check.sh` maps to exit
     125 — so the plugin-load gate from PR #120, and the bisect recipe
     built on it, would call every commit "unjudgeable" instead of
     measuring it.

     Both call sites are gated (`AURA_PROFILE_PLUGINS` /
     `AURA_PROFILE_MAX_US`), so a plain run never reaches them and
     `perf-check.sh`'s default `--run bare` does not either; `--run full`
     does. **Note the hazard when it does:** the child inherits those same
     `AURA_PROFILE_*` variables, so it would reach `scan_all()` itself, with
     nothing bounding the depth. That path was NOT run on purpose. Fixing
     this needs a hidden worker entry in that binary plus a way to install
     it (a `set_worker_command` runtime override), which is its own change.
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
