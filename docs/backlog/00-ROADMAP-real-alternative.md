# Roadmap: what AURA needs to be a real alternative

**Captured 2026-08-14 from the owner ("hva mangler for å være et reelt
alternativ og faktisk brukandes til å lage gode ting?"), written the day
Gate E closed (op log ON: full undo/redo + journal landed).** This is the
index; chunky tracks have their own backlog docs, small items live inline
here. Status keys: [done] landed, [track X] has a backlog doc, [inline]
described here only.

## Already differentiating (landed)

- The Composer (PR #65, 2026-08-17): a harmony document in the core
  document model, theory-driven and *explainable* generation next to the ML
  generators, and an editor that colours its own keys by what each note does
  over the chord. `composer-assistant.md` §2 is honest about the prior art
  (Cubase's chord track, Hooktheory, Scaler) and about which combination is
  actually new.
- AI sidecars: hum-to-song, stem splitting (real backend path since Plan E
  Task 11), AMT infill, LoopJam.
- The Plan E core: one mutation channel, full undo/redo (Ctrl+Z), a
  persisted journal, attribution (user/agent/engine/system), MCP agents
  mutating alongside the user.
- Plan F history storage (PR #23): published `SessionSnapshot`, lock-free
  rebuild assembly, version graph, panic rollback, journal reader
  (detection only).
- Modulation graph (PR #39): several curves per track, automation tracks,
  clip envelopes. Finished-system path is design §8.
- Universal sample import: drag-and-drop from a file manager
  (ImportDropZone) with WAV/MP3/FLAC/OGG/AAC/M4A decode (symphonia).
- Hardware MIDI input + output (PR #17 slice 1, owner-verified with an
  LPK25; PR #21 slice 2: routing, recording, clock/sync, note-out).
- MIDI launch map **v0.1** (PR #42): named launchers, GATE/ONE-SHOT,
  clip-as-instrument, shadow playhead so the arrangement loop stays put.
  Follow-up (sustain / overlapping voices) is in `midi-launch.md`, not
  this cut.

## Tier 1 — usable for finishing real music (weeks)

| Item | Status / doc |
|---|---|
| Library & browser panel with audition preview | **[done]** PR #19 — leftovers in `library-and-browser.md` |
| MIDI recording from hardware (quantize on capture optional) | **[done]** PR #21 — owner ear checks + loop-record still owed; `hardware-midi-io.md` |
| Automation audible (RT attach) + lane UI | **[done]** PR #20 — owner ear check + plugin-param bounce still owed; `automation-audible-and-ui.md` |
| Multi-clip selection, group drag, cross-track paste | **[done]** PR #22 — batch delete/nudge still open; `multi-clip-selection-and-paste.md` |
| Cross-instance / OS-clipboard copy (incl. SMF fallback) | **[done]** PR #22 — owner two-instance check owed; SMF is export-only |
| Metronome/click + count-in | **[done]** PR #38 — engine-side click, CLICK chip + volume pref, count-in 0/1/2/4 bars. App prefs, not project.json. |
| Quantize in the piano roll | **[done]** PR #32 — Q / Shift+Q over the selection; one `midi_set_notes` = one undo. |
| Insert FX chains per track + sends/busses | **[plan G]** G1 plan: `docs/superpowers/plans/2026-08-16-plan-g1-insert-fx-pdc.md`. Product: `insert-fx-sends-sidechain.md` (host CLAP/LV2, no stock DSP). **Tasks 1–4 landed** (PR #52 + PR #55 `118ae23`). Next is Task 5 (mixer strip). G2 bus+sends / G3 sidechain / G4 envelope-follower wait. **PDC before sends ship.** Do **not** start G2. |
| MIDI clock/start-stop out (Hydrogen sync) | **[done]** PR #21 slice 4 — owner Hydrogen ear check owed |
| **Compose with theory you don't know yet** (owner ask 2026-08-17) | **[track H]** H1 landed: a pure theory library, the harmony document in the core, five generators (progression, voice-led chords, bass, melody, groove), the COMPOSER panel with an interactive circle of fifths, and a piano roll that tints its keys by what each note does over the chord. Product doc + H2–H6 roadmap: `composer-assistant.md`. Plan: `docs/superpowers/plans/2026-08-17-plan-h-composer.md`. Handoff (incl. the owner ear-check owed): `docs/handoff/composer-h1.md` |

## Tier 2 — competitive (months)

- Time-stretch / pitch-shift of audio clips (engine + UI; big). **Shares
  its shifter with pitch correction** — whichever lands first owns it;
  see `pitch-correction-autotune.md`.
- **Sing-along from any song** (owner, 2026-08-17): import a song → split
  stems → melody from the `vocals` stem → sing against it in the Pitch
  Coach, scored and recorded. **Three of the four steps are landed**;
  melody extraction is the keystone, which raises its priority above the
  auto-tune work. Two things to settle first: whether the detector survives
  a Demucs stem (testable today, no new code) and tempo alignment — an
  imported song has its own tempo and `import.rs` detects none, so a
  tick-based reference lands in the wrong place. `pitch-as-data.md`.
- **Pitch as data** — owner ask 2026-08-17: extract a pitch stream from a
  vocal/instrument lane and use it as input. Answer: notes go in a MIDI clip
  (exists), the curve goes in the `APTF` pitch track Pitch Coach Task 14
  already specifies (does not exist yet), and NOT in MIDI control points.
  Cheapest first slice is "extract melody from this clip" → MIDI.
  **Four decisions are due before Task 14 freezes the format** — see
  `pitch-as-data.md`.
- **Pitch correction / auto-tune** — owner ask 2026-08-17 after hearing the
  Pitch Coach track a real voice. Detection is done and measured (3.3 cents
  vs the sidecar, no octave errors in 1312 frames); what is missing is a
  formant-preserving shifter and a correction policy. Staged path, offline
  first: `pitch-correction-autotune.md`. Stage C (live insert) waits on G1
  + PDC.
- Pattern instancing (shared `ContentId` — groundwork landed in Plan C/D;
  round-2 §2.1 remint rules bind the first split/merge/copy op).
- Takes & comping (natural continuation of Plan F's history storage).
- Stems/multitrack export (export_song exists; add per-track/stem render).
- Freeze / bounce-in-place.
- External instrument tracks (MIDI out + audio return; track B slice 6)
  — product cut in `external-instrument-return.md`: per-track return
  source, visible freeze clips, PipeWire one-click link. No hidden
  tracks. **X1 slice landed** (PR #37): record an external return onto
  the same MIDI track. The rest of the product cut is still open.
- Two-instance coexistence (fixed MCP port 41717 collides today — dynamic
  port + discovery needed before "copy between instances" is fully real).
- MIDI launch **sustain**: overlapping voices so retriggering a scene
  does not cut the previous one (`midi-launch.md`). Third play mode
  after GATE/ONE-SHOT. Wait until v0.1 has been used.
- **Control surface** — a virtual mixer / pad deck (knobs, gauges,
  mute/solo, N×M pads that breathe with the waveform, Add-all
  recipes, LPD8 template). Host chrome, not a plugin. Track:
  `control-surface.md`. v0.1 is in flight (PR #113).

## Sequencing notes

- Tracks A–F are all landed (PRs #23 / #21 / #22 / #20 / #19 / #39). Do
  not restart them. Leftovers live in each handoff / this table, not as
  a sixth parallel track. Briefing: `next-prompt.md`. Log:
  `docs/handoff/landed-tracks.md`.
- The FX/bus item should be planned like the core rounds were (research →
  plan doc → gates), not improvised — it touches the RT graph invariants
  round-2 §8 reserves for the node-graph round. Product cut is in
  `insert-fx-sends-sidechain.md` (host plugins, PDC in G1, no stock DSP).
- Plan F (history storage, round-2 §6) is landed (PR #23). Do not start
  another history-storage track. Carry-forwards: live-document B-tree
  (trigger = note-delta op), I-1 option-(a) residual, no auto-apply of
  journal tails, version-graph product surface unbuilt.
- In flight: PR #54 (pitch RT, other owner). Launch map v0.1 is
  PR #42+#50; Pitch Coach phase 1 is PR #49 (panel/scoring wait on
  the owner's ear check); external editor is PR #48. Continue G1
  at Task 5.
