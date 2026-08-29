# Task 9 — fix round 4 of 5

Worktree `/home/knobo/prog/dav/.worktrees/plan-v2-players`, branch
`feat/plan-v2-players`, HEAD `bf0cb58`, clean. Read `task-9-fix-3-brief.md`
first — this round finishes what it started. No Critical findings; the two
Importants below are a coverage hole and a defect that task 10 (the NEXT task)
makes live.

## Rules that bind you

- `cargo test --lib -- --test-threads=1`. NEVER without `--test-threads=1`.
- Every suite in the FOREGROUND. Never background-and-wait.
- Every test must reach the real production function. A test that
  re-implements the logic it covers passes while production is broken — that
  has shipped one Important defect on this branch already.
- **Prove each new test bites.** Where a RED is impossible (a regression test
  for code that is already right), mutate the production line it covers, show
  the test failing, and revert. The reviewer set that standard last round and
  it is now the standard.
- Perf gate is thermally sensitive: measure COLD, never right after the suite.
- Do not merge, do not push, do not touch the PR's draft state.

## Item 1 (Important) — the `!flushing` clip-read suppression is untested

`mixer.rs:909`. The reviewer removed `&& !flushing` and the full suite still
passed 1524/0. Nothing in the tree distinguishes the flush path's single most
important difference from the ordinary strip.

The behaviour it protects: a non-raw pad with an insert chain stopped
**mid-clip** — Escape → `stopAllSound` → `launchStop` → `stop_launch_overlay`
→ `ClockTable::stop` freezes the clock INSIDE the clip, not past its end. From
the next block the row is `exclusive_idle` with `flush_left > 0`, so the strip
runs with `on = true` and a frozen `track_base`. Without the suppression,
`frame_pos(track_base, f)` re-reads the same fragment every block, through the
chain and out the fader — fix round 1's defect, bounded to `tail_frames` and
audible as a stutter over the tail. Both existing rigs miss it because they
let the clip run to its natural end, where the frozen `pos` is already past
`length_samples` and `clip_sample` returns zeros anyway.

Add a test that stops the pad MID-clip (`g.clocks.stop(1, …)` after one block)
and asserts conservation: total delivered energy equals the energy of the
frames actually read. A `ramp_source` makes a repeated fragment directly
visible as a non-monotone output. Prove it bites by deleting `&& !flushing`.

## Item 2 (Important) — `render_live_into` is not suppressed during the flush

`mixer.rs:923`, and the comment at `:814` claims the clip read is the only
source difference. `render_live_into` (`mixer.rs:388-396`) queues every
scheduled event in `[pos, pos + run)` before processing. During a flush `pos`
is frozen, so an event inside that window is re-queued once per flush block.

Unreachable today — player rows carry no live node (`_ => Vec::new()`,
`engine.rs:2005`) — but task 10 is "MIDI players with an instrument no track
owns" and is the very next task. Then: a MIDI pad whose clock stops with a
note-on at the frozen position re-triggers that note `ceil(tail_frames/block)`
times.

**Fix now, while the reasoning is in view.** Split the two halves: gate the
EVENT QUEUEING on `!flushing`, but keep calling `node.process`. The node's own
tail is exactly what the flush exists to drain, so skipping `render_live_into`
wholesale would be wrong. `set_block_context` should still be called — decide
which position it gets and say why in the comment.

Correct `:814` to state the rule: the row stops being **FED**, which for a
live row means the event queue as well as the clip read.

Test it now rather than waiting for task 10: a player row with a live node and
a scheduled event, stopped mid-clip, must see the event queued exactly once.
If a live node cannot be built on a player row before task 10, say so plainly
in your report rather than writing a test that fakes one — and pin the gate
some other way.

## Item 3 (Minor, fix now) — `tail_frames` sums parallel branches

`rt.rs:581`. The strip is inserts → `pdc` → pre-fader taps → fader →
post-fader taps → `out_pdc` → `route_out`. The send taps are taken from `post`
BEFORE `out_pdc.process`, so `out_pdc` and the send delays are PARALLEL
branches, not sequential stages. The true worst path is
`inserts + pdc + max(out_pdc, max_send_delay)`.

It never under-counts, so nothing truncates — but the doc comment justifies
the over-count as costing nothing, and it does not: V-17 makes a bus-routed
pad's `out_pdc` and its send edge into the same bus EQUAL, so the window is up
to 2× the tail. With one linear-phase EQ in that bus that is ~4096 extra
frames (85 ms) of FULL-STRIP processing, inserts included, after every pad
release.

Change the arithmetic to the `max`. Correct the comment to state the strip
topology that justifies it.

## Item 4 (Minor) — the `line(&self.pdc)` term is untested

`rt.rs:581`. Mutation `inserts + line(&self.out_pdc) + sends` → 1524 passed.
Unreachable because V-17 forces `track_pdc` to 0 for a player, so `tr.pdc` is
always `None` on the only rows that flush. It was asked for defensively and
stays — but pin it in one line by setting `pad.pdc` directly, the way
`out_pdc_pad` (`engine.rs:6055`) sets `out_pdc`.

## Item 5 (Minor) — the funnel invariant rests on convention alone

`rt.rs:743-748` says `tail_frames` "cannot fall out of step". True of
production — the reviewer verified every call site — but `mixer.rs:1594-1596`
and `:1630-1631` assign `g.tracks[i].pdc` / call `insert_on` AFTER
`RtGraph::new`, leaving `tail_frames = 0`. Harmless for ordinary tracks; a
future player-row rig doing the same gets a silent stale 0.

Add a `debug_assert` in `render_impl` comparing `tail_frames` against a
recompute, OR soften the comment to say what is actually enforced. Your call —
say which and why. (A `debug_assert` is acceptable here: this is an internal
invariant, not user data. The standing ruling against `debug_assert` on this
branch was specifically about a value arriving from the frontend.)

## Item 6 (Minor, document) — `flush_left` does not survive a graph rebuild

`RtTrack::clips` sets it to 0 and `with_buses` does not seed it, while insert
NODES are reused across rebuilds by `LiveNodeRegistry` to preserve plugin
state. A rebuild landing inside a pad's flush window strands the plugin
pipeline exactly as before this commit, and the next press replays it. Narrow
(a few ms), no worse than `95a17ed`, so not a regression — but it is the one
remaining hole in "nothing on this row is skipped until its whole tail has
left it". Record it in `docs/TRAPS.md`. Do not fix it.

## Item 7 (Minor) — V-17 (b)'s tail enumeration is garbled

`docs/superpowers/plans/2026-08-28-plan-v2-players.md:50`: `out_pdc` appears
twice and the dash clause does not parse. The content is otherwise true of the
code. Rewrite that sentence, and drop the claim that `tail_frames` is
"everything the row holds" — per item 3 it is an upper bound on the tail, not
the tail.

## Gates before you report

1. `cargo test --lib -- --test-threads=1` — 1524 at `bf0cb58`, plus yours, 0 failed.
2. `cargo clippy --all-targets` — quote the PER-TARGET split, not a total. The
   carried figure "130" has accounting nobody has reproduced; what matters is
   that it is UNMOVED against `bf0cb58`, measured in the same sitting.
3. `scripts/perf-check.sh --budget 525`, COLD.
4. `idle_players_block_cost` in `--release`, cold — 1.89 µs at `bf0cb58`.
   Item 3 should not move it (raw pads have no tail either way); item 2 must
   not either.
5. Commit.

## Report back

Per item: what changed, the RED or the mutation-proof that the test bites, and
the gate numbers. Say plainly anything this brief got wrong — the last two
rounds each found a real error in mine, and that is working as intended.
