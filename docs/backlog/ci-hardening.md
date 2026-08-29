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

   **What that fix made reachable, and what is still open — none of it
   the same bug.** The first two and the ext-gui paragraph at the end were
   closed by PR #124 and are kept here because the reasoning is worth more
   than the outcome; the middle three are open.

   - **The `X Error … BadWindow` abort — FIXED (2026-08-28, PR #124).**
     It was newly REACHABLE, not newly caused: `main` segfaulted before it
     got there, and the A/B above is 0 of 4 on `main` against 2 of 4 after
     the SIGSEGV fix. `wm_stack` discovers window ids with `xdotool` and
     restacks them in a second round trip, so the id can be dead by the
     time the request lands, and **Xlib's default error handler calls
     `exit(1)`** — which is a product bug before it is a test bug: a DAW
     must not exit, losing the session, because a plugin editor closed a
     millisecond early.

     `x11ewmh::Display` now installs its own handler for exactly as long
     as it lives, resolved through the module's existing runtime `dlopen`
     so the frozen `Cargo.toml` is untouched. Two things made it delicate,
     both measured with a standalone C probe rather than reasoned about:

     - **X errors are asynchronous.** Restoring the previous handler and
       syncing afterwards protects nothing — the deferred error goes to
       whoever is installed *then*, i.e. the default handler again, and the
       probe still dies at rc=1. `Drop` syncs (and closes, which syncs
       again) before it restores.
     - **The handler is process-global and GTK installs its own.** Ours
       answers only for our own display connection and hands every other
       error back to the previous handler, so GDK's behaviour is unchanged;
       a static mutex stops two `Display`s nesting and restoring out of
       order.

     Result: 0 `X Error` aborts in 4 default-parallelism runs, all four
     reaching `test result:`. The sharper A/B is the new regression test
     alone — on `main` it prints `X Error of failed request: BadWindow` and
     the test binary exits 1 mid-test; on the fix it passes.

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
   - **The integration-test half of the same hole — FIXED (2026-08-28,
     PR #124).** It failed the OPPOSITE way and was worse for it. An
     integration binary links the lib WITHOUT `cfg(test)`, so
     `tests/plugin_load_profile.rs` took `current_exe()`'s production
     branch — a bare re-exec, correct only for `src/main.rs` and its
     guard. Measured: the child ran that file's own seven tests and
     returned in **2.9 ms with zero protocol lines**, the parent took the
     `scanning == None` arm, and `scan_all()` dropped its whole CLAP half
     without a line of log saying so.

     `scan_worker::set_worker_command` is the runtime override for such a
     binary; `plugin_load_profile.rs` points it at a hidden `worker_entry`
     the way `test_worker_command` does. The `--exact` filter also
     contains the second hazard, which is that the child inherits
     `AURA_PROFILE_*` and would otherwise reach `scan_all()` itself with
     nothing bounding the depth.

     The consequence was reproduced, not just predicted. With 35 CLAP
     bundles installed, `scanned N plugins` went **326 → 363**. It needs
     the wanted plugin to be CLAP-only, which the defaults are not on the
     measuring machine, so the gate run below names one that is (`Kars`,
     a DPF CLAP instrument):

     ```sh
     AURA_PROFILE_MAX_US=100000 AURA_PROFILE_RUN=full \
     AURA_PROFILE_TRACKS=4 AURA_PROFILE_INSTRUMENT=Kars
     ```

     | | `main` | fixed |
     |---|---|---|
     | verdict | `PERF-VERDICT: SKIP … not installed` | `PERF-VERDICT: OK 138.5 us` |
     | `perf-check.sh` exit | 125 — "cannot judge" | 0 |

     So PR #120's plugin-load gate, and every bisect built on it, would
     have called each commit unjudgeable instead of measuring it.

   - **Orphaned PipeWire clients survive a signal death.** A process killed
     by SIGSEGV never runs `Drop`, so its streams are left standing; the
     daemon's idle fd baseline crept 105 → 164 over a session of
     reproducing this. Same mechanism as the leftover `zynaddsubfx-ext-gui`
     windows below. Recovery is
     `systemctl --user restart wireplumber pipewire-pulse pipewire`.

   **The leftover ext-gui windows — FIXED (2026-08-28, PR #124), and the
   reason it sat here was wrong.** `kill_ext` and `Drop` cover the
   ordinary closes; neither runs under SIGKILL or a segfault, so a crash
   left a `zynaddsubfx-ext-gui` window the user could not close, owned by
   a DSP that no longer existed — and `wm_stack` kept finding it with
   `pgrep` and restacking it. `PR_SET_PDEATHSIG` via `CommandExt::pre_exec`
   on the spawn in `plugins/lv2_ui.rs` is the fix, as recorded.

   What was wrong is the blocker: this entry said it needed `libc` as a
   direct dependency of the FROZEN `Cargo.toml`, so ask the owner. **It
   does not.** `prctl` and `getppid` live in the glibc every Rust binary
   on this target already links, so a bare `extern "C"` block reaches
   them — which is exactly what that module already does for
   `dlopen`/`dlsym` a few lines up. One `rustc` invocation would have said
   so at any point. See `TRAPS.md` §Backend.

   Three kernel-side facts were measured on 6.8 rather than taken from the
   man page, since each would have made the fix silently useless: PDEATHSIG
   survives the following `execve` (cleared only for set-uid binaries);
   a SIGKILLed parent does take the child with it; and the spawning THREAD
   exiting did *not* take it here, which the man page's BUGS section warns
   it might — we spawn from the long-lived plugin main thread either way.
   A `getppid` check closes the fork/prctl race.

   The test kills a real process and reads a real `/proc` entry, and was
   confirmed to have teeth by commenting out the single `die_with_parent`
   call.

6. **The `midi_out::tests::*` race — DIAGNOSED AND FIXED (2026-08-28).**
   Item 4 called it "several `midi_out` tests spawn real background threads
   and race". The threads were a red herring. **It was a product bug**, and
   it reproduces with no test parallelism at all.

   A port id is minted as `"<name>#<index>"` by `list_output_ports` /
   `midi_input::list_ports`, and `open_port` / `select_port` resolved it by
   exact string match. That index is a position in `midir`'s enumeration,
   which is ordered by ALSA client number — so a NEW port lands at the end
   and disturbs nothing, but a port CLOSING renumbers every port after it.

   Measured directly with a two-port probe: a virtual port sitting at
   `"probe-survivor:probe-survivor-port 129:0#3"` becomes `"…#2"` when an
   unrelated, *earlier* port goes away, and opening the id captured a moment
   before fails with `MIDI output port not found` — for a port that is
   plainly still present, same client, same name, same ALSA address. Only
   its place in a list moved.

   That is why the failing set kept changing name from run to run: the tests
   create and tear down virtual ports constantly, so whichever test happened
   to be holding an id when another's port closed was the one that failed.

   **Scope in the product.** Persistence was never exposed to it —
   `midi_out::persist` keys by port *name* and strips the ALSA address on
   purpose, and its doc comment already called the id "volatile". The window
   is inside one session: list the ports, hold an id, open it after
   something else on the box has closed a port. Unplugging a keyboard or
   quitting another synth between opening the port menu and clicking is
   enough.

   **The fix.** `resolve_port_id` treats the index as a tiebreaker rather
   than an identity: exact match first (nothing moved — the common case, and
   the only path that can distinguish same-named ports), then the *unique*
   port carrying that name. Same-named ports are real on backends whose
   names carry no device address, which is the only reason the index exists;
   there it refuses rather than connecting the wrong device.

   **What was left over, and it was a different bug each time.** Fixing the
   ids exposed three timing assumptions that the port failures had been
   masking:

   - Three loopback tests spawned the thread that drives `position` BEFORE
     `open_port`, so position free-ran through the enumeration and
     connection. A note at tick 0 is then missed outright — the failure
     trace shows note-OFFs with no note-ONs. `ThreadShared::note_snapshots`
     (additive, on `PortStatus`, unread by the UI) counts successful 250 ms
     snapshot windows so a test can wait for the output thread to have notes
     before letting position advance.
   - The same tests slept a fixed 400 ms before asserting; they wait for the
     bytes now.
   - `clap_host::post_params_returns_immediately_and_the_change_still_lands`
     slept 300 ms between the fire-and-forget write and the `render_blocks`
     that makes the plugin adopt it. `plugin_main()` is a process-wide FIFO
     queue, so an LV2 test alongside it blows the budget: 3/3 failures next
     to `plugins::lv2_host::`, 3/3 passes next to `plugins::clap_host::`
     alone. It uses `plugin_main().run(|_| ())` as a barrier now.

   **Result.** Default parallelism, one machine, one sitting, `main` against
   this branch:

   | | `main` | this branch |
   |---|---|---|
   | `midi_out::` filtered, fully green | **0 of 10** | **25 of 25** |
   | full `--lib` suite, fully green | **0 of 8** | **45 of 46** |
   | distinct `midi_out` tests seen failing | 10 | 0 |
   | `--test-threads=1` | — | 1420 passed, 65 s |

7. **The PDEATHSIG survivor — ON HOLD (owner's call, 2026-08-29).** This is
   what keeps `--test-threads=1` in CI, and it is deferred deliberately
   rather than left open by accident.

   `plugins::lv2_ui::tests::the_guarded_child_does_not_outlive_a_parent_that_was_sigkilled`
   (PR #124's regression test) was the single failure in the 46
   default-parallelism runs above. Raising its poll deadline from 5 s to
   30 s did **not** settle it: the child was still alive after 30 s, which
   is far too long to be scheduler starvation. It did not recur in 30 runs
   after that.

   **Which of two bugs it is, is not known, and they differ completely:**

   - *Test artefact.* The check reads `/proc/<pid>`, so a recycled pid makes
     a long-dead child look alive. The product would be fine. **Untested
     hypothesis** — the test now prints the surviving pid's `comm` and
     `ppid` on failure precisely to confirm or kill it.
   - *Real product bug.* `PR_SET_PDEATHSIG` genuinely does not take
     sometimes, and the consequence is the one PR #124 set out to fix: a
     `zynaddsubfx-ext-gui` window the user cannot close, owned by a DSP that
     no longer exists, which `wm_stack` keeps finding and restacking.

   **Why holding is reasonable.** CI pins `--test-threads=1`, so nothing is
   flaky there today. The product symptom only appears after AURA dies by
   SIGKILL or segfault — rare now that PRs #123/#124 closed the crash
   classes underneath it — and at ~1 in 46 even then.

   **Take it off hold when any of these happens:**

   - An ext-gui window survives a crashed AURA in real use. That is the
     product bug, observed, and it stops being theoretical.
   - Someone wants `--test-threads=1` gone (a ~5× faster suite: 65 s → 12 s).
   - The diagnostic line above is ever captured. Do not re-derive the
     hypothesis — read the `comm`/`ppid` it prints.

   Cheapest way to catch it deliberately: loop the full `--lib` binary at
   default parallelism and keep the output of any run that is not
   `test result: ok`. It took ~46 runs to see once.

## Why deferred

Getting a first green CI pipeline landed mattered more than full coverage.
Installing plugin packages and building `.venv-sidecars` in CI both add
non-trivial runtime/complexity to the workflow, so they were left out of v1.

## Next steps (when picked up)

- **Split the `rust` job out of `tests.yml` into a path-gated `rust.yml`.**
  Its inputs are exactly `src-tauri/**` — `Cargo.toml` and `Cargo.lock` live
  in there, and `cargo test` cannot see frontend code at all — yet it runs on
  every PR. A frontend-only PR spends ~10 minutes of runner time (apt for
  webkit/lilv/ffmpeg, then `cargo test --test-threads=1`) re-proving
  something that could not have changed. Measured on PR #128, an 11-file
  diff entirely under `src/` and `docs/`.

  It has to be its own FILE, not a `paths:` on the job: GitHub applies
  `paths:` per workflow. `engine.yml` already does exactly this and carries
  the reasoning, the shape to copy, and the `push: branches: [main]` mirror
  the job needs or it loses its `rust-cache` base.

  Leave the frontend job ungated — it costs a minute, and gating both would
  leave a docs-only PR with no checks at all. And keep `engine.yml`'s
  warning in view: `main` is not branch-protected today, but if it ever is,
  a path-gated job must NOT be listed as required — a skipped job blocks the
  PR forever.

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
- The PDEATHSIG survivor is **on hold**, not unclaimed work — item 7 has the
  hypotheses and the conditions for picking it back up. It is the last thing
  between CI and dropping `--test-threads=1`; the `midi_out` and `clap_host`
  families are done.
