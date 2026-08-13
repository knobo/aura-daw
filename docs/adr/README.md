# AURA — Architecture Decision Records

Decisions distilled from `docs/CORE-REDESIGN-ROUND-2.md` (2026-08-13, DRAFT)
and the research dossiers in `docs/research/`.

**Numbering:** `NNNN-<slug>.md`, monotonically increasing, never reused.
**Status convention:** every ADR carries `Proposed` / `Accepted` /
`Superseded (by NNNN)`. All seven below are **Proposed** — round 2 is a
DRAFT, and acceptance is the owner's call, recorded by editing the Status
line, not by the authors.

| # | ADR | One line |
|---|---|---|
| 0001 | [Two-tier identity](0001-two-tier-identity.md) | `ProjectId` families + `Slot`, never-reused ids, persisted per-content note-id watermark; no Handle tier. |
| 0002 | [Time model](0002-time-model.md) | `Ticks`/`Samples` newtypes, anchors as API arguments, supertick integer-period tempo + meter map as project v3, section table with a numeric error bound. |
| 0003 | [The mutation channel](0003-mutation-channel.md) | Closure transactions over one `Session`; engine submits as `Actor::Engine`; gesture begin/end IPC; persisted versioned op log that stays dark until the side-channel inventory is total. |
| 0004 | [Content/placement split](0004-content-placement-split.md) | Field-level split, `ContentId` family, lane reference lands now (takes later). |
| 0005 | [History storage](0005-history-storage.md) | Summarising COW B-tree in memory, version-graph retention over limbo refcounts, replay-only nodes for bulk/scattered ops, janitor thread. |
| 0006 | [UI stack posture](0006-ui-stack-posture.md) | Tauri now; thin-renderer rule; one texture-targeting renderer interface (WebGL2 Linux ceiling); three measurements gate the render architecture. |
| 0007 | [Evidence policy](0007-evidence-policy.md) | Benches live in `benches/`, measured claims cite them, corrections to dossiers are marked, never silent. |
