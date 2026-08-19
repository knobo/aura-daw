# Modulation system — design (Track F)

**Status:** accepted by the owner 2026-08-15 through brainstorming. Rulings
R1–R5 below are the owner's, recorded verbatim in intent; everything else is
the implementer's, taken under R5.

Supersedes nothing. Extends Track D (`docs/PHASE4-PLAN.md` "Track D
handoff"), whose eight scope rulings remain in force except where this
document says otherwise — and it says otherwise in exactly one place
(ruling 8, the in-lane overlay; see §6.1).

---

## 1. Context

Track D made automation audible. What exists today:

- `AutomationLane { id, target_node: String, param_id: u32, points: [{tick, value}] }`
  in `Session.automation`, persisted as `automation[]` rows in `project.json`
  with points in AMEV chunks under `<project>/automation/`.
- `target_node` is either `"track:<trackId>"` (with `param_id == 0` meaning
  gain, the only built-in id) or a plugin instance uuid.
- Track-gain lanes compile at rebuild into `RtGraph::gain_ramps`, a
  slot-indexed `Vec<Option<Arc<Vec<AbsParamEvent>>>>` applied per sample at
  the mixer's gain stage. Plugin-param lanes are driven on the engine control
  thread's ≤2 ms tick, host-only.
- The UI draws exactly one lane per track — gain — as an overlay inside the
  track's timeline row. Plugin params can be *bound* (the `A` button in the
  param panel mints a flat one-point lane) but never drawn.

Three gaps, all named in the repo before this round:

1. **Target identity.** ADR 0004 closes with: automation-lane identity
   (`target_node` strings that cannot address faders) "remains a live gap,
   explicitly assigned to the node-graph round". A string pair cannot name
   pan, a send, or a macro.
2. **Containers.** SCALABILITY §3 defines a pattern as "a named, reusable
   container of MIDI/**automation** events with its own musical length".
   Automation never got the container half, so it cannot loop with a clip and
   cannot be instanced.
3. **Routing.** One lane drives one target by construction. There is no way
   to say "this curve drives the filter on three tracks".

The owner's two questions — dedicated automation tracks routed to several
targets, and clip-local automation that loops with the clip — are these gaps
seen from the user's side. They are one mechanism, so they get one design.

## 2. Owner rulings

- **R1 — the documented model is a full modulation graph.** Sources →
  bindings → targets, where a drawn curve is one source kind among LFO,
  macro, MIDI CC and envelope follower. Implementation is staged; the
  *format* is designed once.
- **R2 — targets are a tagged union now, ports later.** The tagged
  `TargetRef` is accepted on the condition that it is a step toward the port
  model, not a detour, and that the path to the full model is written down in
  local files (this document, `next-prompt.md`, and an ADR).
- **R3 — curve values are normalized 0..1**; range, depth and polarity live
  on the binding.
- **R4 — the first PR ships the foundation plus all three slices:** several
  curves per track (incl. pan), automation tracks routed to several targets,
  and clip envelopes that loop with the clip.
- **R5 — remaining design decisions are the implementer's**, validated by the
  owner driving the app.

## 3. The model

Five document objects. Three ship this round; two are format-only.

| Object | Ships | Purpose |
|---|---|---|
| `Curve` | yes | normalized point data — the reusable *shape* |
| `Binding` | yes | source → target, with mapping. Where all the semantics live |
| `AutomationClip` | yes | a placement of a curve on an automation track |
| `Modulator` | format only | LFO / MIDI CC / envelope follower definitions |
| `Macro` | format only | a named 0..1 value usable as a source and a target |

### 3.1 Curve

```jsonc
{ "id": "cur_<uuid>", "name": "Filter sweep",
  "lengthTicks": 3840,           // natural/loop length; null = timeline curve
  "pointsRef": "automation/<uuid>.bin" }
```

Points are `{tick: u32, value: f32}` with `value` in `[0,1]` and ticks
relative to the curve's own origin. Point records keep the existing AMEV
encoding unchanged (`plugins::automation::encode_points`), which already
carries a version and a column mask — per-point curve *shape* arrives later
as an added column block, so curve shapes cost no chunk-format break.

A curve is content in ADR 0004's sense: several bindings and several
placements may reference one curve, and editing it changes all of them.

### 3.2 Source

The time base is implied by the source kind. There is deliberately **no
separate `timeBase` field**: every combination it could express that is not
already a kind is meaningless.

```jsonc
{"kind": "curve",          "curveId": "cur_..."}      // ships — absolute timeline
{"kind": "automationTrack","trackId": "t_..."}        // ships — that track's clips
{"kind": "clipEnvelope",   "contentId": "con_...", "curveId": "cur_..."}  // ships
{"kind": "lfo",            "modulatorId": "mod_..."}  // reserved
{"kind": "macro",          "macroId": "mac_..."}      // reserved
{"kind": "midiCc",         "modulatorId": "mod_..."}  // reserved
{"kind": "envFollower",    "modulatorId": "mod_..."}  // reserved
```

`clipEnvelope` keys on **`contentId`, not `clipId`** (ADR 0004): the envelope
belongs to the content, so every placement of that content loops the same
envelope, and duplicating a clip duplicates its automation for free.

### 3.3 TargetRef

```jsonc
{"kind": "trackParam",  "trackId": "t_...", "param": "gain"|"pan"|"mute"|"send0"}
{"kind": "pluginParam", "instanceId": "...", "paramId": 7}
{"kind": "selfTrackParam",      "param": "gain"|"pan"}   // clip envelopes only
{"kind": "selfInstrumentParam", "paramId": 7}            // clip envelopes only
{"kind": "macro", "macroId": "mac_..."}                  // reserved
{"kind": "port",  "nodeId": "...", "portId": "..."}      // reserved — node-graph round
```

The two `self*` arms resolve against the track the *placement* plays on, so
shared content stays sane: a clip envelope that dips the volume dips
whichever track it is dropped on. `selfInstrumentParam` resolves through
`TrackState::instrument_id == "plugin:<id>"` to a plugin instance.

`mute` and `send0..N` ship as **format and resolver arms only**: the resolver
returns `None` for them, following `resolve_target`'s existing policy that an
unknown target is ignored, never guessed at. Sends have no engine
infrastructure yet; mute needs a de-click ramp that belongs with the engine
round.

### 3.4 Binding

```jsonc
{ "id": "bnd_<uuid>",
  "source": {"kind": "curve", "curveId": "cur_..."},
  "target": {"kind": "trackParam", "trackId": "t1", "param": "pan"},
  "mode":   "absolute" | "add" | "multiply",
  "depth":  1.0,                         // -1..1, scales the source's excursion
  "range":  {"min": 0.0, "max": 1.0},    // sub-range in the target's normalized domain
  "domain": "normalized" | "native",     // see §7, migration only
  "rangeSnapshot": {"min": 20.0, "max": 20000.0},   // pluginParam: range at bind time
  "enabled": true }
```

### 3.5 AutomationClip

```jsonc
{ "id": "acl_<uuid>", "trackId": "t_auto", "curveId": "cur_...",
  "timelineStartTicks": 0, "lengthTicks": 7680, "contentLengthTicks": 1920 }
```

Deliberately the same field names and the same loop rule as `MidiClip`
(`contentLengthTicks` absent ⇒ same as `lengthTicks`, no repeat), so the
timeline's existing clip geometry, drag and loop code reads the same way for
both.

### 3.6 Automation track

A `TrackState` with `kind: "automation"`. It holds automation clips, and
**the bindings hang off the track, not off its clips** — so routing one
automation track to three targets is three bindings with
`source = {kind:"automationTrack", trackId}` and independent depths. That is
the whole of the owner's first question; it is not a feature, it is where the
binding indirection already pointed.

An automation track renders no audio and takes no mixer slot.

## 4. Value semantics

### 4.1 Domains

A curve is a shape in `[0,1]`. A binding maps it into the **target's
normalized domain**, also `[0,1]`, defined per target kind:

| Target | Normalized 0..1 means | Document base |
|---|---|---|
| `trackParam.gain` | linear multiplier 0..1 on top of the fader | **1.0**, the unity multiplier |
| `trackParam.pan` | -1..1 remapped: 0 → hard L, 0.5 → centre, 1 → hard R | `TrackState::pan`, as `(pan + 1) / 2` |
| `pluginParam` | `min..max` of the param's declared range | the stored knob value, normalized |

Gain's base is the unity multiplier rather than the fader because the mixer
applies the fader *separately*, at the same stage (`gain * ramp`). A
consequence worth knowing: on gain, `absolute` and `multiply` at full depth
compute the same thing. That is not a redundancy to clean up — it is what
keeps the fader independently movable while a curve is playing, and it is
why Track D's ruling 6 survives the model change untouched.

`range` narrows the excursion (`min..max` inside the normalized domain);
`depth` scales it, negative depth inverts.

### 4.2 Combination

Per target, per sample (conceptually — in practice per compiled event). One
binding first maps its source `s ∈ [0,1]` into the target's domain:

```
m = range.min + (range.max - range.min) * s
```

then contributes according to its mode, where `base` is the target's
document value normalized into the same domain:

```
absolute:  v  = base + depth * (m - base)     // depth 1 = full takeover, 0 = inert
multiply:  f *= (1 - depth) + depth * m       // depth 1 = plain multiply, 0 = inert
add:       a += depth * m                     // negative depth subtracts
```

and the target's final value is

```
out = clamp(v * f + a, 0, 1)   mapped back to native units
```

where `v` is the single `absolute` binding's result (or `base` when there is
none), `f` starts at 1 and `a` at 0. Every mode is inert at `depth = 0`,
which is what makes a depth slider safe to drag through zero.

Two consequences worth stating plainly, because they are why this model is
believable rather than merely general:

- **Today's track-gain automation is `mode:"multiply"` with the fader as
  base** — that is Track D ruling 6 exactly ("linear gain multipliers applied
  on top of the fader").
- **Today's plugin-param automation is `mode:"absolute"`** — Track D ruling 2
  exactly ("automation OVERRIDES the stored knob value during playback").

So the migration is a translation, not a behaviour change, and the two
existing behaviours stop being special cases.

### 4.3 Arbitration

At most one `absolute` binding may own a target. If a document contains more
(hand-edited, or a merge), the **first in document order wins** and the
others are reported through the existing warning path and shown struck
through in the UI; nothing is silently blended. The UI refuses to create the
second one, offering to convert it to `add` instead.

This replaces `compile_gain_ramps`'s current documented rule ("if two lanes
name the same track's gain, the LAST one wins, silently") — last-wins was
chosen when nothing could produce a collision; now something can.

## 5. Time, looping, and where the work happens

Everything about looping is resolved on the **control thread, at rebuild**,
by expanding sources into the same absolute-sample event lists the RT thread
already reads. The RT path does not learn what a loop is.

| Source | Expansion |
|---|---|
| `curve` | ticks → absolute samples through the tempo section table (today's `compile_lane`, unchanged). Value held before the first and after the last point. |
| `automationTrack` | for each of the track's clips: the curve repeated every `contentLengthTicks`, cropped to the placement; **outside every clip the binding contributes nothing** (the target falls back to its document value) |
| `clipEnvelope` | for each placement of the owning content: the same repeat rule, over that placement |

So the answer to "will the automation loop when the clip loops" is: yes, by
the same `contentLengthTicks` rule MIDI notes already follow, and it costs
the audio callback nothing.

**Expansion is bounded.** A one-bar envelope under a 20-minute placement is
~1150 repeats; a 32-point curve makes ~37k events, which is fine, but the
product is unbounded in principle. Each binding's expansion is capped
(`MAX_EVENTS_PER_BINDING = 200_000`); on hitting the cap the expansion stops
at the last whole repeat and the binding is flagged for the UI. Per-block
lazy evaluation is the eventual answer and is recorded in §8.

## 6. Engine

### 6.1 Track params (RT)

`RtGraph::gain_ramps` generalizes to a per-slot table of *built-in param*
ramps:

```rust
pub struct TrackRamps {
    pub gain: Option<Arc<Vec<AbsParamEvent>>>,
    pub pan:  Option<Arc<Vec<AbsParamEvent>>>,
}
// RtGraph { ..., track_ramps: Vec<TrackRamps> }
```

Same lifetime discipline as today: built on the control thread, attached
before the graph is published, swapped by the ordinary RCU publish, versioned
with the snapshot. `set_gain_ramps` stays as a thin shim so Track D's mixer
tests keep their shape.

**Gain** keeps per-sample evaluation through `RampCursor` (unchanged).

**Pan** is evaluated at **block boundaries with a per-block linear
interpolation of the `(gl, gr)` pair**, not per sample. `pan_gains` is two
trig calls; at 48 kHz × 32 tracks that is ~3M sin/cos per second on the
callback for a feature nobody can hear the difference on. Two evaluations per
block per track, lerped, is inaudible for pan sweeps at any musical rate and
costs nothing. This is a deliberate divergence from gain's per-sample
treatment and is recorded here so a later reader does not "fix" it.

### 6.2 Plugin params (control thread)

`ParamAutomationDriver` is rebuilt from *bindings* instead of lanes. Its tick
rate, epsilon suppression, re-assert interval, per-instance batching and
host-only discipline are unchanged (Track D ruling 2 holds).

### 6.3 Offline export

`audio::offline::build_graph` compiles the same table, so a bounce follows
gain and pan exactly. The known plugin-param divergence (a bounce does not
drive plugin params at all) is **unchanged and still recorded** — fixing it
needs private plugin instances per bounce, which is the plugin-node round.

## 7. Persistence and migration

`project.json` goes to **schemaVersion 4**:

```jsonc
"modulation": {
  "curves":     [ ... ],
  "bindings":   [ ... ],
  "automationClips": [ ... ],
  "modulators": [],   // reserved, always empty this round
  "macros":     []    // reserved, always empty this round
}
```

- **Reading v1–v3 keeps working forever.** A v3 `automation[]` row migrates
  to one curve plus one binding:
  - `track:<id>` + param 0 → curve (points already 0..1, copied verbatim) +
    binding `{trackParam gain, mode:"multiply", depth:1}`.
  - plugin instance + paramId → binding `{pluginParam, mode:"absolute"}`. The
    points are in the param's **native** units. When the param's range is
    known at load (the usual case — plugins are instantiated during load),
    the points are normalized and `rangeSnapshot` recorded. When it is not
    (plugin missing), the binding is written with `domain:"native"` and the
    points are left alone; the evaluator honours `domain`, so the project
    still sounds right, and the UI offers a one-click normalize once the
    plugin is back.
- Writing always emits v4 and drops `automation[]`. The upgrade is one-way,
  which matches how v2 and v3 were handled.
- Point chunks are unchanged, so a migration rewrites JSON rows only.
- New ops (`Op::ModulationSetCurve`, `Op::ModulationSetBinding`,
  `Op::AutomationClipSet`) are **additive enum arms**, which
  `OP_FORMAT_VERSION`'s own doc comment says do not require a bump.
  `Op::AutomationSetLane` stays, deprecated, applying through the migration
  path so old journals replay.

## 8. The path to the finished system

Required by R2. This section is the contract for the rounds after this one;
`next-prompt.md` links here rather than restating it.

1. **Ports (node-graph round).** `TargetRef` gains
   `{kind:"port", nodeId, portId}`. The resolver keeps the existing arms and
   maps them to ports internally, so no saved project needs rewriting; new
   bindings mint port refs. This is the arm ADR 0004 reserved, and adding it
   touches the resolver only — that is what "a step, not a detour" buys.
2. **Modulators.** `modulators[]` rows plus the reserved source kinds. Each
   needs a control-thread evaluator: an LFO (free-running or tempo-synced),
   a MIDI-CC follower, an envelope follower. The binding, depth, range and
   arbitration semantics in §4 apply unchanged — that is the whole point of
   designing them now.
3. **Macros.** A macro is a `[0,1]` value that is both a source and a target,
   which makes macro-controls-macro a graph and needs a cycle check at bind
   time.
4. **Curve shapes.** The per-point shape column in the AMEV chunk, plus
   `hold`/`bezier` in `segment_value` and handles in the editor.
5. **Automation recording (write/touch/latch).** A control-thread recorder
   writing gestures into curves. The first slice records gain gestures on
   regular audio/MIDI tracks. The finished slice also records directly into
   separate automation tracks, routes each take through that track's selected
   bindings, and gives the automation-track source an explicit Enabled/Bypass
   control. Until then, the separate automation track is a drawn-curve source,
   not a recording destination. This is where "the RT path doesn't distinguish
   live vs. played-back automation" (SCALABILITY §"Parameter automation
   model") finally becomes true.
6. **Sample-accurate plugin params.** Round-2 §8's wait-free param ring;
   removes ruling 2's ≤2 ms granularity.
7. **Lazy expansion.** Replace §5's bounded expansion with per-block
   evaluation from the source objects, removing `MAX_EVENTS_PER_BINDING`.
8. **Per-voice modulation.** Uses the sounding-instance address
   `(ClipId, ContentId, NoteId)` that round-2 §5 already recorded.

**External MIDI control is a separate follow-up, not part of the first
write/touch/latch PR.** Every parameter exposed as an automation target must
also be eligible for MIDI-controller mapping. The UI needs both a **MIDI Learn**
workflow (move a hardware control to bind it) and a manual **Set Value / mapping
configuration** workflow for selecting and editing the MIDI input and its
target value/range without Learn. The resulting live control can then feed the
same write/touch/latch recorder; this must not introduce a second automation
target model.

## 9. Non-goals this round

Held deliberately, so the next round knows they were choices: LFO/macro/CC
implementations (§8.2–3), curve shapes beyond linear (§8.4), automation
recording (§8.5), sample-accurate plugin params (§8.6), `mute` and send
targets (§3.3), plugin-param automation in a bounce (§6.3), per-voice
modulation (§8.8), and any change to Track D's gesture/undo shape (rulings
4 and 7 stand).

## 10. UI

### 10.1 Several curves per track

The track header's `A` becomes a menu: **gain**, **pan**, and every parameter
of the track's plugin instances, each with a checkmark when a curve exists.
Picking one mints the curve and its binding and makes that lane the one the
overlay is editing; a chip row under the track name switches between the
track's existing lanes and shows depth.

Track D ruling 8 (overlay, not an added row) **stands for track lanes** — the
left rail and the lane column share `--track-height` and desynchronise if a
sub-row is inserted. An automation *track* is a real track, so it gets a real
row with no height negotiation at all; that is the escape hatch, and it is
why the automation-track slice also answers the "I want a taller automation
lane" complaint.

### 10.2 Automation tracks

A new track kind in the add-track menu. Its header lists targets
(`→ Reverb · Mix`, `→ Bass · Cutoff`) each with a depth slider and a remove
button, plus **+ add target** which opens the same picker as 10.1 scoped to
the whole project. Its timeline row holds automation clips drawn with the
existing clip geometry, looping like MIDI clips.

### 10.3 Clip envelopes

The piano roll gains an envelope lane below the notes, with a param picker
(self track gain/pan, the track's instrument params) and the same
draw/drag/alt-delete gestures as the timeline overlay. Because the envelope
is keyed by content, a looping clip loops it.

### 10.4 Unchanged

The plugin panel's `A` button keeps its meaning (bind/unbind this param), now
minting a curve + binding. All editing stays gesture-bracketed with
preview-on-drag and one commit on pointerup (Track D rulings 4 and 7).

## 11. Verification

- **No-behaviour-change gate for the foundation.** The full existing suite
  (662 backend + 258 frontend at branch point) must pass unchanged after the
  model swap, and a v3 project must load, play and re-save identically except
  for the JSON rows.
- **Migration round-trip tests**: v3 gain lane, v3 plugin lane with known
  range, v3 plugin lane with unknown range (`domain:"native"`), empty
  `automation[]`, and a v4 file re-read.
- **Audible tests** (offline render, the shape Track D's close-out used):
  a pan sweep moves energy between channels; a gain curve's amplitude follows;
  a looping clip envelope produces the *same* curve in bar 1 and bar 3;
  an automation track bound to two tracks moves both.
- **Arbitration tests**: two absolute bindings → first wins, second flagged;
  multiply + add stack in the documented order.
- **Expansion bound test**: a 1-bar envelope under an absurd placement stops
  at the cap and flags rather than allocating without limit.
- **Frontend**: store tests for the picker/binding paths, gesture tests
  mirroring `automation-gesture.test.ts`, and `svelte-check` clean.
- **The owner's ear check** — the app driven by hand, which is how R5 says
  this design gets validated.

## 12. Phases

| Phase | Content | Testable by the owner |
|---|---|---|
| P0 | Model, format, migration, resolver, engine table generalization — no behaviour change | app behaves exactly as before |
| P1 | Several curves per track + pan target + lane picker | draw pan and two plugin params on one track |
| P2 | Automation track kind, clips, routing to several targets | one curve moving three things |
| P3 | Clip envelopes keyed by content, looping | loop a clip, hear the curve repeat |
| P4 | ADR, `next-prompt.md` path, handoff section in `docs/PHASE4-PLAN.md` | — |
