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
2. MIDI-in → live monitoring to the armed track's instrument.
3. MIDI-in recording → `Actor::Engine` take registration (one tx, undoable).
4. MIDI clock + Start/Stop out on transport changes (Hydrogen sync).
5. User doc: "Connect a MIDI keyboard" + "Sync Hydrogen to AURA" recipes.
6. (Later) external-instrument track routing; JACK transport/Link.
