# Pitch as data: melody extraction and the pitch track

**Captured 2026-08-17 from the owner**, in the same session as the Pitch
Coach ear-check: *"Så ønsker jeg at vi skal kunne ta et vokal (eller
instrument) lane, og generere en pitch strøm som vi kan bruke som input til
dette. Jeg tenker kanskje at vi kan generere et midi track som vi kan bruke
til input. Eller skal vi ha et spesialisert pitch-track? … Eller er
midi-track med noen kontroll-punkter godt nok?"*

## The answer in one paragraph

**Both, and neither is new.** Notes belong in a MIDI clip — the repo already
produces them (`sidecars/hum_to_midi.py`) and the Pitch Coach already reads
them as its reference. The continuous curve belongs in the `APTF` pitch
track that Pitch Coach **Task 14 already specifies**, next to the waveform
tiles, and Task 14 already has the command that makes one from an existing
clip (`pitch_analyze_clip`). So this is not a new subsystem — it is two
existing halves that need to be pointed at each other, plus one small new
feature ("extract melody from this clip") and four decisions taken before
`APTF` version 1 ships.

**Do not encode the curve as MIDI control points.** It is the one option
here that looks cheap and is not:

- Pitch bend is **relative to a synth's bend range**, so writing analysis
  into it bakes a playback setting into a measurement.
- It is 14-bit, per-channel, and has nowhere to put **clarity** or
  **voiced** — the two fields that make the curve trustworthy. `voiced` in
  particular is what stops a corrector from pitching consonants.
- It is a transport format for driving synths. If the curve ever needs to
  *drive* something in AURA, the repo already has a better mechanism for
  that: the modulation system (ADR 0008), where a pitch track becomes a new
  **source kind**, not a new file format.

## Two representations, one each for what it is good at

| | MIDI clip | Pitch track (`APTF`) |
|---|---|---|
| Resolution | semitone × tick (PPQ 960) | 10 ms × cents |
| What it keeps | the **intent** — which note, when | the **performance** — vibrato, scoops, drift, breaths |
| What it throws away | everything between the semitones | which note it was *meant* to be |
| Editable | yes, piano roll, today | not yet — see the staging below |
| Document state? | **yes** — op log, undo, persisted in the project | **no** — derived cache, like waveform tiles |
| Consumers | Pitch Coach reference melody, instruments, MIDI out | scoring, pitch correction, modulation source |

The split matters because of what *document state* costs here: a 100 Hz
float stream per take inside `project.json` or the journal would be a
schema version, an undo semantics question, and a persistence cost, for data
that is reproducible from the audio in a second. Derived-and-cached is the
honest classification, and the repo already has that shelf.

Note this asymmetry, which is the real reason both exist: **you cannot score
a performance against a curve.** "Did you hit the note" needs a note — a
span with a nominal pitch. And you cannot correct a performance from notes
alone: the delta you apply is continuous, and what you must *preserve*
(a deliberate slide, vibrato) only exists in the curve.

## What this unlocks, cheapest first

1. **"Extract melody from this clip" → a MIDI clip.** The smallest
   valuable piece and the direct answer to the owner's ask. Analysis exists
   (`PitchAnalyzer`), the note segmentation exists (`hum_to_midi.py`), and
   the result is an ordinary MIDI clip that lands through the op log like
   any other. It immediately gives the Pitch Coach a reference melody
   derived from a take you already sang, instead of one you had to draw.
   **Build this first**, independent of correction.
2. **The pitch track as the source curve for offline correction** — Stage A
   of [`pitch-correction-autotune.md`](pitch-correction-autotune.md). Needs
   the curve, a target (a MIDI melody, from step 1 or drawn), and a shifter.
3. **The pitch track as a modulation source** — sing a filter sweep, drive a
   synth's pitch from your voice. The modulation system is landed and its
   finished-system path is design §8; this is a new source kind inside it,
   not a parallel mechanism.

## Decisions needed before `APTF` v1 ships (Task 14)

This is the time-sensitive part: the format is specified but not built, so
these are free now and a migration later.

**(a) The live-folded track is not the best possible track.** Task 14 folds
pitch in the recorder's writer thread, which means the **causal** median —
the live detector lags `hum_to_midi.py` by exactly the median half-kernel,
20 ms, and that is documented and deliberate (a live detector cannot look
ahead). An offline pass over the same audio can use the **centred** median
and is strictly better. Scoring does not care. Correction does.
**Recommendation:** `APTF` records *how* it was made — one provenance byte,
`live-causal` vs `offline-centred` — so a consumer can tell whether it has
the good curve, and Stage A can re-analyse when it does not. One byte in a
format that has not shipped, versus a v2 later.

**(b) Store the smoothed curve only, or the raw F0 too?** Scoring wants
smoothed; correction might want raw. **Recommendation:** keep v1 as
specified (`hz`, `clarity`, `rms` per hop) and let Stage A re-analyse
offline when it needs more. Do not widen a format on speculation — with (a)
in place, a consumer knows when to re-analyse.

**(c) Keyed by clip id, as specified.** `cache/pitch/<clipId>.bin` matches
`Store::cache_dir_for`, which keys waveform tiles the same way. Keying by
`ContentId` (ADR 0004) would avoid analysing the same audio twice for a
copied clip, but the tile cache already accepts that cost and consistency
is worth more than the saving. **Recommendation:** follow the tiles.

**(d) Editing the curve is deferred, explicitly.** Dragging a note's pitch
Melodyne-style turns derived data into document state and pulls in the op
log, undo, persistence and a schema version. Nobody has asked for it yet.
Read-only until they do — and when they do, that is its own planned round,
not a patch.

## Non-goals

- **Polyphony.** The detector is monophonic. An "instrument lane" works
  only while one note sounds at a time; a strummed guitar or a piano chord
  is out of scope at every stage here.
- **Range.** The detector's effective floor is **65.04 Hz** (documented —
  `tau_max` truncates, and the Python sidecar behaves identically) and its
  ceiling is 1000 Hz. A bass below C2 and the top of a violin's range fall
  outside; parity with the sidecar outranks widening the range.
- **A third track kind in the timeline.** A pitch track belongs *to* an
  audio clip, not beside it. No "pitch lane" row, no new `TrackKind`.

## Pointers

- Task 14 in [`2026-08-16-pitch-coach.md`](../superpowers/plans/2026-08-16-pitch-coach.md)
  — the `APTF` layout, `pitch_analyze_clip`, and the recorder fold. Read the
  decisions above before implementing it.
- [`pitch-correction-autotune.md`](pitch-correction-autotune.md) — the
  consumer that makes the provenance question matter.
- [`2026-08-16-pitch-coach-PROGRESS.md`](../superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md)
  — the causal-vs-centred median finding, and the R3 numbers behind any
  claim about accuracy.
- `sidecars/hum_to_midi.py` — the existing audio→MIDI path and the note
  segmentation step 1 reuses.
