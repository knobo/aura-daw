# Roadmap: what AURA needs to be a real alternative

**Captured 2026-08-14 from the owner ("hva mangler for å være et reelt
alternativ og faktisk brukandes til å lage gode ting?"), written the day
Gate E closed (op log ON: full undo/redo + journal landed).** This is the
index; chunky tracks have their own backlog docs, small items live inline
here. Status keys: [done] landed, [track X] has a backlog doc, [inline]
described here only.

## Already differentiating (landed)

- AI sidecars: hum-to-song, stem splitting (real backend path since Plan E
  Task 11), AMT infill, LoopJam.
- The Plan E core: one mutation channel, full undo/redo (Ctrl+Z), a
  persisted journal, attribution (user/agent/engine/system), MCP agents
  mutating alongside the user.
- Universal sample import: drag-and-drop from a file manager
  (ImportDropZone) with WAV/MP3/FLAC/OGG/AAC/M4A decode (symphonia).
- Hardware MIDI input slice 1: port select + activity + audible monitoring
  (PR #17, owner-verified with an LPK25).

## Tier 1 — usable for finishing real music (weeks)

| Item | Status / doc |
|---|---|
| Library & browser panel with audition preview | **[track E]** `library-and-browser.md` |
| MIDI recording from hardware (quantize on capture optional) | **[track B]** `hardware-midi-io.md` slice 2-3 |
| Automation audible (RT attach) + lane UI | **[track D]** `automation-audible-and-ui.md` |
| Multi-clip selection, group drag, cross-track paste | **[track C]** `multi-clip-selection-and-paste.md` |
| Cross-instance / OS-clipboard copy (incl. SMF fallback) | **[track C]** same doc, §cross-instance |
| Metronome/click + count-in | **[inline]** engine-side click synth on the RT path, tempo-map-driven (section table already gives sample-exact beats); UI toggle + volume; count-in = N bars of click before record start. No document state beyond a project setting. |
| Quantize in the piano roll | **[inline]** command over the selection (note-ops already has the selection model): snap note starts (and optionally lengths) to grid with strength %; one `midi_set_notes` commit = one undo step. Backend-side math (thin renderer). |
| Insert FX chains per track + sends/busses | **[plan G]** `insert-fx-sends-sidechain.md` — host CLAP/LV2 effects (do **not** write a stock FX suite). Sequence: G1 insert chain + PDC, G2 bus + sends, G3 sidechain listen-taps, G4 envelope-follower modulator (later, not Plan G). Round-2 rule still binds: **PDC before sends ship**. Needs its own research → plan → gates round (graph/mixer work). |
| MIDI clock/start-stop out (Hydrogen sync) | **[track B]** slice 4 |

## Tier 2 — competitive (months)

- Time-stretch / pitch-shift of audio clips (engine + UI; big).
- Pattern instancing (shared `ContentId` — groundwork landed in Plan C/D;
  round-2 §2.1 remint rules bind the first split/merge/copy op).
- Takes & comping (natural continuation of Plan F's history storage).
- Stems/multitrack export (export_song exists; add per-track/stem render).
- Freeze / bounce-in-place.
- External instrument tracks (MIDI out + audio return; track B slice 6)
  — product cut in `external-instrument-return.md`: per-track return
  source, visible freeze clips, PipeWire one-click link. No hidden
  tracks.
- Two-instance coexistence (fixed MCP port 41717 collides today — dynamic
  port + discovery needed before "copy between instances" is fully real).

## Sequencing notes

- Tracks B/C/D/E are parallel-safe post-PR-#12 (see next-prompt.md's
  track map for file footprints; B and D overlap in engine.rs).
- The FX/bus item should be planned like the core rounds were (research →
  plan doc → gates), not improvised — it touches the RT graph invariants
  round-2 §8 reserves for the node-graph round. Product cut is in
  `insert-fx-sends-sidechain.md` (host plugins, PDC in G1, no stock DSP).
- Plan F (history storage, round-2 §6) runs beneath all of this as track A.
