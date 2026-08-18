# The Composer: theory-driven composition assistance

**Captured 2026-08-17 from the owner:** *"en composer helper, som lager/genererer
midi-track med the circle of fifths. Og som kan generere melodi track, eller/og
indikere hvilke midi noter man kan bruke til gjeldende akord. […] hjelpe
begynnende musikkstudenter til å komponere musikk med musikkteori uten at de
kan det selv […] et composer verktøy ingen har sett maken til, med tromme
generator (basert på musikkteori)."*

This is the product doc. The implementation plan for the first slice is
[`docs/superpowers/plans/2026-08-17-plan-h-composer.md`](../superpowers/plans/2026-08-17-plan-h-composer.md).

---

## 1. The idea in one paragraph

A piano roll is a grid of 128 equal keys. Music theory's whole claim is that
**those keys are not equal**: at any moment, given a key and a chord, some
notes are the chord, some are safe colour, some are tension that must resolve,
and one or two are actively wrong. A beginner cannot see which is which — and
that, not a lack of inspiration, is what makes their first tracks sound bad.
So: **give the DAW a harmony document, and let every surface in the app read
it.** One `tick → (key, chord)` map, in the core document model, under the op
log. The piano roll colours its own keys from it. The generators (chords,
melody, bass, drums) are pure functions of it. The circle of fifths becomes
the instrument you edit it with. And every generated note carries a sentence
saying *why* it is that note — so the tool teaches while it works, instead of
being a black box that hands you eight bars.

## 2. What already exists elsewhere (be honest)

We are not inventing the chord track, and the doc should not pretend we are.

| Tool | Has | Does not have |
|---|---|---|
| **Cubase** Chord Track + Chord Assistant | A song-level chord timeline other tracks can follow; a circle-of-fifths assistant; voicing schemes | No explanation layer, no per-chord colouring of the editor's keys, no theory-driven drums, closed |
| **Hooktheory / Hookpad** | The best teaching UI in existence; corpus statistics for "what usually comes next" | Not a DAW; generation is corpus-statistical, not functional; no audio, no plugins, no mixing |
| **Scaler 3** | Chord suggestion, detection, performances | A *plugin*: it cannot colour the host's piano roll or own the song's harmony |
| **Logic** Chord Track / Session Players | Chord timeline + very good style players | Players are sample/ML-driven black boxes; no theory explanation; no drum theory dial |
| **Ableton 12** | App-wide scale awareness, generative MIDI tools | Scale, not *harmony* — no chord-by-chord function, no avoid notes, no why |
| **Orb Producer / Captain Chords** | One-click progressions and melodies | Black-box generation; no pedagogy; no host integration |

What nobody has is the **combination** below, and each item is a direct
consequence of AURA's existing architecture rather than a feature bolted on:

1. **The harmony document is core document state** — in `Session`, under the
   op log, undoable, journaled, in `project.json`, and visible to the
   published snapshot. Not a plugin's private state, not a UI-local model.
   Everything downstream is a *reader*, so features compose instead of
   duplicating theory.
2. **The editor teaches.** The piano roll's keys are coloured per chord region
   by a documented, defensible chord-scale rule (§4.3) — not by a static key
   signature. A beginner *sees* why F sounds wrong over Cmaj7 and right over
   Dm7, in the same instant they play it.
3. **Every artefact explains itself.** Each generated chord, note and drum
   hit carries a provenance sentence ("`Bm7♭5 → E7 → Am`: the ii–V–i of the
   relative minor; G♯ is the leading tone and resolves up to A"). The
   generator already knows *why* it chose what it chose; the only novel thing
   is refusing to throw that away.
4. **Deterministic, theory-driven generation next to real ML generation.**
   Every generator is a pure function of `(harmony, params, seed)`. Same seed,
   same notes; a seed is stored with the result, so "I liked take 3" is
   recoverable and every generator is unit-testable. AURA *also* ships
   ACE-Step, AMT infilling and hum-to-melody — so the theory engine is not a
   substitute for ML, it is the half that can be explained, and the two can
   be composed (generate the harmony with theory, infill the inner voices
   with AMT).
5. **Agents compose with theory too.** The control plane is one seam for the
   UI and the MCP server (ARCHITECTURE §11), so "write me a ii–V–I in D
   dorian at bar 9" is available to an LLM agent through the same commands
   the panel uses, with the same undo and the same policy gate.
6. **Drums from metre, not from a pattern library** (§4.7). A measurable
   syncopation dial (Longuet-Higgins & Lee), Euclidean onset distribution
   (Toussaint/Bjorklund), and velocities derived from the *same* metrical
   weight function that places the kick. Genre becomes a small parameter
   vector, so genres interpolate instead of being 200 canned MIDI files.

## 3. Who it is for, and the one design rule

The owner's brief is explicit: **beginning music students who do not know the
theory yet.** That yields one rule, which every UI decision in this track
must be checked against:

> **Never show a theory word without showing what it does.** A label
> ("half cadence", "avoid note", "ii–V–I") is only ever attached to
> something the user can hear or see change.

And one dial, not a mode switch. Assistance runs continuously from
**autopilot** to **coaching**:

| Level | What the app does | Who it is for |
|---|---|---|
| **Autopilot** | One click: 8 bars of chords, a bass, a groove, a melody. Sounds like music immediately. | first session |
| **Suggest** | You place chords; it ranks the next ones by function with a why for each. | week two |
| **Colour** | You play; it colours the keys and names what you played. | learning |
| **Silent** | Harmony document only; no suggestions. Analysis still available on demand. | when they've outgrown us |

The point of the ladder is that **the same harmony document serves all four**
— so growing out of autopilot costs the user nothing.

## 4. The theory, and how each piece becomes code

This section is the design's spine. Each subsection names the theory, then
the function it becomes. Everything lives in `src-tauri/src/theory/`,
tauri-free and unit-tested, with **no I/O, no locks, no state** — a pure
library the control plane calls.

### 4.1 Spelling: the line of fifths, not pitch classes

A pitch class (0–11) cannot tell F♯ from G♭, and every downstream label
depends on that distinction: key signatures, chord symbols, "is this the
raised 7th or the flat 1st". So the atom is a **tonal pitch class** —
a signed position on the line of fifths (`C = 0`, `G = +1`, `F = -1`,
`F♯ = +6`, `B♭ = -2`).

Everything falls out arithmetically instead of from a lookup table:

* `pitch_class = (fifths * 7) mod 12`
* `letter = "FCGDAEB"[(fifths + 1) mod 7]`, `accidentals = (fifths + 1) div 7`
* transposition by an interval is **addition** on the line of fifths
* a key signature *is* the fifths index of the tonic

This is the one decision in the whole track that is expensive to retrofit
(every symbol, every label, every generated spelling depends on it) and free
to make now. Precedent: Humdrum, music21, MuseScore's `tpc`.

### 4.2 Scales as contiguous windows

Because a major scale is exactly the seven contiguous fifths from `-1` to
`+5` relative to its tonic, **every diatonic mode is a 7-slot window on the
line of fifths with the tonic in a different slot**. Scales are therefore
stored as fifths offsets, which gives correct spelling for free
(`aeolian = [0, +2, -3, -1, +1, -4, -2]`), and the "which chords belong to
this key" question and the "how far away is that key" question become the
*same* lookup.

Non-diatonic scales (harmonic/melodic minor, pentatonics, blues, whole tone,
octatonic) are the same representation with a non-contiguous offset set.

### 4.3 The palette: which notes may I play right now

The user's second ask, and the feature that makes the editor teach. Given
the chord at a tick and the key at that tick, classify all 12 pitch classes:

| Class | Rule | UI |
|---|---|---|
| **Chord tone** | in the chord; labelled by role (root / 3rd / 5th / 7th) | strong |
| **Extension** | in the key, *not* a semitone above any chord tone → an available 9/11/13 | soft |
| **Avoid** | in the key, **a semitone above a chord tone** | struck through |
| **Tension** | outside the key but a semitone below a chord tone (chromatic approach), or a blue note | dashed |
| **Outside** | everything else | faint |

The avoid-note rule is one line and it is not a heuristic — it is Berklee
chord-scale theory's *avoid note*, and it produces the textbook answer on
every diatonic seventh chord in a major key. Worth checking, because it is
the rule the whole feature's credibility rests on:

| Chord in C major | Extensions the rule allows | Avoid notes the rule flags | Textbook |
|---|---|---|---|
| `Cmaj7` | D (9), A (13) | **F** (♭9 over the 3rd) | ✅ |
| `Dm7` | E (9), G (11), B (13) | — | ✅ |
| `Em7` | A (11) | **F** (♭9 over the root), **C** (♭13 over the 5th) | ✅ |
| `Fmaj7` | G (9), **B (♯11)**, D (13) | — | ✅ lydian |
| `G7` | A (9), E (13) | **C** (11 over the 3rd) | ✅ |
| `Am7` | B (9), D (11) | **F** (♭13) | ✅ |
| `Bm7♭5` | E (11), G (♭13) | **C** (♭9 over the root) | ✅ |

The `Fmaj7` row is the one that proves the rule is doing real work: a naive
"non-chord-tones are risky" rule would flag the ♯11, which is precisely the
*best* note on a IVmaj7.

Two immediate products of the palette beyond colour: **"no wrong notes"**
input (snap an incoming hardware/on-screen note to the nearest non-avoid
class — the single highest-value beginner feature in the whole track), and
**naming what the user played** ("that's the ♯11 — it's the lydian colour").

### 4.4 Function, Roman numerals, and what the circle of fifths is *for*

The circle of fifths is usually taught as a key-signature mnemonic, which is
the least useful thing about it. Three facts make it an instrument:

1. **A key's entire diatonic chord vocabulary is a contiguous arc.** C major
   is `F C G | D A E | B` → three majors, three minors, one diminished, in
   circle order. Rotating the arc *is* modulating. Borrowed chords are the
   slots just outside the arc — which is exactly how to teach ♭VII (one step
   counter-clockwise of IV) without saying "mixolydian".
2. **Distance on the circle is relatedness.** One step = one accidental =
   the modulations that sound easy (dominant, subdominant, relative,
   parallel). This turns "where can I go next" into a *geometric* question.
3. **Counter-clockwise motion is the strongest harmonic drive in tonal
   music.** V→I is one step; the circle progression `iii–vi–ii–V–I` is four.
   Functional harmony is, to a first approximation, a walk down the circle.

So: **the circle of fifths widget is not a picture, it is the harmony
editor.** Click a wedge → append that chord. The current key's arc is
highlighted; the wedges outside it are borrowed chords, labelled as such.
Drag the arc → modulate, and the app tells you which chord is the pivot.
The owner's literal ask ("generer midi-track med the circle of fifths") is
one plan option: a chord clip that walks the circle, which is both a demo
and a genuinely useful étude.

Functional analysis is the other half: `(chord, key) → Roman numeral +
function (T / PD / D) + borrowed? + why`. It powers the labels, the
suggestion ranking, and the cadence logic.

### 4.5 Voice leading — the difference between a chord generator and music

Beginner chords sound wrong for mechanical reasons theory has known for 400
years: every chord in root position, all voices leaping in parallel, tendency
tones unresolved. The fix is a scoring function over candidate voicings and a
dynamic-programming pass over the whole progression (not greedy — a locally
smooth choice can strand the next chord):

* minimise total voice movement (L1), maximise common tones
* resolve tendency tones (the 7th of `V7` falls to the 3rd of `I`; the
  leading tone rises)
* keep voices inside a register; wide spacing low, close spacing high
* penalise parallel fifths/octaves in the outer voices — **style-gated**,
  since pop and gospel want them
* voicing styles: close, drop-2, drop-3, shell (1-3-7), rootless, spread

The same machinery produces bass lines (the bass *is* the inversion) and
comp/arpeggio patterns.

### 4.6 Melody — motif and development, not a random walk

The reason generated melodies sound generated is that they are sampled note
by note. Humans do not do that: they state a motif and then *develop* it.
Both halves are algorithmic.

**Constraints (what makes a line singable):**
* chord tone on strong beats; non-chord tones on weak beats, as passing
  tones, appoggiaturas, escapes, suspensions or anticipations (this is
  figuration theory, and it is a rule table)
* 70–80 % stepwise motion; a leap is followed by a step in the opposite
  direction; range ≲ an octave per phrase
* one climax, placed late (≈⅔ through) — the melodic arch
* phrase structure: 4 + 4 as antecedent/consequent, the antecedent ending on
  a half cadence (degree 2 over V), the consequent on an authentic cadence
  (degree 1). *This* is what makes a melody sound finished rather than
  stopped.

**Development operations** (each one line of code, each a real technique):
exact repetition, sequence (transposed repetition, esp. by step or third),
inversion, retrograde, augmentation/diminution, fragmentation, extension.

**Tension curve:** beat strength × harmonic function × palette class gives a
per-note tension number; shape it to an arch and the phrase gets a direction.

### 4.7 Drums from metrical theory

"Drum generator based on music theory" is the request that sounds least
plausible and turns out to have the most rigorous answer, because rhythm is
the best-formalised corner of music theory.

**(a) Metre is a hierarchy.** A bar subdivides by 2s and 3s into a tree
(Lerdahl–Jackendoff); every grid position gets a *weight* = the number of
tree levels it sits on. In 4/4 at 16ths: `4 0 1 0 | 2 0 1 0 | 3 0 1 0 | 2 0 1 0`.
This single function drives **everything**: kick placement (high weight),
the backbeat (the secondary strong beats — beats 2 and 4 *are* weight-2
positions), hat accents, ghost-note placement (weight 0), and per-hit
velocity. One theory object, five uses — which is the tell that it is the
right abstraction.

**(b) Syncopation is measurable, so it can be a dial.** Longuet-Higgins &
Lee (1984): a note on a weak position followed by *nothing* on a stronger
one scores the weight difference. Sum over the bar and you have a number.
So the "SYNCOPATION" slider is not a vibe — the generator displaces onsets
until the measured value hits the target, and the panel can show it.

**(c) Euclidean rhythms.** `E(k, n)` distributes k onsets as evenly as
possible over n steps (Bjorklund's algorithm). Toussaint's result is that
this reproduces a startling share of the world's traditional rhythms:
`E(3,8)` is the tresillo, `E(5,8)` the cinquillo, `E(7,16)` a samba, and
rotations of one pattern give a whole related family (son and rumba clave
are rotations of each other). So hats, percussion and clave lines come from
two integers and a rotation, not from a library.

**(d) Phrase structure is the same hierarchy, one level up** — which is
*why* fills land at the end of 4- and 8-bar groups. Same weight function,
coarser grid.

**(e) Genre is a constraint vector**, not a pattern bank:
`(subdivision, swing, syncopation target, per-role density, accent rule,
allowed rotations)`. Six numbers, so genres can be interpolated, tweaked and
explained — and a groove can be *analysed* back into them.

**(f) Humanising** is bounded, seeded jitter on time and velocity, on top of
metrical accents. Deterministic given the seed, so a groove is reproducible.

### 4.8 Form and modulation

Section grid (intro / verse / chorus / bridge) with a harmonic plan per
section, an energy curve, pivot-chord modulation ("the bridge goes to the
relative minor; the pivot is `Am`, which is `vi` here and `i` there"), and
the one-new-element-per-section rule. AURA already has the launch map's
scene vocabulary to hang this on.

## 5. Architecture: where each piece lives

The invariants this track must respect (they are not negotiable and they
also happen to make the design simpler):

* **The theory core is pure.** `src-tauri/src/theory/` has no Tauri, no
  locks, no I/O, no `Session`. Its tests are the fast ones.
* **Ticks, never seconds** (debt D-02). Harmony regions are integer ticks at
  the project PPQ, like MIDI clips and the tempo map.
* **The harmony document is a map, not a track kind.** `tick → key` and
  `tick → chord`, exactly parallel to the tempo map and the meter map, which
  is the strongest existing analogy in the repo. **No new `TrackKind`**, no
  new lane row — the same instinct `pitch-as-data.md` applied to pitch
  tracks. It lives in `MidiStore`, persists in `project.json`, and rides in
  the published snapshot.
* **Generated output is ordinary document state.** A generated chord clip is
  a `MidiClip` created by `Op::MidiClipAdd` — editable in the piano roll,
  undoable with Ctrl+Z, exportable as `.mid`, byte-identical to one drawn by
  hand. There is **no "generated clip" type** and no regeneration link. This
  is the decision that keeps the feature from becoming a walled garden.
* **Thin renderer** (ADR 0006). No theory in TypeScript. The panel renders
  pushed state and emits ops; the circle widget's geometry is presentation,
  its *content* comes from the backend.
* **One mutation channel.** Every harmony edit is an op through
  `Session::transact` / `ControlPlane::commit`; one gesture = one op = one
  undo. Additive op arms; `OP_FORMAT_VERSION` stays 2.
* **Additive command names**, batch-shaped (D-03).

```
              ┌─────────────────────────────────────────────┐
              │  theory/  (pure, no state, no I/O)          │
              │  tpc · scale · chord · key · circle ·       │
              │  palette · metre · progression · voicing ·  │
              │  melody · groove · annotate                 │
              └───────────────▲─────────────────────────────┘
                              │ pure calls
   ┌──────────────────────────┴────────────────────────────┐
   │  control/composer.rs   (the only stateful seam)       │
   │  harmony_get/set · composer_suggest · composer_palette│
   │  composer_generate → Op::MidiClipAdd / MidiSetNotes   │
   └───────▲───────────────────────────────────▲───────────┘
           │ Tauri commands                    │ MCP tools (later)
   ┌───────┴────────────┐            ┌─────────┴───────────┐
   │ COMPOSER dock panel│            │  agents compose too │
   │ circle · strip ·   │            └─────────────────────┘
   │ generate · why     │
   └────────────────────┘
      piano roll reads the palette and tints its own keys
```

## 6. Roadmap

Phases are ordered so that **each one is usable on its own** and none
requires the next. H1 is the MVP that landed with this doc.

### H1 — the spine (MVP, landed 2026-08-17)

The foundation everything else is a reader of, plus one visible vertical
slice of each of the owner's three asks.

* `theory/` core: line-of-fifths spelling, scales/modes, chords + symbols,
  keys, functional analysis, the circle of fifths, the palette.
* The harmony document: key regions + chord regions in `MidiStore`, one
  additive op, persistence, published snapshot, `harmony_get`/`harmony_set`.
* Generators v1: progression (circle walk, named schemas, functional
  automaton), voice-led chord clip, melody (motif + development), groove
  (metrical weight + Euclidean + syncopation dial).
* `composer_generate` (batch), `composer_suggest`, `composer_palette`.
* COMPOSER dock panel: interactive circle of fifths, progression strip with
  Roman numerals, generate buttons with a seed, the WHY list.
* Piano-roll key tinting from the palette.

### H2 — the editor teaches

* **"No wrong notes" input**: snap hardware/on-screen MIDI to the palette,
  as a per-track toggle. Cheap (one seam in `midi_input`), enormous payoff.
* Chord ribbon in the timeline ruler; drag region edges, split, merge.
* Live analysis of what the user *plays*: name the chord under the cursor,
  name the note they just hit, flag an unresolved tension.
* Palette-aware quantize/nudge in the piano roll: "fix to nearest chord
  tone" over a selection, one op, one undo.
* Detect the harmony of an existing MIDI clip (chord recognition over a
  window) → fill the harmony document from music the user already wrote.
  This is the on-ramp for people who don't start from chords.

### H3 — the arrangement

* Section grid (intro/verse/chorus/bridge) over the harmony document; a
  harmonic plan and an energy curve per section.
* Pivot-chord modulation as an operation, with the pivot named.
* Comp/arpeggio pattern generator (rhythm from metrical weight, voicing from
  §4.5) per track, with instrument-aware registers.
* Bass generator: root / approach / walking / pedal modes.
* Fills and hypermetric variation for the groove; per-section drum intensity.
* Form templates as *constraint sets*, not clip libraries.

### H4 — the coach

* A ranked "what next" surface that is honest about *why* each option ranks
  where it does (function, voice-leading cost, novelty), not a black box.
* Cadence coaching: "your phrase ends on `ii` — that's why it sounds
  unfinished. Two ways out:" with both applied on click.
* Explain-my-progression: analyse what the user made, name the schema if it
  matches a known one ("that's a doo-wop / royal road / Andalusian"), and
  say what it's borrowing.
* An **ear-training loop** that reuses the Pitch Coach: sing the third of
  the chord under the playhead; the coach scores you against the harmony
  document. The two features already share every prerequisite.
* Progressive disclosure driven by usage, not a settings page.

### H5 — theory meets the models

The point where AURA's ML stack and the theory core stop being neighbours:

* **Constrained AMT infilling**: give the Anticipatory Music Transformer the
  harmony document as context and *filter its proposals through the palette*
  — model creativity, theory guardrails. This is the single most interesting
  item in the whole roadmap and it is cheap: AMT infill already ships.
* Harmony-aware hum-to-song: quantise a hummed melody to the palette, then
  *derive* a progression that fits it (melody → harmony is the reverse
  direction and is well-posed).
* Text → harmony ("wistful, 6/8, ends unresolved") → theory generators,
  rather than text → audio. Sidecar-free: it is a small parameter mapping.
* Chord-conditioned ACE-Step prompts built from the harmony document.

### H6 — agents and the long tail

* MCP tools for the harmony document and the generators (the roster is
  frozen this round; candidates go in the phase plan, not in `tools.rs`).
* Microtonal / non-12-TET (the line of fifths survives it; equal-temperament
  assumptions are isolated to one conversion).
* Non-Western metres and rhythm systems (Euclidean/necklace machinery is
  already culture-neutral; the *genre vectors* are where bias lives).
* Import/export interchange: MusicXML harmony, chord charts, iRealPro-style
  lead sheets.
* Corpus statistics as an *optional* second opinion next to the functional
  model, clearly labelled as such — never replacing the explainable path.

## 7. Rulings taken up front

Recorded so a later task does not re-litigate them.

1. **The harmony document is one map pair (keys + chords), session-level,
   ticks.** Not per-track, not per-clip, no polytonality. A song has one
   harmonic context at a time; that is the assumption, and it is stated.
2. **No new `TrackKind` and no new lane.** The chord ribbon is chrome drawn
   over existing surfaces.
3. **Generated notes are plain MIDI clips.** No provenance field on the
   clip, no live regeneration link, no "AI clip" type. Annotations are
   returned *with the command result* for display; they are not document
   state. (If they ever need to persist, that is its own round — and it
   would be a cache, like the pitch curve.)
4. **Determinism is a contract.** Every generator takes an explicit `seed`
   and is a pure function. No `rand::thread_rng()` anywhere in `theory/`.
5. **`theory/` never depends on `control/`, `audio/` or Tauri.** If a
   generator needs project data, the caller passes it in.
6. **Every generated artefact carries an annotation** with a tick span, a
   short label and a sentence. A generator that cannot explain itself is
   not finished.
7. **Chord symbols are spelled from the line of fifths**, never re-derived
   from pitch classes. Enharmonic distinctions are load-bearing.
8. **The palette rule is the documented one (§4.3)**, and its seven-chord
   table is a test, not a comment.
9. **Additive only**: new commands, new op arms, a new optional
   `project.json` key. `OP_FORMAT_VERSION` stays 2; `schemaVersion` is not
   bumped (readers tolerate a missing `harmony` key, and a project that
   never used the Composer resaves byte-diff-free).
10. **No MCP roster growth this round.**

## 8. Non-goals

* **Notation.** No engraving, no staff view. Spelling is correct so that a
  *later* notation surface is possible, but this is not that.
* **Audio-domain chord detection.** Harmony recognition from audio (as
  opposed to from MIDI) is a model problem, not a theory problem.
* **Style transfer / "sound like artist X".** Corpus imitation is the thing
  we are explicitly not building; the differentiator is explainability.
* **Replacing the piano roll.** The Composer writes ordinary clips and gets
  out of the way. Anything it makes must be editable by hand, forever.
* **Polytonality, serialism, spectral music.** Out of scope; the palette
  model assumes functional tonality and says so.
* **A theory quiz.** Teaching happens through the work, not beside it.

## 9. Pointers

* [`docs/superpowers/plans/2026-08-17-plan-h-composer.md`](../superpowers/plans/2026-08-17-plan-h-composer.md)
  — the H1 implementation plan and its scope rulings.
* [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md) §11 (control-plane seam),
  §13 (ticks + tempo map), ADR 0006 (thin renderer).
* [`pitch-as-data.md`](pitch-as-data.md) — the precedent for "derived data
  is not document state", and the melody-extraction slice that feeds the
  Composer a harmony to analyse.
* [`00-ROADMAP-real-alternative.md`](00-ROADMAP-real-alternative.md) — where
  this sits among the other tracks.
* Theory sources worth citing in code comments: Lerdahl & Jackendoff
  (*A Generative Theory of Tonal Music*, metrical hierarchy); Longuet-Higgins
  & Lee 1984 (syncopation measure); Toussaint 2005 (*The Euclidean Algorithm
  Generates Traditional Musical Rhythms*); Caplin (*Classical Form*, sentence
  and period); Berklee chord-scale theory (available notes and avoid notes);
  Huron (*Sweet Anticipation*, melodic-arch and step-inertia statistics).
