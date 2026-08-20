# Next: plugin management (PR 5–7) on `feat/plugin-manager`

Reply to the user in Norwegian — they write Norwegian; the repo
documentation is English.

**You are mid-track, not starting one.** The branch `feat/plugin-manager`
(worktree `.worktrees/plugin-manager`, branched from `origin/main` at
`3178073`) has four commits landed locally and NOT yet pushed or opened as
PRs. Read these two before touching anything:

- Design: [`docs/superpowers/specs/2026-08-20-plugin-manager-design.md`](docs/superpowers/specs/2026-08-20-plugin-manager-design.md)
  — §3.1 defines the signature element, §8 carries the owner's round-two asks.
- Plan: [`docs/superpowers/plans/2026-08-20-plugin-manager.md`](docs/superpowers/plans/2026-08-20-plugin-manager.md)
  — PR 5 and PR 6 are specified there in implementable detail.

## The owner's standing steer (2026-08-20)

> "Ikke kjenn på begrensninger i UI design i forhold til hva som ligger der
> fra før. Hvis vi skal vinne må vi lage en løsning som er bedre enn hva
> andre har."

**Do not let the existing UI set the ceiling.** The bet is that AURA wins
on interface. Where this design and the current app disagree, the design
wins; where the current app is merely adequate, that is not a reason to
keep it. This overrides the usual "follow existing patterns" instinct for
UI work on this track — it does NOT override the frozen IPC surface, the
theme-token rule, or the test gates.

## Done on this branch (do not redo)

| Commit | What |
|---|---|
| `6b259b4` | **Follow the playhead on seek.** Both follow paths share `view.revealSamples`: reveal if off screen, do nothing if already visible. The stopped path is a `$effect` on `transport.snap.positionSamples` with the reveal **untracked** — `revealSamples` reads and writes `view.viewStart`, so a tracked call would yank the view back on every user scroll. |
| `b9d529c` | **Shared browser layer** `src/lib/components/browser/`: `browser-model.ts` (ranking / grouping / row flattening / keyboard index), `BrowserShell`, `BrowserSection`, `BrowserRow`, `ParamChip`, `EmptyState`, `FavoriteToggle`, `browser-folds.ts`. Migrated `InstrumentBrowser`, `SamplesRoot`, `PresetsRoot`; `LibraryPanel` is a pure delegator. |
| `f8755a9` | **Lane multi-select + bulk M/S/A** from three entry points. Value rule in `lane-bulk.ts`; whole batch in one gesture. The rail is an ARIA layout grid with roving tabindex. |
| `7e6bc0b` | **Plugin catalog** at `dirs::config_dir()/aura/plugin-catalog.json`: persistent scan cache seeded in `plugins::init()`, incremental `plugin_scan` (path+mtime+size), favourites / recents / tags / pinnedParams. Three additive commands: `plugin_catalog_get`, `plugin_catalog_update`, `plugin_scan_status`. |

Verified at that point: `cargo test -- --test-threads=1` 1319 + integration
0 failed; `vitest` 941 across 87 files; `svelte-check` 0 errors 0 warnings;
`npm run build` green.

## Do this next

1. **PR 5 — the Plugin Manager.** Plan §"PR 5". Browse mode and rack mode in
   one shell, plus the `Ctrl+P` quick picker. The rack is the thing no other
   DAW has: a project-wide ledger of every live instance, its placements, its
   automated params, and — critically — the orphans and crashed instances
   nothing else in the app will ever show you. Fold in **collapse-all /
   expand-all** (design §8.1) since it is shell work.
2. **PR 6 — automation as an inventory.** Plan §"PR 6": the matrix, pinned
   params in `PluginParamPanel`, and the lane-info plugin strip that turns
   `TrackHeader`'s FX chip into `ParamChip` jump targets.
3. **PR 7 — unified audition** (design §8.2). Double-click any row to hear
   it, gated behind a new `browserAudition` pref defaulting **off**
   (`clipOpenAutoplay` is the precedent). Do this last: it touches every
   browser plus the sampler and plugin hosts, and must not delay the manager.

**Do not:** wire the favourite star to anything other than the catalog
commands that already exist; migrate `ZynPatchBrowser` onto
`BrowserRow`/`BrowserSection` (it is virtualised for ~1318 rows — its own
header comment explains why, and those components mount unconditionally);
bump `OP_FORMAT_VERSION`; break a frozen command name.

## Traps on this branch

- **`no-literals` lives at `src/lib/theme/no-literals.test.ts`**, not under
  `components/`. It globs `../components/**/*.svelte`, so every new
  component is covered. Raw colour literals fail CI; use theme tokens.
- **Global focus ring and `prefers-reduced-motion` are already handled in
  `src/app.css`** (`:where(button, input, select, [tabindex]):focus-visible`
  and a global reduce block). Do not re-implement them per component.
- **`display: contents` is not safe here.** This app ships on WebKitGTK via
  Tauri, which has a history of dropping such elements from the
  accessibility tree. Use a real flex container.
- **Parallel `cargo test` intermittently SIGSEGVs.** Pre-existing: Cardinal's
  exit-time CLAP teardown corrupts the heap — see `scan_worker.rs:234` and
  `:274`, which predate this branch. Use `-- --test-threads=1` for a clean
  signal.
- **Before any new `*.dom.test.ts`**, read the Global Constraints in
  [`2026-08-18-dom-test-environment.md`](docs/superpowers/plans/2026-08-18-dom-test-environment.md).
  jsdom has no Pointer Capture API at all, `getBoundingClientRect()` returns
  an all-zero rect, and Svelte 5 `$state` proxies throw on `structuredClone`.
- **`src-tauri/node_modules/.vite/vitest/…/_svelte_metadata.json` is still
  tracked in git** — a leftover from PR #65 that PR #73's untracking missed.
  Worth its own tiny PR; do not fold it into this track.

## Older context

The pre-existing briefing (Track D automation leftovers, Plan F undo
carry-forwards, the MIDI-output handoff, the Composer deprioritisation)
still applies once this track lands. It is preserved below.

## Landed (do not restart)

| What | Pointer |
|---|---|
| **Pitch analysis action on clip view** — analyse clip button on selected audio clips, persisted APTF cache rebuild, teardown/generation guards, and PitchCoach report cache invalidation | PR #87 `25af6ae`. |
| **Write / Touch / Latch automation modes** — Off / Read / Write / Touch / Latch per track, real-time control-thread point recorder, single-op commit on stop/release with undo | PR #85 `d496903`. Design spec: [`2026-08-18-automation-write-touch-latch-design.md`](docs/superpowers/specs/2026-08-18-automation-write-touch-latch-design.md). Plan: [`2026-08-18-automation-write-touch-latch.md`](docs/superpowers/plans/2026-08-18-automation-write-touch-latch.md) |
| **MIDI output — per-track and per-clip patchbay routing** | PR #84 `cbbc240`. Handoff: [`midi-output.md`](docs/midi-output.md) |
| **Undo / Version graph read-only browser** | PR #82 `b741251`. |
| **DOM test environment (jsdom)** — mounted component tests for `AutomationTrackRow` | PR #80 `ca67b7d`. |
| G1 Task 6 — PDC (`DelayLine` + `compile_pdc`) | PR #71 `9e0b884`. `RtTrack::pdc` is still `None` everywhere in production — wiring `compile_pdc`'s output into a graph rebuild is Task 7, not started. Code review before merge caught and fixed a real bug: the formula didn't handle a bypassed insert's latency correctly (would have jumped the mix on bypass toggle once Task 7 wired it in). Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| Clip Delete/Backspace after a plain pointer-click | PR #70 `7011e81`. Pointer-select never focused the clip element, so its own keydown handler never fired; a window-level fallback now reads `clipSelection` directly. Single-clip only — batch-deleting a multi-selection still needs a `clips_remove` transaction (Track C leftover, unchanged) |
| G1 Task 5 — mixer strip: source-sum → inserts REPLACE → shared fader (`InsertNode`/`compile_inserts`) | PR #66 `c99293b`. `compile_inserts` is not yet wired into `engine::rebuild` (Task 7) — an insert still isn't audible end-to-end. Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| **The Composer, Plan H1** — merged to `main` (PR #65, `63cb7fa`, 2026-08-18): a pure music-theory library, the harmony document in the core, five generators (progression, voice-led chords, bass, melody, groove), the COMPOSER panel, and a piano roll that tints its keys by what each note does over the chord. Owner ear-checked the same day: works; MIDI-out-to-Hydrogen bug found, needs its own unscoped PR. **Composer is now deprioritized — do not start H2+ unless asked.** | PR [#65](https://github.com/knobo/aura-daw/pull/65) (merged). Product doc: [`composer-assistant.md`](docs/backlog/composer-assistant.md). Plan + rulings H-1…H-12: [`2026-08-17-plan-h-composer.md`](docs/superpowers/plans/2026-08-17-plan-h-composer.md). Handoff: [`composer-h1.md`](docs/handoff/composer-h1.md). ARCHITECTURE §16 |
| **MIDI output — eight fixes** (re-cue storm, per-note channel, routed track no longer doubles its internal synth, undo-after-delete silence, forced-channel persistence, piano-roll note channel, ALSA re-address, live port list). An owner ear-check on a real drum machine is still owed. | PR #77 `55257e8`. Handoff: [`midi-out-note-channel.md`](docs/handoff/midi-out-note-channel.md) |
| **CI: apt no longer hangs the Rust job, and the Rust cache now exists.** The job was ~27 min of which the 1231 tests were 20 s; `apt-get update` had stalled 23.5 min and been cancelled. apt is now 3 min (`--no-install-recommends` + bounded retries), and `push: branches: [main]` finally lets `Swatinem/rust-cache` save a cache a PR can restore — without it every PR compiled ~600 crates from scratch. | PR #78 `2b00e91` |
| Lanes UX — rename, fold (lane + group), group, drag-reorder, and the timeline scroll/alignment fix | PR #60 `5f891cb`. Rebased onto phase 3 + the theme system; 10 code-review findings fixed before merge. Handoff: [`lanes-ux.md`](docs/handoff/lanes-ux.md) |
| Pitch Coach **phase 3** — per-note scoring, stored pitch curve, take report | PR #61 `c14916d`. [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| Theme system — token contract, eight built-in themes, user themes from JSON | PR #63 `46df20d`. User docs: [`docs/themes.md`](docs/themes.md). `no-literals.test.ts` now guards every component's `<style>` block — a new component with a raw colour literal fails CI; use a token or a `theme-exempt:` comment. |
| G1 Tasks 2–4 — insert ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23`. Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| G1 Task 1 — `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Pitch Coach **phase 2** — panel, frame bus, lane geometry, prefs | PR #58 `7c3cb87`. Adds `pitch_unsubscribe` (additive). Owner ear-check done; the fixes it produced have NOT been re-checked by ear. **Next: phase 3, Task 12** — but read [`pitch-as-data.md`](docs/backlog/pitch-as-data.md) before Task 14 |
| Pitch Coach phase 1 | PR #49 `84b0313` + PR #54 `f451a5a` (detection off the RT callback, listen mid-take, `pitch_check`). R3 answered with numbers. [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| Gesture tokens | PR #47 |
| External audio editor | PR #48 |
| MIDI launch v0.1 | PR #42 + #50. Hardware GATE / sustain still open: [`midi-launch.md`](docs/backlog/midi-launch.md) |
| Tracks A–F / Plan F | log: [`docs/handoff/landed-tracks.md`](docs/handoff/landed-tracks.md). Rulings: [`docs/PHASE4-PLAN.md`](docs/PHASE4-PLAN.md) |

## Open leftovers (open the pointer if you are on that item)

- **G1 Tasks 7–10** (the rest of the plan, HOLD until an automation/undo leftover above is done — owner steer, 2026-08-18): rebuild/offline wiring, IPC+UI, handoff. Do not jump to Task 9 UI before Task 7 wires the mixer into rebuild — an insert is still silent end-to-end until then (G-11 note in the handoff). Task 6's own known gap for Task 7 to pick up: PDC is applied after inserts but before the fader, so once wired, a track's automation ramps will read the wrong playhead position by its own PDC delay — documented in `pdc.rs`'s module doc.
- **G1 deferred minors**: listed in [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md).
- **MIDI-out to Hydrogen bug** — found during the Composer H1 ear-check
  (2026-08-18), not yet filed or scoped. Needs its own small PR; do not fold
  it into G-series or Composer work.
- **Sing-along from any song** — new owner ask (2026-08-17), and the reason
  melody extraction outranks the auto-tune work: import → split stems →
  melody from the vocal stem → sing against it in the coach. Three of four
  steps are landed. Settle two things before building: does the detector
  survive a Demucs stem (an afternoon, no new code), and tempo alignment
  (`import.rs` detects no tempo, so a tick-based reference lands wrong).
  [`pitch-as-data.md`](docs/backlog/pitch-as-data.md).
- **Pitch as data** — new owner ask (2026-08-17). Melody extraction from an
  existing clip (→ MIDI clip) and the continuous pitch track (→ `APTF`, from
  Pitch Coach Task 14). **If you are about to implement Task 14, read
  [`pitch-as-data.md`](docs/backlog/pitch-as-data.md) first**: four
  decisions are due before that format ships, and one of them (a provenance
  byte for causal-vs-centred smoothing) is free now and a migration later.
- **Pitch correction / auto-tune** — new owner ask (2026-08-17), NOT part of
  the Pitch Coach plan. Detection is done; a formant-preserving shifter and
  a correction policy are not. Offline-first staged path and the four
  rulings it needs before Stage A:
  [`pitch-correction-autotune.md`](docs/backlog/pitch-correction-autotune.md).
  Do not start Stage C (live insert) before G1 inserts + PDC.
- **Pitch Coach phase 3 landed** (PR #61, see the Landed table). Whatever
  is next for Pitch Coach lives in
  [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md)
  — read that file rather than this bullet, which is not being kept
  current for that track.
- **Uncommitted Windows-build work in the main checkout** — status
  UNCLEAR as of this update (2026-08-17): the main checkout has since
  moved from branch `docs/pitch-coach-handoff` to `main` and is now
  clean, with none of the files below present or tracked. It is not
  known from this session whether that work was committed elsewhere,
  intentionally dropped, or is sitting in a stash/another checkout —
  **check before assuming it is gone**. What follows is what a review
  found while the tree was still dirty on branch `docs/pitch-coach-handoff`
  (2026-08-17), recorded here because it exists nowhere else: a `lv2`
  Cargo feature with an `lv2_host_stub`, Windows CLAP search paths,
  platform-aware ffmpeg hints, a `release-windows.yml`, and
  `bundle.active: true`. Not the current session's work, and not in a PR.
  Five things the owner had not seen yet:
  1. **`src-tauri/icons/icon.ico` is untracked** while `tauri.conf.json`
     (a tracked modification) references it. `git commit -am` lands the
     reference without the asset and the first `v*` tag fails at the bundle
     step. `.github/workflows/release-windows.yml` is untracked too.
  2. **`bundle.active: true` with `targets: "all"`** enables bundling on
     every platform, so a local Linux `tauri build` now runs the AppImage
     bundler (which downloads `linuxdeploy` mid-build) and emits deb/rpm
     with no `depends` and no sidecar resources. Scope it via
     `tauri.windows.conf.json` or `"targets": ["nsis", "msi"]`.
  3. **`control/export.rs:291`** still hard-codes `apt install ffmpeg` in
     `capabilities()`, one function from the new platform-aware `HOW`.
  4. **`control/export.rs:880`/`:894`** assert on `"apt install ffmpeg"`, so
     `cargo test` is red on Windows — the platform being added. Assert on
     `"requires ffmpeg"` instead.
  5. **`.github/workflows/tests.yml:67`** only exercises the `lv2` axis on
     ubuntu; nothing in CI compiles the new `#[cfg(target_os = "windows")]`
     branches, which is exactly the rot the step's own comment warns about.
  Unverified by the current session beyond reading the diff — treat as
  leads, not verdicts.
- **Owner ear-checks** (no suite substitutes): automation fade during play (Track D); MIDI note-out / Hydrogen / keyboard record (Track B). Insert FX is not ear-checkable until Task 7 wires `compile_inserts` into `engine::rebuild` (Task 5 landed the mixer walk itself but nothing calls it yet). **Pitch Coach R3 is done** — `cargo run --example pitch_check` reproduces it; a sustained vowel gave no octave errors in 1312 frames. **Pitch Coach phase 2 was ear-checked 2026-08-17** and it found a real bug (the lane was not drawn while stopped, so the reference notes seemed to appear only on record). Fixed in the same PR — but the fix itself has not been heard yet, so one more pass with a microphone is owed.
- **Plan F carry-forwards:** live-document B-tree, I-1 option (a), no journal auto-apply. The read-only version-graph browser was implemented by PR #82 on `codex/undo-version-graph-ui`; the ordered follow-up is the guarded linear `Undo to here` contract documented in the Plan F handoff. Read the Plan F handoff in `docs/PHASE4-PLAN.md` before touching snapshots, the journal reader, the version graph, or `engine::rebuild`. Branch `plan-f-history` is kept so cited SHAs resolve.
- **Track D leftovers:** plugin-param bounce, write/touch/latch, no DOM test env. (Non-blocking CLAP param path closed 2026-08-18, PR #75, branch `clap-nonblocking-params` — see the Track D handoff. A post-merge `/code-review` found two accepted-not-fixed trade-offs from that PR, recorded in the Track D handoff and in both new tests' doc comments — not new work items unless someone picks them up: `post_params` posts into `plugin_main()`'s **unbounded** channel with no back-pressure, so a slow concurrent `run()` (any instantiate/save_state/remove, CLAP or LV2) can let automation writes pile up and drain as an audible catch-up burst — parity with an LV2 risk that already existed, not a new one, and a real fix is a `plugin_main()`-level design question out of scope for a param-write PR; and the two new timing tests wedge that same process-wide singleton, safe only under this repo's `--test-threads=1` CI convention. Follow-up doc+test commit: PR #76, branch `clap-nonblocking-params-followup` — **still open, merge it before starting new Track D work.**) Details: `docs/PHASE4-PLAN.md` "Track D handoff" and [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md).
- **Modulation design §8** — the ordered path to the finished system (ports, modulators, macros, curve shapes, recording, sample-accurate plugin params, lazy expansion, per-voice modulation):
  [`docs/superpowers/specs/2026-08-15-modulation-system-design.md` §8](docs/superpowers/specs/2026-08-15-modulation-system-design.md#8-the-path-to-the-finished-system).
  ADR 0008. Handoff: `docs/PHASE4-PLAN.md` "Track F handoff". Do not restate §8 here (R2).
- **Plan E review leftovers:** M-8 (unowned). Closed items: [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md) — do not re-open them.
- **Track B / C leftovers** that are not ear-checks: recording under an active loop; multi-clip delete (batch `clips_remove` — single-clip Delete-after-click is now fixed, PR #70). See the matching PHASE4-PLAN handoff.

## Standing constraints (all work)

- **`src-tauri/src/theory/` is PURE and stays pure** (Plan H1, ruling H-5):
  no `tauri`, `parking_lot`, `std::fs`, `crate::control`, `crate::audio`, and
  no `thread_rng` — every generator takes an explicit `seed`. The only
  sanctioned crate dependency is `crate::midi::MidiNote`. A generator that
  needs project data is passed it. Same class of rule as the RT contract: it
  is what keeps that suite fast and that library reusable.
- **The harmony document is a map pair, not a track** (H-3/H-5): one op
  (`Op::HarmonySet`), no `rebuild` effect, `OP_FORMAT_VERSION` still 2,
  `schemaVersion` unmoved, and the `project.json` key written only when the
  document is non-empty. Do not add a `TrackKind` or a second harmony op.

These are load-bearing as of Gate E closing. Historical plans called
this `next-prompt.md` §2.

- **The op log is ON.** `journal.ndjson` is a **persisted format** from
  now on: `OP_FORMAT_VERSION` (**2** since the post-merge follow-up PR —
  base64 state blobs, I-5) is load-bearing the moment any project has a
  journal file. Additive `#[serde(default)]` fields on an op or on
  `TxMeta` stay non-breaking; anything else (renaming a field, changing a
  variant's shape, removing a path) needs a version bump AND a reader that
  understands both shapes. Plan F's journal reader (`control/replay.rs`)
  requires `v == 2` on batch lines (ruling F-5); v1 lines are skipped
  with a warn. A change of this kind now costs a migration.
- **Thin renderer** (ADR 0006) still holds: no new authoritative state,
  business logic, or time math lands frontend-side. Every frontend change
  is op emission, gesture emission, or UI/chrome.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive (the same rule that shaped every Plan E task).
- **`transact` closures must not panic.** Plan F landed panic
  *containment* (`catch_unwind` + restore from the pre-tx snapshot;
  ruling F-3) — that is a crash-consistency net, not a license.
  Validate before mutating, every time, exactly as every landed op arm
  does.
- **The M-3 redo invariant**: a transient write must never touch a
  document field an entry's `ops` can address, or a pending redo silently
  lands on a different state than the entry recorded. If you add a new
  transient transaction (transport-like, engine-thread, or gesture
  mid-flight), this is the check to run before shipping it. As of the
  post-merge follow-up PR a `debug_assert!` in the commit path enforces it
  in debug builds (transient ops may address only `ObjectRef::Transport`,
  unless the batch is a mid-gesture fold) — so a violation now fails the
  test suite instead of waiting to be noticed.
- **Undo is bounded**: 200 entries, in-memory, bottom-eviction, cleared at
  epochs (project open/create/save-as). The journal is unbounded and
  append-only; Plan F added a reader (detection + primitive, no
  auto-apply — ruling F-8).
- **Foreground timeout-guarded test runs** and the **dated-count
  convention**: any task that changes test counts updates README.md +
  CONTRIBUTING.md in the same commit, with the date. Current counts live
  there — measure, do not copy a number from a briefing.
- **Gesture lock order is gesture-before-session, everywhere** (Task 14's
  fix) — if a track adds a new gesture-shaped commit path, follow this
  order or reintroduce the TOCTOU that fix closed.

## Branch and worktree rules (all work)

- **New branches start from `origin/main`.** Never branch from a stale
  local main or from another track's branch.
- **Continuing branches merge `origin/main` in** whenever it has advanced,
  at a task boundary — don't let a long-running branch drift.
- **Never use bare `git stash`.** If you need to shelve work, commit it or
  use a named stash; bare `stash` has bitten prior sessions when a worktree
  switch lost track of it.
- **Use a dedicated git worktree per track** (`git worktree add <path> -b
  <branch> origin/main`, one path per track) so parallel tracks do not
  share a working tree.
- **Foreground test runs only, `timeout`-guarded.** No backgrounding a test
  run and moving on — every gate in this project has been foreground since
  Plan A.

```
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```

## Index

- [`docs/backlog/00-ROADMAP-real-alternative.md`](docs/backlog/00-ROADMAP-real-alternative.md) — master product index.
- [`docs/handoff/`](docs/handoff/) — landed-track log and Plan E review log.
- [`docs/PHASE4-PLAN.md`](docs/PHASE4-PLAN.md) — orchestration + per-track handoffs (rulings, carry-forwards).
- [`docs/SIDE-CHANNEL-INVENTORY.md`](docs/SIDE-CHANNEL-INVENTORY.md) — landed inventory. L-4/L-5/R-3 CLOSED (Plan F); L-2 documented benign (F-9).
- [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md) — Plan G product cut.
- [`docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`](docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md) — G1 plan. Implement that. Do not start G2.
- [`docs/backlog/hardware-midi-io.md`](docs/backlog/hardware-midi-io.md), [`multi-clip-selection-and-paste.md`](docs/backlog/multi-clip-selection-and-paste.md), [`automation-audible-and-ui.md`](docs/backlog/automation-audible-and-ui.md), [`library-and-browser.md`](docs/backlog/library-and-browser.md) — Tracks B/C/D/E product docs.
- [`docs/backlog/external-instrument-return.md`](docs/backlog/external-instrument-return.md) — MIDI-out's missing half. Read before adding "hidden tracks" or a PipeWire graph orchestrator.
- [`docs/CORE-REDESIGN-ROUND-2.md`](docs/CORE-REDESIGN-ROUND-2.md) §6 / ADR 0005 — Plan F spec.

When a leftover lands, update this file's current-job line, the landed
table, and the relevant handoff — not a new essay in this file.
