# Task 9 — fix round 3 of 5

Worktree `/home/knobo/prog/dav/.worktrees/plan-v2-players`, branch
`feat/plan-v2-players`, HEAD `95a17ed`, clean. Read the round-2 brief
(`task-9-fix-2-brief.md`) for what has just landed and why.

## Rules that bind you

- `cargo test --lib -- --test-threads=1`. NEVER without `--test-threads=1`.
- Every suite in the FOREGROUND. Never background-and-wait; agents have
  deadlocked on that here.
- A test that re-implements the logic it covers instead of CALLING it passes
  while production is broken. Every test below must reach the real
  `mixer::render` or the real `compile_routing`.
- TDD: RED first. Prove the RED by running it, not by reasoning about it.
- Perf: `scripts/perf-check.sh --budget 525`. Do not interpret the absolute
  number, and **do not measure straight after the test suite** — the gate is
  thermally sensitive; round 2 read 625/640 hot and 445 cold on the same tree.
  A failure needs a null control before you believe it.
- Do not merge, do not push, do not touch the PR's draft state.

## Item 1 (Important) — the bounded flush window

The reviewer found that the `exclusive_idle` early-out strands the INSERT
chain, and that round 2's rewritten comment now asserts it does not.

Failure scenario, verified reachable: a non-raw pad with one linear-phase EQ
(2048-sample applied latency) and an 800-sample one-shot. `ClockTable::advance`
turns the clock off at the clip end, so from block 2 the row is
`exclusive_idle` and skipped — while all 800 real samples are still inside the
plugin's pipeline. **The whole press is inaudible**, and those samples surface
at the onset of the NEXT press. That is exactly the truncated-tail plus
contaminated-onset pair round 2 ruled on for `DelayLine`, one stage upstream.

**Controller ruling: fix it, do not defer, and generalise round 2's fix rather
than bolting a third case onto it.** This is not pre-existing on `main` — the
early-out landed in fix round 1 on this branch, and before it the strip ran
every block, so the inserts did flush. A regression of ours that silently
swallows an entire press is not deferrable, and enumerating hazards one at a
time is what has now missed one twice.

The model to implement: **a pad that has stopped being triggered is still
sounding until its tail is out.** So:

1. At graph build (`engine.rs` around :2005-2030, where `pdc`/`out_pdc` are
   already sized from the `RoutingPlan`), compute a per-row `tail_frames`:
   the row's applied insert-chain latency, plus its `out_delay`, plus the
   largest `delay` over its send edges. Store it on `RtTrack` next to
   `pdc`/`out_pdc`. For a raw player this is 0, by construction — V-6 gives it
   no inserts, no sends, and `node.rs:117` forces `output: None`, so no
   `out_pdc`. Every other row that owns no tail also gets 0.
2. On `RtTrack`, a live counter (`flush_left`, frames). While the clock is ON,
   it is held at `tail_frames`. While the clock is off it counts down by the
   window length.
3. `if exclusive_idle && flush_left == 0 { continue; }` — the bare skip, which
   is what every raw pad and the measured 1.98 µs idle case takes, unchanged.
4. Otherwise run the ORDINARY strip for this row, with two differences: the
   clip read is skipped (the buffer stays zeroed — an off clock does not
   advance, so reading clips would re-read the head of the sample forever,
   which is fix round 1's defect), and the fader's `on` is
   `flush_left > 0 && audible_with_launch(...)` rather than `clock_on && ...`.
   Mute and solo still apply; a muted pad's tail stays muted.

`flush_idle_tail` from round 2 is then **deleted** — the ordinary strip does
its job and more, and two paths that must stay in step is how this defect got
in. Keep round 2's two tests; they must still pass against the general path.

Consequence to state in the comment, not to fix: an insert with an unbounded
tail (a reverb) is hard-cut when `flush_left` reaches 0. That is consistent
with what a transport stop already does to a track's inserts, and "when does a
pad stop ringing" is a separate question this ruling does not answer.

Rewrite the early-out comment at `mixer.rs:826-851`. It must stop enumerating
hazards and state the rule instead: *nothing on this row is skipped until the
row's whole tail has left it.*

Tests, all through the real `mixer::render`:
- the reviewer's scenario: a non-raw pad with a latency chain longer than its
  clip. Assert the pressed samples ARRIVE at the output, and that a second
  press's first block is not the first press's audio. This is the RED — run it
  against `95a17ed` first and show it failing.
- a raw pad still takes the bare skip: assert `tail_frames == 0` for it.

## Item 2 (Important) — the `out_pdc` drain has no test through `mixer::render`

Round 2 created the first player row that can carry an `out_pdc` line (item 1
of that round) and the code that must drain it, but proved them separately:
`compile_routing` proves the number, `mixer::render` proves the SEND drain,
and nothing proves the DRY drain. On a branch that has already shipped a
defect through a test that did not reach production, inspection is the wrong
standard for a newly enabled RT branch.

Add a test shaped like
`an_idle_pad_still_flushes_its_compensated_send_tail_into_the_bus`, with
`pad.out_pdc = Some(DelayLine::new(32, 64, 2))`, `pad.output = Some(0)`, no
send, unmuted: total energy reaching the master equals the uncompensated
control, and a second press's first block equals the first's.

## Item 3 (Minor) — the guard must ask about the row, not enumerate lines

`mixer.rs:853` (as it stands) tests `out_pdc` and send delays by name and
misses `tr.pdc`. Harmless today — `track_pdc` is forced to 0 for players and
only player slots reach the early-out — but naming lines instead of asking
"does this row still hold anything" is the shape of the bug in item 1. Item
1's `tail_frames` should therefore also include `tr.pdc`'s delay, and the
strip's source-alignment line must be advanced during the flush like every
other stage.

## Item 4 (Minor) — V-17 (b) overstates when the lines exist

`docs/superpowers/plans/2026-08-28-plan-v2-players.md:50` reads
"…`out_pdc` whenever the pad is routed into a bus". Both lines are built only
when the delay is non-zero (`bus.rs:94`, `engine.rs:2023`), so a pad whose
send into a bus needs no compensation carries no line and correctly takes the
bare skip. Change to "…whenever that compensation is non-zero", and make the
clause consistent with item 1's rule while you are in there.

## Gates before you report

1. `cargo test --lib -- --test-threads=1` — 1518 passed at `95a17ed`; expect
   that plus your new tests, 0 failed.
2. `cargo clippy --all-targets` — 130 warnings, unmoved for the whole branch.
3. `scripts/perf-check.sh --budget 525`, measured cold.
4. `cargo test --lib -- --ignored idle_players_block_cost --test-threads=1`
   in `--release`: round 1 measured 32 idle pads at 1.98 µs minimum. Item 1
   must not move it — raw pads take the same bare skip. Quote the number.
5. Commit.

## Report back

Per item: what changed, the RED-then-GREEN evidence, and the gate numbers
including the idle-pad µs. Say plainly anything the brief got wrong.
