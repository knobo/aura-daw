# Next: Plan G1 Task 5

`origin/main` is the baseline. Branch from there. Subagent-driven,
task-by-task. Reply to the user in Norwegian — they write Norwegian;
the repo documentation is English.

**Do this:** G1 Task 5 — mixer strip (`source → inserts → fader`). Plan:
[`docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`](docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md)
(product: [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md)).
Handoff of what just landed: [`docs/handoff/g1-insert-fx.md`](docs/handoff/g1-insert-fx.md).

**Do not:** start G2/G3/G4; write a stock FX suite; bump
`OP_FORMAT_VERSION`; restart a landed track (A–F, Plan F, G1 Tasks 1–4,
Pitch Coach phases 1 **and 2**).

**Nothing is in flight.** Pitch Coach phase 2 merged as `7c3cb87`
(PR #58). Stale worktrees for merged branches can be ignored — but keep
the branches `feat/pitch-coach`, `feat/pitch-coach-panel` and
`plan-f-history`, whose per-commit or squashed SHAs are cited in the
handoffs.

This file is the briefing after `/clear`. History and leftovers that
are not every task's business live under [`docs/handoff/`](docs/handoff/).
Trust those files / the code over this file if they disagree, and update
this file (marked correction, ADR 0007) if they do.

## Landed (do not restart)

| What | Pointer |
|---|---|
| G1 Tasks 2–4 — insert ops, commands, `HostRole::Effect`, HostForward restore | PR #55 `118ae23`. Handoff: [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md) |
| G1 Task 1 — `InsertSlot` on `TrackState.inserts` | PR #52 `5c338ff` |
| Pitch Coach **phase 2** — panel, frame bus, lane geometry, prefs | PR #58 `7c3cb87`. Adds `pitch_unsubscribe` (additive). Owner ear-check done; the fixes it produced have NOT been re-checked by ear. **Next: phase 3, Task 12** — but read [`pitch-as-data.md`](docs/backlog/pitch-as-data.md) before Task 14 |
| Pitch Coach phase 1 | PR #49 `84b0313` + PR #54 `f451a5a` (detection off the RT callback, listen mid-take, `pitch_check`). R3 answered with numbers. [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md) |
| Gesture tokens | PR #47 |
| External audio editor | PR #48 |
| MIDI launch v0.1 | PR #42 + #50. Hardware GATE / sustain still open: [`midi-launch.md`](docs/backlog/midi-launch.md) |
| Tracks A–F / Plan F | log: [`docs/handoff/landed-tracks.md`](docs/handoff/landed-tracks.md). Rulings: [`docs/PHASE4-PLAN.md`](docs/PHASE4-PLAN.md) |

## Open leftovers (open the pointer if you are on that item)

- **G1 Tasks 5–10** (not leftovers — the rest of the plan): mixer strip, PDC, rebuild/offline, IPC+UI, handoff. Start at Task 5. Do not jump to Task 9 UI before the mixer hears inserts (G-11).
- **G1 deferred minors** (do not block Task 5): listed in [`g1-insert-fx.md`](docs/handoff/g1-insert-fx.md).
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
- **Pitch Coach phase 3** — scoring, Tasks 12–16, once phase 2's PR is in.
  Task 12 (the shared repeat-expansion helper) also replaces
  `panel-logic.targetNotesFor`, which expands repeats frontend-side today.
  Read
  [`pitch-coach-PROGRESS.md`](docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md)
  first. **The panel has never run against the real engine** — only against
  `DemoBackend`'s synthetic singer in a browser. That ear-check is the
  first thing to do with the phase 2 branch.
- **Owner ear-checks** (no suite substitutes): automation fade during play (Track D); MIDI note-out / Hydrogen / keyboard record (Track B). Insert FX is not ear-checkable until Task 5 lands. **Pitch Coach R3 is done** — `cargo run --example pitch_check` reproduces it; a sustained vowel gave no octave errors in 1312 frames. **Pitch Coach phase 2 was ear-checked 2026-08-17** and it found a real bug (the lane was not drawn while stopped, so the reference notes seemed to appear only on record). Fixed in the same PR — but the fix itself has not been heard yet, so one more pass with a microphone is owed.
- **Plan F carry-forwards:** live-document B-tree, I-1 option (a), no journal auto-apply, version-graph UI. Read the Plan F handoff in `docs/PHASE4-PLAN.md` before touching snapshots, the journal reader, the version graph, or `engine::rebuild`. Branch `plan-f-history` is kept so cited SHAs resolve.
- **Track D leftovers:** plugin-param bounce, non-blocking CLAP param path, write/touch/latch, no DOM test env. Details: `docs/PHASE4-PLAN.md` "Track D handoff" and [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md).
- **Modulation design §8** — the ordered path to the finished system (ports, modulators, macros, curve shapes, recording, sample-accurate plugin params, lazy expansion, per-voice modulation):
  [`docs/superpowers/specs/2026-08-15-modulation-system-design.md` §8](docs/superpowers/specs/2026-08-15-modulation-system-design.md#8-the-path-to-the-finished-system).
  ADR 0008. Handoff: `docs/PHASE4-PLAN.md` "Track F handoff". Do not restate §8 here (R2).
- **Plan E review leftovers:** M-8 (unowned). Closed items: [`docs/handoff/plan-e-review.md`](docs/handoff/plan-e-review.md) — do not re-open them.
- **Track B / C leftovers** that are not ear-checks: recording under an active loop; multi-clip delete. See the matching PHASE4-PLAN handoff.

## Standing constraints (all work)

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
