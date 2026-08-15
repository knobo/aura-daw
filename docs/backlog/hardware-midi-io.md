# Backlog: hardware MIDI I/O — keyboards in, Hydrogen/devices out

**Captured 2026-08-14 from the owner, mid-Plan-E.** Verified against HEAD:
AURA has ZERO hardware-MIDI code today (no midir/ALSA-seq/JACK-MIDI; cpal
is audio-only). MIDI exists solely as document data (clips/notes → internal
synth, CLAP/LV2, sampler; SMF import/export). This doc records the owner's
request and the architecture it lands on, for pickup after Plan E.

## The owner's requirements (2026-08-14)

1. **Automate external MIDI devices from AURA** — e.g. start/stop
   patterns/tracks on the Hydrogen drum machine and other MIDI gear.
2. **MIDI input from keyboards** — plug in a MIDI keyboard, play/record
   into AURA.
3. **User documentation** for the setup when it exists, so it's easy.

## Design notes (post-Plan-E mapping)

- **Library**: `midir` is the standard Rust cross-platform choice (ALSA
  seq on Linux → Hydrogen/keyboards appear as ports; virtual ports
  supported). JACK-MIDI/Link are later options.
- **MIDI-in (keyboard)**:
  - Port selection mirrors the audio-device carve-out (Plan E ruling,
    inventory row 29): app config behind ControlPlane methods with
    attribution logging — no ops.
  - Live monitoring: input events routed on the engine control/RT side to
    the armed track's instrument (internal synth or plugin) — engine
    state, not document state.
  - Recording: buffer events during record; on stop, the take is
    registered as ONE `Actor::Engine` transaction (`MidiClipAdd` with the
    captured notes) — "the op is the registration, never the recording
    itself" (§4.4), exactly like audio-recording finalize (Task 13).
    Undo of a take = one entry, free via Task 17 history.
- **MIDI-out / sync (Hydrogen etc.)**:
  - Two viable Linux paths: (a) MIDI clock + Start/Stop/Continue (+SPP)
    sent from the engine thread on transport changes — Hydrogen slaves to
    this natively; (b) JACK-transport integration (Hydrogen follows JACK
    BBT). Start with (a): it's device-agnostic and pure engine work.
  - Timing source: the engine-global steady/sample clock (Task 16) +
    the section table for tempo — clock ticks derive from the same
    bijection playback uses; no frontend involvement (ADR 0006).
  - Note-out to external gear (playing a MIDI track through an external
    synth) = a track "external instrument" target — bigger feature,
    separate cut.
- **What this is NOT**: none of this touches the document model — no
  format bump needed. Port config may persist in app config (prefs-like,
  backend side since it's per-machine), not in project.json (a project
  opened on another machine must not break on missing ports).

## Suggested cut when picked up

1. `midir` port enumeration + selection commands (ControlPlane, attributed
   config) + a settings UI dropdown (mirror the audio-device UI).
   — **LANDED, slice 1 (PR #17).**
2. MIDI-in → live monitoring to the armed track's instrument.
   — **LANDED, slice 2** (`MidiInHub` SPSC ring → the mixer dispatches into
   the target track's existing live node; works playing *and* stopped).
3. MIDI-in recording → `Actor::Engine` take registration (one tx, undoable).
   — **LANDED, slice 2** (`MidiClipAdd` rides in the same
   `commit_recording_finalize` transaction as the audio `ClipAdd`s, so a
   take is one undo entry).
4. MIDI clock + Start/Stop out on transport changes (Hydrogen sync).
   — **LANDED, slice 2** (a dedicated non-RT `aura-midi-out` thread over
   `SharedRt` + a tempo/event snapshot; `engine.rs` is not involved).
5. User doc: "Connect a MIDI keyboard" + "Sync Hydrogen to AURA" recipes.
   — **LANDED, slice 2**: `docs/midi-input.md`, `docs/midi-output.md`.
6. (Later) external-instrument track routing; JACK transport/Link.
   — **PARTLY LANDED, slice 2**: note-out to external gear works as an
   app-config routing ("play this MIDI track through the output port"),
   which is why no document field, no new op and no `OP_FORMAT_VERSION`
   bump were needed. The *document-model* per-track external-instrument
   target is still deferred.

Plan: `docs/superpowers/plans/2026-08-14-midi-slice-2.md` (slice 2, PR #21).

## Still open after slice 2

- **Recording under an active loop silently produces a wrong take** — the
  one thing here that can cost a user work, so it is first. Events are
  stamped with `SharedRt::position`, which jumps BACKWARDS on a loop wrap;
  `capture::take_notes`' `rel_tick` computes
  `samples_to_tick(sample).saturating_sub(start_ticks)`, so every post-wrap
  event saturates toward 0. Consequences: a note held across the wrap gets
  exactly 1 tick; post-wrap notes land EARLIER in the clip than pre-wrap
  ones; and if stop lands post-wrap, `end_sample < start_sample`, so
  `take_end_ticks` is 0 and `length_ticks` falls back to `last_note_end`.
  Nothing is malformed — every note validates, the clip is sorted, the
  transaction commits, undo removes it in one step — which is exactly what
  makes it dangerous: a *plausible-looking wrong take* the user has to
  notice. Documented as a "don't do that" in `docs/midi-input.md`. Two
  candidate fixes, both **behaviour decisions**, not refactors: (a) refuse
  to arm capture while `shared.loop_spec().active()` (honest, blunt, and
  loop-plus-record is a normal workflow people will want back); (b)
  truncate the take at the first backward jump (keeps the pre-wrap pass,
  silently discards the rest). Neither is obviously right; ask the owner.
- **Seeking during a take corrupts it the same way** — the seek path never
  checks whether a take is running, so the same `saturating_sub` collapse
  applies. Whatever fix is chosen for the loop case should cover this one.
- **A held note is cut at every loop wrap while jamming.** The wrap issues
  `all_notes_off` on the track's node, which is right for clip notes but
  also releases the keys the player is holding, since monitoring shares
  the node. The fix is to re-issue monitoring's held note-ons after a wrap,
  which means the held-key mask has to reach the mixer.
- **Sub-block MIDI-in timestamping.** Events are quantised to the audio
  block they arrived in (~5–11 ms). The follow-up is midir's `stamp_us` +
  a block-start wall-clock anchor.
- **Port/target persistence across restarts.** Blocked on stable port ids:
  today an id is `"<name>#<index>"`, so restoring it can select a
  different device after a replug.
- **MIDI thru** (keyboard → output port while stopped) — a small addition
  to the out thread.
- **CC / pitch-bend / aftertouch / program-change**, in or out. The parser
  and the ring already carry a `kind` byte, so this is additive.
- **MIDI-out latency offset** — external gear runs roughly one output
  buffer early; documented, not compensated.
- **Quantize / count-in / metronome on the take**; loop-aware overdub and
  take comping.
- **JACK transport / Ableton Link** (the design notes' path (b)).
- **The document-model external-instrument track target** (item 6 above).

### Two flaky `midi_out` tests (found by Track C, 2026-08-15)

`midi_out::tests::shutdown_flushes_note_off_before_stop_and_before_the_connection_drops`
and
`midi_out::tests::editing_the_routed_track_releases_a_sounding_note_instead_of_hanging_it`
each failed once under a full parallel `cargo test`, and each was green in
isolation, green under `--test-threads=1`, and green on an immediate full
re-run. Both were observed from a branch that touched **zero Rust files**.

**The reading — recorded as a READING, not as a fact.** Inherent wall-clock
timing under parallel load, not mutual interference. Supporting it: the two
are correctly isolated by identity (distinct ALSA client names and distinct
virtual port names); setup failures are guarded into SKIPS rather than
failures; they failed INDEPENDENTLY in different runs (mutual interference
would tend to fell them together); one was reproduced on a diff containing no
Rust at all; and they are the only two tests in the module that are BOTH
real-ALSA AND timed-assertion — the other ~25 are pure state-machine tests
and never flake. What is unguarded is the ASSERTION BUDGET: "run ~400 ms,
then assert that specific bytes arrived over a real ALSA loopback", with a
delivery path of thread → ALSA seq kernel queue → virtual input callback
thread → mutex push, while ~690 lib tests run one per core.

**Recommended fix — NOT `#[serial]`.** Serializing hides the budget problem,
slows the suite, and does not help on a loaded CI runner where the contention
is external. Poll-until-condition with a generous ceiling instead: poll the
`seen` vector every few ms up to ~5 s and assert as soon as the bytes land —
fast when idle, robust under load, and red only when the behaviour is
actually broken.

**The discriminating check, for whoever picks this up:** record the SHAPE of
the failing assertion. If `seen` is EMPTY or SHORT (bytes missing), the
budget reading holds. If the bytes are PRESENT but WRONG, it is interference
and this reading is wrong.
