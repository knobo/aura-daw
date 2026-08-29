# Plan V — V2: progress and handoff

Live state for [`2026-08-28-plan-v2-players.md`](2026-08-28-plan-v2-players.md),
executed with the subagent loop on branch `feat/plan-v2-players` (draft PR #121,
cut from `origin/main` @ `6fe1112`).

**This file is rewritten after every review round, not at the end.** A session
on this branch has already been killed mid-task by a rate limit (`ede6d89`),
so everything a stranger needs is in git at all times:

| File | What it holds |
|---|---|
| [`…-plan-v2-players.md`](2026-08-28-plan-v2-players.md) | the plan, 15 tasks |
| this file | state, what is open, how to resume |
| [`…-LEDGER.md`](2026-08-28-plan-v2-players-LEDGER.md) | every `Ruling:` with its reasoning and cost-if-wrong |
| [`…-BRIEFS/`](2026-08-28-plan-v2-players-BRIEFS/) | the brief each implementer was given, fix rounds included |
| [`…-players-design.md`](../specs/2026-08-26-plan-v-players-design.md) | the design. **Binding where it and the plan disagree.** |

The working copies live in `.superpowers/sdd/2026-08-28-plan-v2-players/`,
which is **git-ignored**. The four committed files above are the copies that
survive; keep syncing them.

## State: 9 of 15 tasks, task 9 in fix round 4 of 5

| # | Task | State | Commits |
|---|---|---|---|
| 1 | The `Player` document type | done | `3c42e4e` |
| 2 | `Store::players` + project.json | done | `a508902` |
| 3 | Ops: PlayerAdd/Remove, ObjectRef::Player | done | `680b719`, `a375f42` |
| 4 | `MixNode` gains a Player producer | done | `f0bfef2` |
| 5 | Slot derivation over tracks + players | done | `eb00b42` |
| 6 | The clock table | done | `f4e171b`, `9a75eb3` |
| 7 | Mixer reads clocks; overlay deleted | done | `d188210`, `5c7ef0a` |
| 8 | Per-scene clocks; no transport hijack | done | `2115f73`, `2cf1597`, `d62ee5f` |
| 9 | Audio-clip players in the live graph | **fix round 4 in flight** | `d1f8cdd`, `a6403af`, `67ebe50`, `95a17ed`, `bf0cb58` |
| 10 | MIDI players with their own instrument | not started | |
| 11 | Trigger modes | not started | |
| 12 | Migrating launch bindings | not started | |
| 13 | Renderer: a pad is a player | not started | |
| 14 | The performance gate | not started | |
| 15 | Docs + release the claim | not started | |

Suite at `bf0cb58`: `cargo test --lib -- --test-threads=1` 1524 passed / 0
failed / 3 ignored. Clippy `--all-targets` unmoved for the whole branch
(quote the per-target split, not a total — the carried figure "130" has
accounting nobody has reproduced). Perf `--budget 525`: OK 377.1 µs cold.
`idle_players_block_cost` (release, `#[ignore]`d): 1.89 µs for 32 idle pads.

**Always use `--test-threads=1`.** The parallel run SIGSEGVs in
`midi_out::tests` — a pre-existing project crash,
[`docs/backlog/ci-hardening.md`](../../backlog/ci-hardening.md) item 5, not
this branch's.

## Pick up here

**If fix round 4 is still uncommitted**, its brief is
[`…-BRIEFS/task-9-fix-4-brief.md`](2026-08-28-plan-v2-players-BRIEFS/task-9-fix-4-brief.md):
seven items, no Critical. Two Important — the `!flushing` clip-read
suppression at `mixer.rs:909` is entirely untested (the reviewer deleted it
and 1524/0 still passed), and `render_live_into` is not suppressed during the
flush, which is unreachable today but goes live the moment task 10 gives a
player row a live node.

**If it is committed**, review it (opus — see the model policy below), then
run tasks 10–15 in order. Their briefs are pre-extracted in `…-BRIEFS/`.

## Model policy (owner's, 2026-08-29)

Do not use opus for everything.

| | Implementer | Review |
|---|---|---|
| Tasks 9, 10 | opus | opus |
| Tasks 11, 12 | sonnet | **opus** |
| Task 13 | sonnet | sonnet |
| Task 14 | sonnet | none — the controller interprets the number |
| Task 15 | sonnet | sonnet |

The split is by **what happens when it is wrong and nobody notices.** Opus
where a wrong answer is silent: the RT path, and the migration, where failure
looks like a pad that simply makes no sound. Sonnet where failure is loud or
bounded: chrome under ADR 0006, measurement, prose.

Review stays opus for anything under `src-tauri/src/audio/` or `midi/`
regardless of who implemented it, because the failure this branch keeps
repeating is a test that re-implements the logic it covers and passes while
production is broken.

## The two things that outlive this PR

### The perf gate cannot bisect in this area

Task 8's implementer saw ~240 µs on HEAD and ~390 µs on the branch in one
sitting, did not accept it, and ran a null control: adding a single **unused**
`AtomicU64` field to `ClockTable` on a clean tree also produces 400 µs. The
step is not work.

Across this branch the gate has read 411, 366, 224, 230, 299, 445 and 377 µs
against an `origin/main` baseline of 404 — including a round that *added* a
struct field and came in lowest. **Only the null control's step is
established.**

**It is also thermally sensitive.** Fix round 2 read 625 and 640 µs
immediately after the 110-second test suite, and 495/492/445 on the same tree
once cool. Measure cold, never straight after a suite.

Standing rule: a perf *failure* in the 400–500 µs band must be re-measured
with a null control before it is believed. Budget stays **525** (= 404.2 × 1.3).

### The idle-player cost, measured

32 idle pads, 512-frame block, release build: **134.64 µs → 1.98 µs** when the
early-out landed, **1.89 µs** at `bf0cb58`. About 1.3% of the block budget was
being spent producing silence. Shipped as an `#[ignore]`d test,
`idle_players_block_cost`, because the perf harness builds the *offline* graph
and V-15 excludes players from it.

## Standing decisions

- **The PR stays draft, and nothing is merged.** The owner's ear-check is owed
  and only they can do it: put a WAV on a pad with `raw` ticked, start the
  arrangement, hit the pad. It must sound bit-identical to auditioning that
  file in the browser, and the arrangement's playhead must not move.
- **Task 13 owes two carried items**: the singular `overlay` store in
  `src/lib/state/launch.svelte.ts` (with two scenes sounding it shows the last
  one fired), and binding stop-all to players in the UI.
- **Task 15 owes**: the two CI-hardening entries above (including the thermal
  one); a note that the six `launch.rs` tests share the process-global
  `LaunchRuntime` ledger and are correct only under `--test-threads=1`; the
  blocks-rendered counter deferred from task 8 (`flush_pending_for` proves a
  block *began*, not that a node *read*); and `flush_left` not surviving a
  graph rebuild while `LiveNodeRegistry` reuses the insert nodes.
- **Two pre-existing defects found but not fixed**, both for the final review:
  `TransportAction::Stop` releases slots in the same breath as the stop,
  dropping the flush frame; and `stop_drive_launch` lets a stale
  `FireCmd::Release` cut a scene that was re-fired after the release was
  queued.

## Process lessons, each paid for once

- An implementer that starts a long suite in the background and waits on a
  monitor task **deadlocks** and must be killed. Foreground only.
- A test that re-implements the logic it covers passes while production is
  broken. That is how one Important defect shipped in task 8, and how the
  `!flushing` gate reached round 4 untested.
- **A gate on the path every row takes needs a pin per row KIND, not per row
  state.** Fix round 3's brief specified a fader gate that opened every
  track's fader for `tail_frames` after each transport stop. The entire
  1518-test suite passed under it; only a purpose-written per-kind test
  caught it.
- Where a RED is impossible (a regression test for code already right),
  mutate the production line the test covers, show it failing, and revert.
  That is the standard on this branch now.
