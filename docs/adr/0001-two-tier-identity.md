# ADR 0001 — Two-tier identity: ProjectId families and Slot

## Status

**Accepted** (2026-08-13, project owner). Distilled from `docs/CORE-REDESIGN-ROUND-2.md` §2,
which is itself DRAFT; the owner accepts this ADR, not its authors.

## Context

The current code mixes UUID strings, dense engine slots, and ad-hoc indices;
`Store::free_slot` reuses slots, and the async rebuild window lets a freed
slot alias a still-playing deleted track (round-2 O-13). Round 1 proposed a
third, per-run `Handle` tier; review found it had no consumer, cited a
nonexistent dossier, and per-run generational keys are "precisely the wrong
semantics for undo" (dossier 04 §4.3). Blender's ID-reuse corruption
(#149899) shows why identifiers must never be recycled, even though its
snapshot-diff mechanism is one we reject.

## Decision

Exactly **two identity tiers** (round-2 §2):

- **`ProjectId`** — forever, never reused; used by the project file, op log,
  MCP, scripts, and all inter-object references. Typed per family:
  `TrackId`, `ClipId`, `ContentId`, `NoteId`, `TakeId`, `LaneId`,
  `PluginInstanceId`, `SourceId`, `LineageId` (reserved, unpopulated until
  the takes round). Family ids are 128-bit UUIDs.
- **`Slot`** — lives for one compiled graph only; pure derived state assigned
  at graph-compile time; used by the RT schedule, param tables, meters.
  `free_slot` and slot reuse go away; param table and meter routing are
  versioned with the graph snapshot (round-2 §2.4).

A `Slot` never reaches the project file; a `ProjectId` never reaches the RT
thread. There is **no Handle tier** — it returns only as an interning
optimization behind a measurement, if ever (round-2 O-2).

**Note ids** are the one sequential space: `MidiNote` gains a `u32` id unique
within its content object — addressed `(ContentId, NoteId)` — with the
allocator **persisted** as a per-content `next_note_id` watermark in the AMEV
chunk header. Recomputing `max+1` on load would reuse the highest deleted
note's id after save/reload, violating never-reuse against a persistent op
log (round-2 §2.1). Allocation happens inside the transaction (ADR 0003), so
multi-actor races cannot mint the same id.

## Consequences

- Passing a `ClipId` where a `TrackId` is expected becomes a compile error.
- Wholesale slot reassignment per rebuild obliges graph-versioned param
  tables and meter routing, and retires the `MAX_TRACKS = 64` ceiling.
- Split/merge/copy of content must record id partitions/remints in the op,
  because ops in the log are never retargeted.
- The document `NoteId` is not the CLAP per-voice note id; values above
  `i32::MAX` must never cross the CLAP boundary.
- Cross-project merge would require reminting `(ContentId, NoteId)` pairs;
  that operation is explicitly unsupported today.
