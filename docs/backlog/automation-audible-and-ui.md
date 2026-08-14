# Backlog: automation — make it audible, then editable

**Captured 2026-08-14 from the owner.** Status at capture: the DATA layer is
done — `AutomationLane` persists, lives in `Session.automation` (Plan E
Task 10), edits go through `Op::AutomationSetLane` (atomic, attributed,
undoable once Gate E's history is on). What's missing is everything the
user experiences:

1. **RT attach (make it audible):** `engine::rebuild` never reads
   `session.automation` — compiled ramps and `GainAutomatedNode`
   (plugins/automation.rs) exist but are never wired into the production
   graph. Work: at rebuild, compile lanes (tick→absolute-sample ramps via
   the section table) and attach automated-gain (first) /
   plugin-param (later) nodes for tracks with lanes. NOTE: the day this
   lands, `Op::AutomationSetLane`'s apply arm must start setting
   `effect.rebuild = true` — the arm's comment says exactly this
   (control/session.rs, REBUILD PIN comment).
2. **UI (make it editable):** per-track automation lane view in the
   timeline — draw/drag points, target picker (track gain first;
   plugin params later via the plugin param list). Edits are
   whole-lane replaces through `automation_set` (already the command
   shape: "an edit gesture, never one invoke per point"), wrapped in
   gesture begin/end (Task 14) so a drag is ONE undo entry.
3. **Playback semantics**: per-block ramp application already specified in
   plugins/automation.rs's compile contract — verify against the engine's
   block loop when attaching.

## Sequencing / parallel-safety

- Start AFTER PR #12 (Plan E) lands — the RT attach edits `engine.rs`,
  which that PR rewrote (Committer, steady_time).
- File footprint: engine.rs + plugins/automation.rs (attach), timeline
  components + a small store (UI). CONFLICT NOTE: overlaps MIDI slice 2
  (docs/backlog/hardware-midi-io.md) in engine.rs — if run in parallel,
  sequence the engine-touching halves.
- Fits as Track D alongside next-prompt.md's Tracks A (Plan F), B (MIDI
  slice 2), C (multi-clip UI).

## Suggested cut

1. RT attach for track-gain lanes + the rebuild-pin flip + an audibility
   test (offline render with a gain ramp, assert amplitude follows).
2. Timeline lane UI for track gain (draw/move/delete points, gesture-wrapped).
3. Plugin-parameter targets (UI picker + attach path).
