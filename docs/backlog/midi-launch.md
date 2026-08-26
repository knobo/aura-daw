# MIDI launch map

**v0.1 is on `main`** (PR #42 squash `92f0d03`, follow-up PR #50
`1b38821`: play-from-0, overlay all-notes-off, Delete only on the
panel). Mark regions or clips,
bind them to notes, fire them from hardware or from a MIDI clip that
*uses* a named launcher. Drive playback does not steal the arrangement
loop or playhead (shadow playhead). GATE vs ONE-SHOT per launcher.
Piano-roll **▶ play** runs that clip once. Clips whose track is gone
do not drive.

User-facing: `docs/midi-input.md` § Launch. Trace: `AURA_LAUNCH_TRACE=1`.

## Not in v0.1 — next

### Hardware GATE

Drive clips honour GATE (note-off cuts the scene) vs ONE-SHOT (plays to
the marked end). Hardware pads do **not**: a matching note-on seeks,
loops, and plays the region; note-off only clears the held-pad debounce.
Hardware GATE (hold the pad to keep the scene, lift to stop) is not in
v0.1.

### Sustain (overlapping voices) — SUPERSEDED by Plan V, cut V3

**Do not implement this here.** As of 2026-08-26 overlapping voices are
V3 of [`plan-v-players.md`](plan-v-players.md), because the same single
atomic set (`launch_on`/`launch_pos` in `audio/rt.rs`) is what blocks a
polyphonic pad deck, a per-player automation clock and everything else the
owner asked the control surface for. Plan V **retires** this mechanism
rather than adding a third play mode to it: see
[the design's §2](../superpowers/specs/2026-08-26-plan-v-players-design.md)
for the four properties (one playhead, transport hijack on hardware fire,
MIDI-only clip target, automation read on the transport's clock) that make
it a prototype. The text below is kept because the *behaviour* it describes
is still what V3 must deliver.

Today ONE-SHOT restarts the single shadow playhead, and GATE cuts on
note-off. There is no way to trigger the same scene again and let the
previous one ring out.

**Sustain:** each trigger starts a *new* voice. The previous one keeps
playing to its marked end. Notes can overlap — a roll of kicks, or
stacked scene hits, without cutting the earlier one.

This is a third play mode on the launcher (GATE / ONE-SHOT / SUSTAIN),
not a replacement. Implementation sketch: N concurrent overlay
playheads (small fixed cap, not one `launch_pos`), mixer sums launched
tracks at each voice's position. Cut-off when the cap is hit: oldest
voice dies first. Hardware pads and drive clips share the same mode.

Do **not** start this until v0.1 has been used for a bit. The
single-voice overlay is the whole of the current engine hook.
