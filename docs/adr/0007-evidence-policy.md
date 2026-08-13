# ADR 0007 — Evidence policy: benches in-repo, cited claims, marked corrections

## Status

**Proposed** (2026-08-13). Distilled from `docs/CORE-REDESIGN-ROUND-2.md`
(header, §0, §6) — itself DRAFT; the owner accepts this ADR, not its authors.

## Context

Round 1 compressed the dossiers' conclusions and dropped their caveats, and
the caveats turned out to be load-bearing: several "facts" about the codebase
were wrong (74 commands vs the real 66; the decode-cache failure mode; the
slot-change rationale), and measured claims lived in one agent's conversation
rather than anywhere reproducible. The adversarial review of 2026-08-13
could check the reasoning only because the documents preserved it — that
property is worth making policy.

## Decision

- **Benchmark crates live in the repository**, under `benches/`
  (`benches/pdsbench/`, `benches/bulkbench/` since round 2). Byte counts are
  hardware-independent; timings are quoted ±40 %.
- **Measured claims cite their bench.** A number in a binding document
  (ARCHITECTURE, SCALABILITY, an ADR) that came from a measurement names the
  crate that reproduces it; "inaudible" and its cousins are the *names* of
  property tests with numeric bounds, never definitions (e.g. the
  section-table bound: < 64 samples against numeric integration,
  round-2 §3.4).
- **Corrections to dossiers are marked, never silent.** When review overturns
  a research document's claim, the dossier gets an explicit corrections block
  (as `06-time-travel-storage.md` and `10-aura-current-state.md` now carry)
  and the superseding document records the reversal in an
  overturned-decisions register (round-2 §0.1) — because the point of these
  documents is that the reasoning can be checked, not that they look
  consistent.
- **Facts about the codebase are checked at a commit.** Claims like command
  counts or side-channel inventories state the commit they were verified
  against (round 2: `35d801d`); an unverified count is labeled as such.

## Consequences

- Anyone can re-run the storage numbers before re-litigating ADR 0005; the
  agent-editing gesture-mix question (round-2 §10.2) becomes a day's work
  instead of a debate.
- Documents grow slower and read more honestly; drafting cost is paid in
  exchange for reviewability — the review that produced round 2 is the
  existence proof that this pays.
- A future round that edits a dossier in place without a marked correction
  block is violating this ADR, and the violation is detectable in diff
  review.
