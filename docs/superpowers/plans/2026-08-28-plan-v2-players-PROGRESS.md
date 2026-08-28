# Plan V — V2: progress log

Live progress for [`2026-08-28-plan-v2-players.md`](2026-08-28-plan-v2-players.md),
executed with the subagent loop on branch `feat/plan-v2-players` (PR #121).

**If you are picking this up cold:** read the plan, then this file's table.
A task marked `done` is committed — check `git log --oneline` against the
commit range in its row. The controller's full ledger (pre-flight scan,
rulings, review findings) lives in
`.superpowers/sdd/2026-08-28-plan-v2-players/progress.md`, which is
git-ignored and therefore does NOT survive a fresh clone — this file is
what does.

| # | Task | State | Commits | Note |
|---|---|---|---|---|
| 1 | The `Player` document type | **done** | `3c42e4e` | 4/4 tests; review clean |
| 2 | `Store::players` + project.json | **done** | `a508902` | schemaVersion unmoved; review clean |
| 3 | Ops: PlayerAdd/Remove, ObjectRef::Player | **done** | `680b719`, `a375f42` | +SessionSnapshot::players; 1 fix round |
| 4 | `MixNode` gains a Player producer | **done** | `f0bfef2` | reviewed with task 5 |
| 5 | Slot derivation over tracks + players | **done** | `eb00b42` | V-6 agreement verified both halves |
| 6 | The clock table | **done** | `f4e171b`, `9a75eb3` | begin_block() latch; 1 fix round |
| 7 | Mixer reads clocks; overlay deleted | pending | | behaviour-neutral swap; perf gate |
| 8 | Per-scene clocks; no transport hijack | pending | | |
| 9 | Audio-clip players in the live graph | pending | | |
| 10 | MIDI players with their own instrument | pending | | |
| 11 | Trigger modes | pending | | |
| 12 | Migrating launch bindings | pending | | |
| 13 | Renderer: a pad is a player | pending | | |
| 14 | The performance gate | pending | | baseline vs branch, same sitting |
| 15 | Docs + release the claim | pending | | |

## Baselines

- `scripts/perf-check.sh --measure` on `origin/main` @ 6fe1112: **404.2 us** (bare, 32 tracks, best of 3, spread 4.2%). Branch budget 525.
- Same on the branch: _not yet run_
