# Handoff: control surface

Pickup notes for the next agent on this track. Read this first, then
the backlog, then the spec. The research dossier is background.

- Branch: `feat/control-surface`
- Worktree: `.worktrees/control-surface` (cut from `origin/main`)
- Draft PR: #113
- Claim row: `next-prompt.md` Active claims — **delete that row in
  the last commit before merge**.

## If this session died mid-implementation

v0.1 **is on the branch**: layout algebra, SURFACE bottom panel, 3D
widgets, Add-all recipes, LPD8 template, clip fire via `launch.preview`.
Frontend tests were green at 1321.

1. `git -C .worktrees/control-surface status` and `git log
   origin/main..HEAD`. The first commit is the claim
   (`docs: claim the control-surface panel track`).
2. The source of truth for layout is
   `src/lib/utils/control-surface.ts`. If the tests in
   `control-surface.test.ts` fail, fix those before touching Svelte.
3. Do not reopen this worktree after merge. Cut a fresh one from
   `origin/main` for whatever comes next (Plan V's V1).

## What v0.1 must still be, if unfinished

A green `npx vitest run` covering the pure layout algebra, a
SURFACE bottom tab that renders, Add all tracks producing strips
that mute/gain through `project.setMute` / `project.setGain`, and
pads that call `launch.preview` after `launch.mapClip`. Visual wow
on Gauge/Pad/Fader using theme tokens only (`no-literals` is CI).

## v0.2's stop slice went in with v0.1 — done, not pending

`launch_stop` **exists** (PR #113): `SharedRt::end_launch` +
`ControlPlane::stop_launch_overlay` + the command, Escape wired to a
composed stop-all (`src/lib/state/stop-all.ts`), and a toggle pad's second
press cuts its own clip. Hardware GATE (`midi-launch.md`) can share the
command.

**What comes after is not this track any more.** A pad that holds a raw WAV
or its own instrument, knobs bound to no track, and recording what you play
all need a second time base in the engine — see
[`docs/backlog/plan-v-players.md`](../backlog/plan-v-players.md) (Plan V).
This track's remaining cuts are V7 (pad inspector +
`Op::ControlSurfaceSet`) and V8 (hardware map, templates). Start at V1.

`lib.rs` is labelled FROZEN; every later track has still added
commands there. New names are the allowed evolution. Do not rename
`launch_fire`.

## Do not

- Put layout in `project.json` without `Op::ControlSurfaceSet` and
  a serde-default reader. A silent extra key that undo cannot see
  will fail the M-3 redo invariant the moment anything else writes
  it.
- Bind pad blink to Svelte `$state` of the meter bus. 60 Hz
  invalidations of the shell are how Meter was designed *not* to
  work.
- Add a dock tab. The dock shortcut table is a CI-enforced
  bijection (`dock-shortcuts.test.ts`); pitch is a bottom panel
  for the same width reason.
- Drop an Akai SVG in. Homage layout only.

## Commands the panel is allowed to emit (v0.1)

`set_track_gain`, `set_track_pan`, `set_track_mute`,
`set_track_solo`, `set_track_arm`, `gesture_begin` / `gesture_end`,
`launch_set` (via `launch.mapClip`), `launch_fire` (bypass true),
`launch_set_drive`, `plugin_set_param`.

Nothing else. If you need a fourth, it is a new cut.
