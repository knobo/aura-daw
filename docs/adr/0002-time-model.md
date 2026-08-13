# ADR 0002 — Time model: newtypes, unanchored durations, integer-period tempo

## Status

**Proposed** (2026-08-13). Distilled from `docs/CORE-REDESIGN-ROUND-2.md` §3
(DRAFT); the owner accepts this ADR, not its authors.

## Context

AURA is statically domained: MIDI and musical positions are ticks, audio
anchors and engine positions are samples. Round 1 proposed one runtime-tagged
time enum and a stored-anchor duration type; review overturned both (O-4,
O-5): a runtime enum is strictly less safe here than compile-time types
(Ardour's cross-domain `operator<` needs the process-global RCU map its own
source warns about — dossier 02), and a stored anchor goes stale the moment a
property-addressed op moves its owner. Separately, the shipped v2
`tempoMap: [{tick, bpm: f64}]` format exists in real files, and today's code
silently clobbers the user's time signature to 4/4 on every save (dossier 10
trap 3).

## Decision

- **Two compile-time newtypes** — `Ticks(u64)` and `Samples(u64)`, zero-cost,
  no `Ord` across domains, no ambient global map. Conversion is a named call
  on the tempo map (`map.samples_at(t: Ticks) -> Samples`); mixed comparison
  is `cmp_in(&self, other, &TempoMap)` (round-2 §3.1).
- **Durations are unanchored distances**; the anchor is an API argument
  (`map.distance_samples(d: Ticks, at: Ticks)`), never stored state
  (round-2 §3.2).
- **Tempo is an integer period in superticks per quarter note**, supertick =
  1/508 032 000 s (Ardour's superclock constant, 2¹⁰·3⁴·5³·7²). Sample-rate
  independent; a typed BPM is quantized once at entry (error ~1.4×10⁻⁷ BPM at
  120.5) and exact thereafter; ramps carry `period_start`/`period_end`,
  interpolated linear-in-period. This ships as a **v2→v3 project migration of
  shipped data**, with `project.json.v2.bak` per the established chain — not
  as a green-field choice (round-2 §3.3, O-9).
- **The meter map joins the same v3 bump**: minimal
  `meterMap: [{tick, num, den}]`, default `[{0,4,4}]` — bar numbers are
  undefined without it and the current 4/4 clobber is active data loss.
- **The section table** (precomputed constant-tempo segments) carries a
  numeric error bound, not an adjective: curved-ramp subdivision is
  property-tested to **< 64 samples (~1.3 ms at 48 kHz)** deviation from
  numeric integration; the subdivision rule is versioned format semantics;
  edits invalidate only the suffix from the edited tick (round-2 §3.4).

## Consequences

- Mixed-domain collections are compile errors; wrong-domain bugs die at build
  time. If per-object domain locking is ever required, the upgrade is
  enum-over-newtypes at placement fields only.
- The wire keeps flat numbers with domain in the field name
  (`startTicks`, `positionSamples`); no serde enum encodings.
- The frontend's four coordinate systems collapse onto one shipped bijection
  (the section table pushed as data), killing the constant-tempo-grid
  snapping bug (round-2 §3.6).
- Gate tests: tempo round-trip across save/load/rate-change, lossless v2→v3
  migration against a fixture corpus, and the 64-sample section-table bound
  (round-2 §7).
