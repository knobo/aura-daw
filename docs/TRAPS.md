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
- **Parallel `cargo test` intermittently SIGSEGVs** (Cardinal CLAP
  teardown). Use `-- --test-threads=1`. Never run `cargo test` and `tauri
  dev` against the same `src-tauri/target/`.
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
