# AURA — What FL Studio, Bitwig and Ableton Actually Bet On

Status: research dossier, 2026-08-13. Not normative; feeds `ARCHITECTURE.md`
and `SCALABILITY.md`.
Audience: whoever designs the AURA core object model, and every agent who
later wonders why a decision was made.

---

## Why this document exists

`SCALABILITY.md` states the strategic goal as growing into an "**FL
Studio-class DAW**". That sentence contains an unexamined assumption: that
we know what makes FL Studio FL-Studio-class. This document is the attempt
to find out, extended to the two other products whose object models are
worth stealing from.

It is a *reading* of three products, not a feature list. The question asked
throughout was not "what can it do" but "what did they bet the data model
on, and what did that bet make cheap or impossible". Feature comparisons
are worthless for architecture; a feature you can add in a week is not an
architectural fact, and a feature nobody can add in five years is.

The payload is §5 — the leverage ranking. Everything before it is the
evidence.

### Provenance and confidence marking

Primary sources first: vendor manuals, user guides, release notes, spec
headers, and developer statements. Two research agents ran the FL Studio
and Bitwig sweeps; the Bitwig sweep returned in full, the FL sweep stalled
and FL was completed from the Image-Line manual directly. **FL's coverage
is therefore manual-primary and thinner on developer commentary than
Bitwig's.**

Every non-obvious claim carries a marking:

* **`[V: url]`** — verified; the cited text was read this session.
* **`[inf]`** — my inference from verified facts. Not sourced.

Where a claim comes from a forum or a reviewer rather than the vendor, that
is stated inline. §6 is the honest list of what could not be verified.

### The one-sentence thesis

All three products are organized around exactly one spine, and everything
else in the product is a *view* or a *decoration* on that spine.

| Product | Spine |
|---|---|
| FL Studio | `pattern → placement` |
| Ableton Live | `clip in a track` |
| Bitwig Studio | `device graph + voice` |

The features people describe as "signature capabilities" are almost always
cheap consequences of the spine, and the features these DAWs are *bad* at
are almost always the ones that would have required a second spine. That
observation is the whole basis of the ranking in §5.

---

## 1. FL Studio — the content/placement split, taken to its logical end

### 1.1 The pattern/playlist model

FL's central and genuinely unusual bet: **a Pattern is a global, named
container of note and controller data for *many channels at once*, and the
Playlist holds *placements* that reference it.**

The manual is explicit that a playlist track is a lane, not an owner, and
that patterns are not track-scoped:

> "By default, Playlist tracks are **not restricted** to a single
> instrument, audio recording, or Clip type (though they can be)."
>
> "**a single Pattern Clip on a single Playlist track could trigger every
> Channel Rack instrument.**"

`[V: image-line.com/fl-studio-learning/fl-studio-online-manual/html/playlist.htm]`

The traditional 1:1 model is available but is the *opt-in special case*:

> "If you want a more traditional **1:1 Instrument Channel → Playlist →
> Mixer relationship**, use 'Track Mode'." `[V: same]`

The three playlist clip kinds are Pattern Clips ("hold MIDI notes and/or
controller movements"), Audio Clips and Automation Clips `[V: same]`.

**Why this matters architecturally.** Nearly every other DAW conflates two
things that FL keeps separate:

| Concept | FL | Live / Logic / Cubase |
|---|---|---|
| The musical content | `Pattern` (global, named, reusable) | — |
| Where it sounds | `Pattern Clip` (position, length, ref) | `Clip` (owns its content) |

Because content and placement are separate objects, "put the same 4 bars in
30 places and edit them all at once" is free, and so is "one pattern is a
*section* of the arrangement across the whole band". `[inf]`

In Live, duplicating a clip duplicates its content; there is no
linked-instance concept in the arrangement. **That single difference is the
largest single workflow gap between the two products, and it is a data-model
decision, not a feature.** `[inf]`

### 1.2 Two routing layers: Channel Rack vs Mixer

FL keeps a **pre-mixer per-channel processing layer** and a **mixer insert
layer**, and they are not the same thing.

Per channel (Channel Settings): instrument/envelope settings (INS tab),
miscellaneous settings, and — via the plugin Wrapper — "Mixer track routing
(TRACK), channel Pitch (tuning) and inbuilt **arpeggiator** functions"
`[V: .../chansettings.htm]`. The Piano roll's per-note MODX/MODY map onto
**channel filter cutoff and resonance** `[V: .../pianoroll.htm]` — i.e. the
per-channel layer owns a synth-like voice section that sits *between* the
generator and the mixer.

The mixer is a separate, ordinary routing graph: 500 insert tracks +
master; channels route to inserts;

> "It's also possible to route the audio of any Mixer Track directly to an
> ASIO Output and or **to another Insert Track**"
>
> "Insert Mixer Tracks can be routed (sent) to as many other Mixer Tracks as
> required"

sends are post-fader by default; sidechain routing sends audio "**without
the audio being heard** at the destination Mixer Track"; 10 effect slots per
insert `[V: .../mixer.htm]`.

`[inf]` This is a **per-voice/per-note layer and a per-bus layer,
deliberately not unified**. The per-channel layer can be note-aware
(velocity → cutoff per note); the mixer layer cannot, because by then the
voices are summed. This is the same boundary Bitwig calls green→blue
(§2.2). FL drew the line at the channel; Bitwig drew it wherever voice
identity stops being carried. FL's version is cheaper and less powerful.

### 1.3 Patcher — a graph device with a real scheduler

Patcher is a plugin that hosts a graph of plugins inside one channel or one
effect slot. What is architecturally interesting `[V: .../plugins/Patcher.htm]`:

* **Three typed link kinds**: Audio (yellow), **Parameters (red)** —
  "internal automation parameter links" — and Events (green), "note/event
  links (MIDI control data)". **Parameter links are first-class edges,
  i.e. modulation is a wire.**
* **A compiled depth-level schedule with parallelism**: "Modules added at
  the same number of nodes... are processed at the same time
  (**multithreaded**), modules at different node-depth columns are processed
  **sequentially**". That is a topological level-set schedule, not a
  per-sample interpreter. `[inf]`
* **VFX (Voice Effects)** nodes — Color Mapper, Envelope, Key Mapper,
  Keyboard Splitter, Level Scaler, Sequencer — a per-note/per-voice
  processing stage inside the graph.
* **Automatic delay compensation inside the graph**: "Latency compensation
  within Patcher is automatic". Users on the Image-Line forums nonetheless
  report needing manual tricks for parallel paths — e.g. abusing Fruity
  Limiter's attack time as a delay — so the compensation is real but
  incomplete in parallel topologies `[forum reports,
  image-line.com/forum threads 173554, 164435]`.
* **Unbounded nesting**: "add unlimited effects or instruments in a single
  Channel or Effects slot", plus "Patcherize existing plugins".

`[inf]` The key point for a new DAW: **Patcher is a hosted mini-engine with
its own scheduler, bolted on beside FL's main engine.** That is the cheap
way to get modular patching — and it is exactly why Patcher's PDC,
threading and voice semantics don't quite match the host's. Bitwig took the
opposite route (§2.3) and paid for it up front.

### 1.4 Automation clips: the sharpest idea in the product

The manual's definition is the whole trick:

> "Automation Clips move (automate) linked controls on the FL Studio
> interface or plugins" and are "**a type of internal controller** loaded
> into a Channel as a special Generator type." They are displayed "in the
> Playlist window as a line-graph."

`[V: .../playlist_automationclip.htm]`

So: **an automation clip is a Channel** — the same object as an instrument —
that happens to output control values instead of audio, and its playlist
presence is a placement like any other. And crucially:

> Automation Clips are "**not bound to a specific pattern**". `[V: same]`

Creation paths, all leading to the same object `[V: same]`:

1. Right-click a native control → "Create automation clip".
2. VST/AU: tweak the control, then **Tools → Last tweaked → Create
   automation clip**, or find the parameter in the Browser's *Current
   project* folder and right-click it.
3. Add menu → "Automation from last tweaked parameter".
4. **Edit → Turn into automation clip** (converts event data).
5. Audio clip menu → Automate → Panning / Volume.

FL therefore carries **three coexisting automation representations**:
pattern-bound *event data* (the Event Editor, per-channel controller
events), *automation clips* (playlist objects, pattern-independent, with
control points, tension, curve types, and an **LFO Mode** that "converts the
Automation Clip to operate as an LFO"), and *initialized controller values*
stored with the project. Conversion between event data and clips is a menu
command.

`[inf]` The enabling mechanism is the **"last tweaked" global parameter
identity**: every control in the app and in every hosted plugin resolves to
an addressable `(target, param)` pair that the browser can enumerate and the
automation system can bind to. That is a single, uniform parameter
namespace — and it is the reason "everything can be automated" is a true
statement in FL and an aspiration everywhere else.

Compare Cubase, which retrofitted a modulation system in v14 and could only
reach **VST3** parameters, on **audio/group/FX channels only**, with **no
modulation of the modulation depth**
`[V: attackmagazine.com/technique/tutorials/cubase-pro-14-modulators-redefining-creative-automation/;
synthanatomy.com/2024/11/steinberg-cubase-14-daw-update-brings-pattern-sequencer.html]`.
**Every one of those three limitations is a parameter-addressing
limitation, not a DSP limitation.** `[inf]`

### 1.5 The Piano roll's actual differentiator

It is not the drawing tools. It is that **the note record is wide**.

Per note `[V: .../pianoroll.htm]`: on-velocity (VEL), release velocity
(REL), **panning (PAN)**, **channel filter cutoff (MODX)** and **channel
filter resonance (MODY)** — which "can also link to custom parameters as set
in native plugins" — plus a **slide** flag, a **portamento** flag, and a
**colour that is locked to a MIDI channel** (16 colours), so "one piano roll
can control several MIDI channels in the associated plugin". Ghost notes
render other channels' notes in the same pattern (solid) and other patterns
(open).

`[inf]` Two structural demands fall out:

(a) the note is a record with ~8 dimensions, not `(pitch, start, length,
velocity)`;

(b) per-note MODX/MODY only means anything because the **per-channel** layer
(§1.2) is downstream of the note and still voice-aware. **A wide note record
without a per-voice consumer is decoration.**

### 1.6 Performance Mode — the cheapest clip launcher ever shipped

FL did not build a second view. It carved a region out of the existing one:

> Performance Mode designates "the area **before the Start Marker** as a
> 'Performance Zone' from where Clips can be triggered, out of sequence,
> between tracks." "Only one Clip per Playlist track can play at a time."

`[V: .../playlist_performance.htm]`

Per-track press modes (Retrigger, Hold & stop, Hold & motion, Latch),
motion settings (Stay … Random), **trigger sync** (Off, beat intervals,
Auto, Queue, Tolerant) and **position sync**. And the round trip back to
linear:

> "Enabling Record mode will **capture performances and lay out triggered
> Clips after the Start marker** for later rendering." `[V: same]`

`[inf]` **This is worth stealing.** The launcher is not a second data model,
a second sequencer, or a second view — it is *a region of the timeline plus
a per-track trigger policy*, and "record the performance" is literally
"write placements to the right of the marker". Compare §5's Tier 1
discussion of what everyone else paid for this.

### 1.7 PDC — the cautionary tale

FL's compensation is real but hedged with manual escape hatches
`[V: .../plugins/wrapper.htm]`. The wrapper shows "the **total latency**
(manual + plugin latency)" and lets the user offset it by hand for plugins
that misreport. Multi-in/out plugins need help:

> "**Always name the Mixer tracks being used by the plugin.** This ensures
> FL Studio knows they are being used by a plugin which can be important for
> PDC calculations"

There is a "fixed size buffers" mode that "can introduce an extra delay",
and Smart Disable "can interfere with some plugins, including those that are
time-based, envelope based or for long-decaying effects" `[V: same]`.

`[inf]` **PDC that requires the user to *name a mixer track* so the
compensator can discover a routing is a graph-topology gap papered over at
the UI layer.** This is what retrofitted latency compensation looks like,
permanently. See `SCALABILITY.md` §1's ordering constraint — PDC before
sends ship — which this vindicates.

### 1.8 The `.flp` format, and what "lifetime free updates" actually demands

Image-Line's "Lifetime Free Updates" promise "is over 25 years old" and "You
can jump from any older version directly to the latest"
`[V: image-line.com/fl-studio/lifetime-free-updates; support.image-line.com KB 238/300]`.
Backwards, not forwards: old projects open in new FL, never the reverse
`[V: forum.image-line.com/viewtopic.php?t=265037]`.

The format makes this tractable, and the mechanism is worth copying exactly.
An FLP is a header chunk plus a data chunk of TLV events:

```
FLhd  size=6  int16 format  int16 num_channels  uint16 ppq
FLdt  size    <event stream>
```

and **the event ID itself encodes the payload size**
`[V: pyflp.readthedocs.io/en/latest/architecture/flp-format.html]`:

| Event ID range | Payload | Record size |
|---|---|---|
| 0–63 | 1 byte | 2 |
| 64–127 | 2 bytes | 3 |
| 128–191 | 4 bytes | 5 |
| 192–255 | varint-length-prefixed blob | ≥2 |

`[inf]` This is the whole trick, and it is a five-line decision: **any
reader, of any vintage, can skip any event it has never heard of, because
length is derivable from the ID alone.** New features add new event IDs; old
readers step over them; new readers step over retired ones. No schema
negotiation, no version matrix, no migration chain.

PyFLP's maintainers call the format "a really bad and messy combination of
Type-length-value encoded events and structs" `[V: same]` — and they are
right about the aesthetics and wrong about the engineering. **Ugly and
skippable beat elegant and brittle for 25 years.**

Contrast Ableton, which solves the same problem by refusing it:

> "Live does **not allow overwriting** Live Sets that were created by older
> major versions to prevent compatibility problems. Instead, you will be
> requested to 'Save As...'"

`[V: ableton.com/en/manual/managing-files-and-sets/]`

Note that FL's property is *strictly stronger* than "ignore unknown JSON
fields" (which AURA already has, per debt D-06), because it must also hold
for **binary event streams** — which is exactly what AMEV chunks are.

---

## 2. Bitwig Studio — the voice, and the device, as the two universal objects

### 2.1 The Unified Modulation System

Bitwig's own framing is a rejection of fixed modulator counts:

> "While most systems require us to work around a fixed number of modulation
> sources … these choices tend to be arbitrary and formulaic from the user
> perspective. **Some sounds require no LFOs, and some require ten.** In
> Bitwig Studio, these options are left completely open for the user."

`[V: bitwig.com/userguide/latest/the_unified_modulation_system/]`

> "**Modulator devices** are special-purpose modules that are made to be
> loaded into **any** device." `[V: same]`

and, at higher scopes, "modulators … can be loaded within any device — **or
onto any track** for control of all its contained devices and mixer
controls"
`[V: .../devices_modulators_and_other_signal_achievements]`.
Track- and project-level modulators arrived in 5.0: "Modulators have been
available on the device level forever. Now tracks and the project level can
have modulators as well"
`[V: downloads.bitwig.com/stable/5.0.11/Release-Notes-5.0.11.html]`.

`[inf]` **A modulator is not a node in the signal chain.** It is a sibling
object hanging off a container (device / track / project), and it addresses
targets *by parameter identity within that container's subtree* — which is
why a device modulator can reach nested devices, and a track modulator can
reach mixer controls that are not devices at all.

Routing is a mapping mode with a per-connection amount, set relative to the
current value:

> "Clicking a modulation routing button switches to a mode where you can
> select as many destinations as you like, **each with its own modulation
> amount**."
>
> "Because the modulation range is set relatively … you can twist the
> modulation range **past the parameter's normal range**, and this is
> correct." `[V: same]`

**A modulation edge is a first-class object with five fields** `[V: same;
downloads.bitwig.com/stable/3.0.3/Release-Notes-3.0.3.html]`:

1. a signed **amount**;
2. one of seven **transfer functions** (Linear bipolar, Positives,
   Negatives, Absolute, Toward Zero, Exponential, Logarithmic);
3. an optional **scaling source** — "one modulator source [can] scale the
   output of **any individual modulation connection** … these scalings can
   be done **per-modulation**";
4. a defined **evaluation order** — "the transfer function is applied first,
   followed by the modulation scaling";
5. a **delayed-due-to-feedback** state.

Modulator parameters are themselves targets: "**all modulator parameters** …
can themselves be targets of modulations" `[V: same]`.

### 2.2 Per-voice modulation: the one thing nobody else has

Colour is the type system:

> "the **blue** color here indicates a **monophonic modulation** … generates
> only one control signal that is then applied to all targets identically
> (musically speaking, *unison*)."
>
> "But a **green** color indicates a **polyphonic modulation**. Polyphonic
> sources produce multiple control signals, **potentially providing a unique
> signal for each note event** (musically speaking, *divisi*)." `[V: same]`

Polyphony is a per-modulator toggle, not a fixed property: "right-click
anywhere within an occupied modulator slot, and then toggle the
**Per-Voice** option" `[V: same]`.

**The load-bearing sentence — the degradation rule:**

> "while nested devices can be modulated from the top-level device,
> **polyphonic modulation sources are usually made available as summed
> monophonic signals**." `[V: same]`

`[inf]` This is the general law, and every DAW obeys some version of it:
**green collapses to blue at every boundary that does not carry voice
identity.** FL's version of the same law is "the mixer is downstream of the
voices". If you want per-voice modulation to survive a boundary, the
boundary's event vocabulary must carry a voice id. There is no other way.

What the voice model has to provide, all verified `[V: same]`:

* Explicit allocation with a `Voices` count and live voice management
  (readout `0 / 12`).
* Three voice modes — polyphonic, **True Mono** (voice always on), **Digi
  Mono** (two alternating voices; "since using two voices is technically
  polyphonic, voice management is engaged").
* **Voice Stacking**: up to 16 voices per note, with `Stack Spread` (12
  spread modes) and `Voice Control` ("individual modulators for Stack Voice
  1 thru Stack Voice 16") as *modulator sources addressed by stack index*.
  So the modulation addressing space is **(note-voice × stack-index)**, not
  just note-voice.
* 3.0 extended per-voice modulation beyond floats: "For all Bitwig
  instruments (and FX Grid), **integers, boolean, and indexed controls** now
  support polyphonic (per-voice) modulations" — and, same release,
  "**polyphonic voices are now processed in parallel**"
  `[V: downloads.bitwig.com/stable/3.0.3/Release-Notes-3.0.3.html]`.
* Per-voice extends to *effects*: "all Grid effect devices (FX Grid and Note
  Grid) … uniquely have these polyphonic options as well", and "all Note FX
  devices handle notes individually, but **only Note Grid allows modulators
  to work in a per-note (or polyphonic) fashion**" `[V: .../on_grid_signals]`.

**Plug-ins.** Modulators load into VST and CLAP alike, but per-voice is
CLAP-only: "**CLAP plug-ins can support polyphonic modulation.** Of course
individual plug-ins vary in what they support" `[V: same]`; 4.3 shipped
"CLAP plug-ins … including polyphonic modulation, voice stacking"
`[V: downloads.bitwig.com/stable/4.3/Release-Notes-4.3.html]`. For VST the
escape hatch is **Force MPE**: "if you are using a multitimbral plug-in, its
performance may be improved by forcing it to use MPE mode … This modern MIDI
specification interfaces well with Bitwig Studio's per-note modulation
capabilities" `[V: .../vst_plug-in_handling_and_options/]`.

`[inf]` **Why VST can't do it.** VST2/VST3 expose a parameter as one scalar
with one setter; there is no event carrying `(param_id, value, voice_id)`.
VST3 note expressions are a fixed registered set per note, not
arbitrary-parameter-per-note. Bitwig literally has no channel to say "for
voice 7, param 42 is +0.3". **This is why CLAP exists**, and it is the
clearest example in the industry of a company being forced to invent a
plugin ABI because its object model outgrew the available ones.

### 2.3 The Grid

Three devices on one engine — **Poly Grid** (instrument), **FX Grid** (audio
FX), **Note Grid** (note FX); patches convert between them and copy-paste
across instances; Polymer/Filter+/Sweep are Grid patches in fixed shells
("Convert to Poly Grid" / "Convert to FX Grid")
`[V: bitwig.com/userguide/latest/welcome_to_the_grid; bitwig.com/the-grid/]`.
154 modules at 3.0 → 231–235 today `[V: 3.0 release notes; bitwig.com/the-grid/]`.

**Signal types — advisory, not enforced** `[V: .../on_grid_signals]`:

> "While **any signal can be connected anywhere**, there are certain signal
> types within The Grid, often indicated by port color…"

| Type | Colour | Semantics (verbatim) |
|---|---|---|
| Logic | yellow | "any signal level at or above **+0.5** is treated as high logic … only sensitive to these **state changes**" |
| Phase | purple | "**unipolar** signal from 0 to just below 1 … signals are **wrapped** into the range. +1.02 would be used as +0.02" |
| Pitch | orange | "**bipolar** … **0 represents 'middle C' (C3) with each change of ±0.1 representing an octave**" |
| Untyped | red | "of unspecified range and function" |
| Secondary untyped | blue | when a module has two kinds of untyped port |

Sound on Sound, independently: "the colouring is **advisory rather than
mandatory** … **everything is audio-rate and stereo**. In fact, you can even
change colours arbitrarily" `[V: soundonsound.com/reviews/bitwig-studio-3]`.

Elevating **phase** to a signal type is a real design bet: "Along with
pitch, timbre, and loudness, phase is another essential element of sound"
`[V: bitwig.com/the-grid/]`.

**The uniform signal format, and the seam where it ends** `[V: .../on_grid_signals]`:

> "**Every signal in The Grid is stereo.** … so yes, every audio cable is
> stereo, but so are all pitch, phase, and trigger signals as well."
>
> "**all signals within The Grid also operate at four-times (400%) your
> configured sample rate.**"
>
> "modulator signals operate differently from Grid signals. While Grid
> signals run at four-times the current sample rate and are stereo, **all
> modulators are mono and operate at your current sample rate**. This is
> true for all modulators … no matter what their target is."

`[inf]` **That last paragraph is the most honest architectural sentence
Bitwig has published.** The Grid is a *second signal domain* (stereo, 4×)
grafted onto the pre-existing device/modulation domain (mono, 1×), and the
conversion happens at the boundary. Even the most unified DAW in the market
has two domains and a documented downcast at the seam. **Plan for the seam;
do not plan to avoid it.**

**Feedback is compile-time-detected and structurally broken**
`[V: .../special_connections]`:

> "Feedback loops are possible but **prevented when made directly** … once
> the mouse/touch is released, **the cable disappears**."
>
> "To make a feedback connection: insert a **Long Delay** module … Long
> Delay is specially configured to allow feedback and has a **minimum delay
> time of one block size**."

`[inf]` Refusing to create the edge (rather than inserting an implicit
1-sample delay as Reaktor and VCV do) is what you do when the patch is
topologically sorted ahead of time into a straight-line schedule and the
scheduler requires a DAG. "Minimum delay of one **block** size" further
implies a block-based schedule, not per-sample traversal.

**Compilation — what is actually verified, and what isn't.** Verified:
"since Bitwig Studio 5.3, Bitwig's **device/DSP code compiles in
parallel**. When first loading a project, Bitwig **caches the results**"
`[V: bitwig.com/modern-foundations/;
downloads.bitwig.com/5.3%20Beta%201/Release-Notes-5.3-Beta-1.html]`.

**Not verified:** no Bitwig-authored use of "JIT", "LLVM" or "as efficient
as hand-written C" exists on any page reached. The "compiled to native code
in realtime" phrasing circulating on KVR is from Jürgen Moßgraber, a
prolific third-party controller developer, not Bitwig
`[V: kvraudio.com/forum/viewtopic.php?t=513516&start=225]`. **Treat "the
Grid is JIT-compiled with LLVM" as folklore.**

**Voice liveness is computed disjunctively from a per-module flag**
`[V: .../on_grid_signals]`:

> "`Affect Voice Lifetime` … the module is included in the calculation for
> whether each voice is still sounding and should be kept alive."
>
> "Only when **all** conditions being considered have finished is a voice
> extinguished … enabling an additional Affect Voice Lifetime parameter
> **can only keep notes to the same length or allow them to go longer**; it
> will never shorten them."

Plus **pre-cords** — implicit per-voice broadcast busses ("a toggle near an
in port that connects to the same module **buss**": note gate, note pitch,
device phase) which are re-routed wholesale when FX Grid's `Note Source`
changes `[V: .../special_connections]`.

And the downcast rule again, from SOS: "the entire signal path runs in
stereo right up until any point where a single signal is needed … at which
stage the left and right parts are merged together. Similarly, **signals
from polyphonic voicing are averaged together** where a single value is
required" `[V: soundonsound.com/reviews/bitwig-studio-3]`.

**Was the Grid built on existing infrastructure?** Yes, explicitly. Dom
Wilms: "**The whole of the software is modular at its core, but this was the
first time we opened that up to the people.**" Claes Johanson: "We pursued
the path of modularity in full force with version 2 … I personally got very
interested in the idea of **intertwining the sequencer and the tone
generator**" `[V: musicradar.com/news/10-years-bitwig]`. And the Grid
participates in the general machinery: "just as Bitwig Studio's modulators
can control any parameter in The Grid, **any grid signal can be used to
modulate child devices**"; Grid devices are "controllable from the **same
Open Controller API**" `[V: bitwig.com/the-grid/]`.

### 2.4 "Everything is a device" — what that actually buys

**There is one chain type, and it is hybrid**
`[V: .../devices_modulators_and_other_signal_achievements]`:

> "**All device chains in Bitwig Studio support both audio and note
> signals.** … Except for note FX devices, all devices receiving note
> signals **pass them directly to their output** … Except for audio FX
> devices, all devices receiving audio signals **pass them to their
> output**."
>
> "In Bitwig Studio, all audio signal paths are stereo."

Restated with motive `[V: .../bouncing_to_audio]`: "To enable this workflow
of **hybrid tracks**, virtually every Bitwig device **passes thru signals
that are not its focus** … instrument and audio effect devices send on the
note signals they receive, **as following audio devices or modulators may
take advantage of them**."

So there is no note-chain type vs audio-chain type. There is one chain
carrying notes + audio + modulation, and devices are classified by what they
*consume*. **Track types are derived from content, not declared**: bouncing
converts an instrument track into a **hybrid track** "while preserving the
entire device chain" `[V: same]`.

**Container devices, with explicit signatures** `[V: .../container]`:

| Device | Signature |
|---|---|
| Chain | (Audio, Audio) — serial |
| FX Layer / FX Selector | (Audio, Audio) — parallel / one-at-a-time |
| Instrument Layer / Selector | (Notes, Audio) |
| **Note FX Layer / Selector** | **(Notes, Notes)** |
| Mid-Side Split, Stereo Split, Multiband FX-2/-3 | (Audio, Audio) — band/channel split into independent chains |
| Replacer | (Audio, Audio) — audio→note detection feeding an internal Generator chain |
| XY FX / XY Instrument | (Audio/Notes, Audio) — 4 chains, crossfaded |

Selector devices carry a real voice-routing policy engine — `Manual,
Round-robin, Free-robin, Free Voice, Random, Random Other, Keyswitches, CC,
Program Change` — and "Other than Manual, all other modes are **aware of the
layer count**. So adding or removing layers will just work"; "any automation
of the `Index` parameter is **dynamically updated when the device chains are
rearranged**" `[V: same]`. Drum Machine offers 128 drum chains with
**directional choke groups** ("chain A could choke chain B, but chain B
could allow chain A to continue playing") `[V: .../advanced_device_concepts]`.

**The real type system is the named nested-chain slots on ordinary devices**
`[V: .../advanced_device_concepts]`:

> "**Most of the Bitwig devices actually possess one or more device chains
> of their own.**"
>
> * **FX / Post FX** — "processing the device's entire audio output … this
>   chain is **fully stored with this device**"
> * **Pre FX** — "processing signal **immediately before it enters** the
>   device"
> * **Wet FX** — "processes **only the wet portion** of the device's output.
>   The dry signal skips this chain and is mixed back in afterward"
> * **FB FX** — "placed **within the device's feedback loop**. This is
>   common on delay devices"
>
> "the idea of nesting devices allows for … **blending serial and parallel
> structures across a single device chain**."
>
> "**Just like Bitwig devices, plug-ins can be used in any device chain at
> any level.**"

`[inf]` The payoff is compositional and it is the whole point of the
"everything is a device" slogan: **a third-party VST dropped into a Drum
Machine chain inside an FX Layer gets modulator slots, remote-control pages,
and its own Pre/Post/Wet/FB chains for free**, because those are properties
of the *device* abstraction rather than of any particular device. Bitwig
sells exactly this: "Use container devices to give **any plug-in** a dry/wet
knob, mid-side control, or sophisticated spectral processing"
`[V: bitwig.com/modern-foundations/]`.

### 2.5 Out-of-process plugin hosting

Five modes, "progressive with those on the left potentially using less RAM
and those toward the right offering greater safety"
`[V: .../vst_plug-in_handling_and_options/]`:

| Mode | Verbatim |
|---|---|
| **Within Bitwig** | "hosts plug-ins **along with Bitwig Studio's audio engine** … one plug-in crashing would also crash the audio engine" |
| **Together** (default) | "hosts all plug-ins, well, together but does it **separately from the audio engine**" |
| **By manufacturer** | "groups based on their manufacturer. This can be particularly useful when a software creator intends for their various plug-ins to communicate with one another" |
| **By plug-in** | "hosts **each instance of the same plug-in together**" |
| **Individually** | "hosts **every plug-in instance by itself** … This will require more computing resources, but that is the trade-off" |

Plus a per-plugin override list, and non-retroactive semantics ("only new
plug-ins added will follow the updated setting"). Crash behaviour: "**In
many cases, a plug-in crash will happen discreetly, allowing audio to
continue playback seamlessly**"; the device UI is replaced by a **Reload
Plug-in** / **Reload All Plug-ins** notification `[V: same]`.

Overall process story: "Bitwig's architecture is unusual among DAWs, keeping
its **application, audio engine, and plug-ins in separate** [processes] … If
your **audio engine crashes, it doesn't bring down the program** …
**Projects will load immediately, with plug-ins following after** … [you
can] **open multiple projects at once**" `[V: bitwig.com/modern-foundations/]`.
(That page says "threads"; the user guide and support page say "In Bitwig
Studio, **plug-ins run in a separate process**"
`[V: bitwig.com/support/technical_support/what-is-plug-in-crash-protection-26/]`.
Trust "processes".)

**The transport is shared memory, pooled per host process.** Bitwig 2.5
release notes: "Reduce the OS resources we allocate for each plug-in (**we
no longer allocate a new shared memory area for each plug-in**)"
`[V: downloads.bitwig.com/stable/2.5/Release-Notes-2.5.html]`. `[inf]` That
is exactly what makes "By manufacturer"/"By plug-in" cheaper than
"Individually": N instances in one host share one mapped region and one
wake/sleep synchronisation.

**Why the grouping modes exist — from CLAP's author, a Bitwig developer**
`[V: gist.github.com/abique/4c1b9b40f3413f0df1591d2a7c760db4]`:

> "**Reducing IPC overhead** — The overhead comes from **context switching
> between processes**, and the smaller the latency is … the higher the
> overhead becomes. The solution is to **bulk process**. … We could create a
> **single plugin process for multiple plugins from the same vendor**. That
> way, the host can **group the processing requests toward multiple plugins
> into one single bulk request, reducing the amount of context switch**."

**And the counter-case, with numbers** — Paul Davis, Ardour
`[V: ardour.org/plugins-in-process.html]`:

> "the fixed cost of a context switch is on the order of **3usec** … real
> world costs per context switch for audio processing code are between
> **10usec and 300usec** … Let's assume that our real world average is **30
> usec**."
>
> For 128 tracks × 3 plugins: "**256 to 768 context switches per block**" →
> "anywhere from **7.7msec to 23msec** spent doing nothing but context
> switches!" against a 1.3 ms budget at 48 kHz / 64 samples. "We would need
> buffer sizes of about **700 - 2000 samples, or roughly 14-40 msec**." "It
> may work for 4 track Bitwig session with 12 plugins or thereabouts but
> it's not suitable for any large scale work."

`[inf]` **The synthesis of these two is the actionable insight, and it is
not a plugin-layer insight.** Davis's arithmetic is correct *per round
trip*; Bique's answer is to make the number of round trips independent of
the number of plugins. That is only possible if your **graph partitioner**
can hand an entire partition to one process in one dispatch. **Sandboxing
quality is therefore a property of your scheduler, decided when you design
the graph compiler — not a property of your plugin adapter, decided later.**
Davis's own objection to the bulk case ("requires that the DAW has no reason
to access the data from each plugin before running the next plugin in the
chain") names the exact constraint: partitions must be maximal runs with no
host-side work interleaved.

**No vendor has published latency or CPU overhead numbers for this.** The
only figures in circulation are Davis's adversarial model and yabridge's
~5–15% per instance, which measures Wine bridging — a different thing.

**GUI embedding: unverified.** No Bitwig or Bique statement describes the
mechanism. What is verified is platform-specific window management across
the boundary — "Plug-in windows better restore their position and size";
"**Windows**: … whether each plug-in should **automatically stretch its
interface**"; "**macOS**: some old VST plug-ins that have both Cocoa and
Carbon GUI will now open windows with the more modern Cocoa GUI"
`[V: 2.5 release notes]`. `[inf]` That per-platform texture indicates the
host process creates a **native top-level window** per plugin and Bitwig
does cross-process window management (position/size/parenting), not pixel
compositing over IPC. On X11 the natural mechanism is reparenting/XEmbed,
since window IDs are global to the display server — which is also why CLAP's
`clap_window` union carries an `x11` window id alongside `cocoa`/`win32`.
Unconfirmed.

**Three payoffs that are not crash protection**
`[V: bitwig.com/modern-foundations/;
downloads.bitwig.com/stable/4.0/Release-Notes-4.0.html]`:

1. "native bit-bridging for 32-bit and 64-bit plug-ins on Windows and Linux
   — **no third-party bridging necessary**".
2. Cross-ISA on Apple Silicon: "**Intel VSTs can still be used alongside ARM
   VSTs**".
3. Async load: "Projects will load immediately, with plug-ins following
   after."

### 2.6 CLAP, in the spec's own words

Origin: "CLAP was originally developer **Alexandre Bique's private
project**: In 2014, while he was **porting u-he plug-ins to Linux** …"
`[V: cleveraudio.org/the-story-and-mission/]`. MIT licensed; contributors
include Bique plus **Claes Johanson, Nicholas Allen and Placidus Schelbert
of Bitwig**, u-he staff, Cockos, Ardour, Surge, yabridge
`[V: github.com/free-audio/clap Contributors.md]`.

**Voice addressing — the PCKN tuple**
`[V: raw.githubusercontent.com/free-audio/clap/main/include/clap/events.h]`:

> "Clap addresses notes and voices using the **4-value tuple (port, channel,
> key, note_id)** … Values … are either >= 0 if they are specified, or **-1
> to indicate a wildcard** … a PCKN of (-1, 0, 60, -1) will match all
> channel 0 key 60 voices, independent of port or note id."

**Value vs modulation** `[V: same]`:

> "PARAM_VALUE sets the parameter's value … PARAM_MOD sets the parameter's
> modulation amount … **The value heard is: param_value + param_mod.** In
> case of a concurrent global value/modulation versus a polyphonic one, the
> voice should only use the polyphonic one…"

Both structs carry `param_id` **and** the full PCKN tuple. Capability
negotiation is a 2×4 flag matrix,
`IS_AUTOMATABLE{,_PER_NOTE_ID,_PER_KEY,_PER_CHANNEL,_PER_PORT}` ×
`IS_MODULATABLE{…}` `[V: .../ext/params.h]`.

Note expressions are "a **statement of value, not cumulative**" and "an
**offset from the non-note-expression voice default**", delivered "as
**sample accurate** events" — exactly seven types: VOLUME, PAN, TUNING (±120
semitones, double, free microtuning), VIBRATO, EXPRESSION, BRIGHTNESS,
PRESSURE `[V: events.h]`.

**Poly-mod is a two-way voice protocol, not a one-way stream** `[V: events.h]`:

> "**When using polyphonic modulations, the host has to allocate and release
> voices for its polyphonic modulator. Yet only the plugin effectively knows
> when the host should terminate a voice.** NOTE_END solves that issue…"

Backed by `voice-info`: "the host **needs its own voice management and
should try to follow what the plugin is doing**"
`[V: .../ext/voice-info.h]`. And `NOTE_CHOKE`'s doc comment names the
motivating case verbatim: "a plugin is inside a drum pad in **Bitwig
Studio's drum machine**".

`[inf]` This is Bitwig's internal per-voice modulator architecture pushed
across an ABI. A per-voice LFO must instantiate one instance per *plugin*
voice, so the host needs (i) the voice count, (ii) note ids to address them,
(iii) a termination signal. `voice-info` + `note_id` + `NOTE_END` are
precisely those three, and nothing less would do.

**Threading contract** `[V: .../ext/thread-check.h]`:

> "The **audio-thread is symbolic** … the plugin.process() call may be
> scheduled on different OS threads over time. However, the host must
> guarantee that a single plugin instance will not be in two audio-threads
> at the same time. … **The audio-thread can be seen as a concurrency
> guard**."

Every function is annotated (`[main-thread]`, `[audio-thread & active &
processing]`, `[main-thread & !active]`, …). `activate()` is where "the
plugin **may allocate memory**", and "Once activated the **latency and port
configuration must remain constant**" `[V: .../plugin.h]`.

**Host thread pool** — strict fork-join, blocking, non-nesting,
process()-only, always optional `[V: .../ext/thread-pool.h]`, motivated by:
"the maximum number of real-time MMCSS threads in Windows is **32**" and
"**During tests we observed up to twice as many instances before audio
dropouts occur, and a minimum of 20%-25% in other cases**"
`[V: cleveraudio.org/1-feature-overview/_thread-pool/]`.

**The single-queue argument** `[V: cleveraudio.org/1-feature-overview/_midi/]`:

> "To achieve sample accuracy … developers have to zip these abstracted
> events from their various sources into a single queue. Not only does this
> cost more time … **it also creates ambiguity**." (CC64 hold-pedal vs
> NoteOff at the same sample) "There is a **50% chance that the data will be
> interpreted in the wrong order**." "**CLAP has all event types zipped into
> a single queue.**"

**Correction to a common belief:** the word "non-destructive" appears
**nowhere** in the CLAP headers. The spec expresses it structurally (`value
+ mod`, separate `CLAP_PARAM_CLEAR_AUTOMATIONS` / `CLEAR_MODULATIONS`). The
explicit phrasing is vendor marketing
`[V: bitwig.com/stories/clap-the-new-audio-plug-in-standard-201/;
u-he.com/community/clap/]`.

**The licensing plank has expired.** Steinberg now states "**Since version
3.8 VST 3 is licensed under MIT license**"
`[V: steinbergmedia.github.io/vst3_dev_portal/pages/VST+3+Licensing/Index.html]`.
What remains genuinely differentiating in 2026: per-voice *parameter*
modulation with PCKN addressing, the single ordered event queue,
per-function thread annotations plus runtime `thread-check`, the host thread
pool, and community rather than vendor governance. Urs Heckmann named the
last one as the real motive: "**the host isn't a shapeless god entity, it's
just another struct that we communicate with** … some of the people who
support CLAP want to switch from the back seat to the steering wheel"
`[V: lwn.net/Articles/893048/]`.

### 2.7 Launcher and arranger: two sequencers, one clip type, a per-track arbiter

This is the answer to the framing question, and it is not what most people
assume `[V: bitwig.com/userguide/latest/one_daw_two_sequencers]`:

> "Within Bitwig Studio are **two independent sequencers** … **The Arranger
> Timeline and Clip Launcher contain completely separate data.** Editing
> clips on the Arranger Timeline has no effect on those stored in the Clip
> Launcher, and vice versa. But [they] do interact in several critical ways:
>
> * Clips can be **freely copied** between … scenes can as well.
> * The result of all triggered Launcher clips can be **recorded directly to
>   each Arranger track**…
> * **only one of these two sequencers is active at any given time. So on a
>   track-by-track basis, you choose whether the Arranger Timeline or Clip
>   Launcher is in control**…
> * **Each track can play only one clip at a time.**"

The arbiter, exactly `[V: .../triggering_launcher_clips_0]`:

> "By default, each track starts with the Arranger Timeline active. The
> **Launcher will take over** for a track after a Launcher clip is either
> **triggered or recorded, or the track's Stop Clips button is pressed**.
> The **Arranger will regain control only** after the track's **Switch
> Playback to Arranger** button is pressed."

Both directions are launch-quantized. The transport is shared: "The
**transport drives all timing functions** … The Arranger Timeline's **Beat
Ruler** also has influence over the Clip Launcher Panel", and launcher clips
can even carry time-signature changes `[V: same]`.

Automation is domain-split: arranger automation is *track* automation on the
timeline; launcher automation lives *inside the clip*, recorded with a
separate Launcher Automation Record button
`[V: .../automation; .../the_clip_launcher]`. `[inf]`

**Nesting works in both domains via aliasing views**
`[V: .../arranger_clips_and_the_browser_panel;
.../acquiring_and_working_with_launcher_clips]`:

> "**meta clips** … **each meta clip acts as an alias of the clip (or clips)
> that they represent** … they can even be split with the Knife tool.
> **Taking any of these actions on meta clips directly affect the clips that
> they represent**."
>
> "**Each group track has its own row of sub scenes** … **sub scenes act as
> aliases for the clips they contain**."

`[inf]` Meta clips and sub scenes are *rendered views with edit forwarding*,
not stored objects. That is why they can be knife-split but not
independently edited.

**Comping lives inside the clip** — Bitwig 4.0, 2021
`[V: downloads.bitwig.com/stable/4.0/Release-Notes-4.0.html]`:

> "**Since comping lives within the clip, comping clips can be freely
> dragged between the Launcher and Arranger**"

Comping is modelled as one of the audio clip's **expression views**,
alongside gain/pan/pitch/formant `[V: .../working_with_audio_events]`. And
the underlying clip model is itself unusual — Sound on Sound: "**A Bitwig
audio clip is not a single piece of audio: it's actually a sequence of
consecutive audio 'events'**" `[V: soundonsound.com/reviews/bitwig-studio-4]`
— i.e. audio clips and note clips are both event containers, which is why
**Operators** could be added to both at once: "**Devices have modulators;
now notes and audio events have Operators**" `[V: 4.0 release notes]`.

`[inf]` Note the deliberate symmetry: *modulators : parameters :: operators
: events*. The same "orthogonal decorator on an existing object" pattern
applied one level down. **That is what a coherent object model buys you —
new features arrive as decorations rather than as subsystems.**

### 2.8 The Controller API: bound the observation set

The javadoc ships inside the app. Quotes below are the javadoc comments
themselves, transcribed by the community `typed-bitwig-api` project, which
is machine-generated from the Java sources and preserves `// source:` markers
and `@since` tags
`[V: raw.githubusercontent.com/joslarson/typed-bitwig-api/master/bitwig-api.d.ts]`.

Two script formats, `.js` and `.bwextension` (JVM)
`[V: bitwig.com/support/technical_support/community-controller-extensions-and-scripts-29/]`.
Placidus Schelbert: "We have the **open controller API, which has been there
from version 1** … It's public and you get access to that even with the demo
version" `[V: musicradar.com/news/10-years-bitwig]`.

**Declare interest at init; never query** `[V: .d.ts]`:

> "**Marks this value as being of interest to the driver. This can only be
> called once during the driver's `init` method.** … **If a value has not
> been marked as interested then an error will be reported if the driver
> attempts to get the current value.**"
>
> "A subscribed object will notify any observers when changes occur … **This
> allows the driver to improve efficiency by only getting notified about
> changes that are really relevant to it.** … **Subscription is counter
> based.**"

**The bank pattern, with Bitwig's own rationale** `[V: .d.ts]`:

> "**The idea behind the `bank pattern` is that hardware typically is
> equipped with a fixed amount of channel strips or controls … but Bitwig
> Studio documents contain a dynamic list of tracks, most likely more tracks
> than the hardware can control simultaneously.**"
>
> "**Instances of a bank are configured with a fixed number of items and
> represent an excerpt of a larger list of items** … **It basically acts
> like a window moving over the list of underlying items.**"

`[inf]` What the pattern buys: (1) the observation set is fixed at `init`,
so a 400-track project costs the script nothing; (2) `getItemAt(3)` is
always "hardware strip 4", so scripts are written against hardware positions
and never handle add/remove/reorder invalidation; (3) scrolling is host-side
and re-emits callbacks, so the script never diffs a list. **Net: the
observation graph is O(hardware), not O(project).** That is the property
that makes controller scripting viable across a serialisation boundary at
all — and the identical pattern reappears one level down as
**remote-control pages** (a device may have 400 params; the surface has 8
knobs; pages are tag-filterable by `'filter'`, `'osc'`, `'env'`, …)
`[V: .d.ts]`.

---

## 3. Ableton Live — the clip, and the tension it was born in

### 3.1 Origin: a performance tool that grew an arranger

Robert Henke: "In a way Ableton Live was **the answer to our own problems**.
And I believe what made the software such a success, was the fact that its
entire design was driven by **a deep understanding of a certain need**."
They were treating "the studio as a real time sequencing instrument"; the
goal was "a **playful approach to composition**"
`[V: roberthenke.com/interviews/ableton.html]`. Gerhard Behles on the
constraint that forced it: "you could get away with playing one song over a
whole night… but that kind of working method got a little limited over
time."

`[inf]` This matters because **Live is the only one of the three whose
linear arranger is the retrofit.** Everything about the Session/Arrangement
relationship reads as an accommodation between an original model and a later
one — and that accommodation is exactly what the other DAWs have found hard
to copy.

### 3.2 Session vs Arrangement: mutual exclusion plus a logging recorder

The manual's model, verbatim `[V: ableton.com/en/manual/session-view/]`:

> "Each vertical column, or track, **can play only one clip at a time**."
> Horizontal rows are scenes, whose launch buttons "**launch every clip in a
> row simultaneously**."
>
> "**The Session clips and the Arrangement clips in one track are mutually
> exclusive: Only one can play at a time.**" When a Session clip launches,
> "Live **stops playing back that track's Arrangement** in favor of the
> Session clip."
>
> "**Arrangement playback does not resume until you explicitly tell Live to
> resume** by clicking the **Back to Arrangement** button", which "lights up
> to remind you that **what you hear differs from the Arrangement**."

And the round trip:

> "When the Arrangement Record button is on, **Live logs all of your actions
> into the Arrangement: The clips launched; Changes of those clips'
> properties; Changes of the mixer and the devices' controls.**"

`[inf]` Two observations. First, **Live's arbiter is identical in shape to
Bitwig's** (§2.7) — per-track, exclusive, with an explicit "return to
arranger" gesture — which strongly suggests this is the *only* shape that
works, and both companies (who share founding DNA) found it independently or
by inheritance. Second, "Live logs all of your actions" is a broader capture
than Bitwig's clip-output recording: it records *mixer and device control
changes* too. **That means the Session→Arrangement path is a general
performance recorder over the parameter system, not a clip-copying
operation.**

### 3.3 Warping: a per-clip time map

`[V: ableton.com/en/manual/audio-clips-tempo-and-warping/]`:

> "The **Warp switch** toggles warping on or off for an audio clip." Off:
> "the sample plays at its **original tempo, unaffected by the Set's current
> tempo**."
>
> "**Warp Markers** let you lock a specific point in a sample, such as a
> transient, **to a particular place in the timeline**."
>
> "Live automatically analyzes the audio and finds its **transients** …
> amplitude peaks that indicate where notes or beats begin."
>
> "By default, warped clips **follow the Set's tempo**", though a clip can be
> a **tempo leader** via the Lead/Follow toggle.

Warp modes: Beats (dominant rhythm), Tones (distinct pitch), Texture (no
clear pitch), Re-Pitch (playback-rate change, "similar to vinyl
turntables"), Complex/Complex Pro ("entire songs") — with the explicit
warning "**The Complex and Complex Pro modes may be more CPU-intensive than
the other Warp Modes**" `[V: same]`. (Complex Pro is widely reported to be
zplane's élastique; not verified against a primary source — see §6.)

`[inf]` The architectural demand: **every audio clip carries a piecewise map
from song-beat-time to sample-time**, and the clip player is a
time-stretcher, not a sample reader. Consequences that cascade: the
stretcher runs in the RT thread at playback time (hence the CPU warning); it
has algorithm-dependent latency, which the delay compensator must know
about; and the tempo map must be authoritative across the whole set, because
a clip's warp map is meaningless without it. **Retrofitting this onto a
sample-anchored clip model means touching the clip player, the scheduler,
the PDC compiler and the file format at once.**

### 3.4 Racks: encapsulation, and the limits of static mapping

`[V: ableton.com/en/manual/instrument-drum-and-effect-racks/]`:

> "the entire contents of any Rack can be thought of as **a single device**."
>
> "Each chain receives the same input signal at the same time, but then
> processes its signal serially through its own devices. The output of each
> of the parallel chains is **mixed together**, producing the Rack's
> output." (Drum Racks differ: "each Drum Rack chain receives input from
> only a single assigned MIDI note".)
>
> Zones are "**sets of data filters that reside at the input of every
> chain**" — Key Zones, Velocity Zones, Chain Select Zones — with fade
> ranges.
>
> "The **Macro Controls** are a bank of knobs, each capable of addressing
> **any number of parameters from any devices in a Rack**." Mappings support
> ranges: "**Refine the value range if desired using the Min/Max sliders** …
> **Inverted mappings** can be created by setting the Min slider's value
> greater than the Max slider's value."
>
> **Macro Variations**: "store different states of Macro Controls as
> individual presets (or 'variations')."
>
> "**Racks can contain any number of other Racks.**"

`[inf]` **Macros are not a modulation system.** A macro is a static
many-to-one mapping with a per-mapping range — a *fan-out of a value*,
evaluated when the user (or automation) moves the knob. There is no
modulation *source*: no LFO device you can point at an arbitrary parameter,
no per-voice anything. Live's nearest equivalent to modulation is the **clip
envelope in modulation mode** (M4L parameters expose a "Clip Modulation
Mode" of Unipolar / Bipolar / Additive / Absolute
`[V: docs.cycling74.com/userguide/m4l/live_parameters/]`) — i.e. **modulation
in Live is a property of *clips*, not of *devices*, which is why it cannot
be per-voice and cannot be nested.** This is the single largest
architectural gap between Live and Bitwig, and it is not closable by adding
devices.

Racks *are* however a genuine encapsulation success, and an important data
point for §5: **Live shipped them in 6.0, years after 1.0, and they work.**
Nesting was retrofittable because Live's device abstraction was already
uniform.

### 3.5 Max for Live: an embedded runtime with a scripting path back into the host

Verified mechanics
`[V: github.com/Ableton/maxdevtools/.../m4l-production-guidelines.md]`:

* **Latency is declared, not measured**: "Live allows setting **Defined
  Latency** in the patch Inspector, **Live compensates for this latency on
  playback**"; device latency is shown in the status bar on hover.
* **Automation exposure** is opt-in per UI object: `[live.*]` objects with
  "Parameter Visibility" set to "**Automated and Stored**" appear in Live's
  automation lanes and MIDI-map mode
  `[V: same; docs.cycling74.com/userguide/m4l/live_parameters/]`.
* **Devices are referenced, not embedded**: "**Live treats Max for Live
  devices in a similar way as sample files.** Live Sets **reference** a Max
  for Live device (the AMXD file), meaning the device is **not contained
  within** the Live Set." Freezing bundles abstractions, audio, images, JS
  and externals into the AMXD.
* Performance guidance includes "Defer Automation Output" and higher "Update
  Limit" values when "parameter automation causes high CPU load."

The docs do **not** state whether Max runs in-process. `[inf]` The
combination of declared-latency PDC participation, in-chain placement, and
per-instance CPU accounting is consistent with in-process hosting of a Max
runtime; but this could not be verified and should not be stated as fact.

Live 12's **MIDI Tools** (Generators and Transformations in the clip editor)
are partly Max devices: the bundled Velocity Shaper and Euclidean are Max
for Live devices, and "**you can edit or create Max for Live MIDI Tools in a
Max patcher like any other Max device**", with third-party AMXD tools
installable `[V: ableton.com/en/live-manual/12/midi-tools/;
help.ableton.com/.../Using-third-party-MIDI-Tools]`.

`[inf]` This is the strategically interesting move and the cheapest of the
three "modular" answers: **Ableton did not build a modular DSP environment;
they bought one and gave it host API access.** M4L's real leverage is not
DSP — it is the Live Object Model, the ability for a *device* to script the
*host*. That converts an embedded runtime into an extension platform, which
is a different and larger thing than the Grid or Patcher.

### 3.6 Delay compensation

> "Live measures the latency on each track and **delays the others as
> needed** … **the value is determined by whichever signal path causes the
> highest amount of latency**" — three tracks at 100/20/50 ms all become
> 100 ms. Compensation covers "**audio, automation, and modulation**."

`[V: help.ableton.com/hc/en-us/articles/209072409-Delay-Compensation-FAQ;
.../360010545559-How-Latency-Works]`

**Reduced Latency When Monitoring** deliberately breaks it for the armed
track: it "**bypasses Delay Compensation** for a monitored track", trading
sync for playability `[V: help.ableton.com/.../209072249]`.

`[inf]` Note that Live compensates *automation and modulation*, not just
audio. **That is only expressible if automation delivery is part of the same
scheduled graph as audio** — a design constraint worth writing down early.

### 3.7 Ableton Link

`[V: github.com/Ableton/link; ableton.github.io/link/]`:

* Peer discovery is automatic on the local network; the transport is not
  documented in the public README. Multicast UDP is widely reported;
  unverified here.
* **No master.** "Link-enabled apps each have their own independent
  timelines. The Link library maintains a **temporal relationship** between
  these independent timelines." "Each participant chooses to adopt the
  **last tempo value that they've seen proposed** on the network."
* Timing is a `(beat, time, tempo)` triple; a **quantum** aligns bar/loop
  phase.
* API is **capture / modify / commit** on a `SessionState`, with **two
  separate surfaces**: realtime-safe capture/commit for the audio callback,
  and a non-realtime surface for the app thread that "cannot be used from
  audio thread (may block)". Audio-thread commits "can specify buffer
  boundaries or sample positions."
* Output latency is the caller's responsibility: "**output latency should be
  added to system time values**"; a `HostTimeFilter` "performs a **linear
  regression between system time and sample time**".
* Start/stop sync since v3. Dual licensed **GPLv2+ / proprietary**.

`[inf]` Link is the cleanest example in this whole document of a feature
that is *purely additive*: it needs only (a) a tempo map you can query and
nudge, (b) a sample-accurate transport position, and (c) an honest
output-latency figure. Any engine with those three can adopt Link in days.
**It is at the bottom of the leverage ranking for exactly that reason —
which is a compliment to its design, not a criticism.**

---

## 4. The cross-cutting question: is there one object model?

### 4.1 What the industry's own common denominator looks like

The most useful evidence is **DAWproject**, Bitwig's open interchange format
— an explicit attempt to write down the object model all DAWs share
`[V: github.com/bitwig/dawproject]`:

```
<Project>
  <Application>  <Transport>            tempo, time signature
  <Structure>    Track(contentType="notes"|"audio"|"notes audio")
                   → Channel → Devices, Volume/Pan/Mute
  <Arrangement>  Lane → Clips → Clip(time, duration, playStart)
                   → [Notes | Audio+Warp | Points]
  <Scenes>       clip-launcher content
```

with `Clip` able to nest child `<Clips>`, `timeUnit` selectable per element
as `beats` or `seconds` (and mixable within one project), and `<Warp>`
points mapping project time to audio content time.

Two things are striking. First, the shared model *does* include both an
Arrangement and Scenes, and it *does* include nested clips and warp maps —
so those are consensus. Second, the explicit non-goals exclude "low-level
MIDI events", and there is **no representation at all for modulation
routings, per-voice anything, or modular patches**. Devices serialize as
opaque `<State>` blobs.

`[inf]` **That is the industry admitting where the common model stops.**
Everything below the device boundary is vendor-private, because no two
vendors agree on what a device *is*. If you want the Bitwig capabilities,
you are choosing to build the part of the model that nobody has
standardized.

### 4.2 The unification that works: one spine, everything else a view

Here is the model worth writing down. It is essentially Bitwig's,
generalized, with FL's content/placement split restored (which Bitwig
lacks) and FL's performance-zone trick substituted for a second launcher
data store.

**Six object kinds. Everything in the feature list is a view or a decorator
on these.** `[inf]`

1. **`Node`** — a processor with typed ports: audio (channel-counted), event
   (notes + sample-accurate param changes), and **modulation**. A node may
   *own named sub-chains* (`Pre`, `Post`, `Wet`, `FB`, or N parallel
   `Layers`). Tracks, buses, sends, instruments, effects, containers, plugin
   adapters and Grid modules are all Nodes. **There is no separate "track"
   type — a track is a Node with a mixer-strip role.**
2. **`Param`** — addressed by a stable `(node_id, param_id)`, with range,
   flags and a *modulation capability set* modelled on CLAP's flag matrix
   (`automatable`, `modulatable`, `per_note_id`, `per_key`, `per_channel`).
   The uniform parameter namespace is what makes FL's "right-click
   anything" and Bitwig's "modulate anything" *the same feature*.
3. **`Voice`** — a runtime entity with an identity that travels in the event
   vocabulary. Every event and every modulation delivery carries a
   PCKN-shaped address with wildcards. Sub-chains can be instanced per
   voice.
4. **`Pattern`** (content) — a named container of events in musical time,
   with its own length. Multi-channel, FL-style: a pattern may address many
   nodes.
5. **`Placement`** (position) — `{lane, start, length, contentRef,
   transforms}` where `contentRef` is a pattern, an audio source + warp map,
   or an automation curve. Multiple placements may share one content object;
   a linear-DAW "clip" is a pattern with exactly one placement, created
   implicitly.
6. **`Edge`** — audio, event, or **modulation**. A modulation edge is a
   first-class record: `{source, target_param, amount, transfer_fn,
   scaling_source?, per_voice?}`.

Then:

* **Linear arrangement** = placements in lanes, played by song position.
* **Clip launcher** = a set of placements in a *trigger region* plus a
  per-track policy `{quantize, press mode, motion, sync}` and a per-track
  arbiter (`launcher | arranger`). FL proves the region can live in the same
  timeline; Bitwig proves the arbiter must be per-track and both directions
  must be quantized.
* **Step sequencer** = a UI over a pattern with a fixed grid. Zero engine
  cost.
* **Modular patching** = a Node whose sub-graph is user-editable, compiled
  by the same graph compiler. Zero *new* engine concepts if the compiler was
  built for it.
* **Nested device chains** = Nodes owning sub-chains. Falls out of (1).
* **Per-voice modulation** = modulation edges with `per_voice = true`,
  delivered through the voice-addressed event vocabulary, with a
  **documented downcast rule** at every boundary that cannot carry voice
  identity.

### 4.3 Who came closest, and what it cost them

**Bitwig, unambiguously, and it is not close.** It has all six of the target
capabilities except one: it has no content/placement instancing — arranger
and launcher hold "**completely separate data**"
`[V: .../one_daw_two_sequencers]`, and there is no FL-style shared pattern.

What it cost them, all verifiable:

1. **A plugin format.** Per-voice modulation could not cross the VST
   boundary, so they co-created CLAP and shipped it in 4.3 — **eight years
   after 1.0**.
2. **Documented degradation everywhere.** "polyphonic modulation sources are
   **usually made available as summed monophonic signals**" into nested
   devices; polyphonic voices "are **averaged together** where a single
   value is required".
3. **Two signal domains.** The Grid is stereo/4×; modulators are "**mono and
   operate at your current sample rate** … no matter what their target is".
   The most unified DAW on the market still has a seam and a downcast.
4. **Time.** 1.0 in 2014; modulation at 2.0 (2017); the Grid at 3.0 (2019);
   comping at 4.0 (2021); track/project-level modulators at 5.0 (2023). The
   Grid, the flagship modular feature, arrived *five years in* and was
   described by Bitwig as opening up infrastructure that was already there:
   "**The whole of the software is modular at its core, but this was the
   first time we opened that up to the people**."
5. **Conventional features arrived late.** A DAW shipped comping in year
   seven.

**FL Studio is second**, with a different scorecard: it has content/placement
instancing (best in class), step sequencing, a modular graph (Patcher),
per-channel voice-aware processing, and a launcher — but no unified
per-voice modulation across arbitrary devices, and no modulation *system* in
Bitwig's sense (automation clips are a linkable controller, not a modulator
you attach to a device).

**Ableton is third by design, not by failure.** It has the clip and the
launcher and warping and an extension runtime, and it has consciously not
built a modulation system in twenty-five years.

**Two negative results worth internalizing.** Reason built the modular rack
first and then grew an arranger onto it, and the arranger has never been the
product's strength; Sound on Sound notes that as Reason evolved "from its
original virtual studio rack into a complete DAW … some of the compelling
intuitiveness of the original experience got clouded"
`[V: soundonsound.com/reviews/reason-11]`. And in the other direction,
Studio One added a Launcher in 6.5 — after which the characterization from
its own user community is that "the Launcher and Arranger are completely
separate worlds **by design** and are **not connected**"
`[community/SOS characterization via studiooneforum.com and
soundonsound.com/techniques/studio-one-clip-launcher; not a PreSonus
statement]`.

`[inf]` Note the difference from Bitwig's *deliberate* separation: Bitwig's
two sequencers share a clip type, a transport, a beat ruler and a recording
path between them. **A retrofit that keeps the separation but drops the
connections gets the grid without the feature.**

### 4.4 The "second spine" failure mode

**Is trying to have all of them a mistake? No — but trying to have them all
at the same level of the model is.** `[inf]`

The failure mode is not ambition; it is **a second spine**. Every product in
this document that struggles has two competing organizing principles that
must be reconciled at every feature:

| Product | Spine A | Spine B | Symptom |
|---|---|---|---|
| Reason | modular rack | linear sequencer | arranger never the strength `[V: SOS Reason 11]` |
| Studio One | Arranger | Launcher (6.5) | "completely separate worlds… not connected" |
| Cubase | track/channel | retrofitted modulators (v14) | VST3-only, audio/group/FX only, no mod-of-mod-depth `[V: attackmagazine, synthanatomy]` |

The successful designs pick one spine and demote everything else:

* **FL**: spine = `pattern → placement`; the launcher is a *region* of the
  playlist, the step sequencer is a *view* of a pattern, and automation is
  *a kind of channel*.
* **Bitwig**: spine = `device graph + voice`; modulators are *decorators on
  devices*, operators are *decorators on events*, the Grid is *a device
  whose graph is editable*, comping is *an expression view of a clip*.
* **Live**: spine = `clip in a track`; racks are *a device that contains
  devices*, warping is *a property of a clip*, M4L is *a device that happens
  to be a runtime*.

**The tell that you are doing it right is when new features arrive as
decorators on existing objects rather than as subsystems.** Bitwig's own
release note is the best statement of this test in the industry:
"**Devices have modulators; now notes and audio events have Operators.**"

---

## 5. The leverage ranking

This is the payload. The ordering is by **(cost if you decide it now) ÷
(cost to retrofit later)**. Where there is empirical evidence of a retrofit
succeeding or failing, it is cited.

### Tier 0 — decide before the second thousand lines of engine code. Retrofit ≈ rewrite.

**1. The voice as an addressable, first-class runtime entity.**

*Cost now:* one field in the event struct (a `note_id`) plus a rule that
sub-chains may be instanced per voice.

*Cost later:* total. **Nobody in the industry has ever retrofitted per-voice
modulation onto a DAW.** Bitwig had it from the start and still had to
invent a plugin format to make it cross a process boundary.

This is the single highest-leverage decision in the entire document, and its
cost today is close to zero: **adopt CLAP's PCKN addressing as the internal
event vocabulary, everywhere, from now on** — including in on-disk note
records. Then adopt Bitwig's degradation rule as an explicit, documented law
rather than a bug: *voice identity is preserved until it crosses a boundary
that cannot carry it, at which point green becomes blue.* Write that
sentence in the architecture doc.

**2. Musical time as the primary axis; sample time as a projection.**

*Cost now:* a tempo map and integer ticks.

*Cost later:* every position field in every schema, plus warping (which is
meaningless without an authoritative song-tempo relation), plus every UI
that assumed seconds. Live's entire warp model is downstream of this. FL put
`ppq` in the *file header* — position 12 of the format, decided in 1998,
still there.

**3. One uniform parameter namespace, and one path for automation +
modulation + live tweak + recording.**

*Cost now:* a stable `(node_id, param_id)` address with capability flags,
and a rule that a knob move, a played-back curve, a modulator output and a
MIDI CC all arrive at the node through the same mechanism.

*Cost later:* look at what Cubase 14 shipped after 30 years — modulators
that reach **VST3 only**, on **audio/group/FX channels only**, with **no
modulation of modulation depth** `[V: attackmagazine.com, synthanatomy.com]`.
**Every one of those three limitations is a parameter-addressing limitation,
not a DSP limitation.** Meanwhile FL's "right-click anything → create
automation clip" and Bitwig's "modulate anything" are *the same feature*
falling out of the same bet.

**4. Structure as a compiled schedule with latency computed at compile time.**

*Cost now:* topological sort into a flat schedule; every node reports
`latency_samples()`; the compiler inserts compensating delays.

*Cost later:* two separate disasters. First, **PDC retrofitted onto a
shipped product audibly changes users' existing mixes**, which is a social
problem with no engineering fix. Second, you inherit FL's permanent scar
tissue: manual latency offsets in the plugin wrapper, and a compensator that
needs the user to *name a mixer track* so it can discover a routing
`[V: image-line manual, wrapper.htm]`.

Related: **cycles must be illegal except through an explicit one-block delay
node, detected at compile time** — Bitwig's Grid literally makes the cable
vanish rather than allow an implicit delay.

**5. A self-describing, skippable project format.**

*Cost now:* five lines — a length-carrying record header and a rule that
unknown records are preserved and skipped, never rejected.

*Cost later:* impossible; you cannot retroactively make old readers
tolerant. FL's event-ID-encodes-size scheme has carried 25 years of
"lifetime free updates" `[V: pyflp.readthedocs.io;
image-line.com/fl-studio/lifetime-free-updates]`. Ableton's answer to the
same problem is to refuse to overwrite older-version sets
`[V: ableton.com manual]` — a product decision forced by a format decision.

The property you want is *forward tolerance*: an old reader must skip a new
feature without knowing what it is. **Note that this is a strictly stronger
requirement than "ignore unknown JSON fields", because it must also hold for
binary event streams.**

### Tier 1 — cheap with the right bet; expensive but survivable to retrofit.

**6. Content/placement split (pattern instancing).**

Near-free if `Placement.contentRef` exists from day one; the "linear DAW
clip" is then just a pattern with one placement. Retrofit cost is
moderate-to-high: it touches every clip reference in the project format,
undo, and the arrangement UI. Live has never done it and its users notice.
**This is also the cheapest way to get a step sequencer, a channel rack,
*and* song-section arranging — three features for one decision.**

**7. Nested device chains / containers.**

Cheap *if* a Node can own named sub-chains from the beginning and the
compiler flattens them; the payoff is Bitwig's Pre/Post/Wet/FB slots and
"give any plug-in a dry/wet knob". Genuinely retrofittable — Live shipped
Racks in 6.0 and they work — **but only because Live's device abstraction
was already uniform.** The precondition is Tier 0 #3, not this item. If your
devices are uniform, this can wait; if they aren't, it never arrives.

**8. Launcher/arranger duality.**

Lower than most people expect, because two independent implementations
(Ableton and Bitwig) converged on the same cheap answer: **one clip type,
two containers, a per-track exclusive arbiter, quantized in both directions,
and a recording path back to the timeline.** The expensive parts are the
arbiter and the launch-quantization machinery, not the grid UI. And FL shows
you can get most of it for even less: a *region of the existing timeline*
before the start marker, with per-track trigger policies, recording out to
the right of the marker `[V: playlist_performance.htm]`.

**Retrofits demonstrably ship** (Logic Live Loops 10.5, Studio One 6.5) —
but they ship *disconnected*, which is why they don't land. **Budget the
arbiter, not the grid.**

**9. Out-of-process plugin hosting.**

Cheap now if you put a **handle indirection** between the graph node and the
plugin instance and never let a plugin pointer into a node struct.
Ruinously expensive if you don't.

But the important and under-appreciated point: **the quality of your
sandboxing is determined by your graph partitioner, not your plugin
adapter.** Davis's arithmetic (10–300 µs per context switch, 256–768
switches per block at 128×3 plugins, "7.7 msec to 23 msec spent doing
nothing but context switches") is correct *per round trip*; Bique's answer
is to make round trips independent of plugin count by **bulk-dispatching
whole partitions to one process** — which requires the compiler to emit
maximal runs with no host-side work interleaved. Decide that when you design
the schedule, not when you write the bridge.

Secondary payoffs that justify the work on their own: bit-bridging,
cross-ISA hosting, async project load, and a sacrificial scanner.

**10. Warping as a clip property.**

Medium leverage. Requires the clip player to be a stretcher, the warp map to
be part of the clip, and the PDC compiler to know the stretcher's latency.
Retrofittable — everyone eventually did it — but it touches four subsystems
at once, and the CPU characteristics are user-visible enough that Ableton
documents them in the manual.

**11. The wide note record.**

Higher than it looks, and it is a **format** decision. FL's notes carry pan,
cutoff, resonance, release, slide, portamento and a MIDI-channel colour
`[V: pianoroll.htm]`; that width is why people call the piano roll the best.
Get this wrong and the fix is a format migration.

### Tier 2 — genuinely additive. Do not build these early.

**12. Modular patching (Grid / Patcher).**

Counterintuitively the *lowest*-leverage item on the list, and the one most
likely to be built too early. If Tier 0 #4 is done properly, a modular
environment is a **UI and a module library over the graph compiler you
already have** — which is precisely what Bitwig says it was: "The whole of
the software is modular at its core, but **this was the first time we opened
that up to the people**." The Grid arrived in year five. FL's Patcher is
even cheaper: a hosted plugin with its own depth-level scheduler, at the
cost of not quite matching the host's PDC and voice semantics. **Build the
compiler; the patcher is a view.**

*The exception:* if you want Grid-class fidelity you will eventually want a
second signal domain (Bitwig's stereo/4×) with an explicit downcast at the
boundary — so reserve the *concept* of a signal-format boundary even if you
never use it.

**13. Step sequencer / channel rack.** Pure UI over #6. Free.

**14. Ableton Link.** A library. It needs a queryable tempo map, a
sample-accurate transport, and an honest output-latency figure — all of
which you have for other reasons. Days of work, whenever you want.

**15. A controller/scripting API.** Additive *if* you have an observable,
op-log-shaped model. The design lesson to copy is not the API surface but
the **bank pattern**: declare interest at init, expose fixed-size windows
over unbounded lists, make the observation graph O(surface) rather than
O(project).

**16. An embedded runtime (Max-for-Live analogue).** Fully additive, and
strategically the highest-value item in Tier 2 — but note that M4L's
leverage is **the Live Object Model, not the DSP**. A device that can script
the host is an extension platform; a device that can only make sound is a
synth.

### The three sentences for the top of the architecture doc

1. **Voice identity is part of the event vocabulary, everywhere, and it
   degrades to mono only at documented boundaries.**
   *(This is the one that cannot be added later.)*
2. **Content and placement are different objects; a "clip" is a placement.**
   *(This is the one that makes the most features free.)*
3. **Every automatable thing has one address, and there is one delivery path
   for automation, modulation, live tweaks and recording.**
   *(This is the one whose absence has visibly crippled every retrofit
   attempt in the market.)*

Everything else on this list — the launcher, the racks, the patcher, the
step sequencer, Link, the scripting API — is a view, a decorator, or a
library, provided those three are true.

---

## 6. Verification gaps

Claims that could **not** be verified and must not be repeated as fact:

1. **The Grid's code generator.** Verified maximum: DSP code is *compiled*,
   results are *cached*, and since 5.3 compilation is *parallel*
   `[V: bitwig.com/modern-foundations/]`. No Bitwig use of "JIT", "LLVM", or
   "as efficient as hand-written C" exists on any page reached. The
   "compiled to native code in realtime" claim is from a third-party
   developer on KVR.
2. **Plugin GUI embedding mechanism** in any out-of-process host. Only
   platform-specific side effects are documented.
3. **Any vendor-published latency or CPU-overhead numbers for
   out-of-process hosting.** None exist. Only Davis's adversarial model.
4. **"Bitwig was first with native Linux support."** Unsupported — Ardour
   and Renoise predate it. Bitwig only claims "one of the few DAWs with
   native Linux support."
5. **Complex Pro = zplane élastique.** Widely reported; not verified against
   a primary source here.
6. **Whether Max for Live runs in-process.** Not stated in any Ableton or
   Cycling '74 document reached.
7. **Cubase clip launcher.** An eight-page Steinberg forum *feature request*
   thread was found, and no evidence Cubase has shipped one. **Do not repeat
   the claim that it has.** Cubase 14's *Modulators* are verified.
8. **FL Studio developer commentary** (Didier Dambrin et al.). The FL
   research sweep stalled; FL coverage above is manual-primary. The `.flp`
   internals come from PyFLP's reverse-engineering docs, not Image-Line.
9. Web-search budget was exhausted mid-research; later verification was by
   direct URL fetch, so some interview sources (CDM, Attack, Ask.Audio, the
   Sonic State CLAP panel) went unexplored.

---

## 7. What this means for AURA

### 7.1 The finding that costs one bit today and a migration tomorrow

`SCALABILITY.md` §3 already learned the FL lesson correctly for the AMEV
binary event format: `columnMask` with "old readers skip unknown columns,
never break" is exactly §1.8's forward-tolerance property, applied to a
binary stream. That is right, and it is rare.

**But the core 16-byte AMEV record has no note id.**

```
[tick u32][duration u32][kind u8][key u8][velocity u8][channel u8][value f32]
```

Per-voice modulation, per-note expression and MPE all require a stable
per-note identity to address — Tier 0 #1, the one item on the whole ranking
that cannot be retrofitted. **Adding a `note_id` column now costs one
`columnMask` bit. Adding it after projects exist costs a migration of every
event chunk ever written.**

This finding was reached independently by two separate research passes in
this round, from different directions (this document via CLAP's PCKN
addressing; the undo/command research via object-identity requirements for
undo). Two independent derivations of the same missing field is as strong a
signal as this kind of analysis produces.

### 7.2 The decision already taken

**The project owner has decided that AURA adopts the content/placement split
from day one** (Tier 1 #6): a clip is a *placement* referencing a named,
reusable content object, not a container that owns its own events.

The consequences to design for, per §4.2:

* `Placement { lane, start, length, contentRef, transforms }` where
  `contentRef` may be a pattern, an audio source + warp map, or an
  automation curve.
* Multiple placements may share one content object. Editing the content
  changes every placement — that is the feature, and it must be visible in
  the UI, not a surprise.
* A conventional linear-DAW clip is a pattern with exactly one placement,
  created implicitly. The user need never learn the word "pattern" to use
  the DAW linearly.
* This is what makes a step sequencer, a channel rack, and section-based
  arranging fall out as views rather than as subsystems.

Debt item D-02's `patternId` field, reserved in `midi-clip.schema.json` and
currently unimplemented, is the intended seam and should now be built out
rather than left reserved.

### 7.3 What already agrees, and what must change

**Agrees.** `SCALABILITY.md` §3's "support BOTH the FL model and the linear
timeline" is the §4.2 conclusion, reached before this research. §1's
compiled-schedule mandate and §10.5's "every processor reports
`latency_samples()`" are Tier 0 #4. §13's tick-domain time model with
`TempoMap` as the sole bijection is Tier 0 #2. The control-plane seam (§11)
is the beginning of Tier 0 #3.

**Must change or be added.**

1. **`note_id` in the AMEV column set, now** (§7.1).
2. **PCKN-shaped event addressing** — `(port, channel, key, note_id)` with
   `-1` wildcards — as the internal event vocabulary, so the CLAP adapter
   becomes near-trivial and per-voice modulation stays possible.
3. **The degradation rule, written down**: voice identity is preserved until
   it crosses a boundary that cannot carry it, and every such boundary is
   documented. AURA's boundaries today are the mixer bus and the
   `ParamTable` slot.
4. **A `Param` type distinct from a port**, with a CLAP-shaped capability
   flag matrix. This is the correction Zrythm made between v1 and v2 after
   running out of port-flag bits (see `docs/research/` companion dossier);
   AURA can skip the mistake entirely.
5. **Modulation edges as first-class project objects** with stable ids,
   appearing in the op-log, undoable and agent-addressable — not fields on a
   parameter.

### 7.4 What this document says AURA should *not* do yet

Tier 2, in order of temptation:

* **Do not build a modular patching environment.** Build the graph compiler
  properly and the patcher becomes a view over it, five years later, exactly
  as it did for Bitwig.
* **Do not build a clip launcher as a second data model.** If it ships, it
  ships as FL's performance zone: a region of the timeline plus a per-track
  trigger policy, with the per-track arbiter as the only genuinely expensive
  part.
* **Do not build Link, a scripting API, or a step sequencer early.** All
  three are additive over decisions already being made for other reasons.

And the one-line summary of the whole ranking, for anyone who reads no
further: **the three sentences in §5 are the architecture; everything else
in this document is a consequence of them.**
