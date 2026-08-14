# Connect a MIDI keyboard

AURA can list hardware MIDI input ports, let you pick one, and show a live
"is anything arriving" indicator. This is **slice 1**: enumeration,
selection and activity only — playing a key does not yet write notes into a
clip or sound an instrument. Recording/routing lands in a later slice.

## Supported path in this slice

**Linux via ALSA-seq** is the verified, supported path. AURA uses the
[`midir`](https://docs.rs/midir) crate, which gets the ALSA sequencer
backend for free on Linux — no extra system packages beyond what the audio
engine (cpal/ALSA) already needs. Other platforms midir also supports
(CoreMIDI on macOS, WinMM/WinRT on Windows) are not manually verified for
AURA yet.

## Steps

1. **Plug in your MIDI keyboard/controller**, ideally before opening AURA —
   the port list is read once when the app starts (see
   [Limitations](#limitations) below).
2. **Open AURA.**
3. In the bottom master strip, find the **midi in** selector next to the
   audio input/output device selects.
4. **Pick your device** from the dropdown. Selecting **None** closes any
   open connection.
5. **Play a key.** The small dot next to the selector lights up briefly
   every time a MIDI message arrives (roughly every time the dot's status
   is polled while a message is fresh — under ~300ms old).

If you don't see your device in the list, check `aconnect -l` (or
`amidi -l`) at the shell to confirm ALSA sees it at all — AURA only lists
what ALSA-seq reports.

## What this slice does NOT do

* No notes are written into the project/document — nothing you play is
  recorded, undoable, or saved.
* No routing to an instrument/track — you will not hear sound from the
  keyboard.
* No persistence — the selected port resets to "None" every time you
  restart AURA. (Planned for a later slice.)

## Limitations

* The port list is a one-shot read taken when the app starts — it does not
  live-refresh if you plug in a new device while AURA is already open;
  restart AURA to see a newly connected device.
* If the selected device is unplugged mid-session, the connection simply
  stops producing events (the dot goes dark); AURA does not currently
  surface an explicit "device disconnected" message in this slice.
