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
| Investigate WebKitGTK full-viewport repaint persisting despite `contain`/canvas (PR #131) | `investigate/surface-repaint` | 2026-08-29 | Testing `will-change: transform` on the meter/gauge canvases for real GPU layer promotion, since `contain` alone didn't stop the whole window repainting on every meter tick. |

## Next up — unclaimed

1. **Owner ear-checks** are owed on several landed tracks. No suite
   substitutes for them; each backlog file lists its own. The newest two
   are the control surface's (open SURFACE, add two LPD8 racks and a
   channel strip side by side, drag a fader, tap a pad) and G2's: one reverb on a bus, several tracks sent into it, one
   room — then export and confirm the WAV has it.
2. **Plan V — V2: one player, real.** `session.players[]`, ops, a graph
   slot, audio/MIDI sources, one playhead per player,
   `player_fire`/`player_stop`; deletes the launch overlay. V1 (PR #118)
   landed the `MixNode` refactor this stands on and its inherited gate.
   → [`docs/backlog/plan-v-players.md`](docs/backlog/plan-v-players.md)
3. **G3 — sidechain edges** (bass ducks the pads). The graph now has
   buses and sends; a sidechain is the third routing primitive.
   → [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md)
4. **G1 Tasks 9–10** — insert UI polish and the handoff. The audio path
   is live in both the engine and the bounce.
   → [`docs/backlog/insert-fx-pdc.md`](docs/backlog/insert-fx-pdc.md)
5. **Profile on a weak machine.** [`docs/GAP_ANALYSIS.md`](docs/GAP_ANALYSIS.md)
   §9 answered §8.4: on an i9-14900 a 32-track, 66-insert session uses
   8.6% of the deadline, plugin DSP is 28% of a block and AURA's own code
   52% — of which ~3 µs per insert slot is the one large addressable
   line. Nothing is audible there, so the open question is not *what*
   the time goes to but *which machine is the floor* (§9.3). Run the same
   harness on a modest laptop. Only chase the insert path (§9.5's
   flamegraph) if that run says a real session breaks.

6. **Dropping `--test-threads=1` — blocked on one ON-HOLD bug.** The
   `midi_out` and `clap_host` families are fixed; the first was a product
   bug, not a thread race (see [`docs/LANDED.md`](docs/LANDED.md)), and the
   full parallel suite went 0 of 8 fully green to 45 of 46. What remains is
   PR #124's `lv2_ui` PDEATHSIG test, **deliberately deferred** — either a
   pid-reuse artefact or a real orphaned-plugin-window bug, and nobody knows
   which. Do not start it as a job; item 7 lists what should bring it back.
   → [`docs/backlog/ci-hardening.md`](docs/backlog/ci-hardening.md) item 7

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
| **Plan V — players (V2 unclaimed)** | [`docs/backlog/plan-v-players.md`](docs/backlog/plan-v-players.md) |
| Plugin manager (preview slot open) | [`docs/backlog/plugin-manager.md`](docs/backlog/plugin-manager.md) |
| Mixer graph — inserts, sends, sidechain (Plan G; G3 next) | [`docs/backlog/insert-fx-sends-sidechain.md`](docs/backlog/insert-fx-sends-sidechain.md) |
| Insert FX / PDC task list (Plan G1) | [`docs/backlog/insert-fx-pdc.md`](docs/backlog/insert-fx-pdc.md) |
| Pitch — Coach, extraction, correction | [`docs/backlog/pitch.md`](docs/backlog/pitch.md) |
| MIDI in/out | [`docs/backlog/midi-io.md`](docs/backlog/midi-io.md) |
| History, undo, automation, modulation | [`docs/backlog/history-and-automation.md`](docs/backlog/history-and-automation.md) |
| Windows build (open in PR #62) | [`docs/backlog/windows-build.md`](docs/backlog/windows-build.md) |
| Control surface (v0.2.1 landed; the rest is Plan V) | [`docs/backlog/control-surface.md`](docs/backlog/control-surface.md) |
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
