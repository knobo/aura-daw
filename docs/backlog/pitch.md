# Backlog — pitch (Coach, extraction, correction)

Progress file for the Coach track:
[`2026-08-16-pitch-coach-PROGRESS.md`](../superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md)
— **read that rather than this file** for whatever is next in Pitch
Coach; this page is not kept current for that track.
Architecture: [`pitch-track.md`](../pitch-track.md).

Phases 1–3 and melody extraction have landed; see
[`../LANDED.md`](../LANDED.md).

## Open

- **Sing-along from any song** — the 4-step flow (import → split stems →
  extract melody to MIDI → sing in Pitch Coach) is complete. What remains
  is the Arrangement pitch child lane, offline pitch correction, and
  vocal calibration. [`pitch-track.md`](../pitch-track.md).
- **Pitch as data / Arrangement pitch lane** — the persistent child lane
  on audio tracks. [`pitch-track.md`](../pitch-track.md), and read
  [`pitch-as-data.md`](pitch-as-data.md) before Task 14.
- **Pitch correction / auto-tune** — an owner ask from 2026-08-17, NOT
  part of the Pitch Coach plan. Detection is done; a formant-preserving
  shifter and a correction policy are not. The offline-first staged path
  and the four rulings it needs before Stage A are in
  [`pitch-correction-autotune.md`](pitch-correction-autotune.md). **Do
  not start Stage C (live insert) before G1 inserts + PDC.**

## Ear-checks

- **R3 is done** — `cargo run --example pitch_check` reproduces it; a
  sustained vowel gave no octave errors in 1312 frames.
- **Phase 2 was ear-checked 2026-08-17** and found a real bug (the lane
  was not drawn while stopped, so the reference notes seemed to appear
  only on record). Fixed in the same PR — but the fix itself has not been
  heard, so one more pass with a microphone is owed.
