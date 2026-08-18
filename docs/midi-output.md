# Drive external MIDI gear from AURA

AURA can send **MIDI clock and transport messages** to another device (so
Hydrogen — or any drum machine, groovebox or DAW that slaves to MIDI
clock — follows AURA's play/stop/tempo), and can **route MIDI tracks and
individual clips out to external synths**, each to its own port and MIDI
channel.

Any number of hardware output ports can be open at once. Each open port
gets its own card in the **OUT** section of the MIDI panel (the `⇄ MIDI`
tab in the right-hand dock, or press **D**): device name, a **clock**
toggle, live note and clock-pulse counters, and a close button — plus a
picker for any port not already open.

**Linux via ALSA-seq** is the verified path (the same `midir` backend the
input side uses). `aconnect -l` at the shell lists what ALSA can see.

---

## Recipe 1: sync Hydrogen to AURA

1. Start Hydrogen (or whatever should follow). Order does not matter — the
   panel re-reads the port list every couple of seconds, so a device started
   after AURA appears in the picker on its own. (It used to be read once at
   startup, which is why older notes here said to restart AURA.)
2. In AURA's MIDI panel (press **D**), open the device from the **OUT**
   section's port picker.
3. Leave that port's **clock** toggle on (it defaults to on).
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

1. Open the device in the panel's **OUT** section if it isn't already.
2. In **PATCH**, find the MIDI track you want to route. Every MIDI track is
   listed there, so you can see what is already patched where before you
   change anything.
3. Pick its port. Leave **channel** on `from clip` unless the device insists
   on one fixed channel — see *Channels* below; it matters for drums. The cord
   between the track and its port takes that port's colour, so two tracks
   sharing a port are one glance apart.
4. Press play. That track's notes go out to the external instrument, and
   **AURA stops playing its own instrument on that track** — the device you
   patched is the instrument now. Clear the route to get AURA's voice back.
5. To **capture the synth into the project** (so export hears it): next
   to the channel, pick a **return** input — the same list as the master
   strip's audio input picker. Patch the synth (or Hydrogen) into that
   input yourself (a cable, or Helvum / qpwgraph). Arm the track and
   record. A WAV clip lands on *this* MIDI track.
6. That clip is then what you hear and what `export_song` mixes.

Unlike the single-track carve-out this shipped with originally, **more than
one track can be routed at once** — give any other track in **PATCH** its
own port and channel; the previous track's routing is untouched. The panel
lists every route at once, so you never have to remember what you set.

### Channels

A route's **channel** field is an override, not the channel. Left on
`from clip` — the default — every note goes out on the channel it carries in
the document (`MidiNote.channel`, what a `.mid` import preserves and what MIDI
export writes). Pick 1-16 instead and the whole route is forced onto that one
channel, which is what a mono-timbral synth listening on a single channel
wants.

`from clip` is the setting that matters for **drums**. General MIDI puts
percussion on channel 10, and that is what the Composer's groove generator
writes and what a drum machine maps to its kit — so a forced channel 1 (what
every route used to send, unavoidably) reaches the device and triggers
nothing. If a drum machine is receiving but silent, check this field first.

### What the internal instrument does

A routed track does not sound AURA's own instrument: routing REPLACES the
internal voice rather than layering with it. Without that, a drum clip plays
as pitches out of AURA's synth at the same time as the drum machine plays it
as drums, loud enough to hide the device you are trying to hear.

Two exceptions, both deliberate:

* **Live monitoring still goes through AURA.** There is no MIDI thru yet (see
  *Not in this slice*), so an armed keyboard has no path to the external
  device — killing the internal voice would leave you hearing nothing at all.
  Only a routed track's *clips* go quiet internally.
* **`export_song` still renders the internal instrument.** Routing is
  per-machine app config that never travels with the project, so honouring it
  in a bounce would make the same project export differently on the machine
  with the synth plugged in. Record a **return** (step 5) to get the device
  into the mix, or **mute** the track to keep it out — mute is document state
  and does travel.

A clip-level override behaves the same way, one clip at a time: that clip's
notes leave the internal instrument, the track's other clips keep sounding.

**Mute does not stop the MIDI.** Muting a routed track silences its AURA
channel (and keeps it out of a bounce), but the notes still leave for the
device — mute is a mixer control, and the device is not on AURA's mixer. To
stop sending, clear the route or close the port.

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
machine reopens the same ports and reapplies its routing automatically — no
more re-configuring on every launch.

Ports are matched by device **name**, because the `"<name>#<index>"` id
`midir` hands out can shift between restarts. On ALSA the name itself ends in
the sequencer address the kernel assigned (`Hydrogen:Hydrogen Midi-In 128:0`),
and that number is handed out in connection order — so restarting Hydrogen, or
plugging a USB keyboard in first, renames the port. Matching therefore tries
the exact name first and then the name with that trailing address stripped, so
a device that came back at a new address keeps its routing instead of silently
losing it.

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

* **The device is running but not in the picker at all.** AURA sees
  **ALSA-seq** ports (`aconnect -l` is the exact list it reads). A host whose
  MIDI runs over **JACK** — Carla with the JACK engine, for instance — publishes
  JACK ports, which are a different namespace and do not show up there no matter
  how long you wait. Either switch that host's MIDI to ALSA in its own settings,
  or bridge the two (`a2jmidid`). If `aconnect -l` does not list it, neither
  does AURA, and that is a fact about the other program.
* **Nothing happens at all.** Check that port's **clock** toggle is on
  for sync, or that a track/clip is routed to a port for notes. Check
  `aconnect -l` shows both AURA and the target. Check the receiving app is
  actually set to slave to external MIDI clock — most default to their own
  internal clock.
* **The device receives, but plays nothing.** Almost always the channel. A
  drum machine only maps percussion on MIDI channel 10; set the route's
  channel field back to `from clip` (the default) so the notes go out on the
  channel the clip actually says. If the device has a channel filter of its
  own, either set it to *all* or match it to what the clip uses.
* **A track is routed but silent.** The MIDI panel's **PATCH** section says
  why. An amber cord and *"Port is not open — notes are not leaving AURA."*
  means the route points at a port nobody opened — its **Open port** button
  fixes that in place. A red cord and *"Device missing"* means the port the
  route names is not on this machine any more; reconnect it or pick another.
  Routes whose track or clip has since been deleted collect in the panel's
  **left over** footer: they send nothing, and forgetting them is safe.
* **The slave runs at double tempo, or restarts oddly.** Check `aconnect -l`
  for TWO paths from AURA to the same device. On Linux it is easy to end up
  with both `Midi Through:Midi Through Port-0` and the device's own input port
  open in AURA's **OUT** section while the device is *subscribed to Midi
  Through* — then every clock pulse and every note arrives twice, so a slave
  counting 48 pulses per quarter instead of 24 runs at 2x. Close one of the two
  ports (the direct one is the better keep), or point the device's MIDI input
  at AURA instead of at Midi Through. AURA cannot detect this: from its side
  they are two unrelated ports.

  While you are in `aconnect -l`, also look for the device's *output* port
  feeding back into Midi Through and thus into its own input — Hydrogen ships
  with MIDI feedback on, and that loop is its own source of confusion.
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
* **X1 audio return is in:** a routed MIDI track can pick a cpal input
  and record onto itself. PipeWire one-click link (X2) and freeze-then-
  export (X3) are still later — see
  `docs/backlog/external-instrument-return.md`.
