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
| 7 | Mixer reads clocks; overlay deleted | **done** | `d188210`, `5c7ef0a` | 1448 tests green; 1 fix round (3 flush-frame deltas) |
| 8 | Per-scene clocks; no transport hijack | fix round 1 | `2115f73` | transport hijack dead; +rebuild reconciliation |
| 9 | Audio-clip players in the live graph | pending | | |
| 10 | MIDI players with their own instrument | pending | | |
| 11 | Trigger modes | pending | | |
| 12 | Migrating launch bindings | pending | | |
| 13 | Renderer: a pad is a player | pending | | |
| 14 | The performance gate | pending | | baseline vs branch, same sitting |
| 15 | Docs + release the claim | pending | | |

## Baselines

- `scripts/perf-check.sh --measure` on `origin/main` @ 6fe1112: **404.2 us** (bare, 32 tracks, best of 3, spread 4.2%). Branch budget 525.
- Same on the branch @ 5c7ef0a: **231.8 us** (budget 525, spread 1.7%). No regression — both numbers sit inside the documented 260-408 us same-code spread for this machine.

## Overnight run, 2026-08-28

The owner delegated tasks 9-15 and went to bed. Decisions taken on their
behalf are all in the SDD ledger under `Ruling:`; two matter before anyone
touches this branch:

- **The PR stays draft.** The owner ear-check is owed and only they can do
  it: put a WAV on a pad with `raw` ticked, start the arrangement, hit the
  pad. It must sound bit-identical to auditioning that file in the browser,
  and the arrangement's playhead must not move.
- **Nothing is merged.**

### The measurement finding, which outlives this PR

Task 8's implementer saw the perf gate report ~240 us on HEAD and ~390 us on
the branch in one sitting, did not accept it, and ran a null control: adding
a single **unused** `AtomicU64` field to `ClockTable` on a clean tree also
produces 400 us. So the step is not work — it is layout.

What that licenses: the branch's number is not attributable to this change,
and the reviewer separately confirmed the diff adds no per-block work (the
harness project has no launch bindings, so its clock count is 1, *down* from
2 — the branch does strictly less per block in the exact configuration the
gate measures).

What it does NOT license: the stated mechanism, "bimodal on codegen layout
near `audio::clock`". That is one binary per variant with no replication.

The consequence is bigger than this PR: **a `git bisect run` driven by
`scripts/perf-check.sh` will lie in this area.** Task 15 files it in
`docs/backlog/ci-hardening.md` and `docs/TRAPS.md`, stating what was shown
rather than the hypothesis. Standing rule from here: a perf failure in the
400-500 us band must be re-measured with a null control before it is
believed as a regression.

