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

## State: 11 of 15 done; task 12 dispatched

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
| 10 | MIDI players with their own instrument | done (4 fix rounds) | `f7ab47e`, `3da1437`, `9c1c9cc`, `2439ff5`, `5963e47`, `49b6b4b` |
| 11 | Trigger modes | done (1 fix round) | `0837972`, `7ee1abd`, `09c72b2` |
| 12 | Migrating launch bindings | dispatched (sonnet) | |
| 13 | Renderer: a pad is a player | not started | |
| 14 | The performance gate | not started | |
| 15 | Docs + release the claim | not started | |

Gates green at `49b6b4b`, with rounds 2, 3 and 4 of task 10 gated together
(they had been ungated since `f7ab47e`): lib **1542** passed / 0 failed / 3
ignored, `--tests` every target 0 failed, npm **1383** in 126 files.

**Clippy's baseline must be the MERGE-BASE, not a mid-branch commit.**
Measured against `6fe1112` the branch was **+1** on the lib target — 68
against 67 — and the carried "identical 68/68" had been measured against
`3dabad0`, a commit on this branch, which smuggled the branch's own drift
into the baseline. The one addition was real: `derivable_impls` on
`PlayerSource`'s manual `Default`, added by task 4, fixed in `49b6b4b`. The
per-target split is now lib **67**, lib test **121**, `channel_properties`
**1**, `journal_replay` **1**, identical to the merge-base.

**Perf: only the direction is readable.** `--budget 525`: branch 273.5 /
278.7 / 296.7 µs (spread 8.5%) against base `6fe1112` at 503.2 / 499.4 /
502.0 µs (spread 0.8%), same sitting, perf before the suites. The branch
reads 45% *faster* than the base. Passed without a re-measure — a
regression cannot hide under a base that measured slower — but note that
503 sits above the 224–445 band this gate has produced for unchanged code
all branch long. That is the fourth distinct way this gate has surprised
someone here.

**`idle_players_block_cost` has no base side, and a brief that asks for one
is wrong.** The test is one this branch ADDED, so at the merge-base it does
not exist. Branch min **2.01** µs — comparable only against this branch's
own earlier sittings (1.89 at `bf0cb58`, 2.27 at round 4's, 3.26 at task
10's), i.e. unmoved within a spread that does not travel between sittings.

**`--test-threads=1` is NOT obsolete — that claim, made here earlier today, was wrong.** See `docs/TRAPS.md`: #123 removed the SIGSEGV, but `midi::launch::tests`'s process-global `LaunchRuntime` singleton flakes 5 runs in 6 at default parallelism. The paragraph below is kept for its measurements, which stand.

**What #123 did fix, as of the `origin/main` merge.** The
parallel SIGSEGV was diagnosed and fixed in #123 (a libtest binary has no
`AURA_SCAN_WORKER` guard, so the scan worker's child ran the whole suite
again and pinned pipewire at its fd limit). Verified on this branch,
2026-08-30, three consecutive parallel runs: 1568 passed, 0 failed, in
11.5–12.4 s against 112.9 s single-threaded. **Every brief in `…-BRIEFS/`
still carries the old rule; it is stale, not wrong to have followed.**

## Pick up here

**Task 11 is closed** — `0837972`, `7ee1abd`, `09c72b2`. Task 12 is
dispatched to a sonnet implementer; its review is owed on **opus**.

**Task 11 changed almost no production code, and that was correct.**
`player_fire` already read `p.trigger.mode` off the live session every
press, and `ClockTable::advance` already ended a non-looping clock at its
own `end`. What the task actually owed was assertions — and one thing
nobody had noticed.

**The seam that would have shipped missing.** There is no generic `Op::Set`
pipeline from the frontend: every mutation crosses via a bespoke
`#[tauri::command]`, and the player commands were `players_get` /
`player_add` / `player_remove` / `player_fire` / `player_stop` only. Task
13's brief only *reads* `trigger.mode`; tasks 12, 14 and 15 never mention
it. **V2 would have shipped with Loop and Gate unreachable by any user**,
every player stuck at the `OneShot` default, and nothing in the plan would
have caught it. `player_set_trigger_mode` now exists (`lib.rs:331`). The
pad inspector that exposes it is V-12's and stays task 13's.

**`Gate` and `OneShot` are byte-identical in the engine, by design.** The
design's gate — "sounds while held; release cuts it" — is entirely about
who calls `player_stop`, a pointerup the engine cannot see. Production says
so now, at `player_fire`'s `== Loop`. Task 13 owes the pointerup binding.

### Task 11's failure mode was the opposite of this branch's usual one

Not a test that re-implements its logic — none did — but **under-assertion**:
a test that passes against a mutation breaking the very thing it is named
for. Three of them, each proved by mutation rather than by reading. And it
**survived the fix round convened to remove it**: `retriggering_one_player_leaves_another_sounding`
checked `b` with `is_on` only, so a press that rewinds *every* player — the
single-overlay behaviour V-4 deleted — left all 126 `control::tests` green.
An on/off flag cannot see it; only a playhead can.

**A symmetric serialize/deserialize test pins nothing about the wire
contract with a non-Rust caller.** `#[serde(rename = "GATE_XX")]` on
`TriggerMode::Gate` left every Rust test green; only task 13's TS caller
would have found it, at runtime. `"gate"` and `"loop"` are now pinned
explicitly.

### How task 10 actually ended, so you do not re-open it

Round 3 was inherited as two UNVERIFIED custody commits (`4c15651`, and
`19d8c44` for 37 lines still uncommitted on top of it) from an implementer
killed by a usage limit. A fresh implementer read them critically against
the round-3 brief and found all three items genuinely present and finished
— it deleted nothing as wrong, fixed four defects in the doc/comment layer,
and observed three REDs itself rather than inheriting the ghost's claims.
The opus re-review then reproduced every one of those mutation claims
byte-for-byte and added one the implementer had not run: `LIVE_TAIL_FRAMES_AT_48K`
4096 → 2048 fails two `mixer.rs` tests, so the allowance's *magnitude* is
pinned behaviourally and not only by arithmetic.

Round 4 closed the re-review's one Important — nothing pinned the single
argument carrying the rate into `with_buses`, and replacing
`self.cache_rate` at `engine.rs:2531` with a literal left 1058 tests green.
It fails silently by construction, because `render_impl`'s `debug_assert`
reads `graph.rate` and would agree with the bug. Round 4 was test-only: 153
insertions, zero deletions, no production line touched.

### Two things task 10 leaves for later

- **`loopjam.rs:913` is correct for a reason that could stop being true.**
  Its row is `RtTrack::clips(...)` with no live node, so the rate reaches
  only `graph.rate` and nothing on that path consumes it; a test there
  could assert code *shape* and nothing else, which is the kind of test
  this branch keeps being burned by. If that row ever gains a live node the
  site goes silently wrong with no test to say so. Task 15 and the final
  review.
- **`live_tail_frames(0)` is a debug-only guard.** `recompute_tail_frames`
  is `pub`, and in a release build a 0 rate returns a 0 flush window — the
  original defect. Only reachable from test code today. Task 15.

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
