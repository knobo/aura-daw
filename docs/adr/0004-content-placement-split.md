# ADR 0004 — Content/placement split

## Status

**Accepted** (2026-08-13, project owner). Distilled from `docs/CORE-REDESIGN-ROUND-2.md` §5
(DRAFT); the owner accepts this ADR, not its authors.

## Context

Today's clip conflates *what sounds* with *where it sounds*. Pattern
instancing (SCALABILITY §3's FL model), takes/comping, and per-voice
modulation all need those separated, and the split is file-format
irreversible: deferring it means rewriting every placement in every saved
project later. Round 1 gave this highest-blast-radius item two paragraphs;
round 2 fixes the schema at field level.

## Decision

A **placement** (`ClipId`) references a **content object** (`ContentId` — a
new id family, ADR 0001). Field-level split (round-2 §5):

| Placement | Content |
|---|---|
| position (`startTicks`/`startSamples`) | note events (AMEV ref + `next_note_id`) |
| length (may crop content) | audio source reference (`SourceId`) |
| lane/track reference | loop/native length, name |
| mute, color, name-override | |
| transpose/velocity offset (MIDI); gain, fades (audio) | |

- **Audio clips are content-backed too** — a thin content object wrapping a
  `SourceId` — so the placement schema is uniform and SCALABILITY §3's
  `contentRef` union survives. Instancing is free for MIDI, a byproduct for
  audio.
- **The lane reference lands now** [F]: placements reference a `LaneId`;
  every track gets one default lane. This is the deferred take feature's
  file-format hook — only the indirection ships; takes, lanes-UI and comping
  stay deferred (round-2 §5, O-10).
- Existing `midiClips` rows migrate mechanically in the same v3 bump as
  ADR 0002 (one content object per clip, one placement referencing it).
- The sounding-instance address is recorded now:
  `(ClipId, ContentId, NoteId)` resolves the document note; the voice is
  Slot-tier. `LineageId` stays reserved/unpopulated until the takes round
  (round-2 O-3): unpopulated it retrofits byte-identically
  (`lineage := object_id`); mis-populated it is unrecoverable.

## Consequences

- Editing shared content updates every placement — the channel-rack workflow
  arrives as a data-model property, not a second engine.
- Every placement carries one extra reference (lane) from v3 onward; the
  alternative was a whole-file rewrite when takes ship.
- Automation-lane identity (`target_node` strings that cannot address
  faders) remains a live gap, explicitly assigned to the node-graph round —
  a decision, not an omission.
