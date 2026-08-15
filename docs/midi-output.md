# Drive external MIDI gear from AURA

AURA can send **MIDI clock and transport messages** to another device (so
Hydrogen — or any drum machine, groovebox or DAW that slaves to MIDI
clock — follows AURA's play/stop/tempo), and can **route MIDI tracks and
individual clips out to external synths**, each to its own port and MIDI
channel.

Any number of hardware output ports can be open at once. Each open port
gets its own row in the **midi out** group of the bottom master strip (next
to **midi in**): device name, a **clock** checkbox, and a close button, plus
an "+ open…" picker for any port not already open.

**Linux via ALSA-seq** is the verified path (the same `midir` backend the
input side uses). `aconnect -l` at the shell lists what ALSA can see.

---

## Recipe 1: sync Hydrogen to AURA

1. Start Hydrogen (or whatever should follow) **before** opening AURA, or
   restart AURA after — the port list is read once at startup.
2. In AURA's master strip, open the device from the **midi out** "+ open…"
   picker.
3. Leave that port's **clock** checkbox on (it defaults to on).
4. In Hydrogen: **Preferences → MIDI**, set the *input* port to AURA, and
   turn on transport slave / "use MIDI clock" so Hydrogen follows an
   external clock rather than its own.
5. Press **play** in AURA. Hydrogen should start with it and stop with it.

**What AURA sends, per open port**

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

**Tempo changes** reach every slaved port within about 250 ms (each port's
own out thread re-snapshots the tempo map on that cadence).

**Expect an occasional re-cue on a long take, with no loop involved.**
Between transport events each port's clock is paced by the system clock
from an anchor set at start; AURA's audio runs on the soundcard's clock. The
two differ by a few tens of parts per million on ordinary hardware, so the
gap crosses the 20 ms threshold roughly every 5–10 minutes of continuous
play and that port's thread re-anchors with the same `Stop`/SPP/`Continue`
a loop wrap uses. The tempo stays right; you hear the re-cue. Re-anchoring
more often than the threshold would trade the artifact for jitter, which is
why it is where it is.

---

## Recipe 2: play a MIDI track through an external synth

1. Open the device in **midi out** if it isn't already.
2. Next to **trk**, pick the MIDI track you want to route — a port + MIDI
   channel selector appears for it.
3. Pick the port and (optionally) a channel other than 1.
4. Press play. That track's notes go out to the external instrument.
5. **Mute the track in AURA** if you don't want to hear its internal
   instrument at the same time. The track keeps sounding internally
   otherwise — routing to a device does not replace AURA's own voice, and
   muting is the documented way to avoid doubling.

Unlike the single-track carve-out this shipped with originally, **more than
one track can be routed at once** — pick a different track from the **trk**
selector and give it its own port/channel; the previous track's routing is
untouched.

### Per-clip overrides

Right-click a MIDI clip and choose **MIDI output…** to route just that
clip somewhere else — its own port and channel, independent of whatever
its track is routed to. A clip's own routing always wins: the track's route
skips that clip's notes so they are never sent twice. Choosing "Inherit
from track" clears the override.

### Routing persists per machine, per project

Which ports are open, each port's clock setting, and the current
track/clip routing table are all saved to a small file on THIS machine
(`~/.config/aura/midi-routing.json` on Linux) whenever you change them, keyed
by the project's file path. Opening the SAME project again on the SAME
machine reopens the same ports (matched by device **name**, since the
underlying port id can shift between restarts) and reapplies its routing
automatically — no more re-configuring on every launch.

This file is **never part of the project itself** and never travels with
it: opening the project on a different machine (different MIDI hardware) or
sharing the project file does not carry routing along, on purpose — a port
id, and often a port name, only means something on the machine that has the
hardware plugged in. This is the same "app config, not document state"
principle the original single-track carve-out shipped with (see ruling 10
in `docs/superpowers/plans/2026-08-14-midi-slice-2.md`), extended to a real
per-track/per-clip routing table with local persistence.

**Timing caveat, not compensated**: the external device receives its notes
roughly one output-buffer *earlier* than AURA plays its own audio, so it
runs slightly ahead. There is no latency-offset preference yet. With a
small buffer this is a few milliseconds; with a large one it is audible,
and the workaround today is a smaller audio buffer.

---

## Troubleshooting

* **Nothing happens at all.** Check that port's **clock** checkbox is on
  for sync, or that a track/clip is routed to a port for notes. Check
  `aconnect -l` shows both AURA and the target. Check the receiving app is
  actually set to slave to external MIDI clock — most default to their own
  internal clock.
* **Hydrogen restarts itself now and then.** Two causes, both above: the
  loop re-cue on every wrap, and the periodic clock re-anchor on long
  continuous playback. Neither is a fault you can configure away today.
* **A note hangs on the external device.** AURA releases sounding notes
  when you close a port, change a route, edit a routed clip, or close the
  app. If one hangs anyway, clear the route (or close the port) and set it
  again.
* **Timing gets lumpy under heavy CPU load.** Each port's out thread is a
  plain OS thread at default priority and can be starved. Note that
  starvation does NOT show up in the `resyncs` counter — a starved thread's
  clock estimate still tracks the engine, so no jump is detected; the
  pulses it could not send in time are simply dropped (the burst clamp),
  and the slave hears a brief slow-down instead. `resyncs` counts transport
  jumps and the periodic re-anchor above, not starvation.
* **Editing a routed track or clip while it plays** releases whatever it
  has sounding at that moment, so a long note being played out to gear
  stops when you edit it. Deliberate: the alternative is a stuck note.
  Routes to a track or clip that gets deleted are dropped automatically
  within about 250 ms (self-healing — there is no need to manually clear a
  dangling route).
* **A persisted route didn't come back after restart.** The port it points
  at isn't visible on this machine right now (unplugged, renamed, or a
  different machine entirely) — matched by device name, and reapplied only
  once that name reappears in the port list.

## Not in this slice

* No MIDI **thru** (an armed keyboard playing straight out to gear).
* No CC, pitch-bend, aftertouch or program-change — notes plus clock and
  transport only.
* No project-level (shared-across-machines) routing: it is per-machine app
  config on purpose (see the persistence section above).
* No JACK transport / Ableton Link.
* No MIDI-out latency offset (see the timing caveat above).
* No audio **return** — notes leave, the synth's sound does not come
  back, and `export_song` will not hear it. The planned cut is
  `docs/backlog/external-instrument-return.md` (record/freeze onto the
  same track, then bounce). Until that lands, record the device into an
  audio track yourself if the WAV needs to include it.
