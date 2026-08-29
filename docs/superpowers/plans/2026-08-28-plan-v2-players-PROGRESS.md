# Plan V — V2: progress and handoff

Live state for [`2026-08-28-plan-v2-players.md`](2026-08-28-plan-v2-players.md),
executed with the subagent loop on branch `feat/plan-v2-players` (draft PR #121,
cut from `origin/main` @ `6fe1112`).

**Handoff written 2026-08-29.** Everything a successor needs is in this file
and its two neighbours: the plan, and
[`…-LEDGER.md`](2026-08-28-plan-v2-players-LEDGER.md) — the controller's
full ledger, copied out of the git-ignored workspace so it survives a fresh
clone. The ledger holds all 33 `Ruling:` lines with their reasoning and their
cost-if-wrong; this file holds the summary and what to do next.

## State: 8 of 15 tasks complete, task 9 in its second fix round

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
| 9 | Audio-clip players in the live graph | **open — 2 findings** | `d1f8cdd`, `a6403af`, `67ebe50` |
| 10 | MIDI players with their own instrument | not started | |
| 11 | Trigger modes | not started | |
| 12 | Migrating launch bindings | not started | |
| 13 | Renderer: a pad is a player | not started | |
| 14 | The performance gate | not started | |
| 15 | Docs + release the claim | not started | |

Suite at `67ebe50`: `cargo test --lib -- --test-threads=1` 1513 passed / 0
failed / 3 ignored; all 13 `--tests` targets green; `npm test` 1383 passed;
clippy `--all-targets` 130, unmoved for the whole branch.

**Always use `--test-threads=1`.** The parallel run SIGSEGVs in
`midi_out::tests` — a pre-existing project crash,
[`docs/backlog/ci-hardening.md`](../../backlog/ci-hardening.md) item 5, not
this branch's.

## Pick up here: task 9, fix round 2 of 5

Nothing is uncommitted; the round was interrupted before it changed a file.
Two findings are open, both from the round-1 re-review.

**A. Important — the exclusive-idle early-out strands a player's send-edge
delay line.** `mixer.rs:797`'s `continue` skips `tap_into_bus`, and
`d.process(copy)` inside it (`mixer.rs:299`) is the only thing that advances
`RtSend::delay`. `DelayLine` has no reset. So the last `delay` samples of
every press stay in the line and never reach the bus, and the next press
replays them into the bus at its onset — the previous hit's tail on top of
the new one. This is the compensated-send path V-17 exists to protect.
The early-out's safety comment at `mixer.rs:790-795` reasons about `pdc` and
`out_pdc` (both correctly `None` under V-17) and misses the one line V-17
deliberately keeps alive.

Fix: drain zeros through any `snd.delay` before `continue`ing. The `Some`
case is rare, so an idle pad with no compensated send keeps the measured
1.98 µs. Correct the comment. Add a test that a second press does not carry
the first press's tail into its bus.

**B. V-17 is refined and the code must follow.** `out_delay` is currently
zeroed for players unconditionally (`bus.rs:346-350`), including when
`node.output` is a bus. That leaves a pad's *dry* arriving early into a group
bus while its *send* into the same bus arrives on time — the pad smears
against itself and V-17's two halves contradict each other.

The rule: **routed into a bus, a player is compensated to that bus's `t_in`,
exactly as its send edge is. Routed to master, it is not.** Inside a bus
everything must arrive together or the mix smears; to master — the default,
and the path the owner's ear-check uses — "the pad answers the press"
governs, which is the whole point of V-17. Update V-17's text in the plan to
state both halves and why they differ, including that a pad routed into a bus
containing a linear-phase-EQ'd track inherits that latency.

**Three doc minors** in the same round: `mixer.rs:442` points at
`render_windowed`, which does not exist (it is `render_impl`, `mixer.rs:664`);
`engine.rs:5810`'s run command omits `--release` while quoting release
numbers; `docs/backlog/plan-v-players.md` says a pad "can only be stopped
from the backend" when Escape already reaches it via `stopAllSound` →
`launchStop` → `launch_stop` → `stop_launch_overlay`.

**One thing to document rather than fix:** a player whose applied chain
latency exceeds `t_src` gets `t_in[dest].saturating_sub(ready) == 0`, so its
send is late by the excess and no bus waits for it — self-consistent with the
unpadded dry path, but unstated and untested.

## The two things that outlive this PR

### The perf gate cannot bisect in this area

Task 8's implementer saw the gate report ~240 µs on HEAD and ~390 µs on the
branch in one sitting, did not accept it, and ran a null control: adding a
single **unused** `AtomicU64` field to `ClockTable` on a clean tree also
produces 400 µs. The step is not work.

Across this branch the gate has read 411, 366, 224, 230 and 299 µs against an
`origin/main` baseline of 404 — including a round that *added* a struct field
and came in lowest, which is evidence against the "codegen layout" hypothesis
the first report offered. **Only the null control's 160 µs step is
established.** Task 15 files this in
[`docs/backlog/ci-hardening.md`](../../backlog/ci-hardening.md) and
[`docs/TRAPS.md`](../../TRAPS.md) stating exactly that and no more.

Standing rule meanwhile: a perf *failure* in the 400–500 µs band must be
re-measured with a null control before it is believed as a regression.
Budget for the remaining tasks stays **525** (= 404.2 × 1.3).

### The idle-player cost, measured

32 idle pads, 512-frame block, release build: **134.64 µs → 1.98 µs**
minimum (median 142.01 → 2.03). About 1.3% of the block budget was being
spent producing silence, because `on = false` was consumed only inside
`apply_fader_into` — after the strip had already run. Shipped as an
`#[ignore]`d test, `idle_players_block_cost`, because the perf harness builds
the *offline* graph and V-15 excludes players from it.

## Standing decisions for whoever continues

- **The PR stays draft, and nothing is merged.** The owner's ear-check is
  owed and only they can do it: put a WAV on a pad with `raw` ticked, start
  the arrangement, hit the pad. It must sound bit-identical to auditioning
  that file in the browser, and the arrangement's playhead must not move.
- **Task 13 owes two carried items**: the singular `overlay` store in
  `src/lib/state/launch.svelte.ts` (with two scenes sounding it shows the
  last one fired), and binding stop-all to players in the UI.
- **Task 15 owes**: the CI-hardening entries above; a note that the six
  `launch.rs` tests share the process-global `LaunchRuntime` ledger and are
  correct only under `--test-threads=1`; and the blocks-rendered counter
  deferred from task 8 (`flush_pending_for` proves a block *began*, not that
  a node *read*).
- **Two pre-existing defects found but not fixed**, both recorded for the
  final review: `TransportAction::Stop` releases slots in the same breath as
  the stop, dropping the flush frame; and `stop_drive_launch` lets a stale
  `FireCmd::Release` cut a scene that was re-fired after the release was
  queued.

## How to resume the loop

The controller ran `superpowers:subagent-driven-development`: one fresh
implementer per task, a task review after each, fix rounds capped at five,
then a whole-branch review. Briefs for tasks 9–15 are pre-extracted in the
git-ignored workspace at `.superpowers/sdd/2026-08-28-plan-v2-players/`; if
that is gone, regenerate one with
`scripts/task-brief docs/superpowers/plans/2026-08-28-plan-v2-players.md <N>`
from the skill's directory.

Two process lessons worth carrying, both paid for once already: an
implementer that starts a long suite in the background and waits on a monitor
task **deadlocks** and must be killed — run suites in the foreground; and a
test that re-implements the logic it covers rather than calling it will pass
while production is broken, which is exactly how one Important defect shipped
in task 8.
