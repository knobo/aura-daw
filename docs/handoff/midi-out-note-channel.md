# MIDI-out: the note channel, the doubled instrument, and the ALSA address

Branch `worktree-fix-midi-out`, one commit. Fixes the bug
[`next-prompt.md`](../../next-prompt.md) recorded after the Composer H1
ear-check as "MIDI-out to Hydrogen, needs its own unscoped PR".

## What the report was

> I patch a MIDI track to Hydrogen and I can't hear the drum machine playing
> it. Also there shouldn't be a local instrument on a routed MIDI track any
> more, should there?

## How it was diagnosed

Worth repeating, because guessing at this one is expensive and measuring it is
cheap:

1. **The wire, end to end.** A scratch test built a real `ControlPlane` + real
   engine, committed a MIDI clip, opened a `midir` virtual ALSA-seq input, set
   a route through `set_midi_track_route`, and rolled the transport. It showed
   note-on/off *did* leave AURA, with `resyncs: 0` — so neither the clock
   engine nor the note scheduler was at fault. That scratch test is now
   `midi_out::route_e2e_test`.
2. **The reporter's own document.** `project.json` + the AMEV chunk under
   `events/` for the routed track: 100 notes, GM keys 36/38/42/43/47/49/50,
   **every one on channel 9**. Their `midi-routing.json` had the route on
   `channel: 0`.
3. **The reporter's own `~/.hydrogen/hydrogen.conf`.** `channel_filter: -1`
   (all channels), input port `Midi Through Port-0`.
4. **`aconnect -l` on the live session.** See *Still open* below.

That is what identified the seam: `AbsNoteEvent` carried no channel, so the
route's channel (a `u8` defaulting to 0) was stamped over every note. MIDI
*file* export honoured `MidiNote::channel`; live routing never had it.

## What changed

* `AbsNoteEvent::channel`, threaded from `MidiNote` through
  `schedule::expand_notes`. The internal live node ignores it (per-track,
  mono-timbral); `midi_out` does not.
* `RouteTarget::channel: Option<u8>` — `None` (default, "from clip" in the
  panel) follows each note, `Some(ch)` forces the route. Persisted `0`
  migrates to `None` (`persist::de_channel`), because under the old shape a 0
  could not be told apart from "never picked".
* `NoteOutEngine::sounding` indexed by key **and** channel, so `all_off`
  releases on the channel a note started on.
* `midi_out::RoutedOut` + `engine::ControlMsg::SetExternalRouting` +
  `ControlPlane::publish_external_routing`: a routed track/clip loses its
  internal voice. Routing is app config, so it cannot ride the committer's
  document image — it goes over the control channel, same nature as
  `live_in_target`. Two deliberate exemptions, both commented at the code:
  live monitoring (no MIDI thru yet) and the offline bounce (an export must not
  depend on which machine has the cable).
* `persist::port_name_key` strips ALSA's volatile `"<client>:<port>"` suffix so
  routing survives the device restarting at a new sequencer address, on adopt
  and on merge-on-write.
* `append_from` now forwards to `append_from_with_input` instead of
  duplicating its loop, as its doc already claimed.

## Still open — NOT AURA bugs, but they were in the reporter's way

`aconnect -l` on the reporting machine showed **two paths from AURA into
Hydrogen at once**: a direct subscription to `Hydrogen:Hydrogen Midi-In`, and a
second one to `Midi Through:Midi Through Port-0`, which Hydrogen is itself
subscribed to (its configured input port). Every clock pulse and every note
therefore arrived twice — 48 ppq instead of 24, i.e. a slave running at double
tempo. Hydrogen's own output was also looping back into Midi Through and thence
into its own input (`enable_midi_feedback: true`).

AURA cannot detect this; from its side they are two unrelated ports. Added to
`docs/midi-output.md`'s troubleshooting section. **The owner has not yet
re-tested by ear with this branch**, so whether the channel fix alone makes
Hydrogen play is unconfirmed — the wire-level assertion is, the ear check is
not.

If a simpler target is wanted for the next ear check: anything that maps GM
percussion without a session concept is less work than Hydrogen — a soundfont
player on channel 10 (`fluidsynth -o synth.midi-bank-select=gm`) or
`qsynth`. Hydrogen adds transport slaving, a MIDI action map, and a kit
mapping on top of the thing under test.
