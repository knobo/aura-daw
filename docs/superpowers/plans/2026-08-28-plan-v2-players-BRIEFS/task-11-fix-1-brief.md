# Task 11 — fix round 1

The opus review confirmed your central claim: **no production change was
needed for the engine.** It verified in the code, not from your report, that
`player_fire` reads `p.trigger.mode` off the live session on every press,
that `ClockTable::advance` genuinely ends a non-looping clock at its `end`
and wraps a looping one, and that `PropPath::TriggerMode` sets
`effect.rebuild = false` so not republishing is right. All three of your
mutation REDs reproduced exactly, with your own assertion messages. Not
duplicating the Loop test was the right call — `a_loop_mode_player_fires_a_looping_clock`
already pins the brief's property.

The failure the review found is **under-assertion**, not a wrong test. Three
of your tests pass against mutations that break the thing the test is named
for. Plus one real gap in scope.

## Item 1 (Important) — a retrigger rewinds THIS player, and nothing pins it

`control/mod.rs:6672-6685`, `retriggering_one_player_leaves_another_sounding`.

The reviewer inserted `if tables.clocks.is_on(clock) { return Ok(()); }`
before `player_fire`'s `fire` call — making **a retrigger a complete
no-op** — and all 126 `control::tests` passed. `player_fire` is the only
caller of that path in the crate, so nothing anywhere catches it.

Your test asserts only that `b` is on. It never looks at `a`. The task's
headline property, and the whole reason the single overlay was deleted
(V-4), is that a retrigger rewinds **this** player. Assert `a`'s playhead is
back at 0 after the second press. `firing_an_audio_player_sounds_without_touching_the_transport`
(:6358) already has the shape: `tables.clocks.playhead(tables.slots[&TrackId::from(a.as_str())], .., false).pos`.

Earn it against the reviewer's own mutation — the early-return no-op above.

## Item 2 (Important) — one-shot ends *eventually*, not *at the source's end*

`control/mod.rs:6639-6649`.

The reviewer fired non-looping clocks with `len / 8` instead of `len` and
your test stayed green. It advances exactly 48_000 and asserts off, so it
cannot tell "ends at 48000" from "ends at 6000". The only test in the module
that catches the mutation is task 10's
`firing_a_midi_player_sounds_for_the_clips_tick_converted_length`; there is
**no audio-side test that pins the fired length at all.**

The hardcoded 48_000 is fine — that convention matches the neighbouring loop
test and is not the problem. The missing half is the other side of the
boundary: assert `is_on` is still **true** after advancing to just before
the end, then off after the last frame. Earn it against the `len / 8`
mutation.

## Item 3 (Important) — Gate is a frontend distinction, and nothing says so

`grep` confirms `TriggerMode::Gate` and `TriggerMode::OneShot` appear only
inside `mod tests`; production reads exactly one thing, `== TriggerMode::Loop`
(`mod.rs:2029`). Gate and OneShot are **byte-identical in the engine**.

**That is correct and stays.** The design's `gate` is "sounds while held;
release cuts it" — the release is a pointerup, so the difference between
gate and one-shot is entirely *who calls `player_stop`*, and the engine has
no business knowing which. What is wrong is that nothing records it: your
test's doc comment reads as though the mode were doing work, and
`gate_stops_on_release_before_the_source_ends` passes verbatim with
`OneShot`, or with the `set_trigger_mode` line deleted. It pins
`player_stop`, which `two_players_sound_at_once_on_their_own_clocks` (:6373)
already pins.

Say it where a reader will hit it — at `player_fire`'s `== Loop`, in the
production code: gate and one-shot are identical here **by design**, and the
distinction lives in who sends the release. Then make the test honest: either
name it for what it pins, or drop it as redundant and keep the statement in
production. Your call; argue it in the report.

## Item 4 (Important) — the seam that lets a user pick a mode does not exist

This is the real scope gap and it is not yours to have guessed — the brief's
"Produces: no new public names" misled you, and the controller accepts that.

There is **no generic `Op::Set` pipeline from the frontend.** Every mutation
crosses via a bespoke `#[tauri::command]` — 24 of them in that file — and the
player commands are `players_get` / `player_add` / `player_remove` /
`player_fire` / `player_stop` only. Task 13's brief only *reads*
`trigger.mode`; tasks 12, 14 and 15 never mention it. As the plan stands,
**V2 would ship with Loop and Gate unreachable by any user and every player
permanently at the OneShot default.**

Ruling: the command seam is task 11's, because a task called "Trigger modes"
that leaves every mode but the default unreachable has not delivered trigger
modes. Add a `ControlPlane` method and a `#[tauri::command]` in the shape the
existing player commands already use, and delete your test-only
`set_trigger_mode` free function in favour of calling the real one. The pad
inspector that exposes it is V-12's, and stays task 13's.

## Item 5 (Minor) — the OneShot set is inert

`mod.rs:6642` writes the value that is already there (`TriggerMode::default()
== OneShot`), so the test never exercises *selecting* OneShot. Set Loop
first, then OneShot, so the write does something.

## Not in this round

Minor 2 (the two-clip fixture's doc claims two players need distinct clips
to be independent, which `two_players_sound_at_once_on_their_own_clocks`
already disproves by firing two players off the same clip): the fixture is
correct, only its reasoning is. Fix the sentence while you are in the file;
do not change the fixture.

## Gates

Focused tests only, `--test-threads=1` always, **foreground only, and never
the Monitor tool**. No full suite, no clippy, no vitest, no perf — the
gate-runner takes those after you commit.

Note the design doc's real path: `docs/superpowers/specs/2026-08-26-plan-v-players-design.md`.
Earlier dispatches on this branch gave `docs/specs/…`, which does not exist.

Append to `.superpowers/sdd/2026-08-28-plan-v2-players/task-11-report.md`,
commit, and return: status, commit sha, one-line test summary, deviations
with reasons, concerns.
