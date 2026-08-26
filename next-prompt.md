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
| _(none)_ | | | |

## Next up — unclaimed

1. **Owner ear-checks** are owed on several landed tracks. No suite
   substitutes for them; each backlog file lists its own. The newest two
   are the control surface's (open SURFACE, Add all, drag a fader, tap a
   pad) and G2's: one reverb on a bus, several tracks sent into it, one
   room — then export and confirm the WAV has it.
2. **Plan V — V1: `MixNode` as the compiler's input.** The owner's
   biggest ask (a pad that holds a raw WAV or its own instrument, knobs
   on no track, recording what you play) needs a second time base;
   V1 is the behaviour-neutral refactor everything else stands on.
   Read the design's rulings V-1…V-12 before touching code.
   → [`docs/backlog/plan-v-players.md`](docs/backlog/plan-v-players.md)
3. **G3 — sidechain edges** (bass ducks the pads). The graph now has
   buses and sends; a sidechain is the third routing primitive.
   → [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md)
4. **G1 Tasks 9–10** — insert UI polish and the handoff. The audio path
   is live in both the engine and the bounce.
   → [`docs/backlog/insert-fx-pdc.md`](docs/backlog/insert-fx-pdc.md)
4. **Profile a real session under plugin load.** The JIT track measured
   the fader carefully and then concluded the fader is not where a DAW's
   CPU goes — plugins are. Nobody has measured that here. Any further
   engine performance work should start from those numbers rather than
   from a guess. → [`docs/GAP_ANALYSIS.md`](docs/GAP_ANALYSIS.md) §8.4

**Do not start unless asked:** Composer H2+ (deprioritised);
Plan G4 (envelope-follower modulators); the native-GUI embed work;
wiring `aura-engine`'s JIT kernel into `mixer::render` — **decided
against**, see [`docs/GAP_ANALYSIS.md`](docs/GAP_ANALYSIS.md) §8 before
reopening it.

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
| **Plan V — players (V1 unclaimed)** | [`docs/backlog/plan-v-players.md`](docs/backlog/plan-v-players.md) |
| Plugin manager (preview slot open) | [`docs/backlog/plugin-manager.md`](docs/backlog/plugin-manager.md) |
| Mixer graph — inserts, sends, sidechain (Plan G; G3 next) | [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md) |
| Insert FX / PDC task list (Plan G1) | [`docs/backlog/insert-fx-pdc.md`](docs/backlog/insert-fx-pdc.md) |
| Pitch — Coach, extraction, correction | [`docs/backlog/pitch.md`](docs/backlog/pitch.md) |
| MIDI in/out | [`docs/backlog/midi-io.md`](docs/backlog/midi-io.md) |
| History, undo, automation, modulation | [`docs/backlog/history-and-automation.md`](docs/backlog/history-and-automation.md) |
| Windows build (open in PR #62) | [`docs/backlog/windows-build.md`](docs/backlog/windows-build.md) |
| Control surface (virtual mixer / pad deck) | [`docs/backlog/control-surface.md`](docs/backlog/control-surface.md) |
| RT engine / JIT fader kernel (`aura-engine/`, landed as a proving ground; shipping the JIT is decided against) | [`docs/backlog/jit-engine.md`](docs/backlog/jit-engine.md) |
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
