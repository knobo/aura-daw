# Next: the Composer's owner ear-check, then G1 Task 6

`origin/main` is the baseline. Branch from there. Subagent-driven,
task-by-task. Reply to the user in Norwegian — they write Norwegian;
the repo documentation is English.

**Do this, in order:**

1. **Hear the Composer.** Plan H1 is up as PR #65 on
   `feat/composer-assistant` (merged with `main`, not merged INTO it):
   open the COMPOSER dock tab (`O`), pick a plan, GENERATE, and listen.
   The suite proves the notes obey the theory; only an ear can say
   whether the defaults are *nice*. The judgements owed — registers,
   melodic taste, groove feel, and the fact that a generated drum clip
   has no kit to play through — are listed in
   [`docs/handoff/composer-h1.md`](docs/handoff/composer-h1.md).
   **Playback on this box needs an ALSA/HDMI sink**: with a Bluetooth
   default sink the engine opens a stream and never gets a callback, so
   nothing plays and the transport stays at 0 (CONTRIBUTING documents
   the same failure for 18 engine tests). `EXPORT` bounces offline and
   works regardless.
2. **G1 Task 6** — PDC (`DelayLine` + `compile_pdc`). Plan:
   [`docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`](docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md)
   (product: [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md)).
   Handoff of what just landed: [`docs/handoff/g1-insert-fx.md`](docs/handoff/g1-insert-fx.md).

**Do not:** start Composer H2–H6 before the ear-check (the whole point of
H1 is that the next phase reads it, and taste feedback changes what H2
should be); start G2/G3/G4; write a stock FX suite; bump
`OP_FORMAT_VERSION`; restart a landed track (A–F, Plan F, G1 Tasks 1–5,
Pitch Coach phases 1–3, the lanes UX track, the theme system, Composer
H1).

**In flight:** `feat/composer-assistant` (Plan H1, PR #65 — open, with
`main` merged in). Latest on `main`: `c99293b` (G1 Task 5 mixer strip,
PR #66). Stale worktrees for merged branches can be ignored — but keep
the branches `feat/pitch-coach`, `feat/pitch-coach-panel`,
`plan-f-history` and `worktree-lanes-ux`, whose per-commit or squashed
SHAs are cited in the handoffs. (2026-08-18: 16 stale merged-branch
worktrees under `.claude/worktrees/` were removed to free disk space —
the branches themselves were not deleted; `.claude/worktrees/composer`
is live and carries PR #65.)

This file is the briefing after `/clear`. History and leftovers that
are not every task's business live under [`docs/handoff/`](docs/handoff/).
Trust those files / the code over this file if they disagree, and update
this file (marked correction, ADR 0007) if they do.

## Landed (do not restart)

| What | Pointer |
|---|---|
| G1 Task 5 — mixer strip: source-sum → inserts REPLACE → shared fader (`InsertNode`/`compile_inserts`) | PR #66 `c99293b`. `compile_inserts` is not yet wired into `engine::rebuild` (Task 7) — an insert still isn't audible end-to-end. Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| **The Composer, Plan H1** — NOT on `main` yet (PR #65): a pure music-theory library, the harmony document in the core, five generators (progression, voice-led chords, bass, melody, groove), the COMPOSER panel, and a piano roll that tints its keys by what each note does over the chord | PR [#65](https://github.com/knobo/aura-daw/pull/65) on `feat/composer-assistant`. Product doc: [`composer-assistant.md`](docs/backlog/composer-assistant.md). Plan + rulings H-1…H-12: [`2026-08-17-plan-h-composer.md`](docs/superpowers/plans/2026-08-17-plan-h-composer.md). Handoff (ear-check owed, what H2 picks up): [`composer-h1.md`](docs/handoff/composer-h1.md). ARCHITECTURE §16 |
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

- **G1 Tasks 6–10** (not leftovers — the rest of the plan): PDC, rebuild/offline, IPC+UI, handoff. Start at Task 6. Do not jump to Task 9 UI before Task 7 wires the mixer into rebuild — an insert is still silent end-to-end until then (G-11 note in the handoff).
- **G1 deferred minors** (do not block Task 6): listed in [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md).
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
- **Plan F carry-forwards:** live-document B-tree, I-1 option (a), no journal auto-apply, version-graph UI. Read the Plan F handoff in `docs/PHASE4-PLAN.md` before touching snapshots, the journal reader, the version graph, or `engine::rebuild`. Branch `plan-f-history` is kept so cited SHAs resolve.
- **Track D leftovers:** plugin-param bounce, non-blocking CLAP param path, write/touch/latch, no DOM test env. Details: `docs/PHASE4-PLAN.md` "Track D handoff" and [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md).
- **Modulation design §8** — the ordered path to the finished system (ports, modulators, macros, curve shapes, recording, sample-accurate plugin params, lazy expansion, per-voice modulation):
  [`docs/superpowers/specs/2026-08-15-modulation-system-design.md` §8](docs/superpowers/specs/2026-08-15-modulation-system-design.md#8-the-path-to-the-finished-system).
  ADR 0008. Handoff: `docs/PHASE4-PLAN.md` "Track F handoff". Do not restate §8 here (R2).
- **Plan E review leftovers:** M-8 (unowned). Closed items: [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md) — do not re-open them.
- **Track B / C leftovers** that are not ear-checks: recording under an active loop; multi-clip delete. See the matching PHASE4-PLAN handoff.

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
