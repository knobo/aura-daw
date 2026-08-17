# Pitch correction (auto-tune)

**Captured 2026-08-17 from the owner**, right after driving the Pitch Coach
panel against a real microphone for the first time: *"Det at vi har så god
kontroll på pitch nå, er det noe vi kan bruke for å gjøre auto tune med
etter hvert?"*

Short answer: yes, and the part that is usually hardest to get right is
already done and measured. But detection is one third of auto-tune. The
other two thirds — resynthesis that moves pitch without wrecking the voice,
and a policy deciding what to correct towards and how hard — do not exist in
this repo at all. This doc is the staged path, not a promise of a release.

## What is already in place

Everything here is landed and measured; none of it needs redoing.

| Piece | Where | Why it matters for correction |
|---|---|---|
| YIN detector, 100 Hz frames | `src-tauri/src/audio/yin.rs`, `pitch.rs` | F0 + clarity + RMS + voiced per 10 ms hop, gated, median-smoothed, jump-limited |
| 8 kHz decimation | `src-tauri/src/audio/decimate.rs` | Allocation-free, filter state persists across chunks |
| Detection OFF the RT callback | `src-tauri/src/audio/pitch_thread.rs` | `PitchTap` (RT: decimate, hand over) → `PitchWorker` (YIN, gate, median). A live corrector needs exactly this split |
| Sidecar parity | `src-tauri/tests/pitch_sidecar_parity.rs` | Rust and `sidecars/hum_to_midi.py` agree to **3.3 cents** |
| Measured accuracy | Pitch Coach PROGRESS, R3 checkpoint | 0.4 cents median on a synthetic tone; **no octave errors in 1312 frames** of a sustained ~100 Hz vowel |
| Reference-melody derivation | Pitch Coach phase 3 (Tasks 12–16, not yet built) | "What note should this have been" computed once, in Rust |
| Insert chains + `HostRole::Effect` | Plan G1 (Tasks 1–4 landed) | The seam a live corrector plugs into |
| Plugin delay compensation | Plan G1 Task 6 | A corrector has lookahead; the mixer must compensate it |

The detector's honest limits are worth restating, because correction is far
less forgiving than a display: the R3 evidence is **one voice, one vowel,
one room**. An octave error that reads as a harmless blip in the trail
becomes an audible artefact once it drives a shifter.

## What is missing

### 1. Pitch shifting that keeps the voice

Detection says how wrong the note is. Correction has to move F0 while
leaving the formants — the thing that makes it sound like a person — where
they are. Nothing in the repo does this today. Options, cheapest first:

- **Resampling.** Moves formants with the pitch: chipmunk. Unusable for voice.
- **Time-domain PSOLA.** Good on a monophonic voice, modest CPU, needs
  reliable pitch marks (our per-hop F0 is a decent seed). The realistic
  first implementation.
- **Phase vocoder with formant preservation.** More robust across material,
  more artefacts on transients, more parameters to get wrong.
- **WORLD / STRAIGHT-style vocoder.** Best quality, heaviest, naturally
  offline. A Python sidecar is a legitimate host for this — the repo already
  ships Demucs and ACE-Step that way (`sidecars/`, NDJSON protocol).

This is shared work with the Tier 2 roadmap item *"Time-stretch /
pitch-shift of audio clips"*. Whichever lands first should own the shifter
and expose it to the other; two independent shifters would be a mistake.

### 2. A correction policy

The interesting half, and where AURA has something most tools do not.

- **Target.** A fixed scale/key (what a stock auto-tune does), the nearest
  semitone (what it does with no key set), or — uniquely ours — **the
  reference MIDI melody the Pitch Coach already scores against**. Correcting
  towards a melody the user wrote is a better product than correcting
  towards a key, and phase 3 computes that melody anyway.
- **Retune speed and strength.** Instant and total is the stylistic effect;
  slow and partial is the invisible fix. Both are wanted, and the panel's
  `pitchTolerance` preference is already the vocabulary for "how close
  counts".
- **What to leave alone.** Deliberate slides, vibrato, and scoops must
  survive, or every phrase sounds embalmed. The jump limiter in `pitch.rs`
  already distinguishes a real leap from a detector blip; the same idea
  applies to "this glide is intentional".
- **Unvoiced passthrough.** Consonants carry no pitch. Shifting them is how
  auto-tune gets its lisp. `PitchFrame.voiced` is the gate, and it is
  already trustworthy.

### 3. A latency budget

A live corrector needs lookahead, and the honest number comes out of
whichever shifter is chosen. Offline has no such constraint, which is one
more reason it goes first.

## Staged roadmap

**Stage 0 — measurement. Done.** The R3 numbers and the phase 2 panel. The
owner has now heard the detector track a real voice, and the panel exists to
show it. Nothing further needed here.

**Stage A — offline correction of a recorded take.** "Correct this take
towards the reference melody" as a render: produces a **new clip**, leaves
the original untouched, one undoable transaction. No RT discipline, no
latency budget, ear-checkable in one pass. Needs: a shifter (PSOLA first),
phase 3's reference melody, and a correction policy with two controls
(strength, retune speed). **This is the first thing to build**, and it is
worth building even if a live corrector never ships.

**Stage B — the policy surface.** Target selector (melody / scale+key /
nearest semitone), strength, retune speed, preserve-slides, preserve-vibrato.
The math stays in Rust (ADR 0006); the frontend is a small panel and a set
of preferences. Re-rendering with different settings must be cheap, which
argues for keeping the analysed pitch track on disk (Task 14 already does
this for scoring).

**Stage C — live corrector as an insert.** Only after G1's insert chains and
PDC are in. RT rules apply in full: no allocation in `process`, lookahead
declared as plugin latency so the mixer compensates it, dry/wet, and a
bypass that is click-free. The detector already runs off the callback, which
is the part people usually get wrong.

**Stage D — hard tune and formant shift.** Full quantise with zero glide
(the stylistic extreme), plus formant shift as a control of its own. Cheap
once Stage B and the shifter exist, and a real reason people pick a tool.

## Rulings needed before Stage A starts

1. **Where does a corrected take land?** A new clip on the same track, or a
   new take lane (which does not exist yet — Tier 2 "takes & comping")?
2. **Bake or keep the recipe?** Writing the analysed pitch track plus the
   correction settings means a take can be re-corrected later without
   re-analysing. Baking is simpler and loses that.
3. **Which shifter, and where does it live?** Rust-side PSOLA, a Rust crate,
   or a Python sidecar. Sidecar precedent exists and offline work fits it;
   a live corrector later would need the Rust path anyway.
4. **Does correction go through the op log?** A render that produces a clip
   should (it is a document mutation). The analysis pass should not.

## Non-goals for now

- **Polyphonic correction** (Melodyne DNA territory). Out of scope at any
  stage in this doc.
- **Correcting anything but a monophonic vocal.** The detector is tuned for
  voice; instruments with strong inharmonicity are a separate question.
- **A live corrector before the insert chain exists.** Stage C depends on
  G1, and jumping it would mean inventing a second effect seam.

## Pointers

- `docs/superpowers/specs/2026-08-16-pitch-coach-design.md` — the detector's
  design and the owner rulings R1–R6 behind it.
- `docs/superpowers/plans/2026-08-16-pitch-coach-PROGRESS.md` — the R3
  measurements, including the two findings that shaped the panel and would
  shape a corrector's UI too.
- `docs/backlog/insert-fx-sends-sidechain.md` + the G1 plan — the insert
  seam Stage C needs.
- `sidecars/hum_to_midi.py` — the constants the Rust detector copies
  verbatim, and the parity test that keeps them honest.
