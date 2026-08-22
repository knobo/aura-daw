# Backlog — insert FX, sends, PDC (Plan G1)

Plan: [`2026-08-16-plan-g1-insert-fx-pdc.md`](../superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md)
— implement that. **Do not start G2.**
Product cut: [`insert-fx-sends-sidechain.md`](insert-fx-sends-sidechain.md).
Handoff: [`g1-insert-fx.md`](../handoff/g1-insert-fx.md).

Tasks 1–6 have landed; see [`../LANDED.md`](../LANDED.md).

## Open — Tasks 7–10

**HOLD** until an automation/undo leftover is done (owner steer,
2026-08-18). Then: rebuild/offline wiring, IPC+UI, handoff.

- **Do not jump to Task 9 (UI) before Task 7 wires the mixer into
  rebuild.** `compile_inserts` landed in Task 5 but nothing calls it, so
  an insert is still silent end-to-end — the G-11 note in the handoff.
- **Task 6's known gap for Task 7 to pick up:** PDC is applied after
  inserts but before the fader, so once wired, a track's automation ramps
  will read the wrong playhead position by its own PDC delay. Documented
  in `pdc.rs`'s module doc.
- `RtTrack::pdc` is still `None` everywhere in production.

## Deferred minors

Listed in [`g1-insert-fx.md`](../handoff/g1-insert-fx.md).

## Ear-check

Insert FX is **not** ear-checkable until Task 7 wires `compile_inserts`
into `engine::rebuild`.
