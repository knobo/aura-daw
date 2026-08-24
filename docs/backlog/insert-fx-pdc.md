# Backlog — insert FX, sends, PDC (Plan G1)

Plan: [`2026-08-16-plan-g1-insert-fx-pdc.md`](../superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md)
— implement that. **Do not start G2.**
Product cut: [`insert-fx-sends-sidechain.md`](insert-fx-sends-sidechain.md).
Handoff: [`g1-insert-fx.md`](../handoff/g1-insert-fx.md).

Tasks 1–8 have landed; see [`../LANDED.md`](../LANDED.md). Task 7
(rebuild wiring) landed in PR #90; Task 8's OFFLINE half landed with
Plan G2 in PR #109 — the bounce now walks insert chains and the routing
graph through the same compile step the engine uses.

## Open — Tasks 9–10

What is left is Task 9's insert UI polish and Task 10's handoff. The
audio path itself is live in both the engine and the bounce.

- **Task 6's known gap is closed:** the fader/pan ramp lookup subtracts
  the strip's own PDC delay (`FaderCtx::pdc_delay`), so a
  latency-compensated track no longer reads automation for the wrong
  playhead position.
- `RtTrack::pdc` and `RtTrack::master_pdc` are both populated in
  production now, from `audio::bus::compile_routing`.

## Deferred minors

Listed in [`g1-insert-fx.md`](../handoff/g1-insert-fx.md).

## Ear-check

Owed, not blocked: load an effect on a track and confirm it processes,
live and in an export.
