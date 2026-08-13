# Zrythm — competitive & architectural analysis

**Date:** 2026-08-13
**Provenance:** produced by an Opus research agent reading zrythm.org, the
Zrythm user manual, the forum, and **both v1 and v2 source trees** directly.
Claims sourced from reading actual source files are marked *confirmed*;
everything else labelled inference **is** inference.

## Why this document exists

AURA is being re-architected around a node/port graph, a unified parameter
model, an op-log with undo, and a browsable edit history. Zrythm is the
closest existing free-software DAW to that target: it markets itself on
"limitless automation", "connect anything to anything", modulators, and a
serializable undo history — precisely the feature set we are designing for.

It is therefore the single most useful codebase in the world to read before
we commit. Not to copy: Zrythm has made two structural mistakes that cost it
a stable release and four years of rewrite, and it has one marketing claim
that does not survive contact with its own source. Both are worth more to us
than its successes.

This document is research input for the AURA architecture round. It is not
normative. The decisions it drives are listed in the final section and get
their own specs.

---

## 0. Scope note: Zrythm is two different programs

Conflating them produces nonsense, so this is stated first.

| | **v1.0.0** | **v2.0.0-alpha** |
|---|---|---|
| Released | 2024-11-22 | alpha.1 2026-05-30, alpha.2 2026-07-29 |
| Language / UI | C, GTK4 / libadwaita | C++23, Qt6 / QML |
| Build | Meson | CMake |
| Plugins | Carla (all formats) | JUCE (VST3/LV2/AU) + native CLAP + Faust internals |
| Project format | zstd-compressed JSON (yyjson) | zstd-compressed JSON (nlohmann) |
| Status | **Feature-complete. Author now calls it "unmaintained"** ([forum.zrythm.org/t/469](https://forum.zrythm.org/t/469)) | Alpha; roughly **18 named v1 features not yet ported** |

v1 is the shipped, feature-complete product that all the marketing describes.
v2 is structurally a different program that reuses v1's *concepts*.
Throughout this document, claims are tagged **[v1]**, **[v2]**, or **[both]**.

---

## 1. The headline selling points

### 1.1 As Zrythm markets them

From the [front page](https://www.zrythm.org/en/index.html) and the
[features page](https://www.zrythm.org/en/features.html), verbatim or
near-verbatim:

| Marketing headline | Copy |
|---|---|
| **Tagline** | "A highly automated and intuitive digital audio workstation" |
| **Intuitive Editing** | "Easily select, move, resize, clone, link, loop, delete and cut objects with a single tool"; "Adaptive Snapping — snapping behavior adjusts to the current zoom level" |
| **Limitless Automation** | "Automate almost anything with automation events using straight lines, ramps and curves, or with LFOs and envelopes" |
| **Anywhere-to-Anywhere Connections** | "Connect anything to anything" |
| **Automate Anything** | "Automate any parameter including BPM and time signature" |
| **Envelopes** | "Automate with CV signals, macro knobs and LFO plugins" |
| **Mixing Capabilities** | "In-context listening, signal groups, FX tracks, MIDI effects, pre/post-fader aux sends, flexible routing" |
| **Chord Assistance** | "Generate chords from scales, audition/invert chords, save presets, manage progressions"; "Recordable Chord Track" |
| **Featureful Timeline** | "Track Lanes — add multiple layers of audio/MIDI to the same track"; "Bounce in Place"; "Stretching — stretch any type of region" |
| **Never Lose Work** | "Automatic backups"; "Almost every user action is undoable"; **"Serializable Undo History — keep undo history when saving projects"** |
| **Optimized Performance** | "Hardware accelerated UI — most UI drawn on GPU via GTK4"; "SIMD-optimized DSP — lsp-dsp-lib with SSE, AVX and FMA"; "Extensive Caching — pre-calculate expensive computations" |
| **Plugin Capabilities** | LV2, VST2, VST3, AU, CLAP, JSFX via Carla; SFZ/SF2 as plugins; "Plugin Bridging — sandbox plugins in bridge mode"; "Automatable Bypass Mode" |
| **Liberating** | "copyleft free software with fully auditable source code" |

### 1.2 Real differentiator vs table stakes

**Genuine differentiators** — few DAWs have these, and no libre one has all:

1. **A user-visible, user-editable modular port graph with CV as a
   first-class port type.** "Connect anything to anything" is literally
   true: any CV output can be dropped onto any parameter, and every
   connection carries a multiplier and an enable flag. Bitwig has this in
   curated form; Ardour has anywhere-to-anywhere routing but no
   CV-to-parameter modulation; REAPER does it via a parameter-modulation
   UI, not a graph. **This is Zrythm's strongest structural claim.**
2. **Modulators as a *track type*.** A global modulator lane hosting LFO
   plugins plus macro knobs, routable to any parameter in the project.
   Bitwig has per-device modulators; a *project-global modulator rack* is
   unusual.
3. **Chord/scale as project model data, not a UI gimmick.** A chord track
   that emits and transforms MIDI inside the audio graph, plus
   scale-aware chord-suggestion scoring. Nothing in the libre DAW space
   matches this; it is closer to Scaler-as-a-DAW-feature.
4. **Serializable, unlimited undo history persisted into the project
   file.** [v1 only — see §5; v2 regressed this to a TODO.] REAPER has
   undo-history persistence; almost nothing else does.
5. **Automation with five curve algorithms per segment**, including
   Ardour's log curve and Vital's exponential curve, with per-point
   curviness. Most DAWs give you linear / exponential / hold.
6. **Everything-is-automatable including BPM and time signature**, because
   tempo is expressed as ports/parameters [v1] or as a first-class
   `TempoMap` with arranger objects [v2].

**Table stakes dressed as features:**

- "Intuitive Editing" / "Flexible Select Tool" / "Adaptive Snapping" —
  every modern DAW.
- Track lanes, group tracks, FX tracks, pre/post-fader sends,
  sidechaining, stem export, punch in/out, take recording, drum view,
  velocity editor — table stakes.
- "Hardware accelerated UI" — a claim that the GTK4 renderer exists, not
  an architectural achievement. Users measured **~10 FPS on the demo
  project** ([forum t/500](https://forum.zrythm.org/t/500)).
- "Never Lose Work / automatic backups" — table stakes; Zrythm's version
  is unusually simple (each backup is a full standalone project
  directory), which is a virtue but not a differentiator.
- "Cross-Platform Support" — nominally true, but the author himself wrote
  *"A lot of people find Zrythm unusable on Mac and Windows — even for
  basic operations"* ([forum t/194](https://forum.zrythm.org/t/194)).
  Marketing overreach.

**The tagline is a liability.** "Intuitive" triggered a 410-point HN
referendum on the word, including Ardour's Paul Davis writing
[an essay about it](https://discourse.ardour.org/t/reflections-on-intuitive/78337).
Do not copy that positioning.

---

## 2. The four data-model decisions

Zrythm's feature list is not the product of a large team. It is the product
of **four data-model decisions**. Nearly every marketed feature — limitless
automation, modulators, anywhere-to-anywhere routing, macro knobs,
automatable bypass, automatable BPM, MIDI learn on anything, five editors,
chord-track live transformation — is a *consequence* of these four, rather
than a feature someone implemented.

1. **Everything is a node in one graph.**
2. **Parameters are a type, distinct from ports.**
3. **CV is a port type.**
4. **Positions are ticks.**

The rest of this section is the mechanism under each.

### 2.1 The port / connection graph

#### Port model

**[v1]** *Confirmed* from
[`inc/dsp/port_identifier.h`](https://github.com/zrythm/zrythm/blob/v1.0.0/inc/dsp/port_identifier.h):

```c
enum PortFlow { FLOW_UNKNOWN, FLOW_INPUT, FLOW_OUTPUT };
enum PortType { TYPE_UNKNOWN, TYPE_CONTROL, TYPE_AUDIO, TYPE_EVENT, TYPE_CV };
enum PortOwnerType { AUDIO_ENGINE, PLUGIN, TRACK, CHANNEL, FADER, CHANNEL_SEND,
                     TRACK_PROCESSOR, HW, TRANSPORT, MODULATOR_MACRO_PROCESSOR };
struct PortIdentifier {
  int schema_version; char *label, *sym, *uri, *comment;
  PortOwnerType owner_type; PortType type; PortFlow flow;
  PortFlags flags; PortFlags2 flags2; PortUnit unit;
  PluginIdentifier plugin_id; char *port_group, *ext_port_id;
  unsigned int track_name_hash; int port_index;
};
```

The critical detail: **v1 identifies ports by structural equality of a
16-field struct**, via `port_identifier_is_equal()` /
`port_identifier_get_hash()`. And the semantic role of every port lives in
**two 31-bit flag words that were both essentially full** — roughly 60 flags
including `PORT_FLAG_CHANNEL_FADER`, `PORT_FLAG2_FADER_SOLO`,
`PORT_FLAG2_CHANNEL_SEND_AMOUNT`, `PORT_FLAG_BPM`,
`PORT_FLAG2_TRANSPORT_ROLL`, `PORT_FLAG_MODULATOR_MACRO`,
`PORT_FLAG_LOGARITHMIC`, `PORT_FLAG_SIDECHAIN`, `PORT_FLAG_PIANO_ROLL`…
**This is the clearest structural dead-end in v1: the flag space was
exhausted.**

The connection rule, from
[manual: Ports](http://manual.zrythm.org/en/routing/ports.html): *"Ports can
be connected with each other, as long as they are of the same type and
opposite direction, with the exception of CV ports which may be routed to
both CV ports and control ports."* Signals from multiple sources sum
additively into the destination.

**[v2]** *Confirmed* from
[`src/dsp/port_fwd.h`](https://github.com/zrythm/zrythm/blob/master/src/dsp/port_fwd.h)
and [`port.h`](https://github.com/zrythm/zrythm/blob/master/src/dsp/port.h):

```cpp
enum class PortFlow : uint8_t { Input, Output };
enum class PortType : uint8_t { Audio, Midi, CV };   // Control is GONE
using PortVariant = std::variant<MidiPort, AudioPort, CVPort>;
class Port : public utils::UuidIdentifiableObject<Port>,
             public dsp::graph::IProcessable;
```

Three changes worth stealing wholesale:

1. **`PortType::Control` was deleted.** Control ports became
   `dsp::ProcessorParameter` — a separate first-class type with a range,
   label, automatable flag, base value, automation provider and modulation
   input. **This alone eliminated ~25 of the 60 v1 port flags.** *A control
   parameter is not a port; conflating them is what filled the flag space.*
2. **Ports are UUID-identified** and live in a central
   `utils::ObjectRegistry` (flat UUID→object map, QObject parent ownership
   plus refcounting, `acquire_reference()` / `release_reference()`),
   referenced via `TypedUuidReference<T>`. This is what makes two-phase
   deserialization and safe undo-across-delete possible.
3. **The owner-type enum is gone.** The owner injects a
   `std::function<Utf8String(const Port&)> full_designation_provider_`
   callback instead of the port knowing what owns it. Inversion of the
   identity dependency.

`PortConnection` [v2]
([docs](https://docs.zrythm.org/classzrythm_1_1dsp_1_1PortConnection.html))
is a small QObject: `src_id_`, `dest_id_` (UUIDs), `multiplier_` (0–1, the
"amount" slider in the Connections tab), `locked_` (user cannot edit or
remove — used for structural connections), `enabled_`, `bipolar_` (for
CV→CV: whether modulation is centred on the base value or added to it), and
`std::optional<pair<uint8_t,uint8_t>> source_ch_to_destination_ch_mapping_`
for explicit audio channel mapping. `PortConnectionsManager` keeps the
vector plus two `unordered_map<PortUuid, vector<PortConnection*>>`
(`src_ht_`, `dest_ht_`) regenerated on change, and is documented
**main-thread-only for mutation**.

#### Graph nodes: tag-union → interface

**[v1]** `GraphNode` is a fat struct with a `GraphNodeType` tag and one
pointer field per possible payload (`Port*`, `Plugin*`, `Fader*`, `Track*`,
`SampleProcessor*`, `HardwareProcessor*`, `ModulatorMacroProcessor*`,
`ChannelSend*`, …), and `graph_node_process()` is a ~10-arm `switch`. **That
switch is duplicated at six sites in `graph_node.c`.** *Confirmed.*

**[v2]** replaced it with one interface — **this is the highest-leverage
single change in the whole rewrite**:

```cpp
class IProcessable {
  virtual Utf8String get_node_name() const = 0;
  virtual units::sample_u32_t get_single_playback_latency() const;
  void prepare_for_processing(const GraphNode*, sample_rate_t, sample_u32_t);
  virtual void process_block(ProcessBlockInfo, const ITransport&, const TempoMap&)
      noexcept [[clang::nonblocking]];
  virtual void release_resources();
};
```

Everything is an `IProcessable`: `Port`, and `ProcessorBase` — which
subsumes `TrackProcessor`, `Fader`, `ChannelSend`, `Plugin`,
`ModulatorMacroProcessor`, passthrough processors, `Metronome`,
`MidiPanicProcessor`, `AudioInputProcessor`, `MidiInputProcessor`,
`AudioSampleProcessor`, `PortObserver`, plus a dummy `InitialProcessor` that
seeds the cycle. **A plugin is not a special kind of graph node; it is the
same kind of node a fader is.**

`ProcessorBase`
([`src/dsp/processor_base.h`](https://github.com/zrythm/zrythm/blob/master/src/dsp/processor_base.h)):

```cpp
class ProcessorBase : public dsp::graph::IProcessable {
  std::vector<dsp::PortUuidReference>               input_ports_;
  std::vector<dsp::PortUuidReference>               output_ports_;
  std::vector<dsp::ProcessorParameterUuidReference> params_;
  void process_block(...) final;            // params → clear outputs → custom_process_block
  virtual void custom_process_block(...);   // default: passthrough to same-type ports
};
```

Because every processor declares its ports and params uniformly, graph
construction is generic: `ProcessorGraphBuilder::add_nodes/add_connections`
wires *any* `ProcessorBase` — every input port node → processor node → every
output port node. **Adding a new kind of processor requires zero graph
code.**

#### Graph compilation, validation, pruning

Layered builders [v2]:

- `ProcessorGraphBuilder` — generic per-processor node/edge insertion.
- `ChannelSubgraphBuilder` — the strip: MIDI FX → instrument → inserts →
  prefader passthrough → `Fader` → postfader passthrough, with pre/post-fader
  sends branching off, handling 1:1 / 1:N / N:1 / N:N audio fan-out
  explicitly.
- `ProjectGraphBuilder` — top level: metronome, monitor fader,
  `InitialProcessor`, MIDI panic, audio/MIDI input processors, per-track
  `TrackProcessor` + channel, modulator plugins + macros, `PortObserver`
  nodes for meters/scopes; then routes each track to its route target or
  master; then **replays every user connection from
  `PortConnectionsManager`**.

That last step is the whole "anywhere-to-anywhere" feature: **user
connections are just data replayed on top of a structurally-generated
graph.**

**Cycle detection** is a non-destructive Kahn's algorithm in
`Graph::is_valid()` over a local `unordered_map<GraphNode*,int>` seeded from
`init_refcount_`. `ProjectGraphBuilder::can_ports_be_connected()` **builds a
throwaway full project graph, adds the candidate edge, runs `is_valid()`**.
Same trick in v1 (`graph_validate_with_connection()`). Correct, and
**O(whole project) per candidate connection** — see §7.

**Pruning** [v2 only]: `GraphPruner::prune_graph_to_terminals()`
backward-traverses from terminal nodes and deletes everything not upstream.
v1 had a `drop_unnecessary_ports` boolean threaded through `graph_setup()`.
Note the deliberate exception in `ProjectGraphBuilder`: every non-master
track output is *always* wired to master even when it would be pruned, with
the comment *"1. track faders are checked for solo/mute/listen status
regardless of whether they are actually part of the final pruned graph; 2.
outputs are used in meters."*

**Rebuild policy [both]: the entire project graph is rebuilt on every
connection/track/plugin change.** v1: `router_recalc_graph(bool soft)`; v2:
`DspGraphDispatcher::recalc_graph(bool soft)`. Soft = latency update only.
Non-soft = new `Graph`, `build_graph()`, prune,
`scheduler_->rechain_from_node_collection(graph.steal_nodes(), ...)` under an
engine lock, with the old node set swapped out atomically (v1's
`graph_rechain()`). **There is no incremental graph edit path.**

#### Threading

Both versions implement the **Robin Gareus / Paul Davis / David Robillard
scheduler from Ardour** — the v2 files carry their copyright headers.
*Confirmed* from
[`graph_scheduler.h`](https://github.com/zrythm/zrythm/blob/master/src/dsp/graph_scheduler.h)
/ [`graph_thread.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/graph_thread.cpp):

```cpp
class GraphScheduler {
  static constexpr int MAX_GRAPH_THREADS = 128;
  moodycamel::LightweightSemaphore callback_start_sem_{0}, callback_done_sem_{0}, trigger_sem_{0};
  MPMCQueue<GraphNode*> trigger_queue_;
  std::atomic<int> idle_thread_cnt_, terminal_refcnt_;
  std::optional<juce::Thread::RealtimeOptions> realtime_thread_options_;
  std::optional<juce::AudioWorkgroup> thread_workgroup_;   // macOS
};
```

Mechanics:

- `GraphNode` carries `std::atomic<int> refcount_` (incoming edges
  remaining), `init_refcount_`, `parentnodes_` / `childnodes_`,
  `playback_latency_`, `capture_latency_`, `route_playback_latency_`,
  `bypass_`.
- Cycle start: nodes with no incoming edges (`trigger_nodes_`) are pushed
  into the MPMC queue; `callback_start_sem_.signal()`.
- Worker loop: pop a node → `trigger_sem_.signal()` (wake at most as many
  threads as there is work) → process → for each child,
  `refcount_.fetch_sub(1) == 1` → reset to `init_refcount_` and push.
  **Not work-stealing** — a shared MPMC queue with a counting semaphore.
- Terminal nodes decrement `terminal_refcnt_`; the last one signals
  `callback_done_sem_`, spins until all threads idle, waits on
  `callback_start_sem_`, re-seeds.
- Audio callback side is just: store `time_nfo_`,
  `remaining_preroll_frames_`, transport + tempo-map refs;
  `callback_start_sem_.signal(); callback_done_sem_.wait();`
- Thread count `= ZRYTHM_DSP_THREADS` env, default `num_cores - 2` ("1 core
  for the OS, 1 for the main thread"). RT priority 9 via
  `juce::Thread::RealtimeOptions`, falling back to a normal thread if
  `startRealtimeThread()` fails. 128 kB stacks (512 kB Apple). macOS
  `AudioWorkgroup` joined per thread.
- v2 annotates the RT path with `[[clang::nonblocking]]` /
  `[[clang::blocking]]` and **runs realtime-sanitizer in CI**
  (`__has_feature(realtime_sanitizer)` guards in `graph_thread.cpp`).
  **That is a genuinely modern practice worth copying.**

There is a live FIXME in `trigger_node()`: *"is the code below correct?
seems like it would cause data races since we are increasing the size but the
pointer might not be pushed in the queue yet?"*

**Latency compensation [both]** follows Ardour, and cites it in a comment:
*"See Page 116 of 'The Ardour DAW — Latency Compensation and
Anywhere-to-Anywhere Signal Routing Systems'."* `set_route_playback_latency()`
walks backwards accumulating `route_playback_latency_`;
`get_max_route_playback_latency()` over trigger nodes gives the project
preroll; `compensate_latency()` shifts `ProcessBlockInfo` by
`route_playback_latency_ - remaining_preroll_frames`. **`capture_latency_`
exists but is `// TODO` in v2 — capture-side compensation is not
implemented.**

`GraphNode::process_chunks_after_splitting_at_loop_points()` splits a block
at transport loop boundaries, so `process_block` may run several times per
audio callback. This matters for §2.2.

**Debug affordance worth stealing:** the routing graph is exportable to
Graphviz `.dot` and PNG from the hamburger menu
([manual](http://manual.zrythm.org/en/exporting/routing-graph.html)). Being
able to `dot -Tsvg` your DSP graph is enormously valuable and almost free
once the graph is a real data structure.

### 2.2 Automation

#### Data model

**[both]** Automation is a **track-lane per parameter**, containing
**regions/clips**, containing **automation points**. Per
[manual: Automation](http://manual.zrythm.org/en/tracks/automation.html), you
create the region first, then points inside it in the Automation Editor.

**[v2]** *confirmed*
([`automation_track.h`](https://github.com/zrythm/zrythm/blob/master/src/structure/tracks/automation_track.h)):

```cpp
class AutomationTrack : public QObject,
                        public arrangement::ArrangerObjectOwner<arrangement::AutomationClip> {
  enum class AutomationMode : uint8_t { Read, Record, Off };
  enum class AutomationRecordMode : uint8_t { Touch, Latch };
  static constexpr int AUTOMATION_RECORDING_TOUCH_REL_MS = 800;
  dsp::ProcessorParameterUuidReference param_id_;
  std::atomic<AutomationMode> automation_mode_{Read};
  QObjectUniquePtr<AutomationTimelineDataProvider> automation_data_provider_;
  QObjectUniquePtr<utils::PlaybackCacheScheduler> automation_cache_request_debouncer_;
};
```

**This is the mechanism that makes "automate anything" true, and it is one
line of code:**

```cpp
generate_automation_tracks_for_processor(processor)  // walks processor.get_parameters(),
                                                     // skips !param->automatable()
```

Because *every* processor (fader, send, track processor, plugin, modulator
macro, metronome) exposes `std::vector<ProcessorParameterUuidReference>
params_` through the same base class, automation lanes materialise for all of
them by enumeration. **Nobody writes "add automation support for the
fader."** Automating BPM and time signature falls out because in v1 they were
ports with `PORT_FLAG_BPM` / `PORT_FLAG2_BEATS_PER_BAR`, and in v2 tempo
became a first-class `TempoMap` plus `TempoObject` / `TimeSignatureObject`
arranger objects with their own undo commands.

Automation lanes are created lazily and cheaply: `AutomationTracklist` holds
`AutomationTrackHolder`s for *all* automatable parameters and only marks them
visible on demand (`showNextAvailableAutomationTrack()`,
`get_first_invisible_automation_track_holder()`). The manual's "+ assigns the
next available automatable parameter" is exactly this.

#### Curves

*Confirmed*, identical in both versions
([`src/dsp/curve.h`](https://github.com/zrythm/zrythm/blob/master/src/dsp/curve.h)):

```cpp
enum class Algorithm { Exponent, SuperEllipse, Vital, Pulse, Logarithmic };
static constexpr double SUPERELLIPSE_CURVINESS_BOUND = 0.82;
static constexpr double EXPONENT_CURVINESS_BOUND     = 0.95;
static constexpr double VITAL_CURVINESS_BOUND        = 1.00;
double curviness_;  // -1..1, negative tilts down, 0 = linear
```

- Exponent: `y = x^n`
- SuperEllipse: `y = 1 - (1 - x^n)^(1/n)`
- Vital: `(e^(nx) - 1)/(e^n - 1)`, `-10 ≤ n ≤ 10` — *"Taken from Vital
  synth"*
- Logarithmic: `a = log(n); b = 1/(log(1 + 1/n)); y = (log(x+n) - a)*b` —
  *"Taken from Ardour"*
- Pulse: square/step

Header comment in both: *"Can find more at tracktion_AudioFadeCurve.h."* The
same `CurveOptions` type is reused for **audio region fades** — one curve
type serves automation segments and fade-in/out.

v2 adds `dsp::evaluate_curve(a, b, algo, curviness, ratio)` documented as
*"the single source of truth for curve evaluation — used by both the clip
renderer (build-time) and the RT reader (playback) to avoid divergence."*
**That comment exists because they had divergence.** Take the lesson for
free.

#### How the value reaches the parameter — and how sample-accurate it is

**This is where the marketing and the code diverge, and it is the most
useful specific finding in this document.**

**[v1]** *confirmed*, `src/dsp/port.c`, `port_process()`,
`case TYPE_CONTROL`: builds **one** `Position` from
`time_nfo.g_start_frame_w_offset`, finds the AP before it, and calls
`control_port_set_val_from_normalized()`. Then CV modulation loops
`port->srcs` reading **`src_port->buf[0]` — sample 0 only**, with
`depth_range = (max - min)/2`.

**[v2]** *confirmed*,
[`src/dsp/parameter.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/parameter.cpp):

```cpp
float current_val = base_value_.load();
if (during_gesture_.load()) { return; }               // user is dragging: automation+mod suspended
if (automation_value_provider_) {
  const auto val = std::invoke(*automation_value_provider_, time_nfo.transport_position_);
  if (val) { current_val = std::clamp(*val, 0.f, 1.f); last_automated_value_.store(current_val); }
}
for (const auto &[src_port, conn] : modulation_input_->port_sources()) {
  auto m = src_port->buf_[time_nfo.buffer_offset_.in(units::samples)];
  if (conn->bipolar_) m = m * 2.f - 1.f;
  current_val = std::clamp<float>(current_val + m * conn->multiplier_, 0.f, 1.f);
}
last_modulated_value_.store(current_val);
```

**One automation lookup and one CV sample per `process_block` call.
Automation and CV modulation are block-accurate, not sample-accurate, in
both v1 and v2** — while the front page markets "Limitless Automation" and
the dev docs claim "sample-accurate automation".

Three mitigating facts:

1. `GraphNode` splits the block at loop points, so a cycle can yield several
   `process_block` calls.
2. The `AutomationValueProvider` typedef's own doc says the design *"allows
   for flexibility … (e.g. call once to get the automation value for the
   entire cycle for efficiency, or call for each sample position for
   sample-accurate automation)"*. **The hook is there; the caller doesn't
   use it.**
3. A genuinely per-sample API **exists and is unused by the parameter
   path**: `AutomationTimelineDataProvider::process_automation_events(const
   ProcessBlockInfo&, PlayState, std::span<float> output_values)`, and
   [`doc/dev/playback_cache_architecture.md`](https://github.com/zrythm/zrythm/blob/master/doc/dev/playback_cache_architecture.md)
   describes the renderer as producing *"sample-accurate automation."*

The RT read (`AutomationTrack::get_normalized_value`) finds the AP before the
position and the next AP, computes
`ratio = (played - ap_pos)/(next_ap_pos - ap_pos)` **in ticks**, and calls
`evaluate_curve(...)`.

Orchestration is in `ProcessorBase::process_block()` (`final`, not
overridable): run **all** parameters first with
`change_tracker_.record_if_changed(i, param)`, clear all output buffers, then
`custom_process_block()`.
[`doc/dev/parameter_infrastructure.md`](https://github.com/zrythm/zrythm/blob/master/doc/dev/parameter_infrastructure.md)
documents the three-stage pipeline (base → automation override → modulation
offset) and the bidirectional plugin sync: a `-1.f` sentinel feedback guard
and a ~20 ms `QTimer` flush from audio→main thread (`flush_plugin_values()`
exchanges `pending_value` atomics).

The `during_gesture_` short-circuit is a small, elegant detail: **while the
user is dragging a knob, automation and modulation are suspended entirely for
that parameter.** Two lines; makes knob-dragging feel correct without
special-casing anywhere. Steal it.

#### The playback cache layer [v2 only — a major new bet]

v2 introduces an entire tier between the arranger model and the RT thread:

- `TimelineDataCache` (abstract) with `AudioTimelineDataCache`,
  `MidiTimelineDataCache`, `AutomationTimelineDataCache`
- `TimelineDataProvider` per type
- `PlaybackCacheActivityTracker` / `PlaybackCacheActivityAggregator` per track
- `utils::PlaybackCacheScheduler` — a **debouncer**:
  `queueCacheRequestForRange(start, end)` / `queueFullCacheRequest()` /
  `queueCacheRequest(ExpandableTickRange)`, coalescing rapid edits into one
  `cacheRequested()` after a grace period
- `ExpandableTickRange` — range union so partial invalidations merge

**The RT thread never walks the arranger object tree.** It reads a
flattened, pre-rendered cache of intervals. Editing a MIDI note enqueues a
debounced range invalidation; the cache is rebuilt on the main thread and
finalized (`finalize_changes()`, `cachedRangesChanged` signal exposed for
debug visualization). This is the "Extensive Caching" marketing bullet, and
it is a real architectural bet, not a micro-optimisation.

*(Not fully verified: whether the automation cache stores per-sample values
or curve segments. The alpha.2 changelog says "segment-based curve evaluation
on the audio thread," suggesting segments.)*

### 2.3 Modulators — and why they are *not* automation

Zrythm's definition is refreshingly narrow
([manual: Modulators](http://manual.zrythm.org/en/modulators/intro.html)):
**a modulator is any plugin with at least one CV output port.** There is no
special "modulator" object type. `PluginDescriptor::isModulator()` is derived
from the plugin's port counts (`num_cv_outs_ > 0`) or its metadata category;
the manual notes *"Zrythm looks inside the plugin's metadata to determine what
type the plugin is"* and falls back to port configuration.

Consequences:

- **Every LFO/envelope/random plugin ever written for LV2/CLAP with a CV
  output is automatically a Zrythm modulator. Zero per-plugin work.**
- The Modulator Track is just a track whose `TrackFeatures::Modulators` bit
  is set; it owns `plugins::PluginGroup modulators_` and
  `std::vector<ModulatorMacroProcessor> modulator_macro_processors_`.
- Routing a modulator = creating a `PortConnection` from a CV output port to
  a parameter's `modulation_input_port`, with `multiplier_` as the depth and
  `bipolar_` deciding centred vs additive.

**Modulators vs automation, precisely:**

| | Automation | Modulator |
|---|---|---|
| Source | `AutomationClip` on a per-parameter lane, positioned on the timeline | CV signal from a plugin's output port, running continuously |
| Reaches the parameter via | `automation_value_provider_` callback → *replaces* the base value | `modulation_input_->port_sources()` → *offsets* the (possibly automated) value |
| Stage in pipeline | stage 2 | stage 3 (applied after automation) |
| Time-bound | yes, has a position and length | no, free-running |
| Requires a lane per target | yes | no — one CV out fans out to N parameters |
| Storage | arranger objects in the project | just a `PortConnection` row |

`ModulatorMacroProcessor`
([docs](https://docs.zrythm.org/classzrythm_1_1dsp_1_1ModulatorMacroProcessor.html))
is the bridge in both directions: **N CV inputs → 1 macro parameter → 1 CV
output**. Because the macro's own scaling amount is a `ProcessorParameter`,
the macro knob is itself automatable *and* modulatable. That is how "route
LFOs into a macro knob, automate the macro, fan out to many parameters" works
with no special code.

**The design lesson:** modulation is not a feature, it is a consequence of
(a) CV being a port type and (b) parameters having a modulation input port.
**Two data-model decisions buy the entire feature.**

### 2.4 Track types — what generalization carries 12–14 of them

This went through **three distinct designs**, and the trajectory is the
lesson.

**[v1] One fat struct + type tag.** *Confirmed*,
[`inc/dsp/track.h`](https://github.com/zrythm/zrythm/blob/v1.0.0/inc/dsp/track.h):
13 `TrackType` values (`INSTRUMENT, AUDIO, MASTER, CHORD, MARKER, TEMPO,
MODULATOR, AUDIO_BUS, AUDIO_GROUP, MIDI, MIDI_BUS, MIDI_GROUP, FOLDER`) and a
single ~350-line `struct Track` holding every field for every kind —
`lanes[]`, `midi_ch`, `drum_mode`, `recording_region`, `chord_regions[]`,
`scales[]`, `markers[]`, `bpm_port`, `beats_per_bar_port`, `modulators[]`,
`modulator_macros[]`, `channel`, `processor`, `automation_tracklist` —
grouped by `/* ==== AUDIO TRACK ==== */` comment banners. Behaviour
dispatched through free predicates: `track_type_has_channel()`,
`track_type_can_have_direct_out()`, `track_type_can_have_region_type()`,
`track_type_is_foldable()`, `track_type_can_record()`,
`track_type_has_piano_roll()`, `track_type_is_fx()`,
`track_type_get_prefader_type()`. The "base classes" existed only as separate
*header files of free functions* (`channel_track.h`, `foldable_track.h`,
`group_target_track.h`).

**[v2, 2024–2025 intermediate] Virtual-inheritance mixins.** *Confirmed at
commit `b9586069a4~1`:*

```cpp
class ProcessableTrack : virtual public Track;
class ChannelTrack     : virtual public ProcessableTrack;
class RecordableTrack  : virtual public ProcessableTrack;
class LanedTrack       : virtual public Track;
template <typename TrackLaneT> class LanedTrackImpl : public LanedTrack;
class PianoRollTrack   : virtual public RecordableTrack, public LanedTrackImpl<MidiLane>;

class AudioTrack final : public QObject, public ChannelTrack,
                         public LanedTrackImpl<AudioLane>, public RecordableTrack,
                         public utils::InitializableObject<AudioTrack> {
  DEFINE_TRACK_QML_PROPERTIES(AudioTrack)
  DEFINE_LANED_TRACK_QML_PROPERTIES(AudioTrack)
  DEFINE_PROCESSABLE_TRACK_QML_PROPERTIES(AudioTrack)
  DEFINE_CHANNEL_TRACK_QML_PROPERTIES(AudioTrack)
  DEFINE_RECORDABLE_TRACK_QML_PROPERTIES(AudioTrack) ... };
```

Plus a `std::variant` per mixin (`ProcessableTrackVariant`,
`ChannelTrackVariant`, `LanedTrackVariant`, `PianoRollTrackVariant`) with
`to_pointer_variant<>` aliases for visitation. This is virtual multiple
inheritance, not CRTP.

**[v2 current] They tore it down.** Commits `b9586069a4` (2025-07-22, *"use
AutomationTracklist directly (drop AutomatableTrack)"*), `f98475f4e3` +
`4492eb7170` (Aug 2025, *"cleanup track API"* — removed
`processable_track.h`, `channel_track.h`, `laned_track.h`), `78f695d170`
(2026-05-04). Current
[`track.h`](https://github.com/zrythm/zrythm/blob/master/src/structure/tracks/track.h):

```cpp
class Track : public utils::UuidIdentifiableObject<Track> {
  enum class Type : uint8_t { Instrument, Audio, Master, Chord, Marker, Modulator,
                              AudioBus, AudioGroup, Midi, MidiBus, MidiGroup, Folder };
protected:
  enum class TrackFeatures : uint8_t {
    Modulators = 1<<0, Automation = 1<<1, Lanes = 1<<2, Recording = 1<<3, PianoRoll = 1<<4 };
  Track(Type, std::optional<PortType> in_signal_type, std::optional<PortType> out_signal_type,
        TrackFeatures enabled_features, BaseTrackDependencies);

  QObjectUniquePtr<AutomationTracklist>   automation_tracklist_;
  QObjectUniquePtr<TrackProcessor>        processor_;
  QObjectUniquePtr<Channel>               channel_;
  QObjectUniquePtr<plugins::PluginGroup>  modulators_;
  std::vector<QObjectUniquePtr<dsp::ModulatorMacroProcessor>> modulator_macro_processors_;
  QObjectUniquePtr<TrackLaneList>         lanes_;
  QObjectUniquePtr<PianoRollTrackMixin>   piano_roll_track_mixin_;
  QObjectUniquePtr<dsp::TimebaseProvider>  timebase_provider_;
  QObjectUniquePtr<PlaybackCacheActivityTracker> playback_cache_activity_tracker_;
};
```

**The generalization that actually carries all 12 types is:** *one class, a
feature bitflag, and a set of `std::optional`-by-nullptr owned sub-objects.*
The 12 concrete subclasses still exist but now derive directly from `Track`
and differ almost entirely in constructor arguments (`Type`, in/out signal
type, feature flags) plus a handful of overrides. Predicates survive as
`static constexpr` / `consteval` members: `type_can_have_clip_type()`,
`type_is_foldable()`, `type_can_have_automation()`, `type_can_have_lanes()`,
`type_has_piano_roll()`, `type_can_be_group_target()`,
`type_has_mono_compat_switch()`, and
`template<typename T> static consteval Type get_type_for_class()`.

Two further points:

- **`TRACK_TYPE_TEMPO` was deleted.** Tempo moved out of the track system
  entirely into `dsp::FixedPpqTempoMap` + `TempoObject` /
  `TimeSignatureObject` arranger objects. **That is the right call: tempo is
  a timeline property, not a track.**
- **Dependency injection replaced globals.** `struct BaseTrackDependencies {
  const dsp::TempoMapWrapper&; utils::IObjectRegistry&;
  SoloedTracksExistGetter; TrackRecordingCallback; }`. v1 reached for
  `TRACKLIST`, `AUDIO_ENGINE`, `PROJECT` macros everywhere.

**The verdict on this arc:** v1's fat struct + tag was crude but shipped a
whole DAW. **The virtual-inheritance mixin design was theoretically clean and
lasted ~1 year before being deleted.** The final design is v1's idea with
real C++ ownership. *A DAW's track type system is closer to a feature matrix
than to an is-a hierarchy, and the code should say so.*

Signal chain in the graph (from `ChannelSubgraphBuilder` +
`ProjectGraphBuilder`):

```
InitialProcessor ─▶ [MidiPanicProcessor] ─▶ TrackProcessor.midi_in
InitialProcessor ─▶ AudioInputProcessor / MidiInputProcessor ─▶ TrackProcessor inputs
TrackProcessor ─▶ MIDI FX ─▶ Instrument ─▶ Inserts ─▶ prefader passthrough
                                                     ├─▶ prefader sends
                                                     └─▶ Fader ─▶ postfader passthrough
                                                                  ├─▶ postfader sends
                                                                  └─▶ route target / Master
Master postfader ─▶ monitor Fader ◀─ Metronome
PortObserver nodes attach downstream of observed ports (meters/scopes)
```

Matching the manual's Track Inspector: 6 pre-fader + 3 post-fader send slots,
MIDI FX → Instrument → Inserts → Fader → Output.

`TrackProcessor` deserves special mention as the *only* place track-type-specific
behaviour lives, and it does so via **injected callbacks rather than
virtuals**:

```cpp
enum class Capabilities { PianoRoll=1<<0, AudioTrack=1<<1, Recording=1<<2 };
enum class ActiveMidiEventProviders { Timeline, ClipLauncher, PianoRoll, Recording, Custom };
enum class ActiveAudioProviders { Timeline, ClipLauncher, Recording, Custom };
enum class MonitorMode { Off, On, Auto };
// injected: FillEventsCallback, TransformMidiInputsFunc,
//           AppendMidiInputsToOutputsFunc  ("Mainly used by ChordTrack to transform input
//                                            MIDI notes into multiple output notes that
//                                            match the chord"),
//           RecordingCallbackRT, EnabledProvider, TrackNameProvider
```

Note that **the chord track's real-time chordifying is a `std::function` hook
on a generic track processor**, not a subclass. Parameters: `mono`,
`input_gain`, `output_gain`, `monitor_audio`, `recording` — all automatable
by the same mechanism as everything else.

---

## 3. Arrangers, regions and the shared editor abstraction

### 3.1 The object model

**[v1]** *Confirmed*,
[`inc/gui/backend/arranger_object.h`](https://github.com/zrythm/zrythm/blob/v1.0.0/inc/gui/backend/arranger_object.h).
C-style "inheritance by struct embedding": one `struct ArrangerObject` is the
first member of `ZRegion`, `MidiNote`, `ChordObject`, `ScaleObject`,
`Marker`, `AutomationPoint`, `Velocity`, and you cast.

```c
enum ArrangerObjectType { NONE, ALL, REGION, MIDI_NOTE, CHORD_OBJECT,
                          SCALE_OBJECT, MARKER, AUTOMATION_POINT, VELOCITY };
enum ArrangerObjectPositionType { START, END, CLIP_START, LOOP_START, LOOP_END, FADE_IN, FADE_OUT };
```

The base carries **seven positions** (`pos`, `end_pos`, `clip_start_pos`,
`loop_start_pos`, `loop_end_pos`, `fade_in_pos`, `fade_out_pos`), two
`CurveOptions` for fades, `muted`, `region_id`, `int magic` (runtime
validation), a `transient`/`main` pair for in-flight drag copies — **and
Cairo/GDK drawing state**: `GdkRectangle full_rect`, `int textw/texth`,
`cairo_t *cached_cr[2]`, `cairo_surface_t *cached_surface[2]`,
`GdkRectangle last_name_rect`, `bool use_cache`. See §7 for why this
mattered.

Capability queries are macros: `arranger_object_type_has_length()`,
`_has_global_pos()`, `_owned_by_region()`.

**[v1] The arranger widget generalization is one widget with a type enum:**

```c
enum ArrangerWidgetType { TIMELINE, MIDI, MIDI_MODIFIER, AUDIO, CHORD, AUTOMATION };
```

A single `ArrangerWidget` GObject (18 kB header) implements selection, drag,
resize, snap, cursor, marquee, zoom and drawing for all six editors by
branching on `type`. **This is why Zrythm could ship five editors as a
one-person project.** It is also why the code is hard to follow.

**[v2]** Real classes + feature flags + concepts, and `Region` was renamed
`Clip` throughout the UI *and project format* (alpha.2, #4368):

```cpp
class ArrangerObject : public utils::UuidIdentifiableObject<ArrangerObject> {
  enum class Type : uint8_t { MidiClip, AudioClip, ChordClip, AutomationClip, MidiControlEvent,
                              MidiNote, ChordObject, ScaleObject, Marker, AutomationPoint,
                              AudioSourceObject, TempoObject, TimeSignatureObject };
protected:
  enum class ArrangerObjectFeatures : uint8_t {
    Bounds = 1<<0, Name = 1<<1, Color = 1<<2, Mute = 1<<3, ClipOwned = 1<<4 };
private:
  QObjectUniquePtr<dsp::Position> position_;                    // always ticks
  QObjectUniquePtr<dsp::ContentPosition> length_;
  QObjectUniquePtr<ArrangerObjectName> name_;
  QObjectUniquePtr<ArrangerObjectColor> color_;
  QObjectUniquePtr<ArrangerObjectMuteFunctionality> mute_;
  QPointer<ArrangerObject> parent_object_;   // "Only one level of parent-child ... supported"
};
```

The feature objects live in `colored_object.h`, `muteable_object.h`,
`named_object.h`, `fadeable_object.h` — **composition, not inheritance**.
Same pattern as `Track`. `Clip` is an intermediate class adding
loop/clip-start/`contentWarp()`; the four clip types derive from it.
`ArrangerObjectOwner<T>` is the container mixin; `ArrangerObjectListModel` is
the `QAbstractListModel` fed to QML.

Categorisation is done with **C++20 concepts** rather than type flags — this
is the nicest single piece of code in the project:

```cpp
concept ClipObject     = AudioClip | MidiClip | AutomationClip | ChordClip;
concept TimelineObject = ClipObject | ScaleObject | Marker | TempoObject | TimeSignatureObject;
concept LaneOwnedObject= MidiClip | AudioClip;
concept FadeableObject = AudioClip;
concept NamedObject    = ClipObject | Marker;
concept BoundedObject  = ClipObject | MidiNote;
concept EditorObject   = MidiControlEvent | MidiNote | AutomationPoint | ChordObject;
using ArrangerObjectVariant = std::variant<MidiNote, ChordObject, ScaleObject, MidiClip,
  AudioClip, ChordClip, AutomationClip, AutomationPoint, Marker, AudioSourceObject,
  TempoObject, TimeSignatureObject, MidiControlEvent>;
```

Changes worth noting: **`Velocity` is no longer an object type** (velocity is
a property of `MidiNote` with a dedicated view); three new types appeared
(`MidiControlEvent` for per-note/CC lanes, `AudioSourceObject`,
`TempoObject`/`TimeSignatureObject`).

**[v2] The arranger generalization moved from a type enum to QML component
inheritance.** `components/arranger/Arranger.qml` is a 56 kB base with ~50
`required property` declarations (`ArrangerObjectCreator objectCreator`,
`ArrangerObjectSelectionOperator selectionOperator`, `UndoStack undoStack`,
`UnifiedProxyModel unifiedObjectsModel`, `ArrangerTool tool`,
`SnapGrid snapGrid`, `TempoMap tempoMap`, `enum CurrentAction`) specialized
by `Timeline.qml`, `MidiArranger.qml`, `AudioArranger.qml`,
`AutomationArranger.qml`, `ChordArranger.qml`, `VelocityArranger.qml`,
`TempoMapArranger.qml`. Objects: `ArrangerObjectBaseView.qml` →
`ClipBaseView.qml` → `{Midi,Audio,Chord,Automation}ClipView.qml`, plus
`MidiNoteView`, `AutomationPointView`, `MarkerView`, `ScaleObjectView`,
`ChordObjectView`, `TempoObjectView`, `TimeSignatureObjectView`,
`VelocityBarView`.

Performance-critical drawing is *not* in QML:
`zrythm::gui::qquick::*CanvasItem` / `*CanvasRenderer` classes
(`ArrangerGridCanvasItem`, `RulerGridCanvasItem`, `MidiClipCanvasItem`,
`AudioClipWaveformCanvasItem`, `AutomationClipCanvasItem`,
`ChordClipCanvasItem`, `WaveformCanvasItem`, `SpectrumAnalyzerCanvasItem`,
`TempoCurveCanvasItem`, `FadeOverlayCanvasItem`, `LiveWaveformCanvasItem`)
are C++ QQuickItems. The item/renderer split is the standard Qt Quick
scene-graph pattern (render thread separate from GUI thread). **That is the
right shape: declarative layout, imperative hot-path rendering.**

**The shared abstraction, stated plainly:** an editor is (a) a horizontal
tick→pixel mapping, (b) an optional vertical mapping (pitch / value / chord
index / none), (c) a filtered view over `ArrangerObject`s, (d) a common
interaction state machine (select / marquee / move / resize / cut / ramp /
audition), (e) a `SnapGrid`. Everything else is object-type-specific painting
and a couple of specialised gestures. **Zrythm proves five editors can share
~90% of that.**

### 3.2 Regions, linking, bounce/freeze, functions

**Region = clip container.** [v1] `RegionType` is a **bitmask**
(`MIDI=1<<0, AUDIO=1<<1, AUTOMATION=1<<2, CHORD=1<<3`) inside a
`RegionIdentifier`, and one `ZRegion` struct holds all four payloads
(`MidiNote **midi_notes` + `unended_notes[12000]`; `int pool_id` +
`AudioClip *clip` + `float gain` + `Position *split_points` +
`RegionMusicalMode musical_mode`; `AutomationPoint **aps` +
`last_recorded_ap`; `ChordObject **chord_objects`) plus universal `name`,
`GdkRGBA color`, and drawing caches. The bitmask is what makes
`track_type_can_have_region_type(track_type, region_type)` a one-line mask
test — nice.

[v2] splits these into `MidiClip` / `AudioClip` / `AutomationClip` /
`ChordClip` under a common `Clip` base holding loop points, clip-start, and
`contentWarp()`.

**Looping.** Loop start/end/clip-start are positions on the base object in
both versions, so "loop audio, MIDI, automation and chord clips" is **one
implementation**. `LoopSegment` / `loop_segment_iterator.h` [v2] iterates
loop repetitions during rendering.

**Linked regions** are handled by an explicitly external registry rather than
by object references:

```cpp
class RegionLinkGroup { int group_idx_; std::vector<Id> ids_;
  void add_region(Region&); void remove_region(Region&, bool autoremove_last, bool update_id);
  bool contains_region(const Region&) const;
  void update(const Region&);  // "Updates all other regions in the link group" };
class RegionLinkGroupManager { ... };
```

Edit propagation is push-based from `update()`. (Note `RegionLinkGroup` still
says `Region`, not `Clip` — leftover from the incomplete rename. And **clip
linking is on the v2 "not yet ported" list**.)

**Stretching.** [v2] `ITimeStretchEngine` + `RubberBandTimeStretchEngine`,
with `StretchOptions::Algorithm` selectable **per clip**
(`AudioClip::stretchAlgorithm`), `sourceBpm` recorded on the clip, and a
`TimeWarpMap` / `WarpPoint` / `WarpAnchor` / `ContentTimeWarp` system for
warping. v1 had `RegionMusicalMode` (off/on/auto) driving whether an audio
region follows tempo. "Stretch any type of region" works because non-audio
regions stretch by scaling tick positions — **trivial once positions are
stored in ticks rather than samples.**

**Positions are stored in ticks, always.** `dsp::Position` [v2] is *"an
observable, serializable musical position stored as ticks"*, with
`TimelinePosition` (absolute) and `ContentPosition` (relative to clip data)
as distinct types. Tick↔sample conversion goes through
`FixedPpqTempoMap::tick_to_samples()` / `samples_to_tick()`, which also
handles constant *and linear-ramp* tempo events and
`tick_to_musical_position()` (bar:beat:sixteenth:tick). **Strongly-typed
units (`units::samples`, `units::ticks` via the `au` library) prevent the
classic samples/ticks mix-up at compile time. Steal the strong typing.**

**Bounce / freeze.** [v1] "Quick Bounce" (previous settings) and "Bounce…"
from track/region context menus
([manual](http://manual.zrythm.org/en/tracks/track-operations.html)), with a
`bounce_step_selector` letting you choose how far down the chain to render
(pre/post-inserts, pre/post-fader). Implementation is offline rendering of
the same graph — **there is no separate "freeze engine."**
**`SampleProcessor`**
([docs](https://docs.zrythm.org/classzrythm_1_1engine_1_1session_1_1SampleProcessor.html))
is *"a processor to be used in the routing graph for playing samples
independent of the timeline; also used for auditioning files"* — it owns its
own private `tracklist_`, `instrument_setting_`, `fader_`, and can
`queue_file()` / `queue_chord_preset()` / `queue_sample_from_file()`. That is
how file-browser auditioning, chord-pad auditioning and metronome-adjacent
playback all reuse the main graph machinery. **Bounce-to-audio is not yet
ported to v2.**

**Region/object functions** are a small, uniform plugin point: `MidiFunction`
(Legato, Invert, Crescendo, Flip, Flam, Portato…), `AudioFunction`
(`AudioFunctionOpts` — normalize, invert, fade, reverse, pitch-shift,
external app), `AutomationFunction` (flip, flatten). "External App
Integration — edit selected audio parts in any external app" is
`AudioFunction` writing a temp file, spawning the editor, and re-importing.
**Cheap, high perceived value.**

---

## 4. Chord/scale — the music-theory layer, and where it lives

**It lives in the model, in three separate places, and that is the
interesting part.**

**1. In `zrythm::dsp` — the theory primitives.** `ChordDescriptor`
(*"describes a musical chord by its root note, type, accent, inversion, and
optional bass note"*), `MusicalScale`, `ChordPitches`, `DiatonicTriad`,
`ChordPreset`, `ChordSuggestion`, `chord_audition_state.h`, `note_type.h`.
These sit alongside `Fader` and `Port` in the DSP layer — i.e. **chord theory
is engine-level data, not UI state.**

**2. In the graph — the chord track transforms MIDI in real time.**
`ChordTrack` supplies a `TransformMidiInputsFunc` /
`AppendMidiInputsToOutputsFunc` to its `TrackProcessor`, documented as
*"Mainly used by ChordTrack to transform input MIDI notes into multiple
output notes that match the chord."* So "Recordable Chord Track" and "live
audition routing" are **a callback on a generic processor**. `ChordClip`
holds `ChordObject`s; `ScaleObject` is a timeline object on the chord track.

**3. In the UI layer — suggestion and highlighting.**
`zrythm::gui::qquick::ChordSuggestionProvider` is a QObject with
`currentChord`, `currentScale`, `includeSevenths`,
`includeSecondaryDominants`, `includeBorrowedChords`, `maxSuggestions`,
`topSuggestions`, and a `scoreForChord()` method — **a scored suggestion
engine, not a lookup table**. `ChordHighlighter` and `ChordRowListModel`
drive piano-roll scale/chord highlighting at the playhead. `ChordPadBank` /
`ChordPadState` / `ChordPresetManager` / `ChordPresetPack` back the 12-pad
chord pad; presets can be **generated from a scale + root** rather than
hand-entered
([manual](http://manual.zrythm.org/en/chords-and-scales/chord-pad.html)).

**Assessment:** the split is right in principle — theory primitives in the
core, suggestion *policy* near the UI — but `ChordSuggestionProvider` living
in `gui::qquick` means the scoring heuristic **cannot be reused headlessly**
(e.g. by a future scripting API or by a "harmonize this region" MIDI
function). *Inference, not confirmed:* I would expect that to move down a
layer eventually.

A known user complaint here: **scale highlighting is applied to the
piano-roll keys, not to the note lanes** (NovusSentient, r/linuxaudio) — a
purely visual gap that repeatedly gets mentioned.

---

## 5. Undo/redo

### 5.1 [v1] — hand-rolled, and the marketing claim was true

*Confirmed*,
[`inc/actions/undoable_action.h`](https://github.com/zrythm/zrythm/blob/v1.0.0/inc/actions/undoable_action.h):

```c
enum UndoableActionType {
  UA_TRACKLIST_SELECTIONS, UA_CHANNEL_SEND, UA_MIXER_SELECTIONS, UA_ARRANGER_SELECTIONS,
  UA_MIDI_MAPPING, UA_PORT_CONNECTION, UA_PORT, UA_RANGE, UA_TRANSPORT, UA_CHORD };
struct UndoableAction {
  UndoableActionType type;
  double frames_per_tick;    // snapshot of AudioEngine.frames_per_tick at execution
  sample_rate_t sample_rate; // to recompute on load at a different rate
  int stack_idx;             // "Used during deserialization"
  int num_actions;           // grouping: set on the LAST action of a group
};
```

Only **ten** action types, because each is *coarse-grained and
selection-shaped*: `ArrangerSelectionsAction` covers
create/delete/move/duplicate/resize/split/merge/quantize/edit for **all**
arranger object types at once. `TracklistSelectionsAction` covers
create/delete/move/copy/pin/fold/route/rename/colour for N tracks. **This is
the key to "almost every user action is undoable" without writing hundreds of
command classes: the undo unit is an operation on a selection, not on an
object.**

Cross-cutting hooks on the base type:

- `undoable_action_needs_pause()` — does the engine need to stop for this?
- `undoable_action_can_contain_clip()` — for audio-pool GC
- `undoable_action_save_or_load_port_connections(self, _do, before, after)` —
  **every action snapshots the whole `PortConnectionsManager` before and
  after.** Brutally simple, and it means any action that incidentally breaks
  a connection is correctly reversible.
- `frames_per_tick` + `sample_rate` snapshots so an undo history survives
  loading the project at a different sample rate.

Serialization: `undo_stack_serialize_to_json()` in
`src/io/serialization/project.c` writes typed arrays
(`"arrangerSelectionsActions"`, `"mixerSelectionsActions"`, …). CHANGELOG:
*"Make undo history serializable and unlimited (off by default)."* **So
"unlimited" is literally `max_size_ == 0` on `UndoStack`, and "serializable"
is a hand-written per-type JSON writer.**

### 5.2 [v2] — `QUndoStack`, and the feature regressed

`zrythm::undo::UndoStack` wraps `QObjectUniquePtr<QUndoStack>`, exposed to QML
(`undoActions`, `redoActions`, `index`, `canUndo`, `canRedo`, `beginMacro`,
`endMacro`, `macroActive`, `ScopedMacro` RAII). Macros are **lazy** —
`beginMacro()` only records the text in `pending_macros_`, realized on the
underlying stack at the first `push()`, so empty macros never pollute the
history. **Nice detail.**

Commands are now **fine-grained** `QUndoCommand` subclasses in
`src/commands/`: `AddArrangerObjectCommand<T>`, `MoveArrangerObjectsCommand`,
`ResizeArrangerObjectsCommand`, `ChangeParameterValueCommand`,
`ChangeQObjectPropertyCommand`,
`ChangeUuidIdentifiableObjectPropertyCommand`, `AddPluginCommand`,
`MovePluginsCommand`, `RemovePluginsCommand`, `DeleteTracksCommand`,
`RouteTrackCommand`, `SetClipLoopPointsCommand`, `RenameTrackCommand`, …
**~23 classes and growing.**

Engine-pause / graph-rebuild policy is a **hard-coded array of command IDs**,
not a virtual:

```cpp
static constexpr std::array<int,6> command_ids_with_graph_pause = {
  AddEmptyTrackCommand::CommandId, DeleteTracksCommand::CommandId,
  AddPluginCommand::CommandId, MovePluginsCommand::CommandId,
  RemovePluginsCommand::CommandId, RouteTrackCommand::CommandId };
```

(both checks recurse into `cmd.child(i)` so macros inherit). **This is a
regression from v1's virtual `needs_pause()` — a new command that needs a
pause will silently not get one.**

**And the marquee feature is gone:**

```cpp
void to_json(nlohmann::json &j, const UndoStack &u) { j = nlohmann::json::object(); /* TODO: serialize undo stack commands */ }
void from_json(const nlohmann::json &j, UndoStack &u) { /* TODO */ }
```

The v2 README lists **"Serializable undo history"** under "not yet ported
from v1." The `"undoHistory"` key in the project JSON is written as `{}`.

[`doc/dev/undo_system.md`](https://github.com/zrythm/zrythm/blob/master/doc/dev/undo_system.md)
states the intended layering — `dsp → model → {Factories, commands} → undo →
{actions, controllers} → gui`, with *"**Actions** produce commands and push to
UndoStack; **Controllers** never produce commands"*, *"one undo-stack per
project"*, *"no root-context properties, no singletons."* **That is a good
rule set.**

**Why v1's undo was unlimited and cheap:** actions store *deltas plus a
before/after port-connection snapshot*, not project snapshots. Combined with
the UUID registry [v2] / name-hash identity [v1], an action can reference
objects that were deleted and recreated. The cost is per-action serializer
code — which is exactly what v2 has not rewritten yet.

---

## 6. Project format, versioning, backups

**Format history [v1]:** Cyaml/YAML → **zstd-compressed JSON via yyjson**,
changed in v1.0.0-beta.5.0.1 (2023-12-13): *"Project format has changed from
zstd-compressed YAML to zstd-compressed JSON (#4763)."* Four days later:
*"Fix copy-paste (regression from YAML → JSON switch) (#4964)."* Legacy Cyaml
schemas were kept under `inc/schemas/**` purely to upgrade old projects.

**Format [v2]:** zstd-compressed JSON via **nlohmann::json** (yyjson→nlohmann
in commit `75502a468a`, 2025-05-12).

**Directory layout [both]**
([manual: Project Structure](http://manual.zrythm.org/en/projects/project-structure.html))
— a project is a *directory*, not a file:

```
MyProject/
├── project.zpj      zstd-compressed JSON
├── backups/         each backup is a COMPLETE standalone project directory
├── exports/
├── plugins/         plugin state + external file refs   [v1]
└── pool/            audio files in use; unused files deleted on save
```

Two decisions worth stealing outright:

1. **"Each backup is a standalone project on its own — ie, it contains all
   the files it needs to load as a separate project."** No incremental or
   differential backup logic, no partial-restore bugs. Disk is cheap.
2. **The pool is garbage-collected on save**, so audio never orphans and
   never silently bloats.

**Versioning [v1]:** `#define PROJECT_FORMAT_MAJOR 1`,
`PROJECT_FORMAT_MINOR 10`, with `PortIdentifier_v1`-style versioned schema
types and real migration code. CHANGELOG entries confirm both the good and
the ugly: *"Upgrade project format to v4 and auto-upgrade older projects"*
**and** *"Upgrade project format and drop undo history when loading older
projects."*

**Versioning [v2]** — better designed, not yet implemented:

```cpp
ProjectJsonSerializer::SCHEMA_VERSION{ .major = 2, .minor = 1 };
DOCUMENT_TYPE = "ZrythmProject";
// { "schemaVersion": {major,minor}, "appVersion", "documentType",
//   "title", "datetime", "projectData": {...}, "uiState": {...}, "undoHistory": {} }

if (v.major > SCHEMA_VERSION.major) throw ZrythmException("Project file is from a newer version...");
if (v.major < SCHEMA_VERSION.major) { z_warning("... migration may be needed");
  // In the future, add migration logic here:
  // auto migrated_json = migrate_to_current_version(j); }
```

Two v2 additions that are genuinely good:

- **JSON Schema validation.** `data/schemas/project.schema.json` is embedded
  via `project_json_schema.in.h` and validated with
  `nlohmann/json-schema.hpp` on load. **A DAW project format with a
  machine-checkable schema is rare and extremely useful** for tooling and for
  regression tests.
- **Two-phase deserialization.** `src/utils/serialization.h` implements
  `variant_create_object_only()` then `variant_deserialize_data()` — *"This
  allows all objects to be registered before any inter-object references are
  resolved."* This is precisely what makes a UUID-reference object graph
  (`TypedUuidReference<T>`, `IObjectRegistry`) loadable without ordering
  constraints. It also provides `nlohmann::adl_serializer` specializations
  for `QString`, `QUuid`, `T*`, `std::variant` (index under `"type"`, fields
  merged; `"nonObjectValue"` for scalars), smart pointers, `StrongTypedef`,
  `boost::concurrent_flat_map`, and `au::Quantity`.

Per-class serializers are ADL `friend void to_json/from_json` with key names
as `static constexpr std::string_view kFooKey = "foo"sv` members, alongside
`BOOST_DESCRIBE_CLASS` reflection. There is **no** `ISerializable` base
interface (one briefly existed, dropped in `fbecc70ad3`, 2025-04-06).

**Plugin state [v2] changed materially:** *"All plugin types serialize their
state as base64-encoded data within the project JSON file… There are no
separate state files on disk — JSON serialization is the sole persistence
mechanism."* One file, no dangling state directories — but **a project with
40 plugin instances becomes a large base64-laden JSON blob**. That is a real
trade-off; *inference:* expect load-time and memory pressure problems at
scale, and it makes partial/lazy loading impossible.

**Compression** is plain zstd on the whole JSON document; `.zpj` is
inspectable with `zstd -d`. Good for support and for user trust.

---

## 7. Plugin hosting

**[v1]:** native LV2 host (lilv/suil) *plus* Carla for everything else —
then consolidated. By v1.0.0-rc.1 (2024-04-26) the CHANGELOG records *"Remove
lilv dependency (at runtime)"* and *"Use new Carla discovery API for scanning
plugins."* So the marketed "LV2, VST2, VST3, AU, CLAP, JSFX via Carla" is
accurate: **Carla is the abstraction**, and Zrythm's own abstraction sits on
top of Carla's.

**[v2]:** Carla is **removed** (listed under Removed dependencies in
alpha.1). New stack:

```cpp
class Plugin : public utils::UuidIdentifiableObject<Plugin>, public dsp::ProcessorBase;
using PluginVariant = std::variant<JucePlugin, ClapPlugin, FaustPlugin>;
```

- **VST3 / LV2 / LADSPA / AU → JUCE** (`juce_plugin.cpp`)
- **CLAP → hosted natively** (`clap_plugin.cpp`, `CLAPPluginFormat.cpp`,
  `ClapHostBase`) — they did not settle for JUCE's CLAP support
- **Bundled effects → Faust internal plugins** (`src/plugins/faust/`,
  `InternalPluginBase`); alpha.2: *"Reimplement bundled Faust plugins as
  internal plugins instead of generated VST3/CLAP bundles"*
- `CarlaNativePlugin` still exists in the tree, forward-declared, but is
  **not in `PluginVariant`** — dead code
- **VST2, DSSI, SFZ, SF2 are dropped/not-yet-ported** in v2 — a real feature
  loss

**The abstraction that makes formats look alike is two things:**

1. **`Plugin : public ProcessorBase`.** A plugin declares `input_ports_`,
   `output_ports_`, `params_` like any other processor and is scheduled by
   the same graph. Pure-virtual hooks are minimal:
   `process_impl(ProcessBlockInfo)`, `save_state_impl()`,
   `load_state_impl(base64)`, `prepare_plugin_for_processing()`,
   `release_resources_impl()`, `process_passthrough_impl()` (used when
   bypassed), `hasNativeUi()`. Zrythm *always* adds two synthetic parameters
   — `bypassParameter()` (auto-generated if the plugin lacks one) and
   `gainParameter()` — which is exactly why **"Automatable Bypass Mode" is a
   marketed feature: it is a `ProcessorParameter`, therefore automatable,
   therefore free.**
2. **`PluginDescriptor` as a format-neutral metadata record.**
   `from_juce_description()` / `to_juce_description()` bridge in and out;
   fields include `PluginCategory` (38 values),
   `PluginArchitecture{32,64}`, `BridgeMode{None, UI, Full}`,
   `num_audio_ins_/outs_`, `num_midi_ins_/outs_`, `num_ctrl_ins_/outs_`,
   **`num_cv_ins_/outs_`**, `has_custom_ui_`, and
   `std::variant<std::filesystem::path, Utf8String> path_or_id_`. Also
   `serializeToString()` *"intended to be used in the UI for eg, drag n drop
   as a MIME type"* — plugin drag-and-drop is a string round-trip, no object
   lifetime problem.

**The `num_cv_outs_` field is the hinge for the modulator feature:**
`isModulator()` is derived from it.

**Scanning is out-of-process** [v2]: `OutOfProcessPluginScanner` plus a
`plugin_scanner_subprocess` binary, linking `juce_audio_processors_headless`.
There is also a **separate `engine-process/` executable** with
`ipc_message.h` — an out-of-process audio engine, entirely new in v2 with no
v1 analogue. *Inference:* this is aimed at surviving plugin crashes, and it
is the right long-term shape.

**Slots** [v2]: v1's `PluginSlotType` / `MixerSelections` model is gone.
`Channel` owns three `plugins::PluginGroup`s — `midi_fx_`, `instruments_`,
`inserts_`
([`doc/dev/plugin_group_architecture.md`](https://github.com/zrythm/zrythm/blob/master/doc/dev/plugin_group_architecture.md)).
The old `is_plugin_descriptor_valid_for_slot_type()` is `#if 0`'d with a TODO
— i.e. **slot-type validation is currently missing.**

**Plugin bridging/sandboxing** was a v1 feature (via Carla) and is **not
ported to v2**. This is a meaningful regression: one forum report attributes
a 90 %-CPU pathology directly to always-on Carla bridging
([forum t/136](https://forum.zrythm.org/t/136)), so the story is nuanced —
sandboxing cost a lot, and removing it cost robustness.

---

## 8. What Zrythm gets structurally right — ranked by leverage

**#1 — One `IProcessable` interface, and *everything* is a node in *one*
graph.** Faders, sends, track processors, plugins, macro processors,
metronome, MIDI panic, hardware I/O, and even meters (`PortObserver`) are the
same kind of thing. There is no "mixer engine" separate from a "plugin
engine" separate from a "track engine." Consequences that fall out for free:
latency compensation is uniform; parallel scheduling is uniform; graph
export/debugging covers everything; a new processor type needs zero graph
code; sidechaining is not a special case; the monitor section is just more
nodes. **This is the single highest-leverage decision in the codebase, and
v1's failure to make it (the 10-arm `switch`) is what made v1 hard to
extend.**

**#2 — Parameters as a first-class type distinct from ports, with a
three-stage value pipeline.** `base value → automation override → modulation
offset`, evaluated in `ProcessorBase::process_block()` before
`custom_process_block()`. From this one decision you get: "automate anything"
(enumerate `params_`), "modulate anything" (every param has a modulation
input port), MIDI-learn anything (bind to a param UUID), automatable plugin
bypass, automatable send amount, automatable BPM, macro knobs, and
lazily-materialised automation lanes. **v1 tried to do this with port flags
and ran out of bits. The v2 correction is the most instructive diff in the
project.**

**#3 — CV as a port type, and connections as editable data rows.** A
`PortConnection` is `{src_uuid, dest_uuid, multiplier, enabled, locked,
bipolar, channel_map}` — nothing more. Modular routing, sidechaining, macro
fan-out, LFO→cutoff, and "anywhere-to-anywhere" are all *the same feature*,
and the UI for all of them is one table with a slider per row. The `locked_`
flag is a small stroke of design sense: structural connections and user
connections live in the same list, distinguished by one bit.

**#4 — One arranger abstraction for five editors.** Timeline, piano roll,
audio editor, automation editor, chord editor, velocity editor, and (v2)
tempo-map editor share the tick↔pixel mapping, selection model, drag/resize
state machine, snap grid, and tool set. v1 did it with a `type` enum on one
widget; v2 does it with QML component inheritance over a 56 kB base. Either
way, **a one-person project shipped seven editors. This is the highest
feature-per-line ratio in the whole product.**

**#5 — Positions in ticks, with a real tempo map and strong units.**
`dsp::Position` is always ticks; `FixedPpqTempoMap` owns
tick↔sample↔bar:beat:sixteenth:tick with constant *and* linear-ramp tempo
events; `TimelinePosition` vs `ContentPosition` are distinct types; the `au`
units library makes `units::samples` and `units::ticks` non-interchangeable
at compile time. Tempo automation, region stretching, and tempo-independent
editing all become mechanical rather than heroic. Removing
`TRACK_TYPE_TEMPO` in favour of a `TempoMap` + `TempoObject` arranger objects
was the right call.

**#6 — Coarse-grained, selection-shaped undo actions with a global
port-connection snapshot [v1].** Ten action types covering an entire DAW,
because the undo unit is "an operation on a selection" rather than "an
operation on an object," and because every action snapshots
`PortConnectionsManager` before/after so incidental routing damage is always
reversible. Plus `frames_per_tick` + `sample_rate` on the base action so
history survives a sample-rate change. **This is how you get "almost
everything is undoable" without writing 200 command classes — and v2's move
to fine-grained `QUndoCommand`s is arguably a step backwards on this axis.**

**Honourable mentions:** the playback-cache tier with a debouncing scheduler
and expandable range invalidation; standalone self-contained backups; a GC'd
audio pool; JSON-Schema-validated project files; two-phase deserialization
over a UUID registry; `[[clang::nonblocking]]` + realtime-sanitizer in CI;
the routing graph `.dot` export.

---

## 9. What Zrythm gets wrong or pays too much for

### 9.1 Architectural

**Model/view were fused in v1 — and that is what a toolkit migration
actually costs.** `ArrangerObject` — a *model* type in the project file —
contained `cairo_t *cached_cr[2]`, `cairo_surface_t *cached_surface[2]`,
`GdkRectangle full_rect`, `GdkRectangle last_name_rect`, `int textw/texth`,
`bool use_cache`. `Track` contained GTK height values. Regions contained
`GdkRGBA color`. **You cannot swap toolkits under a model like that; you can
only rewrite. This is the concrete, substantiated reason the v2 "port" became
a full rewrite.** *(This is inference from the diffs — the maintainer's stated
reasons are GTK cross-platform quality and Meson, not model/view coupling.)*

**Identity by structural equality [v1].** `port_identifier_is_equal()` over a
16-field struct, including a `track_name_hash` — meaning **renaming a track
perturbed port identity**. v2's UUID registry is the fix, and it is the
enabler for two-phase deserialization, refcounted lifetimes, and undo across
delete/recreate. *Do not build a DAW on structural identity.*

**Flag-space exhaustion.** Two full 31-bit flag words on `PortIdentifier` is
a smell you should be able to detect years earlier. The lesson generalises:
**when a "flags" field starts encoding *which subsystem owns this and what
role it plays*, you needed a type, not a bit.**

**Full graph rebuild on every connection change [both].** `recalc_graph()`
constructs a brand-new `Graph`, calls `build_graph()`, prunes, and rechains —
for adding one send. Worse, v2's `DspGraphDispatcher::recalc_graph`
**exports the graph to DOT twice for debug logging** on every rebuild. And
`can_ports_be_connected()` builds a throwaway *full project graph* per
candidate connection just to run cycle detection — so a routing UI that greys
out invalid targets is **O(project) per target**. At FL-Studio scale
(hundreds of tracks, thousands of connections) **this is a wall.** An
incremental graph edit + incremental cycle check is the obvious improvement.

**Automation and CV modulation are block-accurate, not sample-accurate —
while the marketing says "limitless automation" and the dev docs say
"sample-accurate automation".** v1 read `src_port->buf[0]`; v2 reads one CV
sample at `buffer_offset_` and does one automation lookup per `process_block`.
The `AutomationValueProvider` signature and
`process_automation_events(..., std::span<float> output_values)` both exist to
support per-sample evaluation; neither is used on the parameter path. **At
512-sample buffers a fast LFO on a filter cutoff will step audibly. If you
are building a DAW that markets modulation, this is the thing to get right
from day one — retrofitting per-sample parameter evaluation into an
already-shipped `process_block` contract is painful.**

**Capture-side latency compensation is `// TODO` in v2.** Playback
compensation is the Ardour algorithm and works; recording through a latent
chain does not compensate.

**The v2 undo system is a regression on three axes.**
(a) Persistence is a stub (`/* TODO: serialize undo stack commands */`), so
the marketed "Serializable Undo History" does not exist in v2.
(b) Engine-pause and graph-rebuild requirements moved from a virtual method
(`needs_pause()`) to a **hard-coded `std::array` of six command IDs**. A new
command that mutates the graph and forgets to add itself to that array will
corrupt the running graph, silently. **That is a latent-bug factory.**
(c) Fine-grained commands (23 and counting) vs v1's 10 coarse ones means more
code and more serializers to write when persistence finally lands.

**Two undo systems and two UI toolkits coexist in the v2 tree today.**
`src/gui/backend/backend/legacy_actions/` (`UndoableAction`, `UndoStack`,
`UndoManager`, ten action types) sits next to `src/commands/` +
`src/undo/undo_stack.h` (`QUndoCommand`). And `src/gui/backend/gtk_widgets/`
still contains ~150 GTK widget headers (`arranger.h`, `midi_arranger.h`,
`mixer.h`, `plugin_gtk.h`, `knob.h`, `channel_send.h`,
`port_connections_tree.h`…) alongside `src/gui/qml/` and
`src/gui/backend/qquick/`. **Half-migrated systems are the most expensive
state a codebase can be in, and this one has been in it since 2024.**

**Incomplete renames leak into the project format.** `Region → Clip` was done
"throughout the UI and project format" in alpha.2, but `RegionLinkGroup`,
`RegionLinkGroupManager`, `ClipRenderer`/`RegionRenderer` and various APIs
still say Region. Format-level renames with in-flight code renames is a bad
combination.

**Base64 plugin state inside the project JSON [v2].** *"There are no separate
state files on disk — JSON serialization is the sole persistence
mechanism."* Simple and atomic; but a 40-plugin project becomes one huge JSON
document that must be fully parsed, base64-decoded and schema-validated
before anything appears. No lazy loading, no partial recovery, and
diffing/version-controlling projects becomes useless. *Inference: this will
need to be revisited.*

**No migration path implemented in v2** (`// In the future, add migration
logic here`). v1's history includes *"Upgrade project format and drop undo
history when loading older projects"* — i.e. they have already once shipped a
migration that threw user data away.

**Scripting was removed, not replaced.** Guile → deprecated/disabled
([manual: Scripting](http://manual.zrythm.org/en/scripting/overview.html):
*"Zrythm used to offer scripting capabilities using GNU Guile. This
functionality has been disabled and is considered deprecated."*), "likely
libpeas-based" future, no timeline. Combined with no controller API, **Zrythm
currently has zero extensibility surface beyond plugins.** For a DAW
positioning against Bitwig — whose controller scripting API is a major reason
hardware users choose it — this is a strategic hole, not a technical one.
kephir on r/Bitwig: *"one thing that's stopping me from using it right now
(apart from stability concerns) is current lack of any kind of controller
API… I've come to view my launchpad as absolutely indispensable."*

**`ChordSuggestionProvider` lives in `gui::qquick`.** The scoring engine
cannot be reused headlessly.

**X11 is required, even in v2**, because JUCE-managed windows (device
manager, plugin UIs) need it: *"Zrythm requires X11 (either on its own or via
Xwayland)"* ([forum t/439](https://forum.zrythm.org/t/439)). **A 2026
Linux-first DAW that cannot run on a pure-Wayland session is a problem**, and
it is inherited from the plugin-hosting layer.

**A live threading FIXME in the RT scheduler:** *"is the code below correct?
seems like it would cause data races since we are increasing the size but the
pointer might not be pushed in the queue yet?"* in `trigger_node()`. And
`graph_dispatcher.cpp` still carries a v1-era note: *"FIXME!!!! threading bug
here. clip editor clip & track may be changed in the main (UI) thread while
this is attempted."*

### 9.2 Substantiated product-level problems

**Crashes are the dominant user theme, 2020 → 2026.** Not marginal:

- *"Sadly it's entirely unusable on any OS I've tried (I've tried 3). … It's
  the crashing, really."* — UsefulLanguage, a paying supporter, r/Bitwig
- *"unstable as hell! if you're lucky you can hit play without a message
  saying that 'zrythm found an error', if you're unlucky the program
  crashes."* — [AlternativeTo, Jan 2025](https://alternativeto.net/software/zrythm/about/)
- The project's own forum has a thread titled **"Which plugins are working
  with the least amount of crashes in v1?"** —
  [forum t/330](https://forum.zrythm.org/t/330)
- BPB on the v2 alpha (macOS, Aug 2026): *"specific actions would always
  crash the DAW"*, *"plenty of seemingly random crashes"* —
  [bedroomproducersblog](https://bedroomproducersblog.com/2026/08/06/zrythm-2-alpha/)
- The maintainer's counter, in r/Bitwig: *"everyone keeps saying that it
  crashes a lot but I rarely see any crash reports being reported."* **Both
  things are true, and that gap is itself a product problem: if your users
  are hitting crashes and not reporting them, the reporting path is the
  bug.**

**GTK4 UI performance was genuinely bad, and the "hardware accelerated UI"
bullet did not survive contact with users.**

- Sean_Ramey, a 4-year bundle owner, Dec 2025: demo project *"took FOREVER to
  load"* (he assumed a hang); then *"the UI was SOOOO laggy. Like 10 FPS
  laggy… when I had the mixing console showing, then the FPS got WAY worse.
  It's practically unusable."* Maintainer's reply: *"Your issues are/will be
  addressed in v2."* — [forum t/500](https://forum.zrythm.org/t/500)
- NovusSentient, r/linuxaudio: *"I had to wait about 3 seconds for any
  changes on the GUI to be made"*, and *"the whole program stuttering when you
  add an MIDI note on piano roll while your sequence being played."*
- Contradicting anecdote from trynsta: GTK3→GTK4 *"makes zrythm much
  faster… 10 Vitals at once on over 10 years old laptop and there is no
  xruns."*
- **There are no published benchmarks. None. From anyone.** No CPU-vs-other-DAW
  numbers, no latency measurements, no large-project stress tests. The
  qualitative consensus is that Qt/QML v2 is smoother than GTK4 v1 on Linux
  and that macOS/Windows remain rough — but nobody has measured anything,
  **including the project.**

**Cross-platform was never real in v1, by the maintainer's own admission:**
*"A lot of people find Zrythm unusable on Mac and Windows — even for basic
operations. I saw you're having issues on Windows — on Mac things are even
worse."* ([forum t/194](https://forum.zrythm.org/t/194)) and *"Do note that
Zrythm v1 is pretty broken on Mac"* ([forum t/469](https://forum.zrythm.org/t/469)).

**Two toolkit migrations in four years, both justified on rendering
performance.** GTK3→GTK4 (Jan 2022) was sold as fixing UI slowness.
GTK4→Qt6 (Oct 2024) was sold as fixing UI slowness and cross-platform. A
user's summary: *"porting from GTK3 to GTK4 is already enough to highlight.
Very effective waste of time and resources."* The stated reasons for the
second migration are worth quoting exactly, because they are a UI-toolkit
decision record you can reuse:

> *"Besides being a pain to build (dependency on Rust for SVG support,
> GNU/Linux-only libraries like gettext and appstream for libadwaita, the
> meson build system itself regularly having bugs on Windows/Mac, etc.)
> GTK4's support on Windows and Mac is poor in my experience… the backtraces
> I see usually point to bugs in GTK."*
> *"The GTK maintainers don't really care about platforms besides GNU/Linux…
> 'if it works it works, and if it doesn't fix it yourself and send
> patches'."*
> *"'Pressure' does nothing when there are no developers. GTK needs actual
> Mac and Windows developers."*
> — [forum t/194](https://forum.zrythm.org/t/194); announcement at
> [forum t/185](https://forum.zrythm.org/t/185), covered by
> [Phoronix](https://www.phoronix.com/news/Zrythm-Abandoning-GTK-For-Qt)

A GTK developer conceded the point on r/gnome: *"nobody thinks it's important
enough to work on it, and that has been the case for the last 15 years, so I
think it's a community-wide consensus that GTK doesn't need to work on
Windows."*

**Bus factor 1, confirmed by numbers and by the author.** gitlab.zrythm.org:
top contributor **1838 commits, #2 has 31** (then translators). GitHub
mirror: `alex-tee` 6798, #2 is a localization contributor at 242. Issues
disabled on the mirror; tracker self-hosted; the project moved sourcehut →
Gitea → GitLab, shedding drive-by contributors each time. His own words:
*"I'm pretty much the only developer/maintainer/customer support/UI/UX
designer of this"* ([HN](https://news.ycombinator.com/item?id=29475570)) and
*"Zrythm is primarily a one-person project"* (v1.0.0 announcement).

The observable consequences: v1 is declared unmaintained while v2 is years
from parity; a paying customer requested and received a refund over exactly
that ([forum t/563](https://forum.zrythm.org/t/563)); forum bug reports sit
with zero replies. Mitigations in play: the **NLnet NGI0 Commons Fund grant**
(Apr 2026, first institutional funding —
[forum t/595](https://forum.zrythm.org/t/595),
[nlnet.nl/project/Zrythm](https://nlnet.nl/project/Zrythm/)), and visibly
AI-assisted development (commits like `docs: add UI design guidelines to
AGENTS.md`, a `zrythm-ai-bot` GitLab namespace created July 2026).

### 9.3 Corrections to common assumptions

Two premises the research went in with turned out to be wrong, and they are
worth recording:

- **There was no 2023 relicensing controversy.** The license change was
  GPLv3+ → **AGPLv3+ in v0.5.162, 2019-07-14**. The trademark policy was
  added 2020-07-18 and revised twice. **No 2023 event exists.**
- **"Free software but paid binaries" is not especially controversial.** The
  25-track limit dates from 2022-09-02 (before that: save disabled, before
  that: 30-minute timer). Crucially, **the limit is a build-time CMake flag
  gated behind maintainer mode inside the AGPL source**:
  ```cmake
  cmake_dependent_option(ZRYTHM_IS_TRIAL_VER "Build trial version" OFF "ZRYTHM_MAINTAINER_MODE" OFF)
  if(ZRYTHM_IS_TRIAL_VER)
    set(TRIAL_MAX_TRACKS 25)
  endif()
  ```
  Build from source with defaults → no limit. Arch/Fedora/Guix/Flathub
  packages are unlimited. The bundled Z Series plugins are also AGPL-3.0.
  Reaction was mild — some 2021 HN friction (*"I'm not willing to really give
  you 15$ without any proof this will work for me"*), balanced by *"$15 is
  cheap"* takes; Ardour's Paul Davis treats Zrythm as a peer. **The genuinely
  contested piece is the trademark policy, not the price** — debated in
  [GNU Guix bug #42473](https://issues.guix.gnu.org/42473) and in a long
  LinuxMusicians packaging-ethics thread.
- **Project corruption is not a recurring complaint.** Crashes yes;
  corruption no. One user in 2022: *"the project format is stable, so that's
  good for potential long-term projects."*

### 9.4 Where a modern rewrite would do better

1. **Never let view state into model types.** Rendering caches, rects,
   colours-as-toolkit-types, pixel heights — all of it belongs in a view
   layer keyed by object UUID.
2. **Per-sample (or at least segmented-ramp) parameter evaluation from day
   one.** Define the contract as `process(block, span<param_values>)` rather
   than `process(block)` with a scalar snapshot, and let processors that do
   not care read `[0]`.
3. **Incremental graph editing.** Adding a send should add two nodes and
   three edges, not rebuild the project. Cycle checking should be incremental
   too (maintain a topological order and only repair locally).
4. **One undo model, chosen up front, with persistence designed in.**
   Coarse, selection-shaped actions (v1's insight) *plus* a declarative "what
   does this action affect" descriptor (`needs_pause`, `needs_graph_rebuild`,
   `affects_ports`) as **virtuals, not a hard-coded ID array**.
5. **Separate plugin state from the project document.** Keep state as sidecar
   blobs referenced by UUID so projects stay parseable, diffable and
   partially recoverable.
6. **Ship the extensibility surface early.** A controller/scripting API is
   not a v3 feature; it is how a small team gets hardware support and
   workflow features written by other people.
7. **Publish benchmarks.** Zrythm's biggest credibility problem is that
   "optimized performance" is a marketing bullet with zero numbers behind it
   while users post 10-FPS reports. **A DAW that publishes reproducible
   CPU/latency/large-project numbers would differentiate on trust alone.**
8. **Decouple plugin windowing from the audio/plugin host** so a
   Wayland-native session is possible.

---

## 10. Steal / improve / skip

| # | Mechanism | Verdict | One-line reason |
|---|---|---|---|
| 1 | `IProcessable` — one interface for every graph node (faders, sends, plugins, meters, metronome, HW I/O) | **Steal** | Kills the tag-union switch; every new processor type costs zero graph code. |
| 2 | `ProcessorBase` with declared `input_ports_` / `output_ports_` / `params_` + generic `ProcessorGraphBuilder` | **Steal** | Graph wiring becomes reflection over a uniform declaration. |
| 3 | Parameters as a distinct type from ports (`ProcessorParameter`, not `PortType::Control`) | **Steal** | This is the correction v2 made after v1's flag space exhausted; skip v1's mistake entirely. |
| 4 | Three-stage value pipeline: base → automation override → modulation offset, evaluated before `custom_process_block()` | **Steal** | Automation, modulation, MIDI-learn and macros all become one code path. |
| 5 | Suspend automation+modulation while `during_gesture_` is true | **Steal** | Two lines; makes knob-dragging feel correct without special-casing anywhere. |
| 6 | CV as a first-class port type; modulator = "any plugin with a CV out" | **Steal** | Turns every third-party LFO into a supported modulator for free. |
| 7 | `PortConnection` = `{src, dest, multiplier, enabled, locked, bipolar, channel_map}` | **Steal** | Routing, sidechain, modulation depth and macro fan-out become one editable table. |
| 8 | `locked_` bit distinguishing structural from user connections in one list | **Steal** | Avoids a second parallel "internal routing" data structure. |
| 9 | `ModulatorMacroProcessor`: N CV in → 1 automatable param → 1 CV out | **Steal** | Macro knobs, automatable modulation depth, and fan-out from one 60-line class. |
| 10 | Generate automation lanes by enumerating `params_`, materialise lazily on demand | **Steal** | "Automate anything" becomes a loop, not a feature backlog. |
| 11 | Five curve algorithms + per-point curviness, shared between automation segments and audio fades | **Steal** | High perceived quality, one `evaluate_curve()` function, reused twice. |
| 12 | `evaluate_curve()` as *"the single source of truth… used by both the clip renderer and the RT reader to avoid divergence"* | **Steal** | The comment exists because they had divergence; take the lesson for free. |
| 13 | Positions always in ticks; `TempoMap` owns tick↔sample↔bar:beat; strong unit types (`units::ticks` vs `units::samples`) | **Steal** | Tempo automation and stretching become mechanical; the compiler catches the classic bug class. |
| 14 | Tempo as a `TempoMap` + arranger objects, **not** a track type | **Steal** | v1 made it a track type and v2 deleted it — don't repeat the round trip. |
| 15 | `ArrangerObject` base with feature bitflags + composed feature objects (name/color/mute/fade) | **Steal** | One selection/drag/resize state machine serves 13 object types. |
| 16 | One arranger abstraction specialised per editor (timeline / piano roll / audio / automation / chord / velocity / tempo) | **Steal** | Highest feature-per-line ratio in the product; five editors from one implementation. |
| 17 | Categorise object types with concepts/traits (`ClipObject`, `BoundedObject`, `NamedObject`, `EditorObject`) rather than runtime flags | **Steal** | Compile-time dispatch, no `if (type == X)` scattered through editors. |
| 18 | UUID object registry + `TypedUuidReference<T>` + refcounted lifetime | **Steal** | Enables two-phase load, undo across delete/recreate, and stable identity across rename. |
| 19 | Two-phase deserialization (create all objects → then resolve references) | **Steal** | The only sane way to load a UUID-reference graph; ~30 lines. |
| 20 | Project as a *directory*; backups are complete standalone projects; audio pool GC'd on save | **Steal** | Eliminates an entire class of partial-restore and orphaned-asset bugs. |
| 21 | JSON Schema for the project format, validated on load | **Steal** | Machine-checkable format = free regression tests + third-party tooling. |
| 22 | Routing graph export to Graphviz `.dot` | **Steal** | Nearly free once the graph is a real structure; enormous debugging value. |
| 23 | `[[clang::nonblocking]]` annotations + realtime-sanitizer in CI | **Steal** | Catches RT-safety violations mechanically instead of via user crash reports. |
| 24 | MIDI bindings as a flat list of (CC pattern, device, target UUID) rows, undoable and serialized | **Steal** | Whole "map any control to any parameter" feature in ~150 lines, because targets are UUIDs. |
| 25 | Auto-inject synthetic `bypass` + `gain` parameters on every plugin | **Steal** | "Automatable bypass" becomes a consequence, not a feature. |
| 26 | `PluginDescriptor` with `num_cv_outs_` and `serializeToString()` for drag-and-drop MIME | **Steal** | CV-out count is what makes modulator classification automatic; string MIME dodges lifetime issues. |
| 27 | Out-of-process plugin scanning + out-of-process audio engine (`engine-process/` + IPC) | **Steal** | The single highest-value robustness investment for a plugin host. |
| 28 | Coarse, selection-shaped undo actions (10 types for a whole DAW) | **Steal** | v1's real insight; v2's 23 fine-grained commands are more code for less coverage. |
| 29 | Undo actions snapshotting the global port-connection set before/after | **Steal** | Makes incidental routing damage automatically reversible; embarrassingly cheap. |
| 30 | `frames_per_tick` + `sample_rate` snapshot on every action | **Steal** | Undo history survives a sample-rate change on reload. |
| 31 | Debounced, range-expanding playback cache scheduler (`ExpandableTickRange`) feeding an RT-read cache | **Steal** | Keeps the RT thread off the object model; the right shape for scale. |
| 32 | `SampleProcessor` — a private tracklist inside the main graph for auditioning/bounce | **Steal** | File preview, chord audition and offline render reuse the real engine. |
| 33 | Region link groups as an external registry with push-based `update()` | **Improve** | Right idea, but push-propagation is fragile; prefer shared-content-by-reference with copy-on-write. |
| 34 | Full graph rebuild + rechain on every connection change | **Improve** | Correct but O(project) per edit; do incremental node/edge insertion with local topological repair. |
| 35 | Cycle check by building a throwaway full graph per candidate connection | **Improve** | Maintain a topological order and answer "would this cycle?" in ~O(affected subgraph). |
| 36 | Block-accurate automation/CV (one lookup + `buf[offset]` per block) | **Improve** | Design the contract as per-sample or segmented ramps from day one; retrofitting is brutal. |
| 37 | Engine-pause / graph-rebuild policy as a hard-coded array of command IDs | **Improve** | Make it a virtual/trait on the command; a forgotten entry is a silent corruption. |
| 38 | Plugin state as base64 inside the project JSON | **Improve** | Keep the single-document *logical* model but store blobs as sidecar files keyed by UUID. |
| 39 | Track types as a fat struct + type tag [v1] → single class + `TrackFeatures` bitflags + optional sub-objects [v2] | **Improve** | Take v2's endpoint directly; skip the virtual-inheritance mixin phase they spent a year building and deleting. |
| 40 | Chord/scale theory primitives in the core (`ChordDescriptor`, `MusicalScale`, `ChordPitches`, `DiatonicTriad`) | **Steal** | Model-level theory makes the chord track, chord pad, highlighting and suggestions one feature family. |
| 41 | Chord-track chordifying as a `std::function` hook on a generic `TrackProcessor` | **Steal** | Real-time MIDI transformation without a `ChordTrack` subclass in the audio path. |
| 42 | `ChordSuggestionProvider` (scored suggestions, sevenths / secondary dominants / borrowed chords) living in `gui::qquick` | **Improve** | Excellent engine, wrong layer — put it in the core so MIDI functions and scripts can use it. |
| 43 | Carla as the universal plugin abstraction [v1] | **Skip** | v2 deleted it; JUCE for VST3/LV2/AU + native CLAP is the better split, and CLAP deserves a native host. |
| 44 | Native LV2 hosting via lilv/suil [v1] | **Skip** | Zrythm itself removed the runtime lilv dependency before 1.0; the maintenance cost isn't worth it. |
| 45 | Two 31-bit flag words on the port identifier [v1] | **Skip** | Textbook case of bits standing in for types. |
| 46 | Structural identity (`port_identifier_is_equal`, `track_name_hash`) [v1] | **Skip** | Use UUIDs; renaming should never perturb identity. |
| 47 | Model types containing renderer state (`cairo_t*`, `GdkRectangle`, `GdkRGBA`) [v1] | **Skip** | This is the reason "port to Qt" became "rewrite in Qt". |
| 48 | One `ArrangerWidget` GObject branching on a `type` enum [v1] | **Skip** | Same sharing benefit is available via composition/inheritance without the mega-switch. |
| 49 | GNU Guile scripting | **Skip** | Unpackageable on Windows, now deprecated and disabled. Pick something embeddable everywhere (Lua/QuickJS/WASM). |
| 50 | Shipping a DAW with no controller API | **Skip** | Named explicitly by users as a blocker; it's also how a small team gets hardware support written for free. |
| 51 | Depending on X11 because the plugin-window layer needs it | **Skip** | Isolate plugin windowing so a Wayland-native session is viable. |
| 52 | The "intuitive" positioning | **Skip** | It provoked a 410-point HN backlash and a rebuttal essay from Ardour's lead. Claim something falsifiable instead. |
| 53 | Marketing "SIMD-optimized DSP" / "hardware accelerated UI" with no published numbers | **Skip** | Users measured 10 FPS. Publish benchmarks or don't make the claim. |
| 54 | Trial limit as a build-time flag in the open source tree | **Steal (as a model)** | Zero DRM code, distros ship unlimited builds, you sell convenience — and it caused far less friction than expected. |

---

## 11. What this means for AURA

The concrete decisions this analysis drives. Each is cross-referenced to the
AURA file or doc section it lands in; each gets its own spec.

| # | Decision | Lands in | Zrythm evidence |
|---|---|---|---|
| 1 | **Parameters become a first-class type, never a port.** A `Param` has id, range, flags, default, and a modulation input. Control values do not become graph ports. | new `aura-core` param model; supersedes the flat `ParamId(u32)` in `src-tauri/src/plugins/automation.rs` | §2.1 — v1 exhausted two 31-bit flag words conflating them; v2 deleted `PortType::Control` and removed ~25 flags |
| 2 | **Per-sample (or segmented-ramp) parameter evaluation is the contract from day one.** `process(block, span<param_values>)`, not `process(block)` with a scalar. | `src-tauri/src/audio/dsp.rs` `AudioProcessor::process` signature; `plugins/automation.rs` `GainAutomatedNode` already proves the ramp math | §2.2, §9.1 — Zrythm markets sample-accurate automation and ships block-accurate in **both** versions; the hook exists and is unused |
| 3 | **One node interface for every graph participant** — clip players, faders, sends, meters, metronome, plugin adapters. No "mixer engine" beside a "plugin engine". | replaces `RtGraph { tracks: Vec<RtTrack> }` in `src-tauri/src/audio/rt.rs`; `mixer::render` becomes a schedule executor per ARCHITECTURE §10.1 | §2.1, §8 #1 — v1's 10-arm `switch` duplicated at six sites is what made v1 hard to extend |
| 4 | **CV is a port type and a modulation edge is a data row** `{src, dest, amount, enabled, locked, bipolar}`, stored in the project, in the op-log, undoable. | new routing model; `docs/ipc-schemas/` gains a connection schema | §2.3, §8 #3 — two data-model decisions buy the entire modulator feature |
| 5 | **Incremental graph edit and incremental cycle check**, not full rebuild per connection. | the graph compiler; explicitly *not* Zrythm's `recalc_graph()` | §9.1 — `can_ports_be_connected()` builds a throwaway full project graph **per candidate target** |
| 6 | **Typed UUIDs + a refcounted object registry + two-phase deserialization.** Identity never derives from name, position or structure. | supersedes `String` ids in `src-tauri/src/audio/types.rs`; pairs with the `MidiNote` id gap already flagged in `docs/SCALABILITY.md` §3 | §2.1, §9.1 — v1's `track_name_hash` in the port identifier meant **renaming a track perturbed port identity** |
| 7 | **Track type is a feature bitflag on one type, not a hierarchy.** Skip the virtual-inheritance-mixin phase entirely. | replaces `TrackState.kind: String` and the `"bus"`-rejecting whitelist in `src-tauri/src/control/ops.rs` | §2.4 — Zrythm built the mixin design, shipped it, and deleted it after ~1 year |
| 8 | **Tempo is a `TempoMap` + timeline objects, never a track type.** Ticks everywhere, strong unit types so samples and ticks cannot be interchanged. | extends `src-tauri/src/midi/tempo.rs`; closes the D-02 remainder (time-signature map) in `docs/SCALABILITY.md` | §2.4, §3.2 — v1 had `TRACK_TYPE_TEMPO` and v2 deleted it |
| 9 | **Undo actions carry a declarative effect descriptor as a trait method** (`needs_pause`, `needs_graph_rebuild`, `affects_ports`) — never a hard-coded list of command ids. | the op-log/undo layer (D-03) | §5.2, §9.1 — v2 replaced v1's virtual `needs_pause()` with `std::array<int,6>`; a forgotten entry is silent graph corruption |
| 10 | **Undo is coarse and selection-shaped, and serializable from the first commit.** Persistence is designed in, not deferred. | the op-log/undo layer (D-03); `docs/ipc-schemas/op-envelope.schema.json` | §5 — v1's ten action types covered a whole DAW; v2's 23 fine-grained commands ship with `/* TODO: serialize */` |
| 11 | **Every mutation snapshots the connection set before/after**, so incidental routing damage is always reversible. | the op-log/undo layer | §5.1 — `undoable_action_save_or_load_port_connections`, embarrassingly cheap |
| 12 | **No view state in model types.** Rendering caches, rects and colours live in a view layer keyed by object id. | binding rule for `src-tauri/src/` ↔ `src/`; already implicit in ARCHITECTURE §0 zone split — make it explicit | §9.1 — `cairo_t*` inside `ArrangerObject` is the concrete reason Zrythm's toolkit port became a four-year rewrite |
| 13 | **Plugin state stays a sidecar blob keyed by id**, never inlined into the project document. | keep the existing APST design in `src-tauri/src/plugins/state.rs`; explicitly reject v2's base64-in-JSON | §6 — a 40-plugin v2 project is one large base64 JSON blob with no lazy load and no partial recovery |
| 14 | **One arranger abstraction, specialised per editor.** Timeline, piano roll, automation lane, velocity lane share viewport, selection, gesture state machine and snap grid. | `src/lib/components/` — the shared abstraction the frontend map found missing (`view.svelte.ts` is 90% of it already) | §3.1, §8 #4 — one person shipped seven editors from one implementation |
| 15 | **Ship the extensibility surface early.** The MCP control plane is AURA's controller/scripting API and should be treated as a first-class product surface, not a demo. | `src-tauri/src/mcp/`, `docs/mcp-usage.md` | §9.1 — Zrythm removed Guile and never replaced it; users name the missing controller API as a blocker |
| 16 | **`[[clang::nonblocking]]`-equivalent enforcement in CI.** RT-safety becomes a build failure, not a user crash report. | `rtsan-standalone` / `assert_no_alloc` around the callback; ARCHITECTURE §2.1 rules gain an enforcer | §2.1 — Zrythm runs realtime-sanitizer in CI today; AURA's §2.1 rules are currently enforced by convention only |
| 17 | **Publish benchmarks.** Reproducible CPU / latency / large-project numbers, in the repo. | new `docs/BENCHMARKS.md`; the offline render path in `src-tauri/src/audio/offline.rs` is already the harness | §9.2, §9.4 — "optimized performance" with zero numbers against 10-FPS user reports is Zrythm's biggest credibility problem |
| 18 | **Routing graph `.dot` export from day one.** | the graph compiler; near-free once the graph is a real structure | §2.1 — Zrythm ships this and it is enormous debugging value |

**The bottom line for AURA.** Zrythm's feature list is the product of four
data-model decisions: *everything is a graph node; parameters are a type; CV
is a port type; positions are ticks.* Nearly every marketed feature is a
consequence of those four rather than a feature someone implemented. What it
got wrong falls into two buckets — **bits where types belonged** (port flags,
structural identity, the `GraphNodeType` switch), and **view state inside
model types**, which is what turned a toolkit port into a four-year rewrite
that has cost the project its shipped feature set, its stable release, and —
for now — its serializable undo history.
