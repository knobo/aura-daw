# Task 9 — fix round 2 of 5

Branch `feat/plan-v2-players`, worktree `/home/knobo/prog/dav/.worktrees/plan-v2-players`.
HEAD is `ca312fd`; nothing is uncommitted. Read
`docs/superpowers/plans/2026-08-28-plan-v2-players-PROGRESS.md` §"Pick up here"
first — it is the finding list this brief implements.

## Rules that bind you

- `cargo test --lib -- --test-threads=1`. NEVER without `--test-threads=1`:
  the parallel run SIGSEGVs in `midi_out::tests`, a pre-existing project crash
  (`docs/backlog/ci-hardening.md` item 5), not yours.
- Run every suite in the FOREGROUND. Do not start a long suite in the
  background and wait on it — two agents have deadlocked and been killed doing
  exactly that.
- A test that re-implements the logic it covers instead of CALLING it passes
  while production is broken. One Important defect has already shipped on this
  branch that way. Every test below must reach the real production function.
- TDD: RED first, then GREEN. Show the failing output before the fix.

## Item 1 (Important) — V-17 is refined; `out_delay` must follow

Ruling, already made, do not relitigate: **a player routed INTO A BUS is
compensated to that bus's `t_in`, exactly as its send edge is. A player routed
to MASTER is not.** Inside a bus everything must arrive together or the mix
smears; to master, "the pad answers the press" governs, and that is the
default and the path the owner's ear-check uses.

`src-tauri/src/audio/bus.rs:346-350` currently zeroes `out_delay` for a player
unconditionally, including when `node.output` is `Some(bus)`. That leaves a
pad's DRY arriving early into a group bus while its SEND into the same bus
arrives on time — the pad smears against itself and the two halves of V-17
contradict each other.

Change it to: for a player, `None` (master) → `0`; `Some(_)` (a bus) →
`target_of(node.output).saturating_sub(ready)`, the same expression a track
uses. `ready` is already correct for a player (the player arm at bus.rs:322
resolves it to the player's own applied chain latency).

Rewrite the comment at bus.rs:331-345 to state BOTH halves and why they
differ. It currently argues only the master half.

Tests (in `bus.rs`'s test module, against the real `compile_routing`):
- a player routed to master, with a high-latency track elsewhere in the graph:
  `out_delay`/`out_pdc` is 0.
- the same player routed into a bus: its `out_delay` equals that bus's `t_in`
  minus the player's own applied latency — the SAME number the player's send
  edge into that bus gets. Assert the two are equal; that equality IS the
  ruling.

Also update V-17's text in
`docs/superpowers/plans/2026-08-28-plan-v2-players.md:50` to state both halves,
and to record the consequence: a pad routed into a bus that contains a
linear-phase-EQ'd track inherits that latency. That is accepted — the
alternative is a smeared bus, and master stays at zero.

## Item 2 (Important) — the exclusive-idle early-out strands delay lines

`mixer.rs:797`'s `continue` skips `tap_into_bus`, and `d.process(copy)` inside
it (`mixer.rs:299`) is the only thing that advances a player's `RtSend::delay`.
`DelayLine` has no reset. So the last `delay` samples of every press stay in
the line: the send copy of a one-shot is TRUNCATED at its tail, and the next
press replays those samples into the bus at its onset — the previous hit's
tail on top of the new one. Item 1 makes this worse by giving a bus-routed
player an `out_pdc` line with the same problem on the dry path.

The early-out's safety comment at `mixer.rs:790-795` claims a player row
carries neither line. After item 1 that is simply false, and it was already
false for `snd.delay`.

**Ruling (controller, this round): flush the tail, do not discard it.**
Draining zeros and throwing the output away fixes the contamination but keeps
the truncation, and the finding names both. So when a row is `exclusive_idle`
AND carries any delay line, push one window of SILENCE through the row's tail
stages and emit it:

```
if exclusive_idle {
    if tr.out_pdc.is_some() || tr.sends.iter().any(|s| s.delay.is_some()) {
        // zero post_buf[..w_len*2]; tap pre-fader sends; tap post-fader
        // sends; tr.out_pdc.process(post); route_out(...)
    }
    continue;
}
```

Zeros are exactly what the full strip would have produced here: `on` is false
for an idle pad, so `apply_fader_into` zeroes `post` anyway. Meters are
untouched (`acc` stays zero), which is what rendering-then-zeroing did too.
The rows without a delay line — every raw pad, and the perf case — still take
the bare `continue` and keep the measured 1.98 µs.

Correct the comment to say what the code now delivers.

Tests:
- a player with a compensated (delayed) send, fired and allowed to end: the
  bus receives the FULL send tail, i.e. total energy into the bus matches the
  source's, rather than being short by `delay` samples.
- a second press does not carry the first press's tail into its bus at onset.
Both must drive the real `mixer::render*` path, not a re-implementation of it.

## Item 3 — three doc minors

- `mixer.rs:442` points at `render_windowed`, which does not exist. It is
  `render_impl`, `mixer.rs:664`.
- `engine.rs:5810`'s run command omits `--release` while quoting release
  numbers.
- `docs/backlog/plan-v-players.md` says a pad "can only be stopped from the
  backend"; Escape already reaches it via `stopAllSound` → `launchStop` →
  `launch_stop` → `stop_launch_overlay`.

## Item 4 — document, do not fix

A player whose applied chain latency exceeds `t_src` gets
`t_in[dest].saturating_sub(ready) == 0`, so its send is late by the excess and
no bus waits for it. Self-consistent with the unpadded dry path, but currently
unstated and untested. Add it as a doc comment beside the send-edge
computation in `bus.rs`, and a test that pins the observed behaviour so a
later change has to be deliberate.

## Gates before you report

1. `cargo test --lib -- --test-threads=1` — 1513 passed at `67ebe50`; expect
   that plus your new tests, 0 failed.
2. `cargo clippy --all-targets` — 130 warnings, unmoved for the whole branch.
   Do not move it.
3. `scripts/perf-check.sh --budget 525`. Do NOT interpret the absolute number.
   A FAILURE in the 400–500 µs band must be re-measured with a null control
   (add an unused field to a struct on a clean tree) before you believe it —
   the gate cannot attribute a step in this area. See PROGRESS §"The perf gate
   cannot bisect in this area".
4. Commit. Do not push a merge, do not touch the PR's draft state.

## Report back

What you changed, the RED-then-GREEN evidence for each test, the three gate
numbers, and anything you found that this brief did not anticipate.
