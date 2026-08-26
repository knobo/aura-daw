# 13 — Players and performance: what a pad has to be

Produced: 2026-08-26, against commit `0767383` (branch `feat/control-surface`).
Audience: whoever implements [Plan V](../superpowers/specs/2026-08-26-plan-v-players-design.md).
Status: engineering audit of THIS repository, plus clearly-marked judgment
about what performers expect. Unlike dossiers 01–12 this one cites no
external source code — where it describes other software it describes
*observable behaviour*, not internals, and says so.

## 1. Why this exists

The control surface (PR #113) landed a deck of pads that fire clips. The
owner then asked for the thing a deck is actually for: pads that hold a WAV
or an instrument of their own, mix and match, carry their own knobs, and let
you keep what you played. That is not a panel feature. It is a second time
base in the engine, and it deserved an audit before a plan.

## 2. What AURA has today — verified against the code

| Capability | State | Evidence |
|---|---|---|
| Fire a MIDI clip off the timeline | Yes, one at a time | `launch_fire_from`, `FireOrigin::Preview` |
| Fire an **audio** clip | **No** | `LaunchTarget::Clip` resolves against `session.midi.clips`; an audio id errors `launch clip is gone` |
| Fire a time **region** of any track(s) | Yes | `LaunchTarget::Region { start_ticks, length_ticks, track_ids }`, used by the timeline's mark-region flow — this is the one path that already works for audio |
| Two things sounding off-timeline at once | **No** | `launch_on` / `launch_pos` / `launch_start` / `launch_end` are single atomics in `audio/rt.rs` |
| Stop something fired | Yes, since PR #113 | `launch_stop` → `SharedRt::end_launch` |
| Hardware pad without moving the arrangement | **No** | `FireOrigin::Hardware` issues `SetLoop` + `Seek` + `Play` |
| Per-node parameter clock | **No** | `ParamDriver::tick(position)` takes one position, the transport's |
| A plugin instance with no track | Legal, but silent | `PluginInstanceInfo.trackId` is `Option`; rendering requires a track slot |
| Gain / pan / inserts / sends / output on a non-track node | **Yes, effectively** | Plan G2's `bus::compile_routing` already compiles a DAG of nodes with sends, outputs and per-edge PDC |
| Curve owned by clip content, per placement | Yes | `Source::ClipEnvelope { content_id, curve_id }`, resolved via `content_placements` |
| Macro as a modulation source | Reserved, never produced | `Source::Macro { macro_id }` parses; modulation design §8.3 |
| Ports as modulation targets | Reserved | `TargetRef {kind:"port", nodeId, portId}`, modulation design §8.1, "the arm ADR 0004 reserved" |
| Automation write / touch / latch recording | Yes | PR #85; modulation §8.5 describes the remaining slice (recording into separate automation tracks) |
| Per-voice modulation address | Reserved | `(ClipId, ContentId, NoteId)`, round-2 §5 / modulation §8.8 |

Two conclusions, and they pull in opposite directions:

**The mechanism is a prototype.** One playhead, a transport hijack, a
MIDI-only clip target and automation read on the wrong clock are not
extensible; they are four separate dead ends.

**The document is not.** Every arm Plan V needs was written down as a
reserved extension a round earlier. That is unusually lucky, and it is the
difference between "rewrite the launcher" and "build the arms and retire the
prototype onto them".

## 3. What performers expect — judgment, not citation

Described as behaviour a user would notice, from using hardware and software
launchers. No claim is made about how any product implements these.

1. **Polyphony is the baseline.** A pad deck where one pad cuts another is
   perceived as broken within seconds — a kick and a snare must overlap.
2. **Choke is explicit, not accidental.** The one place a pad *should* cut
   another is a group the user chose: open and closed hi-hat, two takes of
   the same vocal phrase.
3. **Quantized start.** Pressing slightly early must not sound early. A
   launcher that fires on the exact millisecond of the press is unusable
   with a running arrangement; a launcher that fires on the next 1/16 or bar
   is playable by people who cannot play.
4. **One-shot vs gate vs loop is per pad, not global.** A drum hit is a
   one-shot; a held pad on a pad is a gate; a bed of noise loops.
5. **Velocity means something.** Even reduced to velocity → gain, a pad that
   ignores how hard it was hit reads as a button, not an instrument.
6. **Keep what you played.** The gap between "that take was good" and
   "that take exists in the project" is where performers lose work. This is
   why V6 records *elements* (editable clips) rather than a mixdown: the
   owner's own requirement was to edit it afterwards.
7. **Retrospective capture** — keeping a take you did not arm for — is the
   single most-loved feature of every launcher that has it. Named as a
   later cut, not smuggled into V6.

## 4. Two design traps found while auditing

**The hidden-track trap.** The tempting shortcut for "a pad that owns an
instrument" is a track that is hidden from the timeline. It fails in a
specific, checkable way: the offline bounce (`audio/offline.rs`) has no
concept of the launch overlay, so a hidden track holding a clip at tick 0
renders that clip at bar 1 of every export. Every timeline feature — song
end, follow-on-seek, lane UI, arm, the bounce — would need to learn to
ignore a class of track. Ruling V-2 exists because of this.

**The channel-multiplexing trap.** The tempting shortcut for "record eight
pads to one track" is one MIDI channel per pad. It collides with a shipped
feature: `docs/midi-output.md` uses the channel field for external output
routing, per track and per clip. Giving the same field a second, internal
meaning would make those two features fight over one number. Ruling V-9
therefore records to the *instrument*: pads that share an instrument share a
track and are distinguished by note number — which is exactly what a drum
kit already is — and pads with different instruments get different tracks.

## 5. What this plan does NOT need

Worth stating, because each of these was considered and dropped as
unnecessary for R1–R6: a new audio file format or cache; a second graph
compiler (Plan G2's DAG walk takes the nodes as they are); a change to the
RT callback's structure (it already renders slots with flags); a new
automation document type (`ClipEnvelope` is the envelope); and a new
undo/history mechanism (players are ordinary document objects under
ordinary ops).
