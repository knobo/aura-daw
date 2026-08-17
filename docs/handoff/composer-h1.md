# Handoff: the Composer, Plan H1 (2026-08-17)

Branch `feat/composer-assistant`, worktree `.claude/worktrees/composer`,
branched from `origin/main` at `79c3e98`.

**What this is.** The spine of the Composer: a pure music-theory library, a
harmony document in the core document model, five generators, five additive
commands, a panel, and a piano roll that colours its own keys by what each note
does over the chord in force. Every later phase (H2–H6 in the product doc) is a
*reader* of what this built.

* Product doc / roadmap: [`docs/backlog/composer-assistant.md`](../backlog/composer-assistant.md)
* Plan + rulings H-1…H-12: [`docs/superpowers/plans/2026-08-17-plan-h-composer.md`](../superpowers/plans/2026-08-17-plan-h-composer.md)
* Architecture: [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md) §16 (and the
  command names in §3.3)
* Wire forms: `docs/ipc-schemas/harmony.schema.json`,
  `docs/ipc-schemas/composer.schema.json`

## Commits

| Commit | What |
|---|---|
| `3a4cd92` | `theory/` core — spelling (line of fifths), scales, chords, analysis, the circle, the palette, metre, the seeded PRNG |
| `34e6a1e` | the harmony document + progression / voicing / melody / groove generators |
| `9ae6267` | `MidiStore.harmony`, `Op::HarmonySet`, persistence, the published snapshot, `control/composer.rs`, the five commands, `theory/bass.rs`, the TS surface |
| `3b2d701` | the COMPOSER panel, the circle widget, the progression strip, the store, the piano-roll tint, the demo fixture |
| `6256f83` | docs: README, ARCHITECTURE §16, the wire schemas, counts, this file |
| `f619dbe` | the three field-by-field copies that were missing the harmony document, each with the test that fails without the fix |
| this one | the pre-existing hum-test race, diagnosed and fixed (see "Where the numbers came from") |

## The decisions that will bite if they are forgotten

1. **`theory/` is pure and must stay pure.** No `tauri`, `parking_lot`,
   `std::fs`, `crate::control`, `crate::audio`, no `thread_rng`. The single
   allowed crate dependency is `crate::midi::MidiNote` (the tauri-free document
   note type). If a generator needs project data, the CALLER passes it in.
2. **Determinism is a contract, not a nicety.** Every generator takes
   `seed: u64`; `composer_generate` derives one from the request when the
   caller omits it, so "the same request" stays reproducible without a clock.
   A `Math.random()` or a `SystemTime::now()` anywhere in this track breaks a
   test on purpose.
3. **The harmony document is a map pair, not a track.** `MidiStore.harmony`,
   ticks, one op (`Op::HarmonySet`), no `rebuild` effect, `OP_FORMAT_VERSION`
   stays 2, `schemaVersion` unmoved, `harmony` written to `project.json` only
   when non-empty. Do not add a `TrackKind`, a lane row, or a second op.
4. **Generated clips are ordinary clips.** No provenance field, no
   regeneration link, no "AI clip" type. Annotations are display-only and are
   never persisted. If they ever must persist, that is a derived cache (like
   the pitch curve), and its own round.
5. **One transaction per sketch.** `composer_generate` prepares outside the
   lock and commits harmony + tracks + up to four clips together. A part that
   fails writes nothing.
6. **The avoid-note rule is the feature's credibility.** A key tone a semitone
   ABOVE a chord tone. The seven-chord table in `palette.rs`'s tests is the
   spec; the `Fmaj7` row (the ♯11 must come out *available*) is the one that
   proves the rule is doing work rather than flagging everything unfamiliar.
7. **One metrical weight function** (`metre::Grid::weights`) drives kick
   placement, the backbeat, hat accents, ghost notes and every velocity in
   every generator. A part that invents its own accent rule is a bug (H-10).
8. **The dock shortcut is `O`, not `C`.** Bare `c` is already the library's
   CLIPS destination in `App.svelte`, and `dockTabForKey` is tested *before*
   it — binding the composer there would silently shadow a shipped shortcut.

## Owner checks owed (no suite substitutes for these)

* **Ear-check a generated sketch.** Open the COMPOSER panel, pick a plan,
  GENERATE, and listen. The tests prove the notes obey the rules; they cannot
  tell you whether the result is *nice*. Specifically worth judging:
  - are the voice-led chords smooth, or is the register too low/high? (defaults
    are C3–C6 for the comp, E1–E3 for the bass, C4–C6 for the melody)
  - does the melody sound composed rather than sampled? The development ops are
    weighted by taste, and taste is what wants an owner's ear.
  - do the drums groove at the default `syncopation`/`density`, or do the
    genre vectors need moving?
* **The drum track needs a kit.** A generated drum clip is GM keys on channel
  10 with nothing to play them: there is no bundled drum kit, so on the
  built-in PolySynth it sounds like low pitches. Point the track at a sampler
  instrument or a plugin. The clip's own annotation says so, and the honest fix
  (a bundled GM-ish kit, or auto-binding one) is a product decision, not a bug.
* **Try it in a minor key and in 3/4.** Both are covered by tests, neither has
  been heard.

## The rough edge you will hit first

**Generating twice makes a second set of tracks.** Each `composer_generate` is
an independent sketch: with no `trackIds` it creates the tracks it needs, so
pressing GENERATE again gives you `Composer Chords 2` and friends. That is
deliberate for H1 and the alternative is worse — reusing the same tracks would
drop a second clip on top of the first at the same tick, and two MIDI clips at
one position both play, so you would hear doubled notes rather than a new idea.

The workflow that works today is **Ctrl+Z, re-roll the seed, GENERATE** — one
undo removes the whole sketch, which is exactly what H-7's single transaction
bought. A proper "replace the last sketch" (remember the clips, remove them in
the same transaction that writes the new ones) is a small, well-shaped follow-up
and belongs in H2 with the rest of the editing surface.

## Deliberately not done (and why)

* **No MCP tools.** The roster stays frozen this round (H-12/ruling 10).
  Candidates when it opens: `get_harmony`, `set_harmony`, `suggest_chords`,
  `generate_parts` — all already exist as `ControlPlane` methods, so it is a
  `tools.rs` change and a policy decision, not new plumbing.
* **No "no wrong notes" input yet.** `Palette::nearest_safe_midi` exists and is
  tested; wiring it into `midi_input` is H2's first item and the cheapest big
  win left in the track.
* **No chord ribbon in the timeline ruler.** The progression is visible in the
  panel and as bands in the piano roll. The ruler is H2.
* **No harmony detection from an existing MIDI clip.** H2. It is the on-ramp
  for people who do not start from chords, and it is the reverse direction of
  everything here (notes → chords), so it wants its own slice.
* **No comp/arpeggio pattern generator, no section grid, no modulation
  operation.** H3.
* **Annotations are not shown on the timeline**, only in the panel. A
  hover-a-clip-see-the-why surface is worth doing and is not free.

## Where the numbers came from

Counts measured on this branch, `--test-threads=1`, against a real ALSA sink
(see CONTRIBUTING for why both matter). Baseline at branch point was 1083
backend + 738 frontend.

**One pre-existing flake surfaced, was diagnosed, and is fixed here.**
`control::hum::tests::apply_hum_clip_commits_synchronously_and_announces_project_changed`
asserted `rev == before_rev + 1` ("one commit, one rev bump") while its fixture
runs a REAL engine — and opening the output stream submits a transient
`Set{Transport, SampleRate}` commit from the engine control thread
(`engine::commit_output_sample_rate`), which races the `before_rev` read. It
failed in roughly half of the full single-threaded runs on this branch and
passed 15/15 in isolation; a clean `main` lib run passed, which is consistent
with a timing race that a 160-test-longer suite is simply more likely to lose.
The assertion now measures the UNDO DEPTH instead: transient commits never
enter history, so one non-transient commit is exactly one undo entry, and the
check is both race-free and closer to what the test means. `rev` is still
asserted to advance. Nothing in the Composer commits on that path — the flake
is not a regression, it is an assertion that was always racy.
