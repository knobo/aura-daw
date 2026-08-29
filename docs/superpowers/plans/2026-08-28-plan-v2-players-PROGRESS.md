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

## State: 9 of 15 done; task 10 in fix round 3, MID-FLIGHT

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
| 9 | Audio-clip players in the live graph | done (4 fix rounds) | `d1f8cdd`, `a6403af`, `67ebe50`, `95a17ed`, `bf0cb58`, `2542640` |
| 10 | MIDI players with their own instrument | **fix round 3 dispatched, session paused mid-round** | `f7ab47e`, `3da1437`, `9c1c9cc` |
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

**Read this whole section before you touch anything.** The session was
paused by a usage limit at 18:12 CEST on 2026-08-29, in the middle of task
10's fix round 3. The ledger
(`.superpowers/sdd/…/progress.md`, git-ignored but present in the worktree)
has every ruling with its reasoning; this section has what you need to
restart.

### First: find out whether round 3's work survived

```sh
git log --oneline -3
git status --short
```

Round 3's implementer was still writing when the session paused, with
**uncommitted edits to `src-tauri/src/audio/{engine,mixer,offline,rt}.rs`
and `src-tauri/src/control/loopjam.rs`**. Three cases:

- **HEAD is past `9c1c9cc` and the tree is clean.** The implementer
  committed before it died. Go to "then: re-review round 3".
- **HEAD is `9c1c9cc` and the tree is dirty.** Its work is half-written and
  unverified. This branch has been here before (`d1f8cdd`): the controller
  committed the orphaned work labelled UNVERIFIED in the commit message,
  because uncommitted work is one `git clean` from gone, and custody is not
  implementation. Do the same, then have a FRESH implementer read the diff
  critically against the brief before trusting a line of it — its job starts
  with reading, not with assuming the work is right.
- **HEAD is `9c1c9cc` and the tree is clean.** Nothing survived.
  Re-dispatch round 3 from its brief.

Round 3's brief is committed at
[`…-BRIEFS/task-10-fix-3-brief.md`](2026-08-28-plan-v2-players-BRIEFS/task-10-fix-3-brief.md).
It existed only inside a message to a subagent until the pause; it is in git
now precisely so this handoff works.

### What round 3 is, in one paragraph

**The controller's own round-2 ruling was wrong, and round 3 reverses it.**
It ordered `computed_tail_frames` to be `strip.max(live)`, reasoning that
the instrument's release runs *before* the inserts that the other terms
already cover. Running before, in series, is exactly why they **add**. With
a 2048-frame insert chain the release finishes inside the node at 3840
frames but its last material needs 2048 more to leave the chain — it is
still in the pipeline when the window closes at 4096. Stranded tail,
contaminated next onset: the fourth instance of the pair this branch keeps
producing, and the pair the round was opened to fix. The expression becomes
`strip + live`, the allowance becomes rate-true (store the rate on
`RtGraph`; the `debug_assert` must read `graph.rate`, never `render_impl`'s
own `sample_rate` argument, or a device rate change fires it on the RT
thread), and the RED is a two-press test **with an insert chain**.

### Then: re-review round 3, then tasks 11–15

Re-review is scoped to round 3's diff, on **opus** — everything under
`src-tauri/src/audio/` or `midi/` gets an opus review regardless of who
implemented it. After task 10 closes, tasks 11–15 run in order; their briefs
are pre-extracted in `…-BRIEFS/`. **Sonnet implements from task 11 on**, per
the owner's model policy below.

### Task 10's state, so you do not re-litigate it

Committed and reviewed clean: `f7ab47e` (the task), `3da1437` (fix round 1),
`9c1c9cc` (fix round 2). The round-2 re-review verdicted every finding
ADDRESSED with no new Critical/Important, and traced by hand that a MIDI
*track* now carrying `tail_frames == 4096` is genuinely inert — `tail_frames`
has exactly two production reads, and for a track row the second is a dead
store, because every read of `flush_left` is gated on `flushing =
exclusive_idle`, which needs a clock property only player slots have.

Gates were green at `f7ab47e`: lib 1535/0/3, `--tests` all green, npm 1383,
clippy per-target identical to base in the same sitting, perf 262 µs against
a 525 budget. **They have not been run since**, so round 2 and round 3 are
both ungated. The gate-runner brief is in the ledger; run it on **sonnet**,
and see the warning below.

### Two things carried out of task 10

- **Two pads on one plugin instance instantiate two plugin instances.**
  Parked as correct-as-built: the mixer processes one live node per ROW,
  once per block, so two rows cannot share a node without double-advancing
  it. Sharing would mean the pads become one row — a deck concept, V3+. Not
  a regression; two tracks sharing an `instrument_id` already do this. Goes
  to the design doc as a known limit and to the final review.
- **A MIDI pad's release is hard-cut at the allowance.** V-17(b)'s accepted
  consequence, and an ordinary MIDI track already sounds the same at
  transport stop. V3 item.

### The gate-runner deadlocks if you let it sleep

The first gate-runner ended its turn "waiting silently for the background
90-second idle sleep and the Monitor task" and reported nothing after 457
seconds — the documented background/monitor deadlock, hit despite a brief
that said FOREGROUND ONLY. **Forbid the Monitor/wait task by name, and do
not ask for a shell cool-down at all**: ask for three perf runs and the
spread instead. A spread across three runs is better evidence than one
number behind a guessed idle interval, and it removes the only thing the
agent is tempted to background.

Its orphaned `perf-check.sh` processes then contaminated the next run's base
side (three of them, concurrent). Check `ps` for stray `perf-check`
processes before believing any perf pair.

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

**Gate running splits off the implementer.** Through round 4 the opus
implementer ran all four gates itself — sitting through a 110-second suite, a
clippy run and a thermal cool-down, with every line of that output landing in
opus context. From now on: the implementer iterates to green and **commits**;
a **sonnet gate-runner** then runs the four gates against the committed tree
and reports only the numbers, base and branch in the same sitting. It may not
conclude — a perf failure is reported and stopped on, and the null control and
the judgement stay with the controller.

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
- **A conservation test on one press cannot catch a stranded fragment.**
  `tail_frames` is the row's whole path latency, so anything entering the
  strip as the flush opens leaves exactly as it closes; a wrongly re-read
  fragment is stranded for the rest of that press and surfaces only at the
  **next** press's onset. Round 4's brief asked for a one-press assertion and
  it passed against the deliberately broken gate. Two presses, or no bite.
- **A conservation test over ONE press cannot see a flush-time source
  read.** `tail_frames` IS the row's whole path latency, so anything that
  enters the strip at the start of the flush leaves exactly as the window
  closes — a re-read fragment is stranded for the rest of that press and
  surfaces only at the onset of the NEXT one. Fix round 4's brief asked for
  a single-press energy assertion; it would have passed against the deleted
  gate. Two presses is the shortest thing that bites.
