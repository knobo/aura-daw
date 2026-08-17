# Plan H1 — The Composer MVP (theory core + harmony document + generators)

> **STATUS: LANDED 2026-08-17** on `feat/composer-assistant` (branched from
> `origin/main` at `79c3e98`). Handoff, including the owner ear-check owed and
> what H2 should pick up first:
> [`docs/handoff/composer-h1.md`](../../handoff/composer-h1.md). Measured after
> the work, `--test-threads=1`: **1246 backend** (1210 lib + 36 integration,
> plus 2 `#[ignore]`d) and **778 frontend**, from 1083 + 738 at the branch
> point. Every task below is checked; two things in the plan changed shape while
> being built and the boxes are ticked against what was actually done:
>
> * **Task 5 grew a sixth generator**, `theory/bass.rs` (root / root-fifth /
>   walking with real approach notes / pedal). The plan had bass in H3; a
>   generated sketch without one is not listenable, and it is 200 lines.
> * **`progression::generate` returns the key it generated IN**, not just the
>   slots. A minor-mode schema asked for in a major key resolves against the
>   RELATIVE minor (the Andalusian cadence in C major is `Am G F E` — same seven
>   notes, different home), so the caller has to be told which key to write into
>   the document. The plan assumed the requested key always won.
> * **The dock shortcut is `O`, not `C`** — bare `c` was already the library's
>   CLIPS destination and `dockTabForKey` is tested before it.

**Goal:** Land the spine of the Composer — a pure music-theory library, a
harmony document in the core document model, four theory-driven generators,
and a panel that makes all three visible — so that every later phase (H2–H6 in
the product doc) is a *reader* of what this plan builds rather than a rewrite
of it.

**Product doc / roadmap:** [`docs/backlog/composer-assistant.md`](../../backlog/composer-assistant.md).
Read §4 (the theory) and §7 (rulings) before implementing any task here.

**Architecture:** A pure `src-tauri/src/theory/` library (no Tauri, no locks,
no I/O, no `Session`) is called by one stateful seam,
`src-tauri/src/control/composer.rs`, which owns the commands. The harmony
document (`HarmonyDoc`: key regions + chord regions, integer ticks) lives in
`MidiStore` beside the tempo and meter maps, is written by ONE additive op
(`Op::HarmonySet`), persists as an additive `project.json` key, and rides in
the published snapshot. Generated notes land as ordinary `MidiClip`s through
the existing `Op::MidiClipAdd`/`Op::MidiSetNotes` — there is no generated-clip
type. The frontend renders pushed state and emits commands (ADR 0006).

**Tech stack:** Rust (tauri-free core, `serde`), Svelte 5 runes + TypeScript,
`cargo test` + `vitest`.

## Global constraints

- **Baseline: MEASURE IT.** At branch point (`origin/main` = `79c3e98`)
  CONTRIBUTING says **1083 backend (1047 lib + 36 integration, plus 2
  `#[ignore]`) + 738 frontend**. Never land a task that lowers either count.
- **`theory/` is pure.** No `use tauri`, no `parking_lot`, no `std::fs`, no
  `crate::control`, no `crate::audio`. A test that needs a project is in the
  wrong file. This is what keeps the theory suite fast and the library
  reusable (a future notation surface, a sidecar, a WASM build).
- **Determinism.** Every generator takes `seed: u64` and is a pure function.
  No `thread_rng`. Same seed + same params ⇒ byte-identical notes; there is a
  test that says so.
- **Ticks, never seconds** (D-02). Harmony spans and generated notes are
  integer ticks at the project PPQ.
- **One mutation channel.** Harmony edits go through `Op::HarmonySet` via
  `ControlPlane::commit`; one gesture = one op = one undo. `ChangeSet::
  from_ops` is an exhaustive match — the new arm is a compile error until
  mapped.
- **Additive only.** `OP_FORMAT_VERSION` stays **2**; `schemaVersion` is not
  bumped; the `harmony` key in `project.json` is written only when the
  document is non-empty, so a project that never used the Composer resaves
  byte-diff-free. New commands are additive `generate_handler!` lines.
- **No MCP roster growth.**
- **Thin renderer (ADR 0006).** No theory in TypeScript; no authoritative
  harmony state on the client. The circle widget's *geometry* is
  presentation, its *content* is backend-shipped.
- **Theme tokens only** in new Svelte `<style>` blocks — `no-literals.test.ts`
  fails a raw colour literal. *(Correction, as built: the palette ramp borrows
  EXISTING accent tokens rather than adding new ones — a new token means editing
  the `ThemeTokens` contract, all eight built-in themes and the user-theme
  schema, for colours that already have the right meanings. The mapping lives in
  `src/lib/utils/palette-colors.ts` and is tested against every built-in theme.)*
- Run `timeout 900 cargo test` (with `--test-threads=1`, see CONTRIBUTING),
  `timeout 300 npx vitest run` and `npx svelte-check` before each commit that
  touches the corresponding half. Foreground, `timeout`-guarded.

## Scope rulings (H-1 … H-12)

1. **H-1 — The atom is a tonal pitch class (line of fifths), not a pitch
   class.** `Tpc(i16)`, `C = 0`, `G = +1`, `F = -1`. Spelling, key
   signatures, chord symbols and transposition are all arithmetic on it
   (product doc §4.1). Pitch classes are a *projection* (`(fifths * 7) mod
   12`), never the storage.
2. **H-2 — Scales are fifths-offset tables**, so a diatonic mode is a
   contiguous 7-window and spelling is free (§4.2). No semitone-set-only
   scale type.
3. **H-3 — The harmony document is a session-level map pair.**
   `HarmonyDoc { keys: Vec<KeySpan>, chords: Vec<ChordSpan> }` in `MidiStore`,
   sorted by tick, first key span at tick 0 when non-empty. Not per-track,
   not per-clip. No polytonality.
4. **H-4 — ONE op, atomic, value-replacement: `Op::HarmonySet { keys,
   chords }`.** Exactly `Op::TempoSet`'s shape and for the same reason: the
   two maps must move together (a chord region and the key it is analysed
   in are one edit). The inverse carries the previous pair. Region counts are
   tens, not thousands, so whole-list replacement is right; a keyed upsert
   would buy nothing and complicate folding.
5. **H-5 — No new `TrackKind`, no new lane row.** The chord ribbon is chrome.
6. **H-6 — Generated notes are plain MIDI clips**, created with the existing
   ops. No provenance field on the clip, no regeneration link, no "AI clip"
   type. Annotations are returned with the command result for display only —
   they are **not** document state (§7.3).
7. **H-7 — `composer_generate` is batch-shaped and transactional.** One call
   may produce several clips (chords + bass + melody + drums) and they land
   in **one** transaction, so one Ctrl+Z removes the whole idea. Partial
   failure applies nothing.
8. **H-8 — Track creation is composed, not implied.** A generate call with
   no target track auto-creates one via `ops::add_track_tx` inside the same
   transaction (the `hum.rs` precedent), naming it after the part.
9. **H-9 — The palette rule is §4.3's, and its seven-chord table is a
   test.** `Avoid` = a key tone a semitone above a chord tone. No other
   heuristic, no genre exceptions in H1.
10. **H-10 — Velocity and micro-timing come from the metrical weight
    function**, not from per-generator constants. One weight function, five
    uses (§4.7a). A generator that invents its own accent rule is a bug.
11. **H-11 — The engine is not touched.** No RT code, no graph change, no new
    node. Generation is control-plane-only; audibility comes for free because
    the output is an ordinary MIDI clip on an ordinary instrument track.
12. **H-12 — Demo mode gets a fixture, not a second implementation.** The
    browser demo backend answers the composer commands from a small canned
    fixture so the panel is not dead in `npx vite`. Duplicating theory in
    TypeScript would violate ADR 0006; a fixture is data.

## File structure

**Created**

| File | Responsibility |
|---|---|
| `src-tauri/src/theory/mod.rs` | module doc + re-exports; the purity contract |
| `src-tauri/src/theory/tpc.rs` | `Tpc`, `Pitch`, spelling, intervals, parsing |
| `src-tauri/src/theory/scale.rs` | `ScaleType`, `Key`, degrees, key signatures |
| `src-tauri/src/theory/chord.rs` | `ChordQuality`, `Chord`, symbols (render + parse), roles |
| `src-tauri/src/theory/analysis.rs` | Roman numerals, `Function`, borrowed-chord detection, why-strings |
| `src-tauri/src/theory/circle.rs` | circle of fifths: windows, distance, neighbours, walks, pivots |
| `src-tauri/src/theory/palette.rs` | the available-notes classifier (§4.3) |
| `src-tauri/src/theory/metre.rs` | metrical weights, Bjorklund/Euclid, rotation, LHL syncopation |
| `src-tauri/src/theory/progression.rs` | schemas + the functional automaton + `suggest_next` |
| `src-tauri/src/theory/voicing.rs` | candidate voicings + DP voice-leading |
| `src-tauri/src/theory/melody.rs` | motif, development, figuration fix-up, phrase cadences |
| `src-tauri/src/theory/groove.rs` | role-based drum generator, GM map, genre vectors |
| `src-tauri/src/theory/harmony.rs` | `HarmonyDoc`, `KeySpan`, `ChordSpan`, lookup, validation |
| `src-tauri/src/theory/rng.rs` | the one seeded PRNG (SplitMix64) the generators share |
| `src-tauri/src/control/composer.rs` | the stateful seam: the four commands + the transaction |
| `docs/ipc-schemas/harmony.schema.json` | harmony document wire form |
| `docs/ipc-schemas/composer.schema.json` | request/reply shapes for the four commands |
| `src/lib/components/composer/ComposerPanel.svelte` | the dock panel |
| `src/lib/components/composer/CircleOfFifths.svelte` | the interactive circle |
| `src/lib/components/composer/ProgressionStrip.svelte` | chord regions + Roman numerals |
| `src/lib/state/composer.svelte.ts` | the frontend store (pushed state only) |
| `src/lib/state/composer.svelte.test.ts` | store tests |
| `src/lib/components/composer/circle-geometry.ts` (+ test) | pure wedge geometry (presentation) |

**Modified:** `src-tauri/src/lib.rs` (four `generate_handler!` lines + `mod
theory`), `src-tauri/src/midi/mod.rs` (`MidiStore::harmony`),
`src-tauri/src/midi/persist.rs` (`V3Data::harmony`, read/write),
`src-tauri/src/control/op.rs` (`Op::HarmonySet`),
`src-tauri/src/control/session.rs` (`apply_raw` arm, `midi_snapshot`),
`src-tauri/src/control/snapshot.rs` (`MidiSnapshot::harmony`, `ChangeSet`),
`src-tauri/src/control/mod.rs` (`ProjectSnapshot::harmony`, `mod composer`),
`src-tauri/src/control/vergraph.rs` (op summary line),
`src/lib/types/ipc.ts`, `src/lib/tauri.ts`, `src/lib/demo.ts`,
`src/lib/state/ui.svelte.ts` (dock tab), `src/lib/components/Dock.svelte`,
`src/lib/components/pianoroll/PianoRoll.svelte` (palette tint),
`src/app.css` (palette tokens), README, CONTRIBUTING (counts).

**Deliberately untouched:** the RT engine, the MCP roster,
`OP_FORMAT_VERSION`, `schemaVersion`, the sidecar stack, `TrackKind`.

---

## Task 1: the theory core — spelling, scales, chords, keys

**Files:** create `theory/mod.rs`, `theory/tpc.rs`, `theory/scale.rs`,
`theory/chord.rs`; modify `lib.rs` (`pub mod theory;`).

**Interfaces:**

```rust
pub struct Tpc(pub i16);                 // line of fifths; C = 0, G = 1, F = -1
impl Tpc { fn pitch_class(self) -> u8; fn letter(self) -> char;
           fn accidentals(self) -> i8;  fn name(self) -> String;
           fn parse(s: &str) -> Option<Self>; fn plus_fifths(self, n: i16) -> Self; }
pub struct Pitch { pub tpc: Tpc, pub octave: i8 }   // midi = 12*(oct+1) + base + acc
pub enum ScaleType { Ionian, Dorian, …, Blues, WholeTone, Octatonic }
pub struct Key { pub tonic: Tpc, pub scale: ScaleType }
pub enum ChordQuality { Maj, Min, Dim, Aug, Sus2, Sus4, Maj6, Min6,
                        Maj7, Dom7, Min7, MinMaj7, HalfDim7, Dim7, … }
pub struct Chord { pub root: Tpc, pub quality: ChordQuality, pub bass: Option<Tpc> }
```

**Steps**
- [x] Failing tests first: `F#`/`Gb` spell differently and share a pitch
      class; `Cb4` sounds `B3` but keeps its letter; `Key::spelled` gives
      `[A,B,C,D,E,F,G]` for A aeolian; the seven diatonic sevenths of C
      major are `Cmaj7 Dm7 Em7 Fmaj7 G7 Am7 Bm7b5`; symbol round-trip
      (`parse(symbol(c)) == c`) for every quality; key signature of `F#`
      major is 6 sharps.
- [x] Implement. Scales as fifths-offset tables (H-2); chord qualities as
      fifths-offset tables so `Cdim7`'s seventh spells `Bbb`.
- [x] Property test: `Tpc` transposition by n fifths then −n is identity for
      `n ∈ −20..20`; `pitch_class` matches a semitone-table oracle.

## Task 2: analysis, the circle, the palette

**Files:** create `theory/analysis.rs`, `theory/circle.rs`,
`theory/palette.rs`.

**Steps**
- [x] Failing tests: `G7` in C major analyses as `V7`, function `Dominant`;
      `Bb` in C major is `♭VII`, borrowed, with a why mentioning
      mixolydian/subdominant; `A7` in C major is `V/ii`; the C-major circle
      window is `F C G D A E B`; `key_distance(C major, A minor) == 0` and
      `(C, E major) == 4`; **the seven-row palette table from product doc
      §4.3 is a test** (this is H-9 and the rule's credibility).
- [x] Implement. `palette` returns a per-pitch-class role + degree label
      (`"♯11"`, `"♭7"`) + optional why; `nearest_usable` snaps a key to the
      closest non-`Avoid` class (H2's "no wrong notes" seam, exposed now,
      wired later).

## Task 3: metre — weights, Euclid, syncopation

**Files:** create `theory/metre.rs`, `theory/rng.rs`.

**Steps**
- [x] Failing tests: 4/4 at 16ths gives weights
      `[4,0,1,0,2,0,1,0,3,0,1,0,2,0,1,0]`; 6/8 groups by three; `euclid(3,8)`
      is the tresillo `x..x..x.`; `euclid(5,8)` the cinquillo; rotation is
      cyclic and length-preserving; LHL syncopation of a straight
      four-on-the-floor is 0 and of `..x.` patterns is positive; SplitMix64
      reproduces a known vector.
- [x] Implement. The weight function is THE accent/velocity source (H-10).

## Task 4: the harmony document + `Op::HarmonySet` + persistence

**Files:** create `theory/harmony.rs`; modify `midi/mod.rs`,
`midi/persist.rs`, `control/op.rs`, `control/session.rs`,
`control/snapshot.rs`, `control/mod.rs`, `control/vergraph.rs`; create
`docs/ipc-schemas/harmony.schema.json`.

**Interfaces:**

```rust
pub struct KeySpan   { pub tick: u64, pub key: Key }
pub struct ChordSpan { pub tick: u64, pub length_ticks: u64, pub chord: Chord }
pub struct HarmonyDoc { pub keys: Vec<KeySpan>, pub chords: Vec<ChordSpan> }
impl HarmonyDoc { fn key_at(&self, tick: u64) -> Option<&Key>;
                  fn chord_at(&self, tick: u64) -> Option<&ChordSpan>;
                  fn validate(&self) -> Result<(), String>; }   // sorted, non-overlapping, len > 0
Op::HarmonySet { keys: Vec<KeySpan>, chords: Vec<ChordSpan> }
```

**Steps**
- [x] Failing tests: `HarmonySet` round-trips through the op wire form;
      `apply_raw` rejects unsorted/overlapping spans **before mutating**
      (atomicity) and returns an inverse carrying the previous pair; undo
      restores it exactly; `ChangeSet::from_ops` flags the new arm; a save →
      load round-trip preserves the document; a project that never used the
      Composer resaves **without** a `harmony` key.
- [x] Implement. Chord/key spans serialise as `{tick, symbol}` /
      `{tick, tonic, scale}` strings on the wire (stable, readable, and it
      keeps `Tpc`'s integer encoding an implementation detail).

## Task 5: generators — progression, voicing, melody, groove

**Files:** create `theory/progression.rs`, `theory/voicing.rs`,
`theory/melody.rs`, `theory/groove.rs`.

**Steps**
- [x] Failing tests per generator:
      *progression* — a circle walk from C returns `C G D A …`; the
      `I-V-vi-IV` schema in D major is `D A Bm G`; the functional automaton
      always ends on a cadence and never repeats a chord three times;
      `suggest_next` after `C F` ranks `G` (dominant) above `Eb`.
      *voicing* — voice-leading `C → Am → F → G7` moves fewer semitones
      than root-position stacking; every voice stays in the register; the
      7th of `G7` resolves down to `B`… (the tendency-tone test).
      *melody* — every strong-beat onset is a chord tone or an available
      extension, never `Avoid`; no leap > an octave; a leap is followed by a
      step in the opposite direction; the last note is a tonic-triad member
      on a strong beat; the motif recurs (bar 3 shares its interval
      contour with bar 1 under some development op).
      *groove* — the kick is on the downbeat of every bar; the snare is on
      the weight-2 backbeat positions; measured syncopation increases
      monotonically with the syncopation dial; velocities correlate with
      metrical weight; a fill appears at the end of every `fill_every` bars;
      **same seed ⇒ identical notes** (determinism, for all four).
- [x] Implement. Each generator returns `(Vec<MidiNote>, Vec<Annotation>)`;
      `Annotation { tick, length_ticks, label, why }` (H-6).

## Task 6: `control/composer.rs` — the four commands

**Files:** create `control/composer.rs`; modify `control/mod.rs`, `lib.rs`;
create `docs/ipc-schemas/composer.schema.json`.

**Commands (additive):** `harmony_get`, `harmony_set`, `composer_palette`,
`composer_suggest`, `composer_generate`.

**Steps**
- [x] Failing tests: `harmony_set` is one op and one undo entry;
      `composer_generate { parts: [chords, bass, melody, drums] }` lands four
      clips in ONE transaction (one undo removes all four — H-7) and
      auto-creates the tracks it needs (H-8); a rejected request mutates
      nothing; `composer_palette` at a tick inside a chord region returns
      that chord's palette and outside every region returns the key's.
- [x] Implement. `ControlPlane` methods first, commands as thin wrappers
      (ARCHITECTURE §11). Validation and generation happen **outside** the
      transaction (the `hum.rs` prepare-outside pattern); the transaction is
      the mutation only.

## Task 7: the COMPOSER panel

**Files:** create `src/lib/components/composer/*`,
`src/lib/state/composer.svelte.ts` (+ tests); modify `Dock.svelte`,
`ui.svelte.ts`, `tauri.ts`, `types/ipc.ts`, `demo.ts`, `app.css`.

**Steps**
- [x] Failing store tests: the store applies pushed harmony from the
      snapshot, never computes theory; clicking a wedge appends one chord
      through one `harmony_set`; the generate button passes the displayed
      seed; the dice re-rolls the seed and nothing else.
- [x] Pure geometry helper for the circle (wedge paths, hit-testing) with its
      own test — the only frontend logic this plan allows.
- [x] Implement the panel: circle (key arc highlighted, borrowed chords
      outside it), progression strip with Roman numerals, generate controls
      with a seed + dice, and the WHY list.
- [x] Dock tab `c` (`DOCK_SHORTCUT`), demo fixture (H-12).

## Task 8: the piano roll teaches

**Files:** modify `pianoroll/PianoRoll.svelte`, `app.css`.

**Steps**
- [x] Tint each key row by its palette class at the chord in force (existing
      accent tokens, no literals — see the corrected constraint above). The
      tint follows the chord region under the playhead, not a static key, and
      the grid is banded per region.
- [x] Name the chord in force, and what its notes do, in the roll's header
      chip (`♪ Cmaj7 · I`, with the full breakdown as its tooltip). *(As built:
      a header chip that also toggles the tint, rather than a pointer-follow
      status line — the roll has no status line, and a chip that names the
      chord doubles as the legend for the colours.)*

## Task 9: docs, counts, handoff

- [x] README feature section + a screenshot placeholder; CONTRIBUTING test
      counts re-measured with the date; `docs/ARCHITECTURE.md` §16 (the
      theory library's purity contract + the harmony document's place);
      `docs/backlog/00-ROADMAP-real-alternative.md` row;
      `docs/handoff/composer-h1.md`; `next-prompt.md` current-job line.
