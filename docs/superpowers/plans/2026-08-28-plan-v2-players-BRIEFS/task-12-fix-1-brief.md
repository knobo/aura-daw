# Task 12 — fix round 1

The opus review reproduced **every** mutation your report claimed, judged
three of your five deviations CORRECT, and found the migration function
itself careful and well-tested. It also confirmed question 6 against you in
your favour: all three idempotency layers *are* independently pinned, so
that is defence in depth, not a gap.

The findings are not in what you claimed. They are in what nothing looked
at — and both Criticals fail exactly the way this task was warned about:
silently, on open, on work the user had already saved.

## Critical 1 — `loaded_dir = None` leaks the previous project's midi into the next one

`control/mod.rs:4955`.

`adopt_midi_from_dir` (`midi/mod.rs:213-262`) has **three** branches and you
reasoned about two. The `Ok(None)` branch is guarded by `if
midi.loaded_dir.is_some()` — and that guard is what clears the *previous*
project's `clips`, `launch_maps` and `harmony` when the newly-opened project
has no v2+ midi document. Forcing `loaded_dir = None` immediately before the
call makes that guard permanently false, so the reset never runs.

`Ok(None)` is not the exotic legacy case it looks like. `midi::persist`
shares `project.json` with the audio project (`persist.rs:38`) and
`load_from_project` returns `Ok(None)` for `schemaVersion < 1 | absent`
(`persist.rs:345-348`). **Every project that has never had a midi save —
including every freshly created one — takes this branch.**

What a user loses: open project A (clips + launch bindings), then open
project B (new, never midi-saved). B shows A's midi clips, A's harmony and
A's launch bindings; your migration then mints players from A's bindings
into **B's** `store.players`; the next edit persists all of it into B's
`project.json`. A's work is duplicated into B and B's document is corrupted.

The reviewer verified this with a scratch integration test, and confirmed
that commenting out line 4955 makes that test pass while making
`opening_the_same_project_twice_does_not_double_the_player` fail with
`left: 0, right: 1`. So the line is unambiguously the cause, and it is the
same line your own RED exercised without noticing the other branch.

**The bug you found is real and well-diagnosed. Only the fix is wrong** — it
reaches past its own case into two branches it did not consider. Fix it at
the right level: either give `adopt_midi_from_dir` an explicit
force-reload argument so an explicit open bypasses the same-dir cache hit
*without* disabling the `Ok(None)` reset, or run the migration only when
`adopt` actually adopted. Your call; argue it in the report, and write a
test for the A-then-B case as well as keeping the two-open one.

Three narrower parts of your deviation 4 checked out and stand: nobody else
relies on the same-dir skip (`plugins/state.rs:1902` is a test helper); the
`dirty` guard is intact because `midi/mod.rs:214-221` checks the skip first
and `dirty` second; and the cost of a second open is acceptable.

## Critical 2 — the frontend has no `player` variant, and the panel throws on every migrated binding

`src/lib/types/ipc.ts:93-95` is `{kind:"region"} | {kind:"clip"}`. The Rust
wire form — which you pinned correctly at `launch.rs:1240-1255` — is
`{"kind":"player","playerId":"..."}`. TypeScript knows nothing about it, and
because the union is only ever *narrowed* and never exhaustively switched,
`tsc` stays green.

`LaunchMapPanel.svelte:32-42`'s `targetLabel` falls through to
`b.target.trackIds.map(...)`, which is `undefined` for a player target →
`TypeError` during render. It is called per row at `:320`, and
`launch.svelte.ts:105` re-pulls the launch snapshot on `project://changed`,
which `open_project_epoch` emits — so it fires on the very open that runs
the migration.

What a user loses: they open a project that has ever had a clip pad, and the
LAUNCH panel breaks. Every launch binding they own becomes unreachable, and
the only rows that still render are the **dangling** ones. That is the exact
inversion of what this task is for.

Three more on the same seam, all silent:
- `launch-map.ts:143-149` (`bindingFocusSamples`) and `:184-204`
  (`overlayBox`) treat "not region" as clip → `find(c => c.id === undefined)`
  → `null`. Click-to-jump does nothing and the timeline overlay disappears
  for every migrated pad.
- `launch.svelte.ts:292` (`mapClip`) finds an existing binding via
  `b.target.kind === "clip"`. After migration it never matches, so "map this
  clip to a pad" **creates a second binding on a new note** for a clip that
  already has one. `:302` `clipSelfTriggers` likewise returns `false`, so the
  self-trigger warning silently disappears.

**Ruling: this seam is task 12's, and the reasoning is the same one that put
task 11's command seam into task 11.** The reviewer read every remaining
brief: task 13 is the *control surface* (`control-surface.ts`'s
`SurfaceTarget`, `SurfacePanel.svelte`, `Rack.svelte`) — a different type in
different files; task 14 is the perf gate; task 15 is docs. Nothing after
this task owns `ipc.ts`'s `LaunchTarget`, `launch-map.ts`,
`LaunchMapPanel.svelte` or `launch.svelte.ts`. **A migration that makes every
saved launch binding unrenderable has not migrated anything.**

Add the `player` variant to `ipc.ts` and handle it in all four consumers.
Pin the wire form against what the frontend expects — a symmetric Rust round
trip pins nothing about a non-Rust caller, which is the mistake task 11
shipped.

## Important 3 — the drive path's self-trigger guard was missed

`midi/launch.rs:714`:

```rust
if let LaunchTarget::Clip { clip_id } = &b.target {
    if clip_id == clip.id.as_str() { continue; }
}
```

That is the guard stopping a drive clip from firing the pad that plays that
very clip. It is a non-exhaustive `if let`, and your exhaustiveness list
counted `:906` and `:924` but not this one. After migration the target is
`Player`, the guard never matches, and the clip re-fires its own player on
every drive note-on — the material sounds twice and restarts. The frontend's
matching warning goes quiet at the same time (see Critical 2), so the user
gets no hint at all.

The reviewer's full count is seven Rust sites and six frontend. The other
five Rust sites are handled correctly, including `snapshot.rs:580` — the
right line, and sufficient.

## Important 4 — nothing pins WHICH player a binding gets

`midi/launch.rs:198`. The reviewer replaced the target assignment with
`let player_id = players[0].id.clone();` — always the first player — and
**all 41 `midi::launch` unit tests and all 6 `player_migration` integration
tests passed.** Not one fixture in either file has two distinct clips, so
`players[0]` is trivially right everywhere.

`migrate_reuses_an_existing_player_already_on_the_same_clip` asserts an id
rather than a count, which is the right instinct — but with a single player
in the vec it cannot discriminate. The suite needs two clips, two bindings,
and each binding asserted against **its own** clip's player. Earn it against
the `players[0]` mutation.

This is the branch's standing failure — a test that passes against a
mutation breaking the very thing it is named for — and it is the third task
in a row to ship one.

## Not in this round

- **Deviation 5 (in-memory only) stands**, but its argument was thin and the
  reviewer is right that the id-instability half deserved a sentence. Nothing
  outside the document persists a `PlayerId` today, so nothing breaks yet —
  but task 13 is about to persist `playerId` into surface layouts, and after
  Critical 1's proper fix, id churn on re-open is what makes that a hazard.
  Write the sentence into the migration's doc comment. The controller is
  carrying the task-13 interaction separately.
- `session.rs:1659` validates only that `player_id` is non-empty, not that
  the player exists. Consistent with the `Clip` arm and the same loudness as
  before. Note only.
- No test in your diff exercises the *published snapshot* path the frontend
  actually consumes — `cp.players()` and `cp.launch_snapshot()` both read the
  live session. The reviewer confirmed by reading that `republish_full()`
  runs inside the same lock after the migration and that the snapshot carries
  both wholesale, so this is a coverage note, not a defect. Mention it in the
  report; do not build a rig for it.

## Gates

Focused tests only, `--test-threads=1` always, **foreground only, and never
the Monitor tool**. No full Rust suite, no clippy, no perf — the gate-runner
takes those after you commit. **You may and should run vitest scoped to the
frontend files you touch** (`npx vitest run <path>`), since this round has
real frontend work; do not run the whole npm suite.

Append to `.superpowers/sdd/2026-08-28-plan-v2-players/task-12-report.md`,
commit, and return: status, commit sha, one-line test summary, deviations
with reasons, concerns.
