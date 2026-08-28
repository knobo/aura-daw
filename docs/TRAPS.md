# Traps — things that have already cost this project time

A trap earns a line here only if someone lost time to it. This is not a
summary of how the code works; it is the list of ways the code has
surprised people. Read it before your first commit, and add to it when a
session burns an hour on something a sentence would have prevented.

## Frontend

- **`no-literals` lives at `src/lib/theme/no-literals.test.ts`**, not
  under `components/`. Raw colour literals fail CI; use theme tokens, or
  a `theme-exempt:` comment.
- **Global focus ring and `prefers-reduced-motion` are already in
  `src/app.css`.** Do not re-implement them per component.
- **`display: contents` is not safe here** (WebKitGTK). Use a real flex
  container.
- **`position: fixed` inside `.glass` is NOT relative to the viewport.**
  `.glass` sets `backdrop-filter`, which makes the element a containing
  block for fixed descendants, so a popover computed in viewport
  coordinates lands offset by the panel's own origin (`top: 8px` came out
  ~560 px down inside the bottom-docked surface panel). Portal the popover
  to `document.body` (`utils/portal.ts`) and place it with
  `utils/popover.ts`; a scrolling ancestor such as the surface deck would
  clip it in place anyway. Playwright's "element is outside of the
  viewport" on a menu item is the symptom.
- **A Svelte 5 `$effect` tracks reactive reads made anywhere inside its
  callback, including deep inside a function it merely calls.** Splitting
  a `$derived` so the effect only reads a narrower value is not enough by
  itself if the effect's body then calls something (e.g. a store method)
  that synchronously reads other reactive state internally — that read
  still gets attributed to the effect, so it re-fires on unrelated churn.
  Wrap the actual side-effecting call in `svelte`'s `untrack(...)`, after
  reading the narrowed dependency outside it. (`AutomationMatrix.svelte`
  and `LanePluginStrip.svelte` both hit this with `plugins.ensureParams`
  reading `paramCache`; both now follow the same
  read-then-`untrack`-the-call shape.)

## Backend

- **A flood of `Sending key 'state' to UI failed, out of space` is NOT
  harmless log noise.** It is ZynAddSubFX writing from inside `run()` on
  the RT audio thread, and `plugins/stderr_guard.rs` only guards fd 2 while
  that traffic arrives on fd 1 — so on a real terminal it is a blocking
  write on the audio thread (an ALSA underrun follows). Check
  `/proc/<pid>/fd/1` and `fd/2` before assuming the guard covers it. Open
  defect, written up in [`backlog/plugin-manager.md`](backlog/plugin-manager.md).

- **`Session::rev` is not "the current revision" for a history guard.**
  Transient commits — `transport play`, `transport stop`, `transport set
  loop`, every mid-gesture fold — go through `transact` and bump `rev`
  without reaching the undo stack. A guard built on it aborts because the
  user pressed play. The undo ancestry's own head (`HistoryLog::undo_path()`
  → `UndoPath::head()`) is the value that means "the history has not moved";
  it is what `history_undo_to` guards on.

## Tests

- **Before any new `*.dom.test.ts`**, read
  [`2026-08-18-dom-test-environment.md`](superpowers/plans/2026-08-18-dom-test-environment.md).
  jsdom has no Pointer Capture, `getBoundingClientRect()` is zeros,
  `scrollIntoView` is not implemented, and Svelte 5 `$state` proxies
  throw on `structuredClone`.
- **`CSS` is not a global under the `unit` (node) vitest project.**
  `CSS.escape(...)` throws a `ReferenceError` before a stubbed
  `querySelector` is ever called; if that throw lands inside a
  `try/catch` the test can pass for the wrong reason (the "missing
  element" branch never actually runs). Stub `CSS.escape` explicitly
  alongside `document` in any test that exercises a `[data-*]` selector
  built with it.
- **`formatParamValue` special-cases any param name containing "cutoff"
  or "freq"** as a frequency (rounds to an integer + `"hz"` unit). A test
  fixture named e.g. `"Filter / Cutoff"` silently gets frequency
  formatting instead of the plain `toFixed(2)` you were expecting — pick
  a fixture name without those substrings unless you're testing the
  frequency path on purpose.
- **A libtest binary must never be re-executed as a subprocess worker.**
  `plugins::scan_worker::WorkerCommand::current_exe()` re-runs the current
  binary and depends on the `AURA_SCAN_WORKER` guard in `src/main.rs` to
  route the child into `worker_main`. A libtest binary has no such guard,
  so the child runs **the whole suite again**, and the parent BLOCKS until
  that suite ends: every line the child prints resets `LINE_TIMEOUT`, and a
  running suite prints constantly, so the timeout never fires. Meanwhile the
  recursive suite opens every audio device its engine tests ask for.

  **What made it invisible for weeks** is worth knowing before you build any
  subprocess worker: the worker env is INHERITED, so the recursive suite's
  own hidden worker case ran the worker body and returned correct
  descriptors. The parent got the right answer — after running the suite
  twice to get it — so no test failed and no log complained. The only thing
  that separates a scan from a suite is how many tests the child ran; that
  is what the regression test asserts.

  Fixed under `cfg(test)` (2026-08-28). **The same trap is live for
  integration test binaries**, which link the lib without `cfg(test)`, and
  there it fails the opposite way: `tests/plugin_load_profile.rs` has no
  hidden worker case, so its child runs its own seven gated tests, returns
  in 3.5 ms with no NDJSON at all, and the parent aborts the CLAP scan
  ("worker made no progress") — `scan_all()` silently loses every CLAP
  plugin. If you add a subprocess worker of any kind, ask what happens when
  the parent is libtest.
- **A SIGSEGV inside `libpipewire` is not your memory bug — count the
  daemon's file descriptors first.** `pipewire` and `wireplumber` run under
  systemd's `LimitNOFILESoft=1024`. A test process holding ~25 concurrent
  engine streams gets close on its own; anything that multiplies that
  reaches it, and at `EMFILE` the daemon cannot finish a client handshake,
  wireplumber dies with SIGSEGV, and YOUR process segfaults inside
  `libpipewire-module-protocol-native.so` on the broken connection. The
  faulting thread is called `alsa-pipewire` and no AURA frame appears in
  its stack. Two commands settle it in a minute:

  ```sh
  grep 'open files' /proc/$(pgrep -x pipewire)/limits
  while :; do ls /proc/$(pgrep -x pipewire)/fd | wc -l; sleep 0.2; done
  ```

  A clean parallel `--lib` run peaks around 578 against that 1024. Reaching
  1024 means something is multiplying the streams, not that the audio path
  is corrupt. `journalctl --user -u pipewire -u wireplumber` says
  `Too many open files` outright. Recovery when the server is left wedged
  (`pactl info` hangs): `systemctl --user restart wireplumber
  pipewire-pulse pipewire`, then re-check `pactl get-default-sink` — it can
  flip to Bluetooth.
- **`PULSE_SINK` does not redirect this app's audio, whatever CONTRIBUTING
  used to imply.** ALSA's `default` PCM resolves to the **pipewire** plugin
  here (`libasound_module_pcm_pipewire.so` — you can see it in a backtrace,
  and clients show up as `PipeWire ALSA [<binary>]`), and the pulse client
  variable is never consulted on that path. To pin a sink for a test run,
  override the PCM instead:

  ```sh
  # asound-pin.conf
  </usr/share/alsa/alsa.conf>
  pcm.!default { type pipewire playback_node "alsa_output.pci-…analog-stereo" }
  ```

  `ALSA_CONFIG_PATH=asound-pin.conf cargo test …`. Verified 2026-08-28.
- **Never run `cargo test` and `tauri dev` against the same
  `src-tauri/target/`.**
- **Do not run `cargo fmt`.** This tree is not rustfmt-default-formatted:
  `cargo fmt --check` wants to rewrite ~40 files it has no business in,
  and nothing gates on it: `.github/workflows/tests.yml` runs no fmt check
  anywhere, and no clippy on `src-tauri` (its `engine` job does run clippy,
  but only on the standalone `aura-engine` crate). Match the style of the
  code around you instead — a formatting run would bury your diff.
- **A controlled `<input>`/`<select>` fed a value that repeats does NOT get
  pushed back after the user moves it.** Svelte's `set_value` early-returns
  when the bound expression equals the last value it set (an `||`, so the
  element's own drifted value does not save you). Feed a control from a 60 Hz
  read-back that plateaus — an automation hold, a flat lane — and the DOM
  keeps the user's drag while every other part of the row paints the real
  value. Re-assert `el.value` in the handler. And a `<select>` given a value
  that is not one of its options renders BLANK (`selectedIndex = -1`), so
  snap to the nearest option before binding.
- **A param fader's accessible name is the FULL "Group / Name", not the
  short label the chip shows.** The chip is `"Level, 0.50"`; the slider
  beside it is `"Filter / Level (0.50)"`. A `getByRole("slider", {name:
  /^Level/})` finds nothing, and the failure reads like the value being
  wrong rather than the query.

## Tooling and gates

- **Several clippy lints are deny-by-default and CI did not catch them**
  until the `engine` job existed. `approx_constant` is the one that bites:
  writing `0.7071` for a centre-pan gain is an *error*, not a warning, and it
  is right to be — the mixer's own tests use
  `std::f32::consts::FRAC_1_SQRT_2`. Note the `rust` job does **not** run
  clippy on `src-tauri`, only the `engine` job does on `aura-engine`.
- **A concurrency test that stops on the WRITER's count can be vacuous on a
  small runner.** `triple_buffer`'s torn-read test published 50 000 values
  then asserted the reader had read at least once. In release that takes well
  under a millisecond, and on a 2-core GitHub runner with every other lib test
  competing, the reader thread was never scheduled — so `reads == 0` failed
  the assertion whose whole job was proving the test wasn't vacuous. Green on
  a 32-core box, red on CI, and NOT reproducible locally even pinned to two
  cores. Gate the stop condition on what the OTHER thread has observed, and
  `yield_now()` so a single-core machine still progresses.
- **A benchmark fixture smaller than production measures nothing.**
  `TrackRamps::gain` is compiled **session-wide** at graph rebuild, so a real
  automation lane is thousands of breakpoints. `benches/kernel.rs` used 64 and
  reported `strip::plan` 1.9x faster than the mixer's loop while it was in
  fact **40x slower** on a 48 000-point lane, because the plan scanned the
  lane linearly per segment per block. Anything whose cost depends on
  session-wide state needs a session-sized fixture, or the benchmark endorses
  the bug. `aura-engine`'s `long-lane` case exists for exactly this.
- **A stale `target/criterion/` silently mixes runs.** Criterion keys results
  by benchmark *name*, so renaming a benchmark leaves the old directory in
  place and `estimates.json` for the old name still parses fine. A report
  built by globbing that tree can quote a number measured against code that
  no longer exists — which happened here, with `apply_fader` and
  `apply_fader_into` sitting side by side. `rm -rf target/criterion` before a
  run whose numbers you intend to publish.

## Audio engine

- **`ParamTable::default()` has 64 mixer slots and ZERO send lanes.** An
  unknown send index reads back as UNITY (`send_amount_linear`'s
  fallback), so a mixer test that builds its graph with the default
  table will see every send at full amount and pass whatever amount it
  meant to assert. Size the table with
  `ParamTable::with_slots_and_sends(n, sends)` in any test that touches
  a send.
- **A bus is panned by the BALANCE law, not the constant-power one**
  (`mixer::balance_gains` vs `pan_gains`). A return's input is an
  already-panned stereo sum; running it through constant-power again
  would take a second 3 dB off at centre, so a unity send would come
  back quieter than the dry signal it copied. If you add another
  sum-carrying strip, it wants `balance_gains` too.
- **A PRE-fader send is pre-PAN as well as pre-gain.** It leaves the
  strip before the fader does anything at all, so its copy is the
  unpanned source — which is louder at centre than the dry path is.
  That is the standard behaviour, not a bug, and there is a test
  pinning it (`a_pre_fader_tap_is_pre_pan_too`).

- **A send and an output are different wires, and the difference is
  audible.** A send COPIES (the source still reaches the master); an
  output MOVES (it does not). The first ear-check of G2 reported "two
  streams where there should be one" and it was neither a bug nor a
  volume problem — it was a send being asked to do an output's job. If
  someone wants "only through the bus", the answer is
  `TrackState.output`, not a send with the fader pulled down.

- **A plugin can be silent at its own defaults, and it looks exactly like
  a broken insert path.** `ZamEQ2` renders digital silence with no
  parameters touched. The first run of `tests/plugin_load_profile.rs`
  read that as "AURA's insert chain drops audio" — it is not:
  `ZamComp`, `ZamCompX2`, `Calf Compressor`, `Calf Reverb` and `Audio
  Gain (Stereo)` all pass audio through the same host, and
  `lv2_host.rs` does honour `lv2:default` (there is a test asserting the
  initial value IS the default). Before blaming the host, try a second
  plugin.
- **`compile_inserts` skips what it cannot host, and says so only in a
  log line.** A refused instance leaves the strip DRY
  (`plugins::insert_node_for` returns `None`) and a MIDI track whose
  plugin instrument fails falls back to `PolySynth`. Both produce
  plausible audio and plausible timings. If you are measuring anything,
  count the compiled chains — do not trust that asking for a plugin got
  you one.
- **A `PluginDoc` row rebuilt by hand will resolve to no node at all.**
  `insert_node_for` and `live_node_for` both branch on `info.format` to
  pick the CLAP or LV2 host; a row carrying the right instance id but an
  empty `format` matches neither, so every slot is skipped silently.
  Pass the host's own `PluginInstanceInfo` through, do not reconstruct
  it.

- **Run the local suite under `xvfb-run`, and single-threaded.**

  ```sh
  xvfb-run -a cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1
  ```

  Two separate problems, one command. Zyn's LV2 UI is DPF ExternalWindow:
  it draws nothing itself and instead spawns `zynaddsubfx-ext-gui` as its
  own process, so the GUI tests put real windows on your desktop.
  `xvfb-run` gives them a throwaway display that dies with the run.
  `--test-threads=1` avoids the parallel crash below — which is what
  leaves windows behind, because a process killed by a signal never runs
  `Drop`, and `Drop` is the only thing that kills the GUI child.

  **Do not "fix" this by unsetting `DISPLAY`.** The three GUI tests gate
  on it and return early, so they go green having asserted nothing —
  including `zyn_show_gui_starts_ext_gui_against_the_hosted_osc_port`,
  whose entire point is that the process appears. Verified both ways:
  under `xvfb-run` all five zyn tests run and pass; with no display they
  pass while printing "skipping: no display".

- **A parallel `cargo test --lib` can SIGSEGV**, not merely flake. Single
  threaded it passes 1407/1407. If a full run dies with `signal: 11`, you
  have not broken anything — see
  [`backlog/ci-hardening.md`](backlog/ci-hardening.md) item 5, which has
  the evidence and the two pieces of work it implies.

## Runtime noise that is not your bug

- **If the dev log is 99% `[carla] lv2ui_extension_data(...)`, it is not
  your branch's fault and it is fixed** — `OpenLv2Gui` re-queried the LV2
  UI's show/idle interfaces on every 30 ms tick, ~66 Carla log lines a
  second per open editor. Now resolved once at open. If the pattern comes
  back, look for a new per-tick `extension_data` caller before assuming
  Carla is just noisy.
- **DPF plugins may print `Parent Window Id missing`** — expected: the v1
  floating-GUI path has no XEmbed parent.
- **Zyn may log `Sending key 'state' to UI failed`** until the editor is
  actually open.
