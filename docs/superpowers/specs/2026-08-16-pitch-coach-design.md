# Pitch Coach — live vocal pitch feedback against a MIDI reference

**Status:** design, pending owner approval (2026-08-16).

Rulings R1–R6 in §2 are the owner's, recorded from the brainstorming round.
Everything else is the implementer's, taken under R6.

Supersedes nothing. Extends the recording path (`audio/engine.rs`) and adds
one new control-plane subsystem. It does not change any existing command,
event, or v1 schema.

---

## 1. Context

### 1.1 Why this feature exists

A DAW is built for people who already hit the note. The failure mode at home
is take 14, still not knowing whether you sang flat — you find out on mix
day. The existing tools split cleanly: Melodyne and VariAudio *repair* pitch
afterwards (destructive, expensive, and you learn nothing), while SingStar
and Yousician *tell you while you sing* but cannot record a take you can
use. Nobody puts both in the same lane.

AURA already ships hum-to-MIDI. That closes a loop no other DAW can copy
without building the same thing: hum a melody → it becomes a MIDI track →
sing against your own melody → record → see which notes slipped → punch in
on exactly those. That is a songwriting workflow, not a widget.

It is not vocal-only. Everything monophonic gets the same benefit: trumpet,
sax, violin, fretless bass, whistling. Intonation practice for strings is
the identical problem.

The under-rated payoff is emotional. The dominant feeling while tracking
vocals is uncertainty; a lane that says "that one was fine" makes recording
less frightening.

### 1.2 What the engine looks like today

- **All audio is native Rust via cpal.** No Web Audio, no AudioWorklet, no
  `getUserMedia` anywhere in `src/`. ADR 0006 (thin renderer) forbids new
  authoritative state, business logic, or time math on the frontend.
- **The input stream only exists while recording.** It is opened inside
  `EngineControl::open_capture_group` (`src-tauri/src/audio/engine.rs:1788`,
  `device.build_input_stream` at `:1878`) and torn down on stop —
  `InputBundle` (`:257`) owns `_stream`. The `monitor`/`arm` machinery at
  `:299–551` is **MIDI** monitoring, not audio.
- Input meters are computed inside the input callback (`InputCb::capture`,
  `:672`) and pushed to a meter ring. That is the existing precedent for
  "RT computes a cheap scalar → wait-free ring → control thread batches at
  60 Hz → non-reactive frontend bus → canvas rAF".
- `midi::schedule::clip_events(clip, &TempoMap)`
  (`src-tauri/src/midi/schedule.rs:37`) already returns `AbsNoteEvent
  { sample, key, velocity }` — absolute-sample note edges. **The reference
  melody is free.**
- `sidecars/hum_to_midi.py` contains a working YIN with tested constants:
  `TARGET_SR = 8000`, `FRAME_HOP_S = 0.010`, `FMIN/FMAX = 65/1000`,
  `YIN_THRESHOLD = 0.20`, `RMS_GATE_REL = 0.05`, window `w = 2 * tau_max`.
  Its `--self-test` asserts better than 25–30 cents.

### 1.3 The gap this design closes

The owner asked for three modes. They look like three features and are one:

| Mode | Transport | Mic open | Written to disk |
|---|---|---|---|
| Record | rolling | yes | yes |
| Rehearse (hold) | rolling | yes | **no** |
| Tuner (toggle) | **stopped** | yes | no |

The microphone must be open in all three. Today it is open in none but the
first. **So the core of this design is decoupling input-stream lifetime from
recording lifetime.** Everything else follows from that.

## 2. Owner rulings

- **R1 — both modes, one view.** The same note lane serves practice (no
  recording, tuner, repetition, score) and recording (pitch track stored on
  the clip, inspectable afterwards).
- **R2 — all four feedback kinds ship:** cents deviation over time, per-note
  and total score, onset/timing, stability/vibrato.
- **R3 — full live path (approach A), one PR, three internal phases**, with
  an owner checkpoint after phase 1 (backend + numbers) before UI is built.
- **R4 — all audio processing happens in Rust.** YIN, filtering, gating,
  scoring and time math are backend. The frontend is minimal and renders
  pushed state only.
- **R5 — rehearse-hold is both a held key (`H`, configurable) and a large
  press-and-hold button in the panel**, mirroring each other.
- **R6 — the microphone opens only on an explicit listen toggle, or while
  the Pitch Coach panel is open.** Not on track arm. Remaining design
  decisions are the implementer's, validated by the owner driving the app.

## 3. Architecture

Four units, each independently testable.

```
cpal input callback  ──decimate 48k→8k──▶ [rtrb ring] ──▶ pitch thread (YIN)
        │                                                        │
        └──▶ record sinks (existing)                             ▼
                                                        [PitchFrame ring]
                                                                 │
                                            control plane ◀──────┘
                                          /            \
                              Channel<PitchFrameBatch>  scorer (on demand)
                                          │                     │
                                    frontend bus          PitchScoreReport
                                          │                     │
                                    PitchCoach canvas ◀─────────┘
```

### 3.1 InputHub — decoupling the stream from the take

*Ownership zone: `audio/`. Touches the most sensitive file in the repo.*

`open_capture_group` splits in two. A new `InputHub` owns one cpal input
stream per device and refcounts its consumers:

- `RecordSink` — today's per-armed-track `rtrb::Producer<f32>`, attached on
  `start_recording`, detached on stop.
- `PitchTap` — exactly one, fed the mono sum of the hub's captured
  channels (§3.2 step 1). A per-channel selector is out of scope; a singer
  on a stereo interface is on one mic either way.

The hub opens when at least one consumer wants it, and closes when none do.

**The stream is rebuilt whenever the consumer set changes, not mutated in
place.** Attaching record sinks to a running cpal callback would need a
command ring into the callback plus a retire ring back out (the
`GraphPtr`/`retire_tx` pattern `OutputCb` already uses) so the RT thread
never allocates or drops. That is the correct mechanism and also the most
expensive change to the most sensitive file in the repo. Since every
transition — listen on/off, record start/stop — happens on the control
thread and never per block, closing and reopening the stream is equivalent
in behaviour and far cheaper to get right.

The accepted cost is a few milliseconds of pitch blackout at record start,
where today there is a full stream open anyway. If that blip proves
annoying in use, the command-ring version is the upgrade path and nothing
above it has to change.

**Invariants that must not regress** (each gets a test):

- `split_record_targets` semantics: audio targets plus one MIDI target.
- The sample-exact silence-debt overflow policy (`owed`) — takes never
  shrink; overflow bumps `shared.xruns`.
- Count-in: `countin_left`/`countin_beat` and
  `arm_pending_after_countin()` (`:1771`) are untouched.
- Device format gate: still F32-only, `rec_ch = in_ch.min(2)`, device rate,
  no input resampling on the record path.

**Deliberate consequence:** the hub decides device and rate at open time,
not at record time. If the selected input device changes while the hub is
open, the hub closes and reopens; any attached record sink forces the change
to be rejected instead (you cannot switch input device mid-take).

**Free side effect:** input metering works on a listening hub, so levels are
visible before you press record. Worth having on its own.

### 3.2 Pitch analysis — `audio/pitch.rs` (new)

The input callback stays boring. Under CONTRIBUTING rule 1 it does only:

1. Sum to mono, 80 Hz one-pole high-pass.
2. Decimate 48 kHz → 8 kHz through a fixed 6× FIR (pre-computed
   coefficients, no allocation, bounded loop).
3. Push into a `rtrb` ring.

A dedicated **pitch thread** (not RT, ordinary priority) drains the ring and
runs YIN. At 8 kHz, `tau_max = 8000/65 ≈ 123`, so the window is `2*tau_max ≈
246` samples (≈31 ms) and naive O(N²) CMNDF is ~30k operations per 10 ms
hop. No FFT needed; cost is far below 1% of a core.

**Constants are taken verbatim from `hum_to_midi.py`** so the live trainer
and hum-to-MIDI never disagree about what you sang. The one deliberate
divergence: the sidecar uses `resample_linear`, the Rust path uses a proper
anti-aliased FIR decimation. Better, and the cross-check test in §8 compares
within tolerance rather than bit-exact.

Post-processing, in order:

1. Parabolic interpolation on the CMNDF minimum (sub-bin f0).
2. Voicing gate: `aperiodicity < 0.20` **and** `rms > 5%` of a rolling loud
   reference (95th-percentile tracker) **and** `65 ≤ f0 ≤ 1000`.
3. 5-tap None-aware median filter.
4. Jump limiter: reject a frame moving more than 3 semitones from the
   previous voiced frame unless two consecutive frames agree.

Output, at 100 Hz:

```rust
pub struct PitchFrame {
    pub sample: u64,     // transport sample position the sound occurred at
    pub hz: f32,
    pub midi: f32,       // 69 + 12*log2(hz/440)
    pub clarity: f32,    // 1.0 - aperiodicity
    pub rms: f32,
    pub voiced: bool,
}
```

**Timestamping.** `sample` is the input-callback sample position minus half
the analysis window (≈16 ms), so the frame lands where the sound actually
was rather than where the analyser finished. A `pitchLatencyOffsetMs`
preference (−50..50) is added on top for device round-trip calibration.
Frames produced while the transport is stopped carry the current transport
sample and are marked by mode, not by a sentinel.

### 3.3 Scoring — `control/pitch_coach.rs` (new)

*Ownership zone: `control/`. Runs on demand, never in the audio path.*

**Reference melody.** `schedule::clip_events` returns `AbsNoteEvent
{ sample, key, velocity }` — correct timing, but it drops note identity, and
the report needs stable identity to address a row and to seek to it. So the
scorer builds its own reference list carrying `(clip_id, note_id,
repeat_index)` alongside the absolute samples. To stop the two from drifting,
the repeat-expansion and clipping rules are **extracted into one shared
helper** in `midi::schedule` that both `clip_events` and the scorer call;
`clip_events` keeps its existing signature and behaviour. A test asserts the
two produce identical timings for the same clip.

For a chosen MIDI track the scorer folds every clip's notes into one list in
absolute samples. For
overlaps (chords), take the **top note** — the standard lead-line
convention. Any note that overlaps another is flagged `ambiguous: true`; it
still scores, but it is excluded from the aggregate cents statistics so
chord guesswork cannot claim the singer was flat. Never score against a
chord root by default.

**Per-frame comparison.**

1. Octave-fold the sung MIDI value into the target's octave (while the
   difference exceeds 6 semitones, shift by 12). An octave slip in the
   detector is then harmless.
2. Compare against a **120 ms moving mean** of the sung pitch, not the
   instantaneous value. Classical vibrato is 4.5–6.5 Hz at 50–120 cents
   peak-to-peak and would otherwise read as constant error.
3. In tolerance if `|cents| <= tier`. Tiers: forgiving ±100, standard ±50,
   strict ±33, pro ±20. Default standard — ±50 cents is the research
   criterion for "correct pitch class".
4. The first 70 ms of each note is an **onset grace window**: unvoiced
   consonants (`s`, `t`, `k`) and scoops accrue no penalty there.
5. Hysteresis on the hit state: 2 consecutive in-tolerance frames to enter
   "hit", 3 to leave. Prevents flicker in the lane.

**Per-note output.**

```rust
pub struct NoteScore {
    pub note_id: u32,
    pub start_sample: u64,
    pub end_sample: u64,
    pub key: u8,
    pub hit_fraction: f32,          // voiced frames in tolerance / voiced frames
    pub coverage: f32,              // voiced frames / total frames (did you sing at all)
    pub mean_cents: f32,            // signed
    pub median_cents: f32,          // signed
    pub onset_offset_ms: f32,       // signed; negative = early
    pub stability_cents: f32,       // std-dev over the sustain, first 80 ms skipped
    pub vibrato_rate_hz: f32,
    pub vibrato_extent_cents: f32,
    pub ambiguous: bool,
}
```

**Session output.** `PitchScoreReport { notes, in_tolerance_pct,
mean_abs_cents, median_signed_cents, mean_onset_offset_ms, rating,
tolerance_cents, reference_track_id }`. `in_tolerance_pct` is weighted by
note duration; `rating` is a five-tier word derived from it. Aggregate
statistics skip `ambiguous` notes.

**Scores are never persisted.** They are recomputed from the stored pitch
track plus the current MIDI whenever asked. That is cheap and it means the
report is always correct after the reference melody is edited — a persisted
score would silently go stale.

### 3.4 Persistence — the pitch track

`<project>/cache/pitch/<clipId>.bin`, mirroring `cache/waveforms/`:
a small header (magic, version, sample rate, hop samples, frame count)
followed by packed `(f32 hz, f32 clarity, f32 rms)` triples at 10 ms hop —
about 1.2 kB/s. Written by the disk-writer thread
(`src-tauri/src/audio/recorder.rs`) alongside the WAV, so a recorded take
carries its pitch track automatically. Being under `cache/` it is
regenerable; a `pitch_analyze_clip` command re-derives it for imported or
pre-existing clips.

## 4. IPC — additive only

New commands. Registration in `src-tauri/src/lib.rs` is a **request to the
owner**, since that file is FROZEN (CONTRIBUTING rule 4).

| Command | Purpose |
|---|---|
| `pitch_listen_start` / `pitch_listen_stop` | Open/close the hub for listening without recording |
| `pitch_subscribe(channel)` | `Channel<PitchFrameBatch>`, batched at 60 Hz, same shape as meters |
| `pitch_set_reference(trackId \| null)` | Choose the MIDI track holding the target melody |
| `pitch_score(clipId, referenceTrackId, toleranceCents)` | → `PitchScoreReport` |
| `pitch_track(clipId, maxPoints)` | Stored pitch curve, decimated for drawing |
| `pitch_analyze_clip(clipId)` | Derive a pitch track for a clip that has none |
| `set_rehearse_hold(enabled)` | Momentary "keep rolling, write silence" |

New event `pitch://state` carrying `{ listening, rehearseHold, referenceTrackId, deviceRate }`.

New schemas `docs/ipc-schemas/pitch-frame.schema.json`,
`pitch-score-report.schema.json`, `pitch-state.schema.json` — draft-07, and
per D-06 they must **not** set `additionalProperties: false`.

One existing event is extended: `recording://state` gains an **optional**
`rehearseSpans` field (§4.1). That is permitted additive evolution under
CONTRIBUTING rule 2 — no field is removed, renamed, or made required, and
existing consumers ignore it. Nothing else changes.

### 4.1 Rehearse-hold semantics

While `rehearse_hold` is set during a take, the record sink writes **silence**
for exactly the held span instead of the captured samples. The take stays
sample-aligned, the transport keeps rolling, and the analyser keeps running —
so you still see your pitch, you just do not commit it. The held spans are
reported on `recording://state` so the UI can hatch them.

Silence, not a gap, is the deliberate choice: it reuses the existing
silence-debt path, keeps the WAV a single contiguous take, and means no
downstream code has to learn about holes.

The held spans are collected as `rehearseSpans: [{ startSample, endSample }]`
and reported as an optional field on `recording://state`.

## 5. Frontend — deliberately thin (R4, ADR 0006)

- `src/lib/state/pitch.svelte.ts` — a **non-reactive** bus for pitch frames,
  copied from `meters.svelte.ts`. Only mode flags (`listening`,
  `rehearseHold`, `referenceTrackId`, `tolerance`) are `$state`. Per-frame
  data never passes through Svelte reactivity.
- `src/lib/components/pitch/PitchCoach.svelte` — one canvas plus one rAF
  loop. It uses `midi.ticksToSamples()` and `view.xOf()`; it derives no time
  itself. All pointer maths through `canvasPos()`
  (`src/lib/utils/canvas-pos.ts`) — raw `clientX - rect.left` is a known bug
  class here (#11, #33).
- `src/lib/components/pitch/PitchReport.svelte` — the per-note strip.
- No pitch maths, no smoothing, no scoring, no tolerance evaluation on the
  frontend. It draws what Rust pushes.

**Placement.** The bottom panel region (today `<PianoRoll />` in
`src/App.svelte`) gains a small tab strip: *Piano Roll* | *Pitch Coach*,
sharing `ui.rollHeight` and the existing `ROLL_RESIZE` handle. Full width
without stacking a second tall panel. Default tab stays Piano Roll.

## 6. The lane

**Vertical range auto-fits** to the visible phrase's min/max target pitch
±2 semitones. The piano roll's fixed 14 px per key across the whole register
is the wrong instrument for a single voice.

**Target notes** are rounded bars in a muted colour. The portion you actually
held fills in bright as you sing it — the SingStar "filling up" read, which
communicates partial success without a number.

**The sung trail** is a 2 px polyline at *absolute* pitch, unsnapped. It is
**broken** on unvoiced frames: a gap reads as silence, a line reads as a
wrong note, and interpolating across a breath is a lie. Colour maps to
`|cents|`: in tolerance, near, far. A cents badge rides the head of the trail
("+18 ¢"); an arrow at the edge appears when the singer is more than an
octave away and the trail would otherwise be invisible.

**Scrolling.** While recording or rehearsing the playhead is pinned at 35%
and the lane scrolls toward the singer — you must see what is coming.
While reviewing, scrolling is free and follows the normal timeline model.
A preference switches this.

**Rehearse spans** are drawn as a hatched overlay, so what was not recorded
is visible rather than inferred.

**Tuner mode.** When the transport is stopped and listening is on, the same
component takes over the panel: a large note name, a cents needle, and the
last 3 seconds as a sparkline. One component, one code path.

**The report** is a strip under the lane, one row per note: a cents bar
drawn left or right of centre, the timing offset, and stability. Clicking a
row seeks there. The headline is **% within tolerance** and a word — not a
game score. This is feedback, not a leaderboard.

**Game mode** (`pitchTrailSnap`, default off) snaps the trail onto the bar on
a hit, the way SingStar draws it. Never both at once: snapped is legible,
unsnapped is truthful.

## 7. Preferences (`src/lib/prefs/schema.ts`)

| id | kind | default |
|---|---|---|
| `pitchTolerance` | enum `forgiving`/`standard`/`strict`/`pro` (±100/±50/±33/±20 cents) | `standard` |
| `pitchLatencyOffsetMs` | number −50..50, step 1, unit ms | 0 |
| `pitchLaneFollow` | enum `fixed`/`free` | `fixed` |
| `pitchTrailSnap` | boolean (game mode) | false |
| `pitchRehearseKey` | enum `h`/`j`/`none` | `h` |

Category `editing`, except `pitchLaneFollow` and `pitchTrailSnap` under
`interface`.

## 8. Testing

**Rust — detection.** YIN against synthetic sine sweeps and vibrato at
65–1000 Hz, asserting better than 25 cents; noise and silence assert
`voiced == false`; a missing-fundamental signal (H2/H3 only, no H1) asserts
no octave error, which is exactly where FFT-peak methods fail on low male
voice. A cross-check test runs the Rust detector and `hum_to_midi.py` over
the same WAV and asserts agreement within 30 cents on voiced frames.

**Rust — scoring.** Hand-built note/frame fixtures for: perfect run, a
consistently 30-cents-flat run (must report signed mean, not just "missed"),
vibrato at 5.5 Hz / 90 cents (must score as a hit), a late onset, a chord
that must resolve to the top note and be flagged ambiguous, and full silence
(low coverage, not a low hit fraction).

**Rust — hub.** Listen toggle opens and closes the stream; panel open opens
it; recording attaches sinks to an already-open hub; disarming while
listening keeps it open; rehearse-hold writes silence over exactly the held
sample span; the existing recording tests are the regression net for
count-in, silence debt, and `split_record_targets`.

**Frontend.** vitest on the frame bus (batching, ordering, no reactivity
leak), the tolerance→colour mapping, the auto-fit range calculation, and
`canvasPos()` under interface zoom.

**Perf.** A `benches/` entry for the pitch thread asserting the per-hop
budget.

Pre-PR gate: `cd src-tauri && cargo test`, `npm test`, `npx svelte-check`,
`npm run build`.

## 9. Risks

1. **The hub refactor sits in `engine.rs` (4848 lines), in the most
   sensitive area of the repo.** Mitigation: it lands first and alone in
   phase 1, with the existing recording tests as the net, and the owner
   checkpoints before any UI is built.
2. **Speaker bleed.** Without headphones the backing track is tracked as
   pitch. Mitigated by the RMS gate and the 65–1000 Hz band, but not solved.
   The panel says so once, plainly, rather than pretending.
3. **`lib.rs` is FROZEN.** Command registration is a request to the owner,
   not an edit — this blocks the end of phase 1 and must be asked for early.
4. **Watching the screen can make singers less accurate** (Sing&See's own
   research). This is why the post-take report is a first-class surface and
   not an afterthought: the lane is for orientation, the report is for
   learning.
5. **Pre-existing failure on main.** `control::tests::
   transport_play_does_not_downgrade_recording_state` fails at `bcdc481`
   before any change here. It is in the recording state machine this work
   touches, so it must be resolved or explicitly quarantined before phase 1
   lands, or it will mask a real regression.

## 10. Phases

**Phase 1 — backend.** InputHub, `audio/pitch.rs`, the frame ring and
`Channel`, the schemas, the listen/rehearse commands, and their tests. The
verification is numeric: sing a known pitch, read the frames.
*Owner checkpoint here (R3).*

**Phase 2 — panel.** `pitch.svelte.ts`, `PitchCoach.svelte`, the lane, the
live trail, tuner mode, rehearse-hold (key and button), the tab strip, the
preferences.

**Phase 3 — scoring.** `control/pitch_coach.rs`, the pitch track on disk,
`PitchReport.svelte`, and the report surfaces.

All three land in one PR from `feat/pitch-coach` (R3).

## 11. Out of scope

Polyphonic detection; pitch correction or repair; automatic comping across
takes; lyric display or karaoke text; a neural detector (SwiftF0/CREPE are a
credible later offline pass, not this round); MIDI-out of the sung line
(hum-to-MIDI already does that); scoring against audio rather than MIDI.
