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

## The root cause, found later: a re-cue storm

The channel bug above is real, but it is NOT what made the device look broken.
Snooping the ALSA-seq port the device listens on, during a real session, gave:

| | count |
|---|---|
| `Stop` -> `SongPosition` -> `Continue` triples | **31 676** |
| clock pulses | 22 429 |
| note-ons | 950 |

`midi_out`'s clock engine was re-cueing the device roughly 1500 times a second.
`drift_tolerance` was a flat `rate / 50` (960 samples, 20 ms at 48 kHz) while
PipeWire's default quantum is 1024 frames (21.3 ms) — and `SharedRt::position`
only moves when an audio callback runs, so the divergence sawtooths up to one
full block every block. The threshold sat *below* the sawtooth it existed to
ignore, so it tripped forever. Nothing downstream can start under that, and the
flood of realtime bytes overran the receiving client's event pool, so notes were
dropped as collateral — heard as "three or four hits, a gap, three or four
hits".

Fixed by publishing `SharedRt::block_frames` from the callback and sizing the
tolerance as `max(rate / 50, 2 * block)`, plus making a small forward resync
advance through the gap instead of `reseek`ing past it. `midi_out::tests::
a_pipewire_sized_block_does_not_re_cue_the_device` is the regression test.

**Why the existing test missed it:** `block_quantized_position_does_not_trigger_a_resync`
used a 480-frame block — 10 ms, which happens to fit under 20 ms. Block size was
the variable that mattered and nothing varied it. A test that pins the intended
*behaviour* at one arbitrary parameter value is not the same as a test of the
behaviour.

## Also in the reporter's way — NOT AURA bugs

`aconnect -l` on the reporting machine showed **two paths from AURA into
Hydrogen at once**: a direct subscription to `Hydrogen:Hydrogen Midi-In`, and a
second one to `Midi Through:Midi Through Port-0`, which Hydrogen is itself
subscribed to (its configured input port). Every clock pulse and every note
therefore arrived twice — 48 ppq instead of 24, i.e. a slave running at double
tempo. Hydrogen's own output was also looping back into Midi Through and thence
into its own input (`enable_midi_feedback: true`).

AURA cannot detect this; from its side they are two unrelated ports. Added to
`docs/midi-output.md`'s troubleshooting section.

Carla, which the owner moved to mid-session, publishes **no ALSA-seq port at all**
when its engine is JACK — so it is invisible to AURA's `midir` ALSA backend no
matter how long you wait. The way in was PipeWire's ALSA-seq/JACK-MIDI bridge
(client `PipeWire-RT-Event`), patched in Carla's own patchbay. Also documented.

The owner confirmed by ear that a routed track reaches another MIDI application,
and that the internal synth used to double it. The re-cue fix was reported as
"better" but has not been confirmed against a fresh wire capture yet.

If a simpler target is wanted for the next ear check: anything that maps GM
percussion without a session concept is less work than Hydrogen — a soundfont
player on channel 10 (`fluidsynth -o synth.midi-bank-select=gm`) or
`qsynth`. Hydrogen adds transport slaving, a MIDI action map, and a kit
mapping on top of the thing under test.
