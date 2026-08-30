# Standing constraints — these bind every task, on every track

Read this before your first commit on a branch. Nothing here is
negotiable by a single task; changing one of these is its own PR with its
own argument.

Historical plans called this `next-prompt.md` §2.

## Branch and worktree rules

- **Every job starts in its own worktree, cut from `origin/main`:**

  ```sh
  git fetch origin
  git worktree add .worktrees/<short-name> -b <branch-name> origin/main
  ```

  Both halves matter, and each one has already cost this project a
  session. [`CLAUDE.md`](../CLAUDE.md) carries the why. A worktree whose
  branch has merged is **spent** — do not reopen it and do not branch
  from it.
- **Claim the job in [`next-prompt.md`](../next-prompt.md) before you
  write code**, and push that claim immediately. Two agents finishing the
  same task is the most expensive failure this project has had; see the
  protocol at the top of that file.
- **Continuing branches merge `origin/main` in** whenever it has
  advanced, at a task boundary — don't let a long-running branch drift.
- **Never use bare `git stash`.** If you need to shelve work, commit it
  or use a named stash; bare `stash` has bitten prior sessions when a
  worktree switch lost track of it.
- **Foreground test runs only, `timeout`-guarded.** No backgrounding a
  test run and moving on — every gate in this project has been foreground
  since Plan A.

  ```sh
  timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
  timeout 300 npx vitest run
  ```

## Architecture

- **Thin renderer** (ADR 0006) still holds: no new authoritative state,
  business logic, or time math lands frontend-side. Every frontend change
  is op emission, gesture emission, or UI/chrome.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive (the same rule that shaped every Plan E task).
- **`src-tauri/src/theory/` is PURE and stays pure** (Plan H1, ruling
  H-5): no `tauri`, `parking_lot`, `std::fs`, `crate::control`,
  `crate::audio`, and no `thread_rng` — every generator takes an explicit
  `seed`. The only sanctioned crate dependency is `crate::midi::MidiNote`.
  A generator that needs project data is passed it. Same class of rule as
  the RT contract: it is what keeps that suite fast and that library
  reusable.
- **The harmony document is a map pair, not a track** (H-3/H-5): one op
  (`Op::HarmonySet`), no `rebuild` effect, `OP_FORMAT_VERSION` still 2,
  `schemaVersion` unmoved, and the `project.json` key written only when
  the document is non-empty. Do not add a `TrackKind` or a second harmony
  op.

## The op log and undo

- **The op log is ON.** `journal.ndjson` is a **persisted format** from
  now on: `OP_FORMAT_VERSION` (**2** since the post-merge follow-up PR —
  base64 state blobs, I-5) is load-bearing the moment any project has a
  journal file. Additive `#[serde(default)]` fields on an op or on
  `TxMeta` stay non-breaking; anything else (renaming a field, changing a
  variant's shape, removing a path) needs a version bump AND a reader
  that understands both shapes. Plan F's journal reader
  (`control/replay.rs`) requires `v == 2` on batch lines (ruling F-5); v1
  lines are skipped with a warn. A change of this kind now costs a
  migration.
- **`transact` closures must not panic.** Plan F landed panic
  *containment* (`catch_unwind` + restore from the pre-tx snapshot;
  ruling F-3) — that is a crash-consistency net, not a license. Validate
  before mutating, every time, exactly as every landed op arm does.
- **The M-3 redo invariant**: a transient write must never touch a
  document field an entry's `ops` can address, or a pending redo silently
  lands on a different state than the entry recorded. If you add a new
  transient transaction (transport-like, engine-thread, or gesture
  mid-flight), this is the check to run before shipping it. As of the
  post-merge follow-up PR a `debug_assert!` in the commit path enforces
  it in debug builds (transient ops may address only
  `ObjectRef::Transport`, unless the batch is a mid-gesture fold) — so a
  violation now fails the test suite instead of waiting to be noticed.
- **Undo is bounded**: 200 entries, in-memory, bottom-eviction, cleared
  at epochs (project open/create/save-as). The journal is unbounded and
  append-only; Plan F added a reader (detection + primitive, no
  auto-apply — ruling F-8).
- **Gesture lock order is gesture-before-session, everywhere** (Task 14's
  fix) — if a track adds a new gesture-shaped commit path, follow this
  order or reintroduce the TOCTOU that fix closed.

## Performance

- **If you touch the render path, run the gate before you open the PR.**
  That means `src-tauri/src/audio/mixer.rs`, `rt.rs`, `insert.rs`,
  `bus.rs`, `pdc.rs`, `offline.rs`, `midi/playback.rs`, or anything they
  call per block. Also: adding work to a per-sample or per-block loop,
  changing what the graph compiler emits, or adding a plugin-host call.

  ```sh
  scripts/perf-check.sh --measure                 # on origin/main first
  scripts/perf-check.sh --budget <that x 1.3>     # then on your branch
  ```

  The point is the comparison, not the absolute number. Measure `main`
  and your branch **in the same sitting on the same machine**, and quote
  both in the PR. A number on its own is not evidence: on the machine
  this was built on, ten invocations of unchanged code read 388–408 µs —
  and one batch of four read 260–275, for a reason never identified.
  Whatever that was, it is bigger than most regressions worth finding,
  and only a same-sitting pair rules it out.

  This is a rule you follow, not a CI job. A perf gate on a shared
  runner flakes, and a flaky gate gets deleted — so the check lives
  where a human or an agent can see the two numbers side by side and
  judge. `docs/GAP_ANALYSIS.md` §9 is what "normal" looks like and §9.2
  says which column is trustworthy.

- **`bare` is the default target because it needs no plugins.** It is
  AURA's own mixer, fader, sends, bus pass and built-in synth — the code
  we actually write — and it runs on any machine, including one with an
  empty plugin catalogue.

  **What it does not cover:** with no inserts there is no insert chain
  and no PDC, so a regression in `insert.rs` or `pdc.rs` is invisible to
  it. Those need `--run full` or `--run cheap_fx`, which need plugins
  installed. A plugin-free stand-in is possible in principle
  (`insert::GainHalfEffect`, `LatencyDummy`) but would have to bypass
  `compile_inserts`/`compile_routing` and rebuild the compiler's output
  by hand — a test that drifts from what production does is worse than
  a documented gap.

- **A regression that already landed is a bisect**, not an
  investigation:

  ```sh
  cp scripts/perf-check.sh /tmp/perf-check.sh
  git bisect start <bad> <good>
  git bisect run /tmp/perf-check.sh --budget <n> --harness-from main
  ```

  `--harness-from` is what makes this work across commits that predate
  the harness; without it every one of them fails to build and is
  marked BAD. Confirm the commit bisect names by hand before believing
  it — see the noise section in the script's `--help`.

- **Do not add timing assertions to the RT path** to get a number. It
  breaks the RT contract, and it has to come out again afterwards,
  which means nobody can ever reproduce the measurement. The harness
  gets its numbers by subtraction from outside instead; §9 explains
  how.

- **No continuously-animated SVG.** A WebKit Timelines capture on the
  native (Tauri/WebKitGTK) build showed a `requestAnimationFrame` loop
  mutating SVG attributes (`Gauge.svelte`'s needle/arc rotation, up to 44
  segment elements) getting folded into a full-viewport repaint —
  `quad: [0,0,3840,0,3840,1384,...]`, ~600ms per paint, on every meter
  tick. Browser demo mode (Chromium/Gecko) hid this; only profiling the
  real webview surfaced it. `Gauge.svelte` is now the second reference
  shape alongside `Meter.svelte` — both fully rewritten to canvas, no SVG
  or per-element DOM mutation left in either. SVG stays fine for static
  vector art (icons, one-shot illustrations). Anything redrawn every
  frame — meters, VU needles, playhead markers, gauges — is one of:
  - **canvas 2D**, imperative draw calls inside the rAF tick (`Meter.svelte`
    and `Gauge.svelte` are the reference shape: read the state, clear,
    redraw, no DOM/CSSOM involved at all).
  - **a plain HTML element moved via `style.transform`**, with
    `will-change: transform` on it (`Timeline.svelte`'s `.marker`).

  Either way the element needs `contain: paint` on the canvas itself (or
  `contain: content` on a wrapper with non-canvas children, like
  `Gauge.svelte`'s `.face-canvas`/`.ladder-canvas` do) so its own redraw
  can't escape into the surrounding page even once the technique is right
  — containment and technique are two separate fixes, verify both. Watch
  containment doesn't clip something that's meant to bleed outside the
  box (a glow, a baseline shared with a sibling) — `TransportBar.svelte`'s
  clock lost its baseline alignment with `.bars` and had its playing-state
  glow clipped this way; contained elements with a shadow/glow or a
  baseline dependency need the containment moved to a padded ancestor
  instead, not dropped onto the glowing element itself. If a WebKit
  Timelines re-capture ever shows a full-page quad again, this is the
  first section to reread.

- **Guard every per-frame `textContent`/attribute write against the
  unchanged case, always** — even one that "obviously" changes every
  frame doesn't, the moment the thing it's showing goes idle.
  `TransportBar.svelte`'s clock/bars and `MasterBar.svelte`'s dB readout
  wrote `textContent` unconditionally on every rAF tick; while transport
  is `"stopped"`, `positionAt()` returns a fixed value, so this wrote the
  identical string 60×/sec, forever, with the app just sitting there. A
  WebKit Timelines capture with the app fully idle (no playback, no
  interaction) showed a full-viewport `invalidate-styles` →
  `recalculate-styles` → `layout` → `paint` cycle repeating anyway, at the
  same ~160ms cadence found under real interaction — the unconditional
  write was the entire cause. Fixed with the same `if (text !== shown)`
  guard `Meter.svelte`/`Gauge.svelte` already used; re-capturing idle
  afterward showed zero non-paint layout events. `Pad.svelte`'s `.led`
  brightness tick has the analogous unconditional `style.opacity =` write
  and was NOT given this treatment — it's gated by playback/lit state
  already and wasn't implicated by profiling, but if a future capture
  points at it, this is the fix.

- **A full-viewport paint that scales with window AREA, not with what
  changed, is not a bug the frontend can fix.** After the SVG rewrite,
  the containment pass, and the `textContent` fix above, idle capture
  showed zero spurious invalidation — but resizing the Surface panel (no
  playback) and resizing-while-playing both still produced a `paint`
  record covering the exact full window every time, with duration
  tracking window pixel area almost exactly (2× the area → 1.94× the
  duration, across two captures at different window sizes). That's
  WebKitGTK's compositor doing a full-surface raster on any invalidation
  rather than an incremental/tile-based repaint of just the damaged
  region — a platform/compositor characteristic, not something reachable
  by more `contain`, `will-change`, canvas rewrites, or (seriously
  considered and rejected — see the reasoning if this comes up again)
  WebAssembly/SIMD: script execution was already under 5% of total time
  across every capture in this investigation, so nothing on the
  computation side was ever the bottleneck; the cost sits entirely in
  rasterization, which happens identically no matter what language issued
  the draw calls. Reaching further into this needs WebKitGTK's own
  compositing/GPU-acceleration configuration, not application code — a
  different, environment-level investigation.

- **A `Forced Layout` entry with a stack frame from a `ResizeObserver`
  callback that reads geometry (`offsetWidth`/`clientWidth`/
  `getBoundingClientRect`) is usually two independent triggers landing
  close together, not a bug on its own.** Found in
  `TransportBar.svelte`'s `recompute()` (measures each toolbar item's
  `offsetWidth` to decide overflow) during resize-while-playing: the
  clock tick's legitimate per-frame `textContent` write left layout
  dirty, and a resize-triggered `ResizeObserver` firing moments later
  forced a synchronous flush on its `offsetWidth` read. Sub-millisecond
  each, 17 times over 14.6s — not worth chasing given the paint cost
  above dwarfs it by orders of magnitude, but the fix if it ever is
  worth doing is deferring the read to the next `requestAnimationFrame`
  rather than reading synchronously inside the observer callback.

## Tests and docs

- **The dated-count convention**: any task that changes test counts
  updates `README.md` + `CONTRIBUTING.md` in the same commit, with the
  date. Current counts live there — measure, do not copy a number from a
  briefing.
- **Pure logic goes in `src/lib/utils/` with a `*.test.ts`**; component
  behaviour goes in `*.dom.test.ts`.
- **Theme tokens only** in every `<style>` block — see
  [`TRAPS.md`](TRAPS.md).
