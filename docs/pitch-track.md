# Pitch track, melody extraction, editing, and correction

This is the normative product and architecture plan for pitch as data in AURA.
It consolidates the landed Pitch Coach/APTF work and the remaining work for a
persistent arrangement pitch lane, melody extraction, manual editing, offline
pitch correction, and later live auto-tune.

Historical detail remains in
[backlog/pitch-as-data.md](backlog/pitch-as-data.md) and
[backlog/pitch-correction-autotune.md](backlog/pitch-correction-autotune.md).
If those documents disagree with this document about the intended finished
product, this document wins.

## 1. Keep three meanings separate

| Data | Meaning | Storage |
|---|---|---|
| Source pitch curve | What was actually sung or played | Derived APTF cache |
| Reference MIDI | Intended notes and note boundaries | Project document/op log |
| Pitch edit layer | Non-destructive interpretation/correction | Project document/op log |

```text
source APTF + pitch edits = effective performance curve
effective curve + target + policy = pitch-shift envelope
```

Do not encode analysed pitch as MIDI pitch bend: bend is relative to a
channel's bend range and cannot faithfully carry clarity, RMS, or
voiced/unvoiced state. Do not write MIDI-derived targets as analysed APTF
either. MIDI is intent; APTF is measurement.

## 2. Landed on main

The following already exists:

- YIN monophonic F0 detection at a 10 ms hop;
- `PitchFrame { sample, hz, midi, clarity, rms, voiced }`;
- bounded RT decimation with YIN analysis off the audio callback;
- live Pitch Coach and MIDI-reference scoring;
- pitch folding while recording;
- offline `pitch_analyze_clip(clip_id)`;
- lazy `pitch_track(clip_id, max_points)`, analysing on cache miss;
- APTF v1 read/write under `cache/pitch/<clipId>.bin`;
- source-to-timeline mapping and frontend downsampling;
- `hum_to_song`, which can create a MIDI clip from audio.

Key code:

- `src-tauri/src/audio/{yin,pitch,pitch_thread,pitch_store}.rs`
- `src-tauri/src/control/{pitch_coach,hum}.rs`
- `src/lib/pitch/lane.ts`
- `src/lib/components/pitch/PitchCoach.svelte`

## 3. Persistence and APTF v1

Source pitch is reproducible derived data like waveform tiles. It is not
`project.json` state and does not enter the journal or undo/redo.

```text
<project>.aura/cache/pitch/<clipId>.bin
```

A normal reopen preserves it. A missing or invalid cache is regenerated from
the clip's source audio. Consumers ask through the backend; the frontend never
opens cache files directly.

APTF v1 is little-endian:

| Bytes | Field |
|---:|---|
| 4 | magic `APTF` |
| 2 | `u16` version |
| 1 | provenance |
| 1 | reserved |
| 4 | source sample rate |
| 4 | hop in source samples |
| 8 | first frame position |
| 4 | frame count |
| 12 x N | `f32 hz`, `f32 clarity`, `f32 rms` |

`voiced` is `hz > 0`; MIDI is derived from Hz; position is
`first_sample + index * hop`.

Provenance:

- `CausalMedian`: currently produced, live-compatible;
- `CentredMedian`: better offline analysis, required before correction.

APTF remains read-only and regenerable. Manual edits never overwrite it.

## 4. Arrangement pitch lane

Pitch belongs to an audio clip. Do not add an independent
`TrackKind::Pitch` that can drift away from its audio. Add a collapsible
child lane:

```text
Vocal
├── audio clips / waveform
└── pitch
```

The lane must:

1. load `pitch_track` lazily and draw voiced MIDI/cents points;
2. break the line across unvoiced frames;
3. share placement, trim, zoom, split, loop, and sample mapping with its clip;
4. show MIDI targets as note rectangles behind the curve;
5. overlay the effective edited curve without altering the source;
6. expose analysing, cache-miss, error, and re-analyse states;
7. fetch/downsample by visible range and pixel width;
8. follow lane height, grouping, ordering, and collapse behaviour;
9. remain read-only until edit mode is explicitly entered.

Pitch Coach may reuse the geometry, but is not the arrangement pitch lane.

## 5. Audio clip to MIDI melody

The user action is **Extract melody to MIDI**, distinct from **Analyse pitch**.

```text
decoded audio -> centred APTF -> note segmentation -> MIDI clip
```

The existing `hum_to_midi.py` segmentation supplies initial defaults:
minimum note duration, short unvoiced gap, median pitch per segment, and a
pitch-movement split threshold. Production extraction should consume the
Rust/APTF analysis instead of running a second detector.

The backend command must:

- accept `clip_id`;
- ensure centred offline analysis;
- segment only voiced frames;
- create or target a MIDI track;
- use stable detected-segment IDs;
- return created track/clip IDs;
- land the document change as one undoable transaction;
- optionally select the new track as Pitch Coach reference.

### Musical time

Audio is sample-domain; MIDI is tick-domain. Conversion at an unrelated
project BPM is wrong. Use the project's `TempoMap`. Until automatic
imported-song tempo-map derivation exists, require a correct project tempo map
or an explicit user-supplied BPM/map, and state that limitation in the UI.

## 6. MIDI to target curve

MIDI can be rendered as an ideal target curve, but this is a view, not APTF.

- note key defines target MIDI;
- note start/end define the target span;
- gaps have no target;
- pitch bend is used only when its range is explicit;
- overlapping notes use Pitch Coach melody resolution and are marked
  ambiguous.

The MIDI clip remains the persisted source of truth.

## 7. Manual editing

Manual editing is non-destructive document state layered over APTF. A first
model should be clip-relative and sample-domain:

```rust
struct PitchEditLayer {
    clip_id: String,
    version: u16,
    segments: Vec<PitchEditSegment>,
}

struct PitchEditSegment {
    id: String,
    start_sample: u64,
    end_sample: u64,
    source_median_midi: f32,
    target_midi: Option<f32>,
    offset_cents: f32,
    strength: f32,
    transition_ms: f32,
    preserve_vibrato: f32,
}
```

Names may change during schema review; semantics must not:

- moving the audio clip does not move edits relative to its audio;
- trim hides/clips edits deterministically;
- split partitions edits;
- one gesture is one op-log transaction;
- reset removes edits, not source analysis;
- re-analysis preserves edits where sample spans remain valid;
- copied clips clone edit layers by default;
- all edits survive save/reopen.

Initial operations:

- drag a detected note segment vertically;
- set an exact target note or cents offset;
- straighten drift by strength;
- set transition/retune time;
- preserve or reduce vibrato;
- split/merge detected segments;
- reset selection or the whole clip.

Freehand per-frame drawing is deferred because it creates large document state
and unclear undo semantics.

## 8. Effective curve API

One backend path must combine APTF and edits. Consumers must not implement
their own combination rules. A visible-range response distinguishes:

- source curve;
- effective edited curve;
- MIDI target spans;
- provenance;
- editable segments;
- ambiguous regions.

Pitch Coach scores the original performance unless the UI explicitly requests
a corrected-preview score. Correction consumes the effective curve.

## 9. Offline correction first

```text
source audio
+ centred APTF
+ edit layer
+ target (MIDI / scale+key / nearest semitone)
+ policy
-> formant-preserving shifter
-> new audio clip
```

Policy controls: target, strength, retune speed, preserve slides, preserve
vibrato, unvoiced passthrough, and later independent formant shift.

Evaluate PSOLA first for monophonic voice. Plain resampling is unacceptable
because it moves formants with pitch. Rendering must leave the original
untouched, create a new source/clip, preserve placement deliberately, be one
undoable transaction, retain a revisable recipe, provide A/B preview, and
never shift unvoiced consonants merely because a target exists.

## 10. Live auto-tune later

Live correction is an insert effect reusing the same target/policy model. It
requires declared lookahead, PDC, allocation-free processing, click-free
bypass/dry-wet, bounded detector handoff, and defined late/drop behaviour.
Do not ship it before offline correction passes listening and detector-error
tests.

## 11. Validation gates

Detector/extraction:

- clean voices across the supported range (currently about 65-1000 Hz);
- vibrato, slides, breaths, consonants, silence, and octave changes;
- different voices and Demucs stems with bleed/phase artefacts;
- no catastrophic octave-driven note creation.

Persistence:

- APTF survives reopen; absent/corrupt/wrong-version cache regenerates;
- Save As/copy-project deliberately preserves or rebuilds caches;
- edits survive reopen and re-analysis never silently deletes them.

Timeline/UI:

- move, trim, split, copy/paste, loop, tempo and sample-rate changes;
- alignment at all zoom levels;
- long clips do not flood IPC or block rendering.

Correction:

- unvoiced passthrough, click-free boundaries, preserved formants;
- explicit octave-error artefact tests;
- A/B and undo restore the original exactly;
- live latency is measured and declared.

## 12. Delivery order

1. **Landed:** detector, APTF persistence, Pitch Coach, scoring.
2. **Landed:** APTF segmentation to an undoable MIDI clip ("Extract melody to MIDI").
3. Arrangement pitch child lane.
4. Centred offline analysis.
5. MIDI target overlay.
6. Pitch-edit schemas, ops, persistence, trim/split/copy semantics.
7. Segment editor.
8. Effective-curve backend API.
9. Offline formant-preserving correction to a new clip.
10. Policy UI and revisable correction recipes.
11. Live insert after quality and latency gates.
12. Hard-tune and formant-shift extensions.

## 13. Non-goals

- polyphonic detection/correction;
- source pitch stored as MIDI control data;
- MIDI targets stored as measured APTF;
- destructive APTF editing;
- a pitch track detachable from its audio;
- first-version freehand per-frame editing;
- live auto-tune before credible offline correction.
