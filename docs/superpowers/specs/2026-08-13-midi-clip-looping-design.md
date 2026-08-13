# MIDI clip looping on the timeline + clip stamping — design

**Date:** 2026-08-13
**Status:** approved in chat (brainstorming session); implementation deferred until the
identity-groundwork branch (`worktree-zrythm-arch`) lands — see §10.

## 1. Goal

1. **Drag-to-loop (primary):** dragging a MIDI clip's right edge along the timeline
   extends its *playback* length; the clip's content repeats until the placement ends.
   One clip box on the timeline, repeat boundaries drawn inside it; editing the notes
   once updates every repetition.
2. **Stamping (secondary):** copy a selected clip and paste it at a later position —
   Ctrl+C / Ctrl+V (paste at playhead on the selected track) and Ctrl+D (duplicate
   immediately after). Stamped copies are independent clips with their own notes.
3. Pasting/moving a clip to another track intentionally keeps today's semantics: the
   instrument binding lives on the track (`TrackState.instrumentId`), so the clip plays
   with the target track's instrument. Standard DAW behavior; no change.

## 2. Alignment with ADR 0004 (content/placement split)

ADR 0004 (accepted 2026-08-13, on the identity-groundwork branch) assigns
**"length (may crop content)" to the Placement** and **"loop/native length" to the
Content**. This feature is that ADR's first user:

- Today's `MidiClip.lengthTicks` **stays** and is the *placement* length.
- The new field is a *content* property, named so it migrates mechanically into the
  content/pattern object at the v3 schema bump (SCALABILITY §3's `patterns[]` model).

Full pattern instancing (`patternId`, shared content across instances) remains the
reserved v3 milestone; this design is the bridge, not a substitute.

## 3. Data model

- `MidiClip.contentLengthTicks?: u64` (Rust `Option<u64>`, TS `contentLengthTicks?`).
  - **Absent ⇒ content length = `lengthTicks`** (today's semantics; full v2 schema
    backward compatibility — optional field, never `required`).
  - Set the first time the right edge is dragged: content length = the clip's length
    at that moment; `lengthTicks` becomes the dragged playback length.
  - Placement may be shorter than content (crop) or longer (loop). The piano roll
    always edits the *content*.
  - Ticks, per the SCALABILITY rule (no new musical fields in seconds/samples).
- Persistence: `#[serde(default)]` field on `PersistedClip` in `midi/persist.rs`
  (row writer + reconstruct, defaulting to `lengthTicks` when absent). The AMEV chunk
  format is untouched (it encodes only ppq + notes). Schema docs: add the optional
  field to `midi-clip.schema.json` `$defs/clip` and `$defs/persistedClip`.

## 4. Playback (Rust)

All scheduling flows through one seam: `midi/schedule.rs::clip_events()`.

- Wrap the existing note loop in a repetition loop:
  `for rep in 0..ceil(placement_len / content_len)`, emitting each note at
  `timeline_start + rep * content_len + note.tick`.
- Keep the existing two crop rules unchanged: events at/after placement end are
  dropped, and note-offs clamp to placement end (they now also produce the cut at the
  final partial repetition).
- Offline export (`audio/offline.rs`) shares this seam — live playback and bounce stay
  identical by construction. RT never sees ticks (ARCHITECTURE §13): the repeat is
  pre-expanded control-side into the flat `AbsNoteEvent` list.
- Known cost, accepted for now: an N-bar placement of a 1-bar content materializes N×
  the events per graph snapshot. SCALABILITY §3 already plans the merge-cursor
  replacement; nothing here blocks it.
- `midifile.rs` SMF export walks raw notes today and ignores clip windows entirely
  (pre-existing); making it repeat-aware is **out of scope** here and noted as a
  follow-up.
- AMT infill validation (`midi/amt.rs`) validates the region against *content* length.

## 5. New command: `midi_set_clip_bounds`

Additive registration (zone C), signature:
`midi_set_clip_bounds(clip_id, timeline_start_ticks, length_ticks, content_length_ticks?)`.

- Serves the edge-drag (sets placement + content length atomically) and clip moves.
- Closes a real pre-existing hole: `midi.svelte.ts::moveClip()` is frontend-only —
  a dragged clip today never reaches the scheduler nor the project file. Drag-move is
  rerouted through this command.
- Validation: `length_ticks >= 1`, `content_length_ticks >= 1` when present, clip must
  exist. Persists via the normal midi mutation path (auto-save into the open project,
  Rebuild).

## 6. Frontend

- **Edge-drag gesture** (`MidiClipView.svelte`): ~8 px right-edge zone, `ew-resize`
  cursor (same visual idiom as the ruler's loop pins), pointer capture like existing
  gestures, snap to the timeline grid (Alt bypasses). Left-edge crop/offset: out of
  scope (YAGNI).
- **Rendering** (`MidiClipView.svelte`): the mini note-preview becomes
  content-relative (`pxPerTick` from content length) and draws each repetition with a
  thin separator line at every content boundary. The note-flash rAF applies the same
  modulo. Browser demo engine (`demo.ts`) gets the identical modulo in its voice
  scheduling and meter envelope.
- **Piano roll**: all bounds read *content* length (initial zoom fit, note-creation
  limit, marquee clamp, end shading). The playhead overlay wraps modulo content length
  while the timeline playhead runs through the repeats.
- **Stamping**: Ctrl+C copies the selected clip (id held in the existing
  `midi.selectedClipId`), Ctrl+V pastes at the playhead on the selected track,
  Ctrl+D duplicates immediately after the source clip. Implemented with the existing
  `midi_add_clip` + `midi_set_notes` (independent copies). Coordinated with the
  undo/redo work: once the transaction channel lands, these route through it like
  every other mutation.
- **Clip-edit loop** (PR #2 feature): opening the editor loops the *content*
  (`midi.svelte.ts::open()` uses content length). The transport loop no longer spans
  the stretched placement — the piano roll loops the bar you are editing.
- `App.svelte` edge-jump list: fix the pre-existing tempo-map bug while splitting the
  lengths (`ticksToSamples(start+len) - ticksToSamples(start)`, not
  `ticksToSamples(len)`).

## 7. Testing (TDD)

- Rust `schedule.rs`: sibling of `clip_window_clips_notes` for the repeat case
  (2× repeat, partial final repeat with note-tail clamp, `content == placement`
  no-op, tempo change mid-repetition).
- Rust `persist.rs`: round-trip of `contentLengthTicks` (present and absent).
- Rust command test: `midi_set_clip_bounds` moves + resizes + persists.
- Frontend `clip-edit-loop.test.ts`: update the wiring tests to content-length bounds.
- Frontend store test for copy/paste/duplicate placement math.
- Gesture/rendering layers have no component-test infra: verified live in the app.

## 8. Out of scope (follow-ups)

- Pattern instancing / shared content across stamps (v3 milestone, ADR 0004).
- Left-edge crop and content offset.
- Repeat-aware SMF export (pre-existing gap widened knowingly).
- Undo/redo (arrives with the transaction channel; stamping adopts it then).
- Audio-clip looping (this design is MIDI-only).

## 9. Risks

- **Merge overlap** with the identity branch in `midi/types.rs` / `midi/persist.rs` /
  AMEV header — mitigated by sequencing (§10).
- Event-count blow-up for long placements of short content — accepted, bounded by the
  planned merge-cursor (SCALABILITY §3).
- The clip-edit loop decoupling from the visual right edge changes a subtle mental
  model; mitigated by the repeat separators making "content vs placement" visible.

## 10. Sequencing

1. This spec lands now (docs-only branch).
2. Implementation starts after `worktree-zrythm-arch` (typed ids, note_id + AMEV
   watermark, transaction channel) merges to main, and adopts its types directly
   (`ClipId` in the new command, note ids untouched).
3. Drag-to-loop first (data model → scheduler → command → gesture → rendering),
   stamping second, both TDD.
