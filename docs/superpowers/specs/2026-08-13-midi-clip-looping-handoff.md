# Handoff: pending work after 2026-08-13 session

Companion to `2026-08-13-midi-clip-looping-design.md` (the approved spec).

## Merged today (all in main)

- #2 clip-edit loop playback (solo chip), #3 solo/mute fix, #4 resizable panels
  (+ app-shell height fix), #5 UI zoom, #6 project save/open/new + all heavy
  commands off the main thread.

## Waiting on

The identity-groundwork branch (`worktree-zrythm-arch`: typed ids, note_id +
AMEV watermark, transaction channel/undo, ADRs 0001/0002/0004) to merge into
main. **Do not start the clip-looping implementation before it lands** — both
touch `midi/types.rs` and `midi/persist.rs`.

## Next steps, in order

1. When the identity branch is merged: rebase/branch fresh from main.
2. Run the writing-plans skill against the spec to produce the implementation
   plan (the spec is approved; plan was intentionally deferred).
3. Implement drag-to-loop first (data model → `clip_events` repeat →
   `midi_set_clip_bounds` command → edge-drag gesture → repeat rendering),
   then Ctrl+C/V/D stamping. TDD throughout; adopt the landed typed ids
   (`ClipId`) in the new command.

## Message for the other agent (if not already delivered)

> Another agent (working with knobo in a separate worktree) has an approved
> design that builds directly on your work — no files touched on your branch,
> implementation deliberately sequenced after your branch merges:
>
> - MIDI clip looping on the timeline: drag the right edge → content repeats.
>   It adopts ADR 0004's content/placement split as its first user:
>   `MidiClip.lengthTicks` stays the *placement* length; a new optional v2
>   field `contentLengthTicks` (serde default, never required) carries the
>   content/loop length, intended to migrate mechanically into the content
>   object at the v3 bump.
> - Planned additive command `midi_set_clip_bounds` (zone C) — will use your
>   typed ClipId once available. Repeat expansion goes in
>   `midi/schedule.rs::clip_events()`, control-side only.
> - Touch surfaces after you land: `midi/types.rs`, `midi/persist.rs`
>   (PersistedClip row + reconstruct), `midi-clip.schema.json`. If your AMEV
>   header bump or v3 field table wants to account for a content-length field,
>   that's the hook.
>
> Spec: `docs/superpowers/specs/2026-08-13-midi-clip-looping-design.md`
> (branch `clip-looping-spec`). Nothing needed unless the field/command naming
> collides with your plans.

## Housekeeping (only with knobo's explicit go-ahead)

- Worktrees to remove later: `.claude/worktrees/agent-a9f6acb173082ec81`
  (panel-resize, merged), `.claude/worktrees/agent-aa9c431d2c4517e79`
  (ui-zoom, merged). `clip-edit-fix` is this session's own worktree.
- Branches safe to delete after their PRs merged: `clip-edit-loop` (local),
  `clip-edit-solo-mute-fix`, `pr4-fix`, `panel-resize`, `ui-zoom`,
  `project-ops`. `clip-looping-spec` stays until the spec PR/merge.
- **Never touch** `.claude/worktrees/zrythm-arch` (other agent, locked) or the
  main checkout without asking.
- The main checkout at `/home/knobo/prog/dav` is several commits behind
  origin/main — pull it when the other agent is done, not before.

## Smaller follow-ups noted along the way (unowned)

- Sampler instruments (SFZ bindings) are not persisted across open — the
  `instruments[]` v2 field exists in the schema but nothing writes/reads it.
- Repeat-aware SMF export; left-edge crop/content offset (spec §8).
- Dirty tracking + autosave/journal per SCALABILITY §4; recent-projects list.
- `App.svelte` edge-jump tempo-map bug is fixed as part of the clip-looping
  work (spec §6) — drop this line if that lands.
