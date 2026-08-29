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

## State: 9 of 15 tasks, task 9 fix round 4 committed

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
| 9 | Audio-clip players in the live graph | **fix round 4 done, round 5 (review) owed** | `d1f8cdd`, `a6403af`, `67ebe50`, `95a17ed`, `bf0cb58`, round 4 |
| 10 | MIDI players with their own instrument | not started | |
| 11 | Trigger modes | not started | |
| 12 | Migrating launch bindings | not started | |
| 13 | Renderer: a pad is a player | not started | |
| 14 | The performance gate | not started | |
| 15 | Docs + release the claim | not started | |

Suite after fix round 4: `cargo test --lib -- --test-threads=1` 1527 passed
/ 0 failed / 3 ignored (1524 at `bf0cb58`, plus round 4's three). Clippy
`--all-targets` unmoved for the whole branch — the per-target split, which is
the only figure anyone has reproduced, is lib **68**, lib test **122** (66
duplicates), `channel_properties` **1**, `journal_replay` **1**; identical on
the branch and on the base measured in the same sitting. (The carried total
"130" has accounting nobody has reproduced. Ignore it.)

**Both perf numbers need their null control in the same sitting, and round 4
is the proof.** Perf `--budget 525`: OK **434.0** µs on the branch against
**422.6** µs on the base, same sitting, cold, harness spread 5–7% — the
carried 377.1 µs was another sitting and comparing against it would have read
as a 15% regression. `idle_players_block_cost` (release, `#[ignore]`d):
**2.27** µs on the branch against **2.22** µs on the base, same sitting —
where the carried figure is 1.89 µs. Unmoved in both cases; the absolute
numbers are not portable between sittings.

**Always use `--test-threads=1`.** The parallel run SIGSEGVs in
`midi_out::tests` — a pre-existing project crash,
[`docs/backlog/ci-hardening.md`](../../backlog/ci-hardening.md) item 5, not
this branch's.

## Pick up here

**Fix round 4 is committed.** All seven items landed: the `!flushing` clip
read is pinned by a conservation test that stops the pad MID-clip;
`render_live_into` now gates only its EVENT QUEUE on the flush and keeps
calling `node.process`, pinned both ways by
`a_flushing_row_does_not_re_queue_the_events_under_its_frozen_playhead`;
`tail_frames` maxes `out_pdc` against the send edges instead of summing them
(they are parallel branches off one tap point) and its `pdc` term is pinned;
`mixer::render_impl` now `debug_assert`s each row's `tail_frames` against a
recompute, which caught two real test rigs immediately; and `flush_left` not
surviving a graph rebuild is written up in `docs/TRAPS.md` rather than fixed.

Next: review it (opus — see the model policy below), then run tasks 10–15 in
order. Their briefs are pre-extracted in `…-BRIEFS/`. Task 10 is the one that
makes round 4's item 2 reachable: it gives a player row a live node, which
`mix_nodes` cannot build today (`_ => Vec::new()`).

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
  block *began*, not that a node *read*). `flush_left` not surviving a graph
  rebuild while `LiveNodeRegistry` reuses the insert nodes is now written up
  in [`docs/TRAPS.md`](../../TRAPS.md) §Audio engine and needs no further
  handling here.
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
- **A conservation test over ONE press cannot see a flush-time source
  read.** `tail_frames` IS the row's whole path latency, so anything that
  enters the strip at the start of the flush leaves exactly as the window
  closes — a re-read fragment is stranded for the rest of that press and
  surfaces only at the onset of the NEXT one. Fix round 4's brief asked for
  a single-press energy assertion; it would have passed against the deleted
  gate. Two presses is the shortest thing that bites.
