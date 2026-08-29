# Task 10 — fix round 3 of 5

Dispatched 2026-08-29 ~18:00 CEST by resuming the round-1/2 implementer
(opus). **This brief existed only inside a SendMessage until now**; it is
committed here so the round survives a session pause.

The round-2 re-review came back with every finding ADDRESSED and no new
Critical/Important — and one Minor that is not Minor. **The controller's
round-2 ruling was wrong and this round reverses it.**

The controller ordered `strip.max(live)` with the reason "the allowance
covers the instrument's release, which runs *before* the inserts and the
delay lines that the other terms already account for". Running before, in
series, is precisely why they **add**. The reviewer spotted the
mischaracterisation but graded it a comment defect, because the controller
had ordered the expression and V-17(b) accepts a bounded hard cut. That
grading is wrong, and the fault is the controller's for pre-committing to
the arithmetic.

The consequence is not a comment. With a 2048-frame insert chain: the
release finishes inside the node at 3840 frames, but the last of that
material still needs 2048 more frames to traverse the chain — it is still
in the pipeline when the window closes at 4096. Stranded in the insert
chain, truncated tail, contaminated next onset. That is the **fourth**
instance of the exact pair this branch keeps producing, and it is the pair
this round was opened to fix. Task 9's round 4 already established the
reasoning that was contradicted: `tail_frames` is the row's whole path
latency, so a fragment *entering* the strip at flush start leaves exactly
as the window closes — but release material keeps entering for the whole
release, so the last of it enters at 3840 and needs `strip` more frames to
get out.

## Item 1 — the expression becomes `strip + live`

Series, not parallel. Rewrite the justification at the constant and at the
expression to say why: the release happens inside the node, its output then
traverses the inserts, the `pdc` and the longer of the parallel output
branches.

`the_live_node_allowance_is_a_floor_not_an_addition` is now wrong in its
name and its assertion — it is the test ordered with the bad ruling.
Retarget it into a sum test: a live-node row with an 8192-frame chain gets
`8192 + allowance`, and a chainless live row gets exactly the allowance.
Keep both directions pinned. Say in the report what it was renamed to.

**The RED for this item is a two-press test WITH an insert chain on the
row** — the existing `a_cut_pads_frozen_release_does_not_land_on_the_next_press`
shape, plus a chain long enough that `max` truncates. Show it failing
against `9c1c9cc`, then fix. That is the test that would have caught the
bad ruling, and it is the one that has to exist.

## Item 2 — make the allowance rate-true

The controller deferred this in round 2 because opening the funnel for a
half-amplitude 96 kHz residue was disproportionate. Round 3 opens that same
expression anyway, and the reviewer has since priced it, so the
proportionality changed.

Facts the reviewer established, to verify rather than trust: `with_buses`
has six call sites (two production, four test); `RtGraph::new` delegates to
it and all 51 of its call sites are inside `mod tests`, so keeping a 48 kHz
default on `new` costs zero mechanical edits; and neither production site
lacks a rate in scope — `offline::build_graph` takes `rate: u32`, and
`Control::rebuild` has `self.cache_rate`, already used two statements
earlier for the same graph.

**The trap, which is the whole reason to be careful here:**
`computed_tail_frames` is called from two places — `recompute_tail_frames`
at build, and `render_impl`'s `debug_assert`, which has its own
`sample_rate` parameter. Using `self.cache_rate` for one and `render_impl`'s
argument for the other is two sources of truth that disagree across a device
rate change, and they would fire the assert on the RT thread. Single-source
shape: add `rate` to `with_buses`, store it on `RtGraph`, and have the
`debug_assert` read `graph.rate` — never its own argument.

`self.cache_rate` can be 0 before a device opens. Define the behaviour
explicitly: clamp to 48 kHz, and say so at the clamp.

The constant's doc currently claims `with_buses` "has no sample rate in
hand". That is true of the signature and false of both call sites — correct
it.

## Item 3 (Minor) — one uncovered bail path

The registry-ordering pin covers the missing-clip and `rate == 0` bails but
not a `TempoMap::new` failure; no case in the suite makes it fail. If a
fixture can make it fail cheaply, add the third bail to the same test. If it
cannot be made to fail without contorting a fixture, say so in the report
and leave it — the comment covers the invariant.

## Gates

Focused tests only, `--test-threads=1` always. No full suite, no clippy, no
vitest, no perf — the gate-runner takes those. Note that this round raises
`tail_frames` for every live-node row, so the per-press cost goes up by the
chain length. That is the correct direction; it is the over-count raised as
a Minor on task 9 and deliberately accepted, because the other direction
swallows audio.

Append to `.superpowers/sdd/2026-08-28-plan-v2-players/task-10-report.md`,
commit, and return only: status, commit sha, one-line test summary,
concerns.
