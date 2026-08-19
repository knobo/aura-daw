# Pitch Coach

Live vocal pitch feedback against a MIDI reference melody, plus a per-note
report on the take you just recorded.

Open it from the bottom panel's **PITCH** tab. Opening the panel opens the
microphone — that is deliberate (ruling R6): arming a track does *not* start
listening, and the panel closing hands the mic back.

## Wear headphones

Without them, the backing track comes out of the speakers, back into the
microphone, and gets detected as your pitch. Mitigated, not solved: the
detector has no way to tell your voice from a loudspeaker playing a melody.
The panel says so in its footer, once.

## The three views

**Tuner** — no reference melody selected, or the `TUNER` chip pressed while
stopped. One big reading in Hz, the note name beside it, and a stability
ribbon whose *width is the steadiness*: a steady note reads as a thin bright
seam and a wobble opens it up.

Steadiness leads, not accuracy, and that is measured rather than
stylistic. Distance to the nearest note saturates near 50 cents for anyone
sitting midway between two semitones — which is exactly where a person who
cannot yet hit a pitch lives. It reads as failure and is not one. Steadiness
improves while accuracy is still poor, so it is the number that shows
progress.

**Lane** — a reference melody is selected. Target bars scroll under the
playhead with your pitch drawn as a continuous trail, coloured by how close
you are. The lane is drawn whenever a melody exists, **including while
stopped**, so you can see what is coming before you press record.

**Report** — after a take. See below.

## Picking a reference melody

The `reference` dropdown lists the project's MIDI tracks. Pick the one
carrying the tune; its notes become the targets. `none` clears it and the
panel falls back to the tuner.

Chords are resolved to a single line by taking the **top note**, which is
where a lead line usually sits. Those notes are still drawn, but they are
flagged in the report (a `◇` marker) and left out of the totals — the melody
was guessed, and a guess should not move your score.

What counts as a chord is **notes that start together** (within 30 ms), not
notes that merely overlap. A melody drawn legato overlaps itself at every
note, and grouping by overlap is transitive: one overhanging note would chain
the whole phrase into a single target. A note that another note *covers*
(more than half of the shorter one) is still flagged, so a drone held under
the tune is not silently treated as the melody.

If **every** reference note came out of a chord, the report says so instead of
showing a score: there is no unambiguous note left to compute one from, and a
0 % on a take that was sung well would be a lie. Pick a track that carries a
single line.

## The report

Recording a take with a reference selected scores it, note by note, and shows
the result under the lane. The `REPORT` chip hides and shows it.

Scores are **never saved**. The report is recomputed from the take's stored
pitch curve every time you open it, change the tolerance, or edit the melody
— so it can never describe a melody that no longer exists.

The header:

| | |
|---|---|
| the big percentage | how much of the take landed inside the tolerance, weighted by note length. Material you did not sing counts as outside it, so one in-tune blip per note cannot read as a perfect take |
| the word beside it | the same number in English: Finding it → Getting there → Solid → Sharp → Locked in |
| **off by** | mean absolute error, in cents |
| **lean** | median *signed* error. The sign is the actionable half: "consistently 30 cents flat" is advice, "off by 30 cents" is not |
| **timing** | mean entry offset; positive is late |
| **within** | the tolerance this report was computed with |

One row per note, sorted by time or by `WORST` first. Clicking a row seeks
the transport to that note.

| column | what it means |
|---|---|
| **note** | the target note. `◇` means it came out of a chord |
| the bar | signed error: right of the centre line is sharp, left is flat. Cyan inside the tolerance, amber up to twice it, magenta beyond. Scaled to the tolerance, so a tighter tier draws the same error as a longer bar |
| **hit** | how much of the note was in tune |
| **sung** | how much of the note you sang at all |
| **entry** | how late (or early) you came in |
| **steady** | drift over the sustain, with vibrato averaged out. `∿` marks detected vibrato — hover for its rate and width |

**hit** and **sung** are two different questions on purpose. A note you
skipped and a note you sang badly both score 0 on **hit**, and the advice
differs completely; **sung** is what separates them. A row that was barely
sung is dimmed and its timing reads `—`, because an entry offset measured
over three frames is noise.

Per row, **hit** is a share of what you *sang* — the headline is the one that
also counts the silence, which is why a row can read 100 % while the take
does not.

## Rehearse hold

Hold `H` (configurable) or the panel's press-and-hold button to rehearse a
passage without committing it. The take keeps recording, but writes
**silence** for the held span, so everything after it stays sample-aligned —
a rehearsal is a hole in the take, never a shift. The lane draws a hatched
veil while the hold is down.

Both controls drive one engine flag and are refcounted: releasing one while
the other is still down does not end the hold.

## Preferences

Under **Preferences → Pitch**:

| preference | what it does |
|---|---|
| **Tolerance** | how close counts as a hit: forgiving ±100¢, standard ±50¢, strict ±33¢, pro ±20¢. ±50 cents is the standard research criterion for "the right pitch class" — half a semitone, past which a listener hears a different note |
| **Latency offset** | ±50 ms, shifts detected pitch against the timeline to cancel your interface's round-trip delay. Signed, so it corrects both ways. Set it by singing a note exactly on a target bar and nudging until the trail sits on it |
| **Lane follow** | whether the lane pins the playhead or scrolls with the timeline |
| **Snap pitch trail** | game mode: parks your line on the target when you hit it. Off shows your true pitch, which is more useful for practice |
| **Rehearse key** | `H`, `J`, or none |

## What the detector is, and what has been measured

YIN, run on a decimated 8 kHz stream off the real-time callback, with an RMS
gate, a median smoother and a jump limiter. Its constants are copied verbatim
from `sidecars/hum_to_midi.py`, and a parity test keeps the two from
drifting: they agree to **3.3 cents** once the live detector's unavoidable
20 ms lag is accounted for (a live median is causal; the sidecar's is
centred, and no live detector can look ahead).

Measured on one voice, in one room:

* a synthetic tone at A3 read back **0.4 cents** median error, 100 % voiced;
* a whistle held ten seconds stayed inside a 3.5 Hz window at clarity 0.98;
* a sustained open vowel at ~100 Hz gave **no octave errors in 1312 frames**,
  which is the result that matters — a low vowel carries strong harmonics at
  exactly the frequencies where YIN reports the wrong octave.

Its 9.6 cents of jitter on that vowel is the *voice*: the same chain reads
0.1 cents on a synthetic tone.

Two known limits. **One voice, one vowel, one room** — no woman's voice, no
falsetto, no vibrato, no backing track under it. And level matters: at a low
input level the same tone came back 55 % voiced with readings an octave down.
If the trail looks broken, check your gain before you blame the detector.

## Where the pitch curve lives

A take's pitch curve is folded as it records and written to
`cache/pitch/<clipId>.bin` (`APTF`, see `audio/pitch_store.rs`). It is
regenerable: deleting `cache/` costs nothing but the re-analysis, which runs
at roughly 100× real time. A take recorded before this existed — or one whose
cache cannot be read — is analysed on demand the first time you score it.

The curve is read-only and take-local. Nothing edits it, and nothing widens
the format on speculation; see [`backlog/pitch-as-data.md`](backlog/pitch-as-data.md)
for the four decisions that shape it, and
[`backlog/pitch-correction-autotune.md`](backlog/pitch-correction-autotune.md)
for what detection would need before it could drive pitch correction.

## Voice Configuration, Registers, and Calibration (Challenges & Roadmap)

### 1. Vocal Registers & Octave Adaptation
* **Voice Registers:** Human vocal ranges vary widely:
  - *Bass/Baritone:* E2 (82 Hz) – E4 (330 Hz)
  - *Tenor/Alto:* C3 (130 Hz) – F5 (698 Hz)
  - *Soprano:* C4 (261 Hz) – C6 (1046 Hz)
* **Octave Displacement:** Extracting a melody from a high vocal stem (e.g. pop/female lead in C4–C5) and transposing down 1 octave (-12 ST) allows singing in a comfortable male chest voice. However, dropping an entire song by 12 semitones can push the lowest notes down to E2–A2 (80–110 Hz), which can be at the absolute physiological limit of non-bass singers.
* **Pitch Coach Octave Folding:** Pitch Coach uses octave folding (`diff - 12 * round(diff/12)`), so singers hitting the correct pitch class in any octave receive full scores.

### 2. Low-Frequency Detection Limits (80 Hz High-Pass)
* The RT decimation filter (`src-tauri/src/audio/decimate.rs`) includes an 80 Hz one-pole high-pass filter to reject microphone rumble and DC offset.
* This attenuates fundamental frequencies below E2 (~82 Hz), causing the detector to latch onto the 2nd harmonic (160+ Hz) for very low notes.
* *Future improvement:* Lower the decimation HP filter cutoff to 50–55 Hz (allowing C2/D2 fundamental detection down to 65 Hz) or provide an input profile setting.

### 3. Interactive Vocal Calibration & Auto-Detection
* **Calibration Flow ("Sing these notes"):** A guided range calibration test where the user sings a few reference prompts to detect their lowest comfortable pitch, highest pitch, and natural tessitura.
* **Audio Stem Profile Detection:** Heuristics or sidecar analysis on imported vocal stems to recommend optimal transposition or target voice range.

### 4. Polyphonic & AI-Generated Stems (Suno / Udio / Choirs)
* Stem splitting on complex AI tracks (e.g. from Suno or Udio) often yields vocal stems with mixed male/female duets, multi-part backing harmonies, and choir stacks.
* Monophonic YIN pitch detection is designed for single-voice input and may jump between voices or track the loudest harmonic in dense choral passages.
* *Future hybrid AI + DSP direction:* Integrating neural multi-pitch or vocal lead separation (e.g. BasicPitch, CREPE, or lead vocal demixing) to isolate the primary melodic line before DSP segmentation.

