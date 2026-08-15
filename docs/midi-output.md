# Drive external MIDI gear from AURA

AURA can send **MIDI clock and transport messages** to another device (so
Hydrogen — or any drum machine, groovebox or DAW that slaves to MIDI
clock — follows AURA's play/stop/tempo), and can **play one MIDI track's
notes out to an external synth**.

Both live in the bottom master strip, in the **midi out** group next to
the **midi in** one: a port selector, a **clock** checkbox, and a **trk**
selector for note-out.

**Linux via ALSA-seq** is the verified path (the same `midir` backend the
input side uses). `aconnect -l` at the shell lists what ALSA can see.

---

## Recipe 1: sync Hydrogen to AURA

1. Start Hydrogen (or whatever should follow) **before** opening AURA, or
   restart AURA after — the port list is read once at startup.
2. In AURA's master strip, pick the device in the **midi out** dropdown.
3. Leave **clock** checked (it is on by default).
4. In Hydrogen: **Preferences → MIDI**, set the *input* port to AURA, and
   turn on transport slave / "use MIDI clock" so Hydrogen follows an
   external clock rather than its own.
5. Press **play** in AURA. Hydrogen should start with it and stop with it.

**What AURA sends**

| When | Message |
|---|---|
| Play from position 0 | `Start`, then clock |
| Play from anywhere else | `SongPosition` (SPP), then `Continue`, then clock |
| While playing | 24 pulses per quarter note (`Clock`) |
| Stop | `Stop` |
| Loop wrap, or any backward seek | `Stop` → `SongPosition` → `Continue` |

That last row is a deliberate choice and **you will hear it**: on every
loop wrap the slave is re-cued to the loop start rather than free-running.
The alternative (silently re-anchoring the clock) keeps the tempo but lets
the slave's bar phase drift away from AURA's, so a looped section would
slowly stop lining up. If your Hydrogen dislikes the re-cue, that is the
knob to change — it is a one-line behaviour swap in `midi_out.rs`.

**Tempo changes** reach the slave within about 250 ms (the out thread
re-snapshots the tempo map on that cadence). Clock pacing between blocks
is interpolated but re-anchored on the engine's own observed position, so
it cannot drift away from AURA over a long song.

---

## Recipe 2: play a MIDI track through an external synth

1. Pick the device in **midi out** (same selector as above).
2. Pick the track in the **trk** selector next to it — only MIDI tracks
   are listed.
3. Press play. That track's notes go out to the external instrument, on
   MIDI channel 1.
4. **Mute the track in AURA** if you don't want to hear its internal
   instrument at the same time. The track keeps sounding internally
   otherwise — routing to a device does not replace AURA's own voice, and
   muting is the documented way to avoid doubling.

Only **one** track can be routed out at a time, and the routing is a
per-machine app setting: it is not stored in the project, and it resets
when you restart AURA (same reason as the input port — a port id can point
at a different device after a replug).

**Timing caveat, not compensated**: the external device receives its notes
roughly one output-buffer *earlier* than AURA plays its own audio, so it
runs slightly ahead. There is no latency-offset preference yet. With a
small buffer this is a few milliseconds; with a large one it is audible,
and the workaround today is a smaller audio buffer.

---

## Troubleshooting

* **Nothing happens at all.** Check the **clock** checkbox is on for
  sync, or that a track is selected in **trk** for notes. Check
  `aconnect -l` shows both AURA and the target. Check the receiving app is
  actually set to slave to external MIDI clock — most default to their own
  internal clock.
* **Hydrogen starts but drifts / restarts oddly.** That is likely the loop
  re-cue above, firing on every wrap.
* **A note hangs on the external device.** AURA releases sounding notes
  when you change port, change routed track or close the app. If one hangs
  anyway, toggle the **trk** selector to None and back.
* **Timing gets lumpy under heavy CPU load.** The out thread is a plain OS
  thread at default priority and can be starved. The symptom is visible
  rather than mysterious: the `resyncs` counter in `midi_output_status`
  climbs.

## Not in this slice

* No MIDI **thru** (an armed keyboard playing straight out to gear).
* No CC, pitch-bend, aftertouch or program-change — notes plus clock and
  transport only.
* No per-track "external instrument" as a *project* setting: routing is
  app config, so it is not saved with the project (see ruling 10 in
  `docs/superpowers/plans/2026-08-14-midi-slice-2.md`).
* No JACK transport / Ableton Link.
* No MIDI-out latency offset (see the timing caveat above).
