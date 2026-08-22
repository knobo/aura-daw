# Backlog — MIDI in/out

Handoffs: [`midi-output.md`](../midi-output.md),
[`midi-out-note-channel.md`](../handoff/midi-out-note-channel.md).
Product docs: [`hardware-midi-io.md`](hardware-midi-io.md),
[`midi-launch.md`](midi-launch.md).
**Read [`external-instrument-return.md`](external-instrument-return.md)
before adding "hidden tracks" or a PipeWire graph orchestrator** — it is
MIDI-out's missing half.

Per-track/per-clip routing and eight follow-up fixes have landed; see
[`../LANDED.md`](../LANDED.md).

## Open

- **MIDI-out to Hydrogen bug** — found during the Composer H1 ear-check
  (2026-08-18), not yet filed or scoped. Needs its own small PR; do not
  fold it into G-series or Composer work.
- **Hardware GATE / sustain** — [`midi-launch.md`](midi-launch.md).

## Ear-checks

MIDI note-out / Hydrogen / keyboard record (Track B). An ear-check on a
real drum machine is still owed for the eight fixes in PR #77.
