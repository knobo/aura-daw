# Task 10 — fix round 4 of 5

Dispatched 2026-08-29 by resuming round 3's finisher (opus). Round 3's
re-review came back **APPROVE with one Important**, all three brief items
ADDRESSED and every mutation claim in the round-3 report reproduced
byte-for-byte. This round closes the one Important and one Minor.

**Both items are test-only.** No production behaviour changes. If you find
yourself editing production code to make either test pass, you have found a
real defect — stop and report it rather than adjusting the test.

## Item 1 (Important) — pin the production rate wiring

The reviewer mutated the production RT-graph call site
`src-tauri/src/audio/engine.rs:2531` from `self.cache_rate` to a literal
`48_000` and ran `cargo test --lib -- --test-threads=1 audio:: control::
midi::`: **1058 passed, 0 failed.** Nothing in the suite catches it.

What that leaves open: on a 96 kHz device every live-node row's flush window
is sized at 4096 frames instead of 8192 — the release ramp is covered
halfway, and the half-amplitude stranded voice that item 2 of round 3 exists
to remove comes straight back. It fails **silently**, because
`render_impl`'s `debug_assert` reads `graph.rate`, which would carry the
same wrong 48000; the invariant check agrees with the bug. That is by
design — the consistency check is what makes the device-rate-change window
safe — but it means one unpinned argument is the only thing between a
correct 96 kHz window and a wrong one.

`Control` already lets tests set `cache_rate` directly (`engine.rs:8011`,
`8307`, `8336`). Set it to 96_000, rebuild, and assert the published graph's
live row carries `live_tail_frames(96_000)`, and that `graph.rate` is
96_000.

The two other production sites are in the same position and the reviewer
inferred rather than mutation-tested them: `src-tauri/src/audio/offline.rs:290`
and `src-tauri/src/control/loopjam.rs:913`. Check each. `offline.rs` is
cheap — `build_graph` takes `rate: u32` outright. For `loopjam.rs`, if a
non-48 kHz rebuild cannot be driven without contorting a fixture, say so in
the report and leave it; do not build a rig to reach it.

**Earn each pin by mutation.** Replace the site's rate expression with a
literal `48_000`, show your new test failing, revert. A pin that passes
against the mutation is not a pin.

## Item 2 (Minor) — pin that the raised tail is inert per row KIND

`rt.rs:667-671` claims the allowance is inert for a track: `tail_frames` is
consumed only under `flushing`, and `flushing` is `exclusive_idle`, which no
track row is. The reviewer verified the claim by reading — `mixer.rs:906/909/926`
are the only consumers of `flush_left`, all gated on `exclusive_idle`, and
`node_playhead` (`mixer.rs:527-579`) sets `exclusive_idle` only on the
`ph.exclusive` branch. So a live MIDI **track** row built by
`playback.rs:252/292` gets `tail_frames` recomputed to `strip + 4096` in
`with_buses` and writes a larger `flush_left` that nothing ever reads.

Nothing pins that per row kind. `engine.rs:6358-6365` pins the raw-pad case
(`tail_frames == 0`) only. This branch has already shipped a gate that
opened **every track's** fader for `tail_frames` after each transport stop;
the entire 1518-test suite passed under it, and only a purpose-written
per-kind test caught it. That is what this item buys.

Add one assertion that a live MIDI *track* row does not open its fader — or
otherwise produce output — after a transport stop, despite now carrying a
non-zero `tail_frames`. Earn it by mutation: make `flushing` true for a
track row (or remove the `exclusive_idle` gate on one of the three
`flush_left` reads), show the test failing, revert.

## Not in this round, and why

- **Minor 1** (`the_live_node_allowance_adds_to_the_strip` derives its
  expectation from `live_tail_frames`, the helper it covers): no action.
  The reviewer's own 4096 → 2048 mutation showed the allowance's magnitude
  is pinned behaviourally by the two `mixer.rs` tests, which is where the
  safety net belongs. The arithmetic test is the weaker half of a pair, not
  a net on its own, and that is fine.
- **Minor 3** (`live_tail_frames(0)` is a debug-only guard; `recompute_tail_frames`
  is `pub` and in release a 0 rate returns a 0 flush window): note only,
  goes to task 15. Only reachable from test code today, and the finisher's
  call to keep a single clamp so the docs stay true was the right one.
- **Round-3 report Concern 3 is withdrawn** — the reviewer traced that
  `open_output` calls `rebuild()` unconditionally on success, and `rebuild`
  → `ensure_loaded` sets `self.cache_rate = self.engine_rate()` before the
  `with_buses` at 2531. A device opening at 96 kHz *does* force a rebuild
  at the real rate. Do not write a test for the withdrawn concern.

## Gates

Focused tests only, `--test-threads=1` always, **foreground only, and do not
use the Monitor tool**. No full suite, no clippy, no vitest, no perf — the
gate-runner takes those.

Append to `.superpowers/sdd/2026-08-28-plan-v2-players/task-10-report.md`,
commit, and return only: status, commit sha, one-line test summary, concerns.
