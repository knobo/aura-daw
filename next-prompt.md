# AURA — start here

Reply to the user in Norwegian — they write Norwegian; the repo
documentation is English.

This file is a dispatcher, not a briefing. It stays short on purpose:
what is claimed, what is free, and where the detail lives. Detail belongs
in [`docs/backlog/`](docs/backlog/), never here.

## Before you write a single line of code

**1. Cut your own worktree from `origin/main`.**

```sh
git fetch origin
git worktree add .worktrees/<short-name> -b <branch-name> origin/main
```

Both halves are load-bearing and each one has already cost this project a
session — [`CLAUDE.md`](CLAUDE.md) explains why. Never reuse an existing
worktree, never branch from whatever happens to be checked out, and never
reopen a worktree whose branch has merged: it is spent.

**2. Check what is already being worked on.** The claims table below is
the durable record, but a claim that has not merged yet only exists as an
open PR, so check all three:

```sh
gh pr list --state open
git ls-remote --heads origin
git worktree list
```

**3. Claim the job before you start it.** Add your row to *Active
claims*, commit it **as the first commit on your branch**, push, and open
the PR immediately — draft is fine. An unpushed claim is not a claim, and
two agents finishing the same task is the most expensive failure this
project has had.

**4. Release the claim in the PR that lands the work** — merging does
not do it for you. The last commit before you merge deletes your row and
moves the outcome into [`docs/LANDED.md`](docs/LANDED.md) with a pointer
to its backlog file. If you abandon a job, delete your row in its own
small PR so the next agent can pick it up.

A row left behind is worse than no table: it points at a branch that no
longer exists, and the next agent cannot tell a stale row from live work.
The first PR to use this table left its own row behind, which is how this
sentence got written — if you find a row whose branch is gone from
`git ls-remote --heads origin`, delete it.

## Active claims

| Job | Branch | Claimed | Notes |
|---|---|---|---|
| JIT-fused fader + triple-buffer publish + RT bench harness | `feat/jit-fused-fader` | 2026-08-24 | New standalone `aura-engine/` crate — `src-tauri/Cargo.toml` stays frozen. → [`docs/backlog/jit-engine.md`](docs/backlog/jit-engine.md) |

## Next up — unclaimed

1. **Step 7 — unified audition** (plugin track). Double-click any browser
   row to hear it, behind a new `browserAudition` pref defaulting off.
   Last step on that track; it touches every browser plus sampler and
   plugin hosts. → [`docs/backlog/plugin-manager.md`](docs/backlog/plugin-manager.md)
2. **Owner ear-checks** are owed on several landed tracks. No suite
   substitutes for them; each backlog file lists its own.
3. **G1 Tasks 7–10** — on HOLD until an automation/undo leftover is done
   (owner steer, 2026-08-18). → [`docs/backlog/insert-fx-pdc.md`](docs/backlog/insert-fx-pdc.md)

**Do not start unless asked:** Composer H2+ (deprioritised);
Plan G2; the native-GUI embed work.

## Read before your first commit

- [`docs/STANDING-CONSTRAINTS.md`](docs/STANDING-CONSTRAINTS.md) — the
  rules that bind every task: worktree and branch discipline, the thin
  renderer, frozen commands, the op log, undo, the dated-count
  convention.
- [`docs/TRAPS.md`](docs/TRAPS.md) — things that have already cost this
  project time. Add to it when something costs you an hour.

## Backlog

| Track | File |
|---|---|
| Plugin manager (Step 7 next) | [`docs/backlog/plugin-manager.md`](docs/backlog/plugin-manager.md) |
| Insert FX / sends / PDC (Plan G1) | [`docs/backlog/insert-fx-pdc.md`](docs/backlog/insert-fx-pdc.md) |
| Pitch — Coach, extraction, correction | [`docs/backlog/pitch.md`](docs/backlog/pitch.md) |
| MIDI in/out | [`docs/backlog/midi-io.md`](docs/backlog/midi-io.md) |
| History, undo, automation, modulation | [`docs/backlog/history-and-automation.md`](docs/backlog/history-and-automation.md) |
| Windows build (open in PR #62) | [`docs/backlog/windows-build.md`](docs/backlog/windows-build.md) |
| Everything else, by product area | [`docs/backlog/`](docs/backlog/) |

## Index

- [`docs/LANDED.md`](docs/LANDED.md) — what is already merged. Check it
  before starting anything.
- [`docs/backlog/00-ROADMAP-real-alternative.md`](docs/backlog/00-ROADMAP-real-alternative.md) — master product index.
- [`docs/PHASE4-PLAN.md`](docs/PHASE4-PLAN.md) — orchestration and
  per-track handoffs (rulings, carry-forwards).
- [`docs/handoff/`](docs/handoff/) — landed-track log and review logs.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md), [`docs/SCALABILITY.md`](docs/SCALABILITY.md).

## Keeping this file small

When a job lands, the PR that lands it also updates this file: remove the
claim row, and move the outcome into `docs/LANDED.md` plus the track's
backlog file. New detail goes in the backlog file — if you find yourself
writing a paragraph here, it belongs somewhere else.
