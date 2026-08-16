# ADR 0005 — History storage: COW B-tree, version-graph retention, replay-only nodes

## Status

**Accepted** (2026-08-13, project owner). Distilled from `docs/CORE-REDESIGN-ROUND-2.md`
§2.3/§6; accepted together with the resolution of all six owner decisions from the consistency review.

## Context

Browsable history at 10⁶–10⁷ events needs any past revision cheap to
materialize. Dossier 06 measured the candidates; SCALABILITY §3 had specified
a flat chunk rope. Round 1 also proposed limbo-plus-refcount retention for
deleted objects, which as written specified a leak (no decrement path under a
forever-log) and violated dossier 07 rules 10/17/18 (plugin instances must
never die by refcount) — overturned as round-2 O-14. Bulk-op retention was an
open question until measured.

## Decision

- **The in-memory session structure is a summarising COW B-tree**, replacing
  the flat chunk rope (which the AMEV file format does not depend on).
  Evidence, reproduced byte-exact in `benches/`: 4 114 B vs 23 969 B retained
  per point-edit version at 10⁶ events — Θ(log N) vs Θ(√N) — and sorted
  insert (what a piano roll *is*) 114× faster than the mutable baseline while
  retaining history (round-2 §6). The corrected framing stands: the rope's
  per-chunk summaries *can* answer viewport queries; the tree wins on
  retention and insert, not on a false "cannot do at all".
- **Retention is the version graph, not a registry**: a deleted object simply
  stops being referenced by the current version while history versions keep
  it alive — "undo never un-allocates an ID … no resurrection problem"
  (dossier 06 §1.1). No limbo registry, no per-object refcounts. Plain model
  objects only: plugin instances follow their format's state machine and the
  history retains state *blobs* (dossier 07 rules 10/17/18). The in-memory
  window is bounded (bytes ceiling, steps floor); the persisted op log keeps
  semantics forever without keeping live objects.
- **Replay-only history nodes are first-class**: bulk or scattered ops store
  op + inverse payload instead of a materialized snapshot. Measured need:
  1 000 scattered notes retain 2.3 MB, humanize-10k retains 15.3 MB — and an
  agent-driven editor makes bulk transforms the common case. Measured cost:
  ~600 µs replay for a 100k-note quantize (hover-safe). Random ops must be
  seeded (PRNG seed stored) so their inverses are O(1); ops minting
  non-deterministic ids (paste/duplicate) carry the minted ids in the op
  payload or are excluded. Caps are absolute per-op-class and **measured**
  (10 k-gesture weighted simulations, `benches/bulkbench/RESULTS.md`):
  point edits retain ≤ 8 KB (p99 measured ~5 KB); bulk capped at 256 KB;
  replay-only kicks in at **64 KB own-created bytes** (256 KB measured as
  a no-op). Replay-only bounds node charges and saves agent iteration
  bursts; the 512 MB budget is defended by eviction (~54 700 HUMAN /
  ~6 550 AGENT steps). Transpose/velocity-class gestures MUST route
  through placement offsets — the measured 5.4× lever (round-2 §6).
- **The janitor thread stays mandatory** — retire-queue drops happen off-RT
  (measured worst drop 83.8 ms ≈ 32 buffers; dossier 06).

## Consequences

- Implementation (Plan F, 2026-08-14): landed at per-clip Arc granularity
  with the within-clip tree deferred to the note-delta-op round — see the
  Plan F handoff, ruling F-1.
- Deep undo on million-event material costs O(changed nodes), and the
  falsifier-driven "weighted mean" threshold is replaced by decidable caps.
- Delete-then-undo (gate test 2) preserves identity and inbound references by
  construction; the RT schedule remains an independent strong owner of what
  it plays.
- If the tree does not land, a separate refcount registry becomes necessary
  again — the reconciliation in round-2 §2.3 is contingent on §6 shipping.
