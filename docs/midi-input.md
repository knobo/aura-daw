# Connect a MIDI keyboard

AURA lists hardware MIDI input ports, lets you pick one, shows a live "is
anything arriving" indicator, and lets you **hear** what you play. Since
**slice 2** it also lets you play a keyboard **through a track's own
instrument** and **record** what you play into a MIDI clip — one clip, one
undo step.

## Supported path

**Linux via ALSA-seq** is the verified, supported path. AURA uses the
[`midir`](https://docs.rs/midir) crate, which gets the ALSA sequencer
backend for free on Linux — no extra system packages beyond what the audio
engine (cpal/ALSA) already needs. Other platforms midir also supports
(CoreMIDI on macOS, WinMM/WinRT on Windows) are not manually verified for
AURA yet.

## Steps: pick a port, play, hear it

1. **Plug in your MIDI keyboard/controller**, ideally before opening AURA —
   the port list is read once when the app starts (see
   [Limitations](#limitations) below).
2. **Open AURA.**
3. In the bottom master strip, find the **midi in** selector next to the
   audio input/output device selects.
4. **Pick your device** from the dropdown. Selecting **None** closes any
   open connection (and silences anything still sounding).
5. Leave the **monitor** checkbox next to it checked (it's on by default
   whenever a port is selected).
6. **Play a key.** Two things should happen:
   * The small dot next to the selector lights up briefly on every
     incoming MIDI message (roughly every time the dot's status is polled
     while a message is fresh — under ~300ms old).
   * You **hear a tone** — a simple built-in sine-ish sound, not
     necessarily any instrument you have loaded elsewhere in the app.
     Release the key and the tone stops (with a short release tail).

Uncheck **monitor** to go back to silent (activity-indicator-only) mode
without closing the port — the dot still lights, you just won't hear
anything. Recording still captures what you play with monitor off.

That built-in tone is the *unarmed* case. Arm a MIDI track and you hear
that track's instrument instead — next section.

If you don't see your device in the list, check `aconnect -l` (or
`amidi -l`) at the shell to confirm ALSA sees it at all — AURA only lists
what ALSA-seq reports.

## Play a track's own instrument (arm it)

Click the **R** (arm) button on a **MIDI track**. From then on your
keyboard plays *that track's* instrument — the synth, sampler or plugin
the track actually uses — through the main audio graph, with the track's
gain/pan/mute/solo applied. It works whether the transport is playing or
stopped, so "press play, arm a track, jam along" is a normal thing to do.

* Disarm the track to go back to the built-in preview beep.
* **Only one MIDI track receives input at a time** — arming another moves
  the routing (last armed wins). Arming an *audio* track does not touch
  MIDI routing at all.
* Right after you move the routing there is a short (up to ~85 ms) window
  where the newly armed track is deaf while the previous one releases its
  notes. A key struck inside that window is dropped.

## Record what you play

1. Arm the MIDI track (as above).
2. Hit **record**, play, hit **stop**.
3. One MIDI clip appears on that track at the position recording started,
   with the notes you played. **Ctrl+Z removes the whole take in one
   step**; Ctrl+Shift+Z brings it back; saving and reopening keeps it.

You do **not** need an audio input device for a MIDI-only take — no input
stream is opened and no WAV file is written. Recording does not depend on
the **monitor** checkbox either: notes are captured whether or not you can
currently hear them.

## Preview-grade monitoring when nothing is armed (important)

With **no MIDI track armed**, the sound you hear is **preview-grade**: the
same audition mechanism `sampler_preview_note` uses to let you hear a
loaded instrument without a project open — a small dedicated output
stream, control-thread message passing, and the sampler voice engine. It
is **not** the low-latency instrument-track path. Expect a small,
inconsistent amount of latency — good enough to confirm "yes, my keyboard
works", not good enough to gig on. The tone is a fixed built-in sound (not
whatever instrument you loaded elsewhere), so monitoring works out of the
box in a brand new project.

Armed, you get the real path instead: the track's own instrument through
the main audio graph. You never get both at once, and never neither.

## Limitations

### Do not record while a loop is active

**Recording with the loop turned on produces a take whose timing is
wrong.** Nothing errors, nothing looks broken, and the clip is a perfectly
ordinary clip — but the notes are in the wrong places: everything you play
after the loop jumps back lands *earlier* in the clip than what you played
before it, and a note held across the jump comes out impossibly short. You
have to notice it yourself.

Until this is fixed: **turn the loop off before you arm and record**, and
if you did record under a loop, undo the take and do it again without one.

(Mechanism, for whoever fixes it: incoming events are stamped with the
transport position, which jumps backwards on a loop wrap, and the take's
tick conversion clamps at zero. See `docs/backlog/hardware-midi-io.md`.)

### Everything else

* The port list is a one-shot read taken when the app starts — it does not
  live-refresh if you plug in a new device while AURA is already open;
  restart AURA to see a newly connected device.
* If the selected device is unplugged mid-session, the connection simply
  stops producing events (the dot goes dark, the tone stops); AURA does
  not currently surface an explicit "device disconnected" message in this
  slice.
* Unarmed monitoring is preview-grade latency (see above), and
  mono-timbre — every key sounds the same built-in tone, just at a
  different pitch. Armed, you hear the track's real instrument instead.
* **Nothing is remembered across restarts**: the selected port, the
  monitor toggle and the routing all reset. Deliberate — a port id is
  `"<name>#<index>"`, which can point at a *different* device after a
  replug or reboot, so restoring it could silently select the wrong one.
* **Timing is quantised to the audio block a message arrived in** (~5–11 ms
  at typical buffer sizes, plus the driver's own delivery latency). Notes
  are not placed inside the block, and there is no quantize, count-in or
  metronome on the take yet.
* No CC, pitch-bend, aftertouch or program-change is recorded or routed —
  notes only.
* No MIDI **thru**: an armed keyboard plays AURA's instrument, it is not
  forwarded to the MIDI output port (see `docs/midi-output.md`).
