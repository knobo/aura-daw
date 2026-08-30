# Plan V — players: a pad that is an instrument, not a shortcut

Date: 2026-08-26
Track: [`docs/backlog/plan-v-players.md`](../../backlog/plan-v-players.md)
Research: [`docs/research/13-players-and-performance.md`](../../research/13-players-and-performance.md)
Builds on: [`2026-08-15-modulation-system-design.md`](2026-08-15-modulation-system-design.md) §8,
[`2026-08-26-control-surface-design.md`](2026-08-26-control-surface-design.md),
[`docs/adr/`](../../adr/README.md) 0004 / 0006

## 1. What the owner asked for

> "Det skal være mulig å konfigurere midi-launcher clips til å by-passe
> lydsporet og spille wav'en ren uten automatiseringer og endringer, og at
> de da ligger rett på en pad i surface'en, slik som jeg tenker at et
> midi-instrument/plugin skal kunne gjøre. Disse tingene skal mikses og
> matches, og vi skal kunne legge inn egne knobs vi skal kunne justere på
> uten å nødvendigvis ha det liggende på et spor. Så skal vi kunne recorde
> det vi gjør inn på ett eller flere spor, slik at vi kan editere det
> etterpå […] slik at dette blir et instrument vi kan spille."

Unpacked, that is six requirements:

| # | Requirement |
|---|---|
| R1 | A pad fires an **audio** clip, not only a MIDI clip |
| R2 | A pad can play a WAV **raw** — bypassing the track it came from, no inserts, no automation, no track gain |
| R3 | A pad can own an **instrument or plugin** that no timeline track owns |
| R4 | Pads mix and match freely: audio, MIDI+instrument, and control-only pads on one deck |
| R5 | The surface has **its own knobs**, adjustable live, not parked on a track |
| R6 | What you play is **recorded into editable elements** on one or more tracks, automation included |

And one non-functional requirement, stated plainly: *no hacks*. Long-lived
structure we can build on, not a patch that just about works.

## 2. The verdict on the launch overlay

The owner asked whether the existing launch feature is such a hack. Read
honestly: **the mechanism is, the document is not.**

The mechanism (`midi/launch.rs`, `audio/rt.rs`) has four properties no
professional launcher can keep:

1. **One shadow playhead.** `launch_on`, `launch_pos`, `launch_start`,
   `launch_end` are single atomics ([`audio/rt.rs`](../../../src-tauri/src/audio/rt.rs)).
   Two pads cannot sound at once; retrigger rewinds the one overlay. A pad
   deck where one pad cuts another by accident is not a deck.
2. **Hardware fire hijacks the transport.** `FireOrigin::Hardware` calls
   `SetLoop` + `Seek` + `Play` on the arrangement transport
   ([`midi/launch.rs`](../../../src-tauri/src/midi/launch.rs)). Pressing a
   pad moves the user's arrangement.
3. **`LaunchTarget::Clip` is MIDI-only.** It resolves against
   `session.midi.clips`; an audio clip id errors with "launch clip is gone".
   R1 is unreachable through it.
4. **Automation is read at the wrong clock.** `ParamDriver::tick(position)`
   takes ONE position — the transport's
   ([`plugins/automation.rs`](../../../src-tauri/src/plugins/automation.rs)).
   A clip fired on the overlay plays its notes at the overlay playhead and
   its automation at the arrangement playhead. That is already wrong today,
   silently.

The document, by contrast, was designed for exactly where we are going. The
modulation design's §8 "Where this goes next" reserved, in writing, the four
arms this plan needs:

- **§8.1 Ports** — `TargetRef {kind:"port", nodeId, portId}`, "the arm ADR
  0004 reserved", explicitly a resolver-only change.
- **§8.3 Macros** — "a `[0,1]` value that is both a source and a target",
  with the cycle check already anticipated. `Source::Macro { macro_id }` is
  parsed today and never produced.
- **§8.5 Automation recording into separate automation tracks**, routing
  each take through that track's bindings.
- **§8.8 Per-voice modulation**, keyed by the sounding-instance address
  `(ClipId, ContentId, NoteId)` that round-2 §5 already records.

`Source::ClipEnvelope { content_id, curve_id }` goes further: it is a curve
owned by clip **content**, resolved per *placement*
([`modulation/compile.rs`](../../../src-tauri/src/modulation/compile.rs)),
"keyed by content, not placement (ADR 0004), so duplicating a clip
duplicates its automation for free".

**So the plan is not a rewrite. It is: build the reserved arms, and retire
the overlay onto them.**

## 3. The one idea this plan rests on

> **A pad press is an ephemeral placement.**

An arrangement plays *placements* of content at absolute positions. A pad
plays the same content at a position that only exists from the moment you
press it. Everything else follows from taking that sentence literally:

- the pad's clip needs **its own playhead**, starting at 0 on press → R1, R2
- a playhead needs a **node** to render into: gain, pan, inserts, sends,
  output → R2, R3, R4 (and `bus::compile_routing` from Plan G2 already
  compiles a DAG of exactly such nodes)
- automation for a pad is `ClipEnvelope` evaluated at **that node's** local
  position, not the transport's → R6, and it is what makes an envelope a
  trigger-relative envelope with no new document type
- a knob that belongs to the deck is a **macro**: a source that binds to any
  port, including a player's own → R5
- recording is **capturing ephemeral placements as real ones** → R6

We call the node a **player**.

## 4. The player

```
Player {
  id
  name
  source:  { kind: "audioClip", clipId, mode }        // R1, R2
         | { kind: "midiClip",  contentId, instrument } // R3
         | { kind: "none" }                            // control-only pad, R5
  raw:     bool          // R2: no inserts, no envelopes, unity gain, no macros
  trigger: { mode: "oneShot" | "gate" | "loop" | "toggle",
             quantize: "off" | "1/16" | "1/8" | "1/4" | "1/1" | "bar",
             chokeGroup: u8?,
             velocityToGain: bool }
  node:    { gainDb, pan, inserts[], sends[], output }  // identical to a track's
  record:  { target: "auto" | trackId, lane: "auto" | "new" | laneId }  // R6
}
```

Three things to notice:

- **`node` is not a new concept.** It is the same shape a track and a bus
  already compile into. That is the whole reuse argument: metering, PDC,
  sends, output routing, the bounce's graph walk and the mixer's slot flags
  work unchanged the day a player produces a `MixNode`.
- **`raw: true` is R2 exactly.** Not "route around the track" — the player
  never touches the source track. Raw means: this node has no chain, no
  modulation, unity gain, straight to its output. A WAV played clean.
- **`source.instrument` on a MIDI player** is what R3 asks for: a plugin
  instance owned by a player. `PluginInstanceInfo.trackId` is already
  optional, so an instance without a track is legal today — it simply has
  nowhere to render. A player is that somewhere.

## 5. Rulings

Numbered so later work can cite them, in the project's house style.

| # | Ruling |
|---|---|
| **V-1** | A player is a **document object**, not surface chrome. It lives in the session (`session.players`), mutates through `Op`s, and is undoable. The surface's *layout* stays chrome (ruling S-1 stands); what a pad *is* does not. |
| **V-2** | A player is **not a track**. Tracks are timeline objects (absolute placements, lanes, arm, song end, bounce). Making players hidden tracks would force every timeline feature to learn to ignore them — starting with the offline bounce, which has no overlay concept and would render a pad's clip at bar 1. |
| **V-3** | Tracks, buses and players all compile to one **`MixNode`**. New code targets `MixNode`; `Track`/`Bus`/`Player` are three producers of it. The RT graph already thinks in slots + flags and does not change. |
| **V-4** | Every player has its **own playhead**. There is no global "overlay" after V2; the launch overlay's single atomic set is deleted, not extended. |
| **V-5** | Automation for a player is evaluated at **that player's local position**. `ParamDriver::tick(position)` becomes per-node; the transport is simply the node whose position is the transport's. This is the fix for the existing silent bug in §2.4. |
| **V-6** | `raw: true` is **absolute**: no inserts, no sends beyond the direct output, no envelopes, no macros, unity gain, no velocity scaling. A raw player is a bit-exact copy of the source's samples at unity, or it is not raw. |
| **V-7** | A **macro** is a `[0,1]` document value that is both source and target (modulation §8.3), with a cycle check at bind time. Surface knobs bind to macros; macros bind to ports. A knob never writes a param directly. |
| **V-8** | **Recording captures elements, never a mixdown.** A MIDI player records a MIDI clip; an audio player records a *placement of the same source* (no new file, no re-encode, content identity preserved so its envelopes survive); a knob records into a curve. "Print/freeze to audio" is a separate, later, explicit action. |
| **V-9** | The **recording target is the instrument, not the pad**. Pads sharing one instrument record to one track, distinguished by note number (that is what a drum kit is). Pads with different instruments record to different tracks. MIDI channel is never used to multiplex two instruments onto one track — the channel field belongs to external output routing (`docs/midi-output.md`) and must not acquire a second, contradictory meaning. |
| **V-10** | **Automation lane resolution**: exactly one connected automation track → use it; none → create one, named after the target, on first write; several → the pad inspector asks, and the answer sticks per binding. Knob moves during a take go through the **existing** write/touch/latch recorder (PR #85), not a second one. |
| **V-11** | **Arm is per deck, with a per-pad override.** A performance is one take across many pads; arming eight pads individually to record a groove is the hack we are avoiding. |
| **V-12** | A player's parameters are **visible in one place**: the pad inspector. Source, raw flag, chain, sends/output, macros, trigger mode, record target — one object, one panel, nothing coupled invisibly to a track. |

## 6. Pedagogy — how a user keeps track of all this

The owner's own worry ("hvordan skal man pedagogisk og logisk holde orden").
Three rules that answer it:

1. **The pad IS the strip.** Click a pad in edit mode and you see everything
   that pad is (V-12). There is no second place where a pad's behaviour is
   configured, and no track whose settings secretly apply.
2. **The deck shows its own signal flow.** A player's output is a normal
   routing choice (`MixNode.output`), so a pad can go straight to master, or
   into a drum bus with the others. Same picture the mixer already draws.
3. **Recording says where it will land before you press it.** Each pad
   displays its resolved record target (V-9's rule, evaluated live). No
   dialog after the fact asking which track that take belonged to.

## 7. Cuts

Detail, gates and dependencies live in the backlog file. The order is not
negotiable — each cut is a foundation, not a feature.

| Cut | What | Why it must come here |
|---|---|---|
| **V1** | `MixNode` as the compiler's input. Tracks and buses become producers. No behaviour change. | Everything else needs a node that is not a track. Doing it alone, with the bounce and the whole suite as the gate, keeps it provably behaviour-neutral. |
| **V2** | One player, real: document + ops, a graph slot, audio-clip and MIDI-clip sources, own playhead, one-shot/gate/loop, `player_fire`/`player_stop`. Retires the overlay; existing launch bindings migrate. | R1, R2, R3 all become true here, and the transport-hijack and MIDI-only-clip defects die with the overlay. |
| **V3** | Polyphony: N players sounding at once, voice cap, choke groups, quantized start. | R4. A deck is polyphonic or it is a toy. |
| **V4** | Per-node automation clock (`ClipEnvelope` at the player's position); modulation §8.1 ports. | Fixes §2.4's silent bug and makes trigger-relative envelopes fall out. |
| **V5** | Macros: document rows, cycle check, surface knobs bound through them. | R5. |
| **V6** | Recording: elements + automation, V-9's target rule, V-10's lane rule, V-11's deck arm. | R6 — the cut that turns the deck into an instrument you can keep what you played from. |
| **V7** | The performance UI: pad inspector, per-pad record display, `Op::ControlSurfaceSet` so the layout is document-owned and undoable (the old control-surface v0.3). | Needs V2–V6 to have something to inspect. |
| **V8** | Hardware map (old v0.4) and more templates (old v0.5), now against players rather than the overlay. | Cheapest last, and it inherits everything above. |

## 8. Open questions for the owner

Answers change the size of V3 and V6. Recorded here unanswered rather than
guessed at.

**1, 2 and 4 were answered on 2026-08-30, each as recommended** — that is
what unblocked V3 (PR #137). 3 and 5 are still open and block nothing.

1. **Voice cap** — 8, 16, 32? Each sounding player costs a slot and a
   playhead. A fixed cap is RT-friendly and honest; "unbounded" is neither.
   *Recommendation: 32, with visible voice stealing (oldest first).*
   **Answered 2026-08-30: yes — `ControlPlane::VOICE_CAP`, ruling V-19.**
2. **Choke groups** — one group per pad (classic hi-hat), or arbitrary sets?
   *Recommendation: one `chokeGroup: u8?` per player; sets can wait.*
   **Answered 2026-08-30: yes — `Player::choke_group`, ruling V-20.**
3. **Are players visible in the mixer** alongside tracks and buses, or only
   on the surface? *Recommendation: visible, collapsed under one "DECK"
   group — a player with sends and an output belongs where routing is read.*
4. **Retrospective capture** ("I just played something good, keep it") — own
   cut after V6, or part of it? *Recommendation: own cut; it needs a rolling
   buffer decision of its own.*
   **Answered 2026-08-30: yes — its own cut, after V6.**
5. **Does a raw audio player ignore the deck's master gain too**, or only
   the source track's chain? *Recommendation: only the source track's — the
   deck's own output stage is still a mixer node like any other.*

## 9. Non-goals, deliberately

So the next round knows these were choices, not oversights: time-stretching
or pitch-shifting a fired WAV; slicing a WAV across pads; per-note (as
opposed to per-player) modulation, which modulation §8.8 already reserves;
scene launching across many players at once; MIDI-out from a player to
external gear; and printing a pad performance to audio in the same pass that
records it.

## 10. Known limits, found while building V2

- **Two pads on one plugin instance instantiate two plugin instances.**
  Correct as built, not a defect to schedule: the mixer processes one live
  node per ROW, once per block, so two rows cannot share a node without
  double-advancing it — sharing would mean the two pads become one row,
  which is a deck concept and a design change, not a key change. It is
  also not a regression: two tracks sharing an `instrument_id` already
  instantiate twice today. Consequence: a drum deck of eight pads on one
  Zyn loads Zyn eight times and splits its state, so a param edit lands on
  one copy. A shared-instance deck row is V3+ territory if it is ever
  wanted.
- **A bare MIDI pad's release is hard-cut at the tail allowance, not
  decayed.** This is V-17(b)'s accepted consequence: `tail_frames` bounds
  how long a row keeps running after its clock stops, and what a live
  node holds beyond its declared latency — a synth's release — is not in
  that number, so an unbounded tail (a pad whose instrument release
  outlasts the allowance) is cut cleanly mid-decay. This is not new
  harshness: an ordinary MIDI track already sounds the same way when the
  transport stops — its release decays into a fader gated shut rather
  than a frozen voice, which is audibly similar since either way the
  decay does not finish. A per-instrument release allowance (or a
  minimum guaranteed pad tail) is V3 material, not V2's to fix.
