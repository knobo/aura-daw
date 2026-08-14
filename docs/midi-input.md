# Connect a MIDI keyboard

AURA can list hardware MIDI input ports, let you pick one, show a live "is
anything arriving" indicator, and — as of **slice 1b** — let you actually
**hear** what you play through a small built-in preview instrument. This is
still not the real instrument/routing path: playing a key does not write
notes into a clip, does not touch the project, and does not route to a
track's instrument. Recording/routing into the project lands in a later
slice.

## Supported path in this slice

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
anything.

If you don't see your device in the list, check `aconnect -l` (or
`amidi -l`) at the shell to confirm ALSA sees it at all — AURA only lists
what ALSA-seq reports.

## Preview-grade monitoring, not the real RT path (important)

The sound you hear when monitoring is **preview-grade**, the exact same
audition mechanism `sampler_preview_note` already uses to let you hear a
loaded instrument without a project open: a small dedicated output stream,
control-thread message passing, and the sampler voice engine. It is
**not** the low-latency real-time instrument-track path. Expect a small,
inconsistent amount of latency (typically a handful of milliseconds, but
not guaranteed) — good enough to confirm "yes, my keyboard works and I can
hear it," not good enough to gig on. Real low-latency routing into a
track's instrument through the main audio graph is planned for a later
slice, after the in-flight architecture work lands.

The monitor tone itself is a fixed built-in sound (not whatever
instrument you may have loaded via "load sampler instrument" elsewhere in
the app) — this keeps monitoring working out of the box with zero setup,
even in a brand new project with nothing loaded.

## What this slice does NOT do

* No notes are written into the project/document — nothing you play is
  recorded, undoable, or saved.
* No routing to a track's instrument — the monitor tone is a fixed
  built-in sound, not your project's instruments.
* No persistence — the selected port (and the monitor toggle) reset every
  time you restart AURA. (Planned for a later slice.)

## Limitations

* The port list is a one-shot read taken when the app starts — it does not
  live-refresh if you plug in a new device while AURA is already open;
  restart AURA to see a newly connected device.
* If the selected device is unplugged mid-session, the connection simply
  stops producing events (the dot goes dark, the tone stops); AURA does
  not currently surface an explicit "device disconnected" message in this
  slice.
* Monitoring is preview-grade latency (see above), and mono-timbre — every
  key sounds the same built-in tone, just at a different pitch.
