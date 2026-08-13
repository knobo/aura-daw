# State of the art: DAW engine architecture

Research dossier, 2026-08-13. Companion to `docs/ARCHITECTURE.md` and
`docs/SCALABILITY.md`.

> **Why this document exists.** AURA's engine is a flat `Vec<RtTrack>` summed
> into the cpal output buffer. `docs/SCALABILITY.md` §1 promises an audio
> graph; this document is the evidence base for *which* graph, gathered by
> reading the source of the four engines that have already made every one of
> these decisions and lived with the consequences. It exists so the next
> contributor does not re-derive them, and so the two places where our
> written plan is **wrong** are recorded with their reasons.
>
> **Provenance.** Every claim carrying a URL was verified by fetching the
> actual source file, header, or spec — mostly raw source, not summaries.
> Primary sources: Ardour, Tracktion Engine, JUCE, Zrythm 2.x, Firewheel,
> nih-plug, CLAP headers, Bitwig user guide, Ableton Link. Blocks marked
> **⊳ Judgment** are engineering opinion, not sourced. Blocks marked
> **⚠ Gap** could not be verified and must not be treated as settled — §10
> lists them all in one place.

---

## 1. The processing graph

### 1.1 Node granularity — the single biggest structural choice

**Ardour (2005-era, still shipping).** A graph node *is a whole track*.
Verbatim from
[`libs/ardour/ardour/graphnode.h`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/ardour/graphnode.h):

```cpp
/** A node on our processing graph, ie a Route */
class LIBARDOUR_API GraphNode : public ProcessNode, public GraphActivision
```

Inside a Route, processing is a linear `ProcessorList` walked serially.
Consequences: no intra-track parallelism, no fine-grained buffer reuse, and
sends and sidechains are special-cased at the Route level rather than being
ordinary edges.

**Tracktion / Zrythm / JUCE / Firewheel (modern).** Every clip player, gain
stage, summing point, latency compensator and plugin is a node with typed
ports. Tracktion's `SummingNode`, `GainNode`, `LatencyNode`, `ConnectedNode`
are all first-class
([module listing](https://github.com/Tracktion/tracktion_engine/tree/master/modules/tracktion_graph)).
Zrythm's `IProcessable` interface is implemented by anything schedulable
([`src/dsp/graph_node.h`](https://raw.githubusercontent.com/zrythm/zrythm/master/src/dsp/graph_node.h)).

The payoff is stated explicitly in Tracktion's own rewrite document, on the
*old* engine
([`tracktion_graph_part_1_introduction.md`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/docs/tracktion_graph_part_1_introduction.md)):

> "operated track-by-track. Complex elements like Racks remained
> single-threaded, causing other processors to idle rather than distributing
> work efficiently across available resources"

### 1.2 Graph *compilation* is a fixed-point transform, not one pass

This is the detail most homegrown engines get wrong. Tracktion runs
`Node::transform()` repeatedly until nothing changes — because inserting
latency nodes and connecting sends *changes the topology*, which can require
more latency nodes
([`tracktion_Node.h`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/tracktion_graph/tracktion_Node.h)):

```cpp
static inline std::vector<Node*> transformNodes (Node& rootNode, bool disableLatencyCompensation)
{
    for (;;)
    {
        bool needToTransformAgain = false;
        auto allNodes = getNodes (rootNode, VertexOrdering::postordering);
        TransformCache cache;

        for (auto node : allNodes)
        {
            Node::TransformOptions options { rootNode, allNodes, cache, disableLatencyCompensation };
            const auto res = node->transform (options);
            if (res == TransformResult::none) continue;
            needToTransformAgain = true;
            if (res == TransformResult::nodesDeleted) break;   // allNodes invalidated, restart
            assert (res == TransformResult::connectionsMade);
        }
        if (! needToTransformAgain) return allNodes;
    }
}
```

Note `TransformResult { none, connectionsMade, nodesDeleted }` — the
distinction matters because deletion invalidates the iteration and forces a
restart, while a mere connection does not.

Ordering is **post-ordered DFS**. The ADC 2020 slides
([PDF](https://github.com/drowaudio/presentations/blob/master/ADC%202020%20-%20Introducing%20Tracktion%20Graph/Introducing%20Tracktion%20Graph.pdf))
spend several slides on exactly this: `Pre-ordered DFS: ❌ / Post-ordered
DFS: ✅`, with a worked example where pre-order puts the root first.

Ardour instead uses **Kahn's algorithm**, citing the original paper
([`libs/ardour/graph_edges.cc:281`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/graph_edges.cc)):

```cpp
/* Do the sort: algorithm is Kahn's from Wikipedia.
 * `Topological sorting of large networks', Communications of the ACM 5(11):558-562.
 */
```

**Ardour's edge discovery is O(n²), and it knows it:**

```cpp
for (auto const& i : nodes) {
    for (auto const& j : nodes) {
        bool via_sends_only = false;
        if (j->direct_feeds_according_to_reality (i, &via_sends_only)) {
            edges.add (j, i, via_sends_only);
        }
    }
}
```

It is instrumented accordingly —
`Session::resort_route took %1ms ; DSP %2 %%`
([`session.cc`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/session.cc)).
Every pair of routes is asked "do you feed him?", which walks the port
connection tables.

**⊳ Judgment.** At 500+ tracks this is the wrong shape. Build the edge set
*from* the connection model when connections are made, not by interrogating
every node pair on every rebuild. AURA already holds the connection intent in
the project model; do not rediscover it.

### 1.3 Cycles and feedback

Three distinct policies, all verified:

**Reject and keep the old graph.** Ardour: `topological_sort` returns false,
`rechain_process_graph` refuses to swap, and the session emits
`FeedbackDetected()`. Verbatim comment:

> "The topological sort failed, so we have a problem. Tell everyone and stick
> to the old graph; this will continue to be processed, so until the feedback
> is fixed, what is played back will not quite reflect what is actually
> connected."

**Tolerate feedback through sends.** Ardour tags each edge:
`direct_feeds_according_to_reality(node, bool* via_send_only)`
([`route.cc:3979`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/route.cc)).
An edge that exists *only* via an internal send can be treated differently by
the sort — this is how a feedback send is legal at all.

**Assert after ordering.** Tracktion validates the produced order
([`tracktion_NodePlayerUtilities.h`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/tracktion_graph/players/tracktion_NodePlayerUtilities.h)):

```cpp
static inline bool areThereAnyCycles (const std::vector<Node*>& orderedNodes)
{
    // Iterate from the first node to the last; find each input of the Node;
    // ensure that the input is in a lower position than the current
    ...
    if (inputPosition > position) ++numCycles;
}
```

Firewheel lists "Cycle detection for invalid audio graphs" as a shipped goal
([DESIGN_DOC.md](https://raw.githubusercontent.com/BillyDM/Firewheel/main/DESIGN_DOC.md)).

### 1.4 Buffer allocation — two rival schools

**(a) Liveness analysis over a fixed order — JUCE.**
`RenderSequenceBuilder`
([`juce_AudioProcessorGraph.cpp`](https://raw.githubusercontent.com/juce-framework/JUCE/master/modules/juce_audio_processors_headless/processors/juce_AudioProcessorGraph.cpp))
does what a compiler backend does — register allocation:

```cpp
static int getFreeBuffer (Array<AssignedBuffer>& buffers)
{
    for (int i = 1; i < buffers.size(); ++i)
        if (buffers.getReference (i).isFree()) return i;
    buffers.add (AssignedBuffer::createFree());
    return buffers.size() - 1;
}

void markAnyUnusedBuffersAsFree (const Connections::DestinationsForSources& c,
                                 Array<AssignedBuffer>& buffers, const int stepIndex)
{
    for (auto& b : buffers)
        if (b.isAssigned() && ! isBufferNeededLater (c, stepIndex, -1, b.channel))
            b.setFree();
}
```

Buffer 0 is a permanent read-only silence buffer
(`AssignedBuffer::createReadOnlyEmpty()`). The output is a flat
`std::vector<std::unique_ptr<RenderOp>>` (`ClearOp`, `DelayChannelOp`,
process ops), executed **strictly sequentially on the calling thread**:

```cpp
for (const auto& op : renderOps)
    op->process (context);
```

`juce::AudioProcessorGraph` is single-threaded. Full stop. Its
`isBufferNeededLater` is also a linear scan of all remaining nodes, making
compilation ~O(n²).

**(b) Refcounted lock-free pool — Tracktion.** From the design notes in
`tracktion_Node.h`, verbatim:

```
Buffer optimisation:
- As there will be a lot of nodes, it makes sense to reduce the memory footprint by reusing audio and MIDI buffers
- There are two ways I can think of doing this:
    1. Analyse the graph and where nodes are sequential but not directly connected, use the same buffer
    2. Use a buffer pool and release it when all nodes have read from it.
        - This would probably require all nodes that need the output buffer to call "retain" on the node before the
          processing stage and then "release" once they're done with it. When the count drops to 0 the buffers can
          be released back to the pool
```

They shipped (2).
[`tracktion_AudioBufferPool.h`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/utilities/tracktion_AudioBufferPool.h):

> "A lock-free pool of audio buffers… Note that the buffers can be
> pre-allocated but if you ask for a buffer which isn't in the pool, it will
> either resize an existing one or allocate a new one. **After processing a
> constant audio graph for a while though this should be completely
> allocation and lock-free.**"

Pre-sized by
`node_player_utils::reserveAudioBufferPool(root, allNodes, pool, numThreads, blockSize)`.

**⊳ Judgment — this is one of the two places our written plan is wrong.**
`docs/SCALABILITY.md` §1 currently specifies "preassigned buffer indices from
a buffer pool" — *static* assignment. That is correct only for a
single-threaded, fixed-order schedule. The moment the schedule goes multicore
(Stage 2 of our own roadmap) execution order is nondeterministic and a
statically assigned buffer can be read by node A while node B overwrites it.
Use a **pre-reserved pool with per-node retain/release refcounts**. Same
zero-allocation property, survives parallelism unchanged. Cheap now; a
rewrite later.

### 1.5 Latency / PDC — where the industry actually split

**Design A — shift the time each node reads (Ardour, Zrythm).** Each node
carries `playback_latency_` and `route_playback_latency_`;
`GraphNode::compensate_latency()` adjusts the block's transport position by
route latency minus remaining preroll
([`graph_node.h`](https://raw.githubusercontent.com/zrythm/zrythm/master/src/dsp/graph_node.h)):

```cpp
/**
 * @brief The playback latency of the node, in samples.
 * @see Page 116 of "The Ardour DAW - Latency Compensation and
 * Anywhere-to-Anywhere Signal Routing Systems".
 */
units::sample_u32_t playback_latency_;
```

**Design B — insert delay lines on the shorter paths (JUCE, Tracktion).**
JUCE computes per node:

```cpp
const auto maxInputLatency = getInputLatencyForNode (c, node.nodeID);
...
const auto thisNodeLatency = maxInputLatency + processor.getLatencySamples();
delays[node.nodeID.uid] = thisNodeLatency;
totalLatency = jmax (totalLatency, thisNodeLatency);
```

and emits `sequence.addDelayChannelOp (bufIndex, maxLatency - nodeDelay)` per
channel — with a comment showing the subtlety:

```cpp
// If the input needs to be delayed by some amount, this will modify the buffer
// in-place which will produce the wrong delay if a subsequent input needs a
// different delay value.
```

**Tracktion explicitly abandoned Design A when they rewrote.** From their
rewrite doc, on the *old* engine: "when the Edit time to render passes through
a plugin node, that time gets shifted forwards" to read future audio data —
and this

> "breaks down in intricate routing situations, particularly when bus
> send/return structures nest within each other. The mechanism also creates
> inaccessible timeline regions where transients and note events may be
> missed."

That is a direct, dated, from-the-implementer verdict: **time-shift PDC does
not survive nested send/return topologies.**

CLAP corroborates that latency changes must be structural
([`ext/latency.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/ext/latency.h)):

```c
// Tell the host that the latency changed.
// The latency is only allowed to change during plugin->activate.
// If the plugin is activated, call host->request_restart()
// [main-thread & being-activated]
void(CLAP_ABI *changed)(const clap_host_t *host);
```

Dave Rowland's ADC 2023 deck lists the full PDC problem set
([slides](https://github.com/drowaudio/presentations/blob/master/ADC%202023%20-%20Why%20you%20Shouldn't%20Write%20a%20DAW/Why%20you%20Shouldn't%20Write%20a%20DAW.pdf)):

> "Plugins can introduce latency · Leads to offsets in the times that
> subsequent nodes are processing · Compensate for these delays · Sync with
> live input recording · **Ensure plugins read the correct automation values**
> · **Ensure plugins receive the correct timeline timestamps** · Extra
> complicated via aux busses · **Extra extra complicated when bypassing**"

— costed at £423k cumulative in his running tally.

### 1.6 The under-appreciated hard part: continuity across rebuilds

From the ADC 2020 slides, verbatim:

> **Continuity**
> • If the topology changes, the graph will need to be rebuilt
> • If any nodes have latency, this means they will have a history of previous samples
> • If this history is not persisted between graphs, there will be a gap/inconsistency in playback and hence a glitch
> • In order to avoid these discontinuities, any history buffers will need to be persisted between graphs
> • **This means each node must be uniquely identifiable and the same between graphs**

The implementation: `NodeProperties::nodeID` (a `size_t`),
`PlaybackInitialisationInfo::nodeGraphToReplace` handed to every node's
`initialise()`, and `LatencyProcessor::hasSameConfigurationAs()` /
`hasConfiguration(numLatencySamples, sampleRate, numChannels)` so a delay
line's contents can be adopted rather than recreated.

Compiler-*synthesised* nodes need stable IDs too. Tracktion derives them by
hashing
([`tracktion_LatencyNode.h`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/tracktion_graph/nodes/tracktion_LatencyNode.h)):

```cpp
NodeProperties getNodeProperties() override
{
    auto props = input->getNodeProperties();
    props.latencyNumSamples += latencyProcessor->getLatencyNumSamples();
    constexpr size_t latencyNodeMagicHash = size_t (0x95ab5e9dcc);
    if (props.nodeID != 0)
        hash_combine (props.nodeID, latencyNodeMagicHash);
    return props;
}
```

Uniqueness is asserted in debug (`areNodeIDsUnique`).

**⊳ Judgment.** AURA has this property for *instruments* via
`rt::LiveNodeCell` keyed by `"synth@48000"` / `"sampler:<id>@48000"` /
`"plugin:<instanceId>@48000"`. It does **not** have it for delay lines,
filter state, or synthesised nodes, because those do not exist yet. Generalise
the registry key from `track_id` to node id before they do.

### 1.7 In-place processing and silence

CLAP declares in-place capability per port pair
([`ext/audio-ports.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/ext/audio-ports.h)):

```c
// in-place processing: allow the host to use the same buffer for input and output
// if supported set the pair port id.
// if not supported set to CLAP_INVALID_ID
clap_id in_place_pair;
```

Firewheel adds **silence masks**:

> "every audio buffer in the graph is marked with a 'silence flag'. Audio
> nodes can read `ProcInfo::in_silence_mask` to quickly check which input
> buffers contain silence. If all input buffers are silent, then the audio
> node can choose to skip processing."

Nodes report their output silence via `ProcessStatus`.

Tracktion has the analogous opt-in hints:

```cpp
struct NodeOptimisations
{
    ClearBuffers clear = ClearBuffers::yes;
    AllocateAudioBuffer allocate = AllocateAudioBuffer::yes;
};
```

with the API philosophy stated as *"Easy to use, hard to abuse… Process calls
will always provide empty buffers so nodes can simply 'add' in to them…
**Optimisations are opt-in**."*

### 1.8 Split-cycle processing

Zrythm passes a three-field block descriptor rather than a bare frame count
([`graph_node.h`](https://raw.githubusercontent.com/zrythm/zrythm/master/src/dsp/graph_node.h)):

```cpp
struct ProcessBlockInfo
{
    units::sample_u64_t transport_position_;  // timeline position at start of this chunk
    units::sample_u32_t buffer_offset_;       // offset within the cycle's audio buffer
    units::sample_u32_t nframes_;             // frames to process from that offset
};
```

with `process_chunks_after_splitting_at_loop_points()`:

> "Splits processing into multiple chunks when the playhead crosses the
> transport loop points, ensuring seamless audio playback during looping."

### 1.9 2005 vs modern, side by side

| | 2005-era (Ardour) | Modern (Tracktion 2020+, Zrythm 2.x, Firewheel) |
|---|---|---|
| Node granularity | one node = one track | one node = one processing element |
| Compilation | Kahn sort of routes, O(n²) edge discovery | fixed-point transform passes + post-order DFS |
| Buffers | per-route buffers | pre-reserved lock-free pool, refcounted release |
| PDC | shift each node's read position | insert latency nodes; latency is a node property |
| Continuity | implicit (routes persist) | explicit stable node IDs + adopt-old-graph handoff |
| Sends/sidechains | special-cased on Route | ordinary edges |
| Block size | fixed | any size up to prepared max |
| Precision | 32-bit sum, some 64-bit | complete 32/64-bit pipeline choice |
| Silence | none | silence masks, opt-in skip |
| Swap | new `GraphChain`, butler-thread delete | `LockFreeObject::pushNonRealTime`, RT-safe |

### 1.10 What we should adopt

1. **Fine-grained node/port graph from day one**, with sends and sidechains
   as ordinary edges (SCALABILITY §1 already says this — keep it).
2. **Compilation as a fixed-point transform pipeline**, not a single pass.
   Passes: connect sends/returns → compute latencies → insert latency nodes →
   order (post-order DFS) → assert acyclic → assign buffer pool → build the
   flat schedule.
3. **Latency nodes, not time-shifting.** Tracktion's verdict is decisive and
   dated.
4. **Stable node IDs with an adopt-from-old-graph handoff.** Every node gets
   an ID derived from model identity; synthesised nodes hash their input's ID
   plus a per-kind salt. Pass the old compiled graph into the new one's
   prepare step so delay lines, filter state and plugin instances migrate
   rather than reset.
5. **Refcounted buffer pool, pre-reserved** — *change* the current
   "preassigned buffer indices" plan.
6. **Silence masks.** At 500 tracks, most buffers are silent most of the time.
7. **Split-cycle `ProcessBlockInfo { transport_position, buffer_offset,
   nframes }`** rather than a bare frame count. We already need this for exact
   loop wraps; we will need it again for sample-accurate transport jumps and
   sample-accurate automation.
8. **Build edges from connection intent, not pairwise interrogation.**

**Trade-off.** A fine-grained graph means many more nodes (a 500-track session
is easily 10k nodes), which makes compile time and per-node dispatch overhead
matter. Tracktion's answer — the `TransformCache`, node ID hashing, and
`stable_sort` of ready nodes to the front — is the cost of admission.
Coarse-grained is simpler and faster to compile; it just cannot be
parallelised or latency-compensated properly. Pay the cost.

---

## 2. Multicore audio rendering

### 2.1 Nobody ships work-stealing

This is the clearest single finding in this section. Three independent
implementations, same algorithm: **a ready-queue with per-node atomic
dependency counters.**

**Ardour**
([`graphnode.cc`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/graphnode.cc)):

```cpp
void GraphNode::prep (GraphChain const* chain)
{
    /* This is the number of nodes that directly feed us */
    _refcount.store (init_refcount (chain));
}

/** Called by an upstream node, when it has completed processing */
void GraphNode::trigger ()
{
    if (PBD::atomic_dec_and_test (_refcount)) {
        /* All nodes that feed this node have completed, so this node be processed now. */
        _graph->trigger (this);
    }
}

void GraphNode::finish (GraphChain const* chain)
{
    bool feeds = false;
    for (auto const& i : activation_set (chain)) { i->trigger (); feeds = true; }
    if (!feeds) _graph->reached_terminal_node ();
}
```

**Tracktion**
([`tracktion_LockFreeMultiThreadedNodePlayer.cpp`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/tracktion_graph/tracktion_LockFreeMultiThreadedNodePlayer.cpp)):

```cpp
struct PlaybackNode
{
    Node& node;
    const size_t numInputs;
    std::vector<Node*> outputs;
    std::atomic<size_t> numInputsToBeProcessed { 0 };
    std::atomic<bool> hasBeenQueued { true };
};

Node* LockFreeMultiThreadedNodePlayer::updateProcessQueueForNode (PreparedNode& preparedNode, Node& node)
{
    for (auto output : playbackNode->outputs)
    {
        auto outputPlaybackNode = static_cast<PlaybackNode*> (output->internal);
        // fetch_sub returns the previous value so it will now be 0
        if (outputPlaybackNode->numInputsToBeProcessed.fetch_sub (1, std::memory_order_acq_rel) == 1)
        { ... }
    }
}
```

**Zrythm**
([`graph_scheduler.cpp`](https://raw.githubusercontent.com/zrythm/zrythm/master/src/dsp/graph_scheduler.cpp))
— same code, and the file's copyright header credits Robin Gareus (Ardour),
i.e. it is a direct descendant:

```cpp
GraphScheduler::trigger_node (GraphNode &node)
{
  if (node.refcount_.fetch_sub (1) == 1)
    {
      node.refcount_.store (node.init_refcount_);   /* reset for next cycle */
      trigger_queue_.push_back (&node);
    }
}
```

**⊳ Judgment on why not work-stealing.** A DAW's DAG is known at compile time,
small (thousands of nodes, not millions of tasks), and has extremely uneven
task costs. Work-stealing deques optimise for dynamic task generation and load
balance across many producers — neither applies. The real win is the opposite
of stealing: *avoiding the queue entirely* for serial chains.

### 2.2 The serial-chain optimisation — where the win actually is

Tracktion's `RETURN_MID_NODES_OPTIMISATION`, verbatim:

```cpp
// We can return one Node to be processed on this thread, otherwise we can
// queue it for another thread to possibly process
if (nodeToReturn == nullptr) { nodeToReturn = &outputPlaybackNode->node; }
else { preparedNode.nodesReadyToBeProcessed->try_enqueue (&outputPlaybackNode->node); ... }
```

and:

```cpp
void LockFreeMultiThreadedNodePlayer::processNode (PreparedNode& preparedNode, Node& node)
{
    auto* nodeToProcess = &node;
    // Attempt to process serial Node chains on this thread
    // to reduce context switches and overhead
    for (;;)
    {
        nodeToProcess->process (numSamplesToProcess, referenceSampleRange);
        nodeToProcess = updateProcessQueueForNode (preparedNode, *nodeToProcess);
        if (! nodeToProcess) break;
    }
}
```

A track's `clip → gain → pan → plugin → plugin → bus-send` chain runs entirely
on one thread, in cache, with zero queue traffic. Only *branches* enter the
queue.

Ardour's equivalent is wake-throttling in `run_one()`
([`graph.cc`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/graph.cc)):

```cpp
uint32_t idle_cnt   = _idle_thread_cnt.load();
uint32_t work_avail = _trigger_queue_size.load();
uint32_t wakeup     = std::min (idle_cnt + 1, work_avail);
for (guint i = 1; i < wakeup; ++i) _execution_sem.signal ();
```

### 2.3 How workers wait — the part that decides RT-safety

Tracktion parameterises this into six named strategies
([`tracktion_NodePlayerThreadPools.h`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/tracktion_graph/tracktion_NodePlayerThreadPools.h)):

```cpp
enum class ThreadPoolStrategy
{
    conditionVariable,      /**< Uses CVs to pause threads. */
    realTime,               /**< Uses pause, yield and sleeps to suspend threads. */
    hybrid,                 /**< Uses a combination of the above, avoiding CVs on the audio thread. */
    semaphore,              /**< Uses a semaphore to suspend threads. */
    lightweightSemaphore,   /**< Uses a semaphore/spin mechanism to suspend threads.*/
    lightweightSemHybrid    /**< Uses a combination of semaphores/spin and yields to suspend threads.*/
};
```

The `realTime` backoff ladder, verbatim, with the key comment:

```cpp
void wait()
{
    // The pause and sleep counts avoid starving the CPU if there aren't enough queued nodes
    // This only happens on the worker threads so the main audio thread never interacts with the thread scheduler
    thread_local int pauseCount = 0;
    if (shouldWait())
    {
        ++pauseCount;
        if      (pauseCount < 50)  pause();
        else if (pauseCount < 100) std::this_thread::yield();
        else if (pauseCount < 150) std::this_thread::sleep_for (std::chrono::milliseconds (1));
        else if (pauseCount < 200) std::this_thread::sleep_for (std::chrono::milliseconds (5));
        else pauseCount = 0;
    }
    else pauseCount = 0;
}
void waitForFinalNode() override { pause(); }   // <-- called on the AUDIO thread: spin only
```

**The invariant: the audio callback thread never blocks. Worker threads may.**
Ardour breaks this — its terminal-node handoff does
`while (_idle_thread_cnt.load() != n_workers) { sched_yield (); }` then
`_callback_start_sem.wait()` — but Ardour's "audio thread" *is* a graph
thread, a different topology.

`pause()` is two `_mm_pause()`-class instructions
(`tracktion::core::pause()` called twice). Timur Doumler recommends exactly
this for the spin case:

> "use a tuned spinlock with exponential back-off using CPU pause instructions
> (`_mm_pause()`) rather than yielding or sleeping, measured empirically for
> the specific hardware and audio buffer sizes"
> — [timur.audio](https://timur.audio/using-locks-in-real-time-audio-processing-safely)

### 2.4 Thread priority and platform workgroups

```cpp
const auto rtOpts = juce::Thread::RealtimeOptions()
                      .withPriority (10)
                      .withApproximateAudioProcessingTime (player.getBlockSize(), player.getSampleRate());
for (size_t i = 0; i < numThreads; ++i)
{
    threads.emplace_back ([this] { runThread(); });
    setThreadPriority (threads.back(), 10);
    tryToUpgradeCurrentThreadToRealtime (rtOpts);
}
...
void runThread()
{
    juce::WorkgroupToken token;
    workgroup.join (token);        // macOS AudioWorkgroup
    for (;;) { if (shouldExit()) return; if (! process()) wait(); }
}
```

Zrythm does the same
(`std::optional<juce::AudioWorkgroup> thread_workgroup` threaded through
`GraphScheduler` and `GraphThread`), and additionally annotates its worker
with the clang function-effect attribute:

```cpp
void run_worker () noexcept [[clang::nonblocking]];
```

`[[clang::nonblocking]]` makes the compiler *statically reject* allocations,
locks and virtual calls it cannot prove safe inside that function. This is the
single most useful new tool in RT audio (Dave Rowland, *"Can Audio Programming
be Safe?"*, ADC 2024 — [video](https://youtu.be/Uda9h52pzuA),
[slides](https://github.com/drowaudio/presentations)).

### 2.5 Swapping the graph while workers run

Tracktion: the prepared graph is published through a `LockFreeObject`:

```cpp
lastGraphPosted = newPreparedNode.graph.get();
lastAudioBufferPoolPosted = newPreparedNode.audioBufferPool.get();
preparedNodeObject.pushNonRealTime (std::move (newPreparedNode));
```

and the RT side retains it with a `try_lock` that *degrades to nullptr* rather
than blocking
([`tracktion_LockFreeObject.h`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/utilities/tracktion_LockFreeObject.h)):

> "Whilst this is happening, retainRealTime will still be lock-free but will
> return nullptr signifying no object can be used."

Ardour builds the new `GraphChain` off-thread and hands destruction to the
butler, with a candid comment:

```cpp
/* Ideally we'd use a memory pool to allocate the GraphChain, however node_lists
 * inside the change are STL list/set. It was never rt-safe to re-chain the graph.
 * ...
 * However, the graph-chain may be in use (session process), and the last reference
 * be held by the process-callback. So we delegate deletion to the butler thread.
 */
_graph_chain = std::shared_ptr<GraphChain> (new GraphChain (g, edges),
                                            std::bind (&rt_safe_delete<GraphChain>, this, _1));
```

### 2.6 Plugin-side parallelism

CLAP lets a plugin borrow the host's pool
([`ext/thread-pool.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/ext/thread-pool.h)),
with an explicit warning:

> "Be aware that using a thread pool may break hard real-time rules due to the
> thread synchronization involved.
> If the host knows that it is running under hard real-time pressure it may
> decide to not provide this interface."

The example is per-voice parallelism: `host_thread_pool->request_exec(host, N)`
for N voices, falling back to a serial loop.

### 2.7 Measured benefit

Tracktion's own claim for the rewrite (ADC 2020 slides): **"Improved CPU
performance (20% faster)"** — that is the whole rewrite, not parallelism
alone. Design aim: *"Ensure nodes can be processed multi-threaded which scales
independently of graph complexity."*

**⚠ Gap.** Elk Audio OS / Twine, ADC talk measurements on core scaling, and
PREEMPT_RT / `AvSetMmThreadCharacteristics` / rtkit specifics were not
verified. Treat any core-count scaling numbers as unverified.

### 2.8 What we should adopt

1. **Ready-queue with per-node atomic dependency counters**, seeded from source
   nodes each block. Not work-stealing.
2. **The serial-chain optimisation** — when a node completes and exactly one
   successor becomes ready, run it on the same thread without touching the
   queue. This is where most of the win is.
3. **The audio callback thread never blocks.** It spins (`pause()`) waiting for
   the final node. Worker threads use a backoff ladder (pause → yield → short
   sleep). Make the strategy pluggable, like Tracktion — you will want to A/B
   it per platform.
4. **Threads created once at engine start**, at RT priority, joined to the
   platform workgroup (macOS `AudioWorkgroup` and its equivalents), never
   spawned per block.
5. **`[[clang::nonblocking]]` discipline, Rust equivalent.** No
   `std::sync::Mutex`, no `Vec::push`, no `Box`, no `format!` in the RT path.
   Enforce with `rtsan-standalone` and a panicking global allocator in RT unit
   tests, not with review alone.
6. **Publish the compiled graph through a lock-free object that degrades to
   "no graph, output silence" rather than blocking.**

**Trade-off.** Spinning workers burn power and heat, and on a laptop that is a
real user complaint; condition-variable pools are kinder but can miss a wakeup
and cost you a block. Ship the hybrid (spin briefly, then semaphore) and expose
it as a preference. Also: leave one core unreserved — a fully saturated machine
makes the OS scheduler your enemy.

---

## 3. Getting data to and from the RT thread

### 3.1 The rules, and who states them

Ross Bencina's *Real-time audio programming 101: time waits for nothing*
(2011) remains the canonical statement.
**⚠ rossbencina.com was unreachable throughout the research session**
(connection refused). The LWN summary
([lwn.net/Articles/452630](https://lwn.net/Articles/452630/)) preserves the
thesis: *"The main problems I'm concerned with here are with code that runs
with unpredictable or un-bounded execution time"* — allocation, locks, GC,
page faults, I/O, unbounded algorithms, waiting on hardware. Treat the primary
text as unverified; the argument is not in dispute.

Timur Doumler, verified from
[timur.audio](https://timur.audio/using-locks-in-real-time-audio-processing-safely):

> "The time between subsequent audio processing callbacks is typically between
> 1-10 ms."

Missing it produces an "audible glitch, rendering your product worthless for
professional use." Do not "perform operations that might block the thread or
otherwise take an unknown amount of time, such as allocating memory,
performing any system call, or doing any I/O." Waiting on a mutex "not only
blocks the audio thread but also leads to priority inversion."

And the point people miss — **`try_lock` is not a fix**:

> "the audio thread will have to interact with the OS thread scheduler at that
> point so that that other thread can then be woken up. And that's a system
> call."

Because `unique_lock`'s *destructor* calls `unlock()`, which may need to wake a
waiter. His recommendation:

> **"Design your audio engine in such a way that this case never occurs. This
> can be achieved using immutable data structures."**

His talk series is the current reference set: *Using locks in real-time audio
processing, safely* (ADC 2020,
[video](https://www.youtube.com/watch?v=zrWYJ6FdOFQ)); *Thread synchronisation
in real-time audio processing with RCU* (ADC 2022,
[video](https://www.youtube.com/watch?v=7fKxIZOyBCE)); *Wait-free thread
synchronisation with the SeqLock* (ADC 2024,
[session page](https://conference.audio.dev/session/2024/wait-free-thread-synchronisation-with-the-seqlock/)).
The SeqLock abstract states the split precisely: ADC 2022 covered *"the
real-time thread needing to read a large persistent object mutated on another
thread"*; ADC 2024 covers *"the reverse case: the real-time thread needs to
write the value while remaining wait-free"*, contrasting SeqLock against
*"double buffering—the traditional audio industry solution."*

Dave Rowland & Fabian Renn-Giles, *Real-time 101* (ADC 2019,
[Pt I](https://youtu.be/Q0vrQFyAdWI), [Pt II](https://youtu.be/PoZAo2Vikbo),
[slides](https://github.com/drowaudio/presentations)) is the other canonical
pairing; Rowland's *Lock-free queues in the multiverse of madness* (ADC 2025,
[video](https://youtu.be/zA6kcyze1hc)) is the current deep dive.

### 3.2 Pattern 1 — RCU / atomic snapshot swap (for *structure*)

Ardour's `pbd/rcu.h` is a clean production reference. Its header comment states
the design assumption verbatim:

> "Serialized RCUManager implements the RCUManager interface. It is based on
> the following key assumption: among its users we have readers that are bound
> by RT time constraints, and writers who are not. **Therefore, we do not care
> how slow the write_copy()/update() operations are, or what synchronization
> primitives they use.**"

The reader is a hazard counter, not a lock:

```cpp
std::shared_ptr<T const> reader () const
{
    std::shared_ptr<T> rv;
    /* Keep count of any readers in this section of code, so writers can
     * wait until managed_object is no longer in use after an atomic exchange
     * before dropping it. */
    _active_reads.fetch_add (1, std::memory_order_release);
    rv = *managed_object;
    _active_reads.fetch_sub (1, std::memory_order_release);
    return rv;
}
```

Ardour uses this for the route list, the tempo map, and — notably — for the
*per-node graph activation sets*:
`SerializedRCUManager<ActivationMap> _activation_set;`.

### 3.3 Pattern 2 — the atomic pointer steal (farbot)

Fabian Renn-Giles' `farbot` is vendored inside Tracktion
([README](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/3rd_party/farbot/README.md),
presented at Meeting C++ 2019). `RealtimeObject<T, RealtimeObjectOptions>` is
parameterised by *who is allowed to mutate*:

```cpp
enum class RealtimeObjectOptions { nonRealtimeMutatable, realtimeMutatable };
```

The RT-read path is a pointer *steal*, which is both the lock and the liveness
signal
([`detail/RealtimeObject.tcc`](https://raw.githubusercontent.com/Tracktion/tracktion_engine/master/modules/tracktion_graph/3rd_party/farbot/include/farbot/detail/RealtimeObject.tcc)):

```cpp
const T& realtimeAcquire() noexcept
{
    currentObj = pointer.exchange (nullptr);   // take it; nullptr means "in use"
    return *currentObj;
}
void realtimeRelease() noexcept { pointer.store (currentObj); }

T& nonRealtimeAcquire() { nonRealtimeLock.lock(); copy.reset (new T (*storage)); return *copy.get(); }

void nonRealtimeRelease()
{
    T* ptr;
    // block until realtime thread is done using the object
    do { ptr = storage.get(); } while (! pointer.compare_exchange_weak (ptr, copy.get()));
    storage = std::move (copy);     // OLD OBJECT DESTROYED HERE, on the non-RT thread
    nonRealtimeLock.unlock();
}
```

Two things worth internalising: the RT side is **wait-free**
(`realtimeAcquire`/`realtimeRelease` documented as *"wait- and lock-free"*),
and **deallocation happens on the writer thread, always**.

The API is doc-commented as: *"Only a single real-time thread can acquire this
object at once!"* and *"This method uses a lock [and] should not be used on a
realtime thread"* for the non-RT side. Tracktion's own `LockFreeObject`
variant adds graceful degradation, quoted in §2.5.

### 3.4 Pattern 3 — FIFOs, with the choices made explicit

farbot's `fifo` is parameterised on **four** axes: producer concurrency
(single/multiple), consumer concurrency, overrun policy, underrun policy. The
README is unusually honest about the consequences:

> "Note, that with the `full_empty_failure_mode::overwrite_or_return_default`
> option the **ordering of the FIFO is lost** when overrunning or
> underrunning, i.e. newer elements may be returned before older elements in
> this case."

> "The `fifo` will never lock nor block. Additionally, depending on the above
> options the push/pop operation may be wait-free: if the consumer/producer is
> accessed from only a single thread *or* the consumer/producer uses
> `overwrite_or_return_default` then the pop/push will be wait-free
> respectively. Otherwise the particular (i.e. push or pop) operation will not
> be wait-free."

**⊳ Judgment.** "Lock-free" ≠ "wait-free", and MPMC push is *not* wait-free.
AURA's strict-SPSC choice (`rtrb`) is correct and should stay the default;
where an MPMC queue is genuinely needed (Ardour's `PBD::MPMCQueue`,
Tracktion's `rigtorp::MPMCQueue`, Zrythm's), it should be a deliberate,
documented exception on the *worker* side, never on the callback side.

### 3.5 Pattern 4 — deferred free ("the garbage queue")

Every mature engine has one:

- Ardour: `rt_safe_delete<GraphChain>` as the `shared_ptr` deleter → butler
  thread.
- farbot: `AsyncCaller` — *"a lambda can be deferred to be processed on a
  non-realtime thread. This is useful to be able to execute potential
  non-realtime safe code on a realtime thread (like logging, or
  deallocations, ...)."*
- Rust: [`basedrop`](https://docs.rs/basedrop/latest/basedrop/) — `Owned` and
  `Shared` are *"smart pointers analogous to `Box` and `Arc` which add their
  contents to a queue for deferred collection when dropped"*, plus
  `Collector`, `Handle`, `Node`, and `SharedCell` (*"a thread-safe shared
  mutable `Option<Shared<T>>`"*).

**⊳ Judgment — this is the #1 Rust-specific footgun for AURA.**
`Arc<Graph>` swapped on the RT thread: if the RT thread happens to drop the
last clone, `Drop` runs *on the audio callback*, freeing potentially thousands
of nodes and buffers. ARCHITECTURE §2.3 already says "the callback publishes
the retired graph back so the control thread can drop it" — that rule is
correct and must be enforced **structurally** (a newtype whose `Drop` is
`unreachable!()` on the RT thread, or `basedrop`), not by convention.

### 3.6 Pattern 5 — latest-value channels

For RT→UI (meters, playhead, spectra) and UI→RT (a single parameter value), a
queue is the wrong shape: you only ever want the newest value, and dropping is
correct.

- [`triple_buffer`](https://docs.rs/triple_buffer/latest/triple_buffer/): *"a
  single producer thread is frequently updating a shared data block, and a
  single consumer thread wants to be able to read the latest available
  version"* — `publish()` / `update()`.
- **SeqLock** (Timur, ADC 2024) is the wait-free-*writer* variant: the RT
  thread writes; readers retry on a torn read. Better than triple buffering
  when the RT side is the writer and the object is small-to-medium.
- farbot's `realtimeMutatable` `RealtimeObject` is the same idea with a
  different mechanism (the spectrum example in the README).

### 3.7 Pattern 6 — batch the event flush

Firewheel's design doc contains a subtlety that is easy to miss and painful to
retrofit:

> "This method first flushes any events that are in the queue and sends them to
> the audio thread. **(Flushing events as a group like this ensures that events
> that are expected to happen on the same process cycle don't happen on
> different process cycles.)**"

**⊳ Judgment.** This matters for AURA's op-log: a batch of ops that the user
perceives as atomic ("delete 50 clips", "set 8 track gains") must land on the
RT thread in the same block, or you get an audible zipper. Publish batches, not
individual commands.

### 3.8 Failure modes, catalogued

| Failure | Mechanism | Mitigation |
|---|---|---|
| Priority inversion | RT thread waits on a mutex held by a preempted low-priority thread | never lock; if you must, spin with `pause()` backoff, never `std::mutex` |
| Hidden syscall in `try_lock` | the *destructor*'s `unlock()` may wake a waiter | don't lock at all |
| Free on RT thread | last `Arc`/`shared_ptr` clone dies in the callback | deferred-free queue (`basedrop`, butler thread) |
| Queue overflow | non-RT consumer stalls (fsync, GC, scheduler) | size for worst case; choose overrun policy explicitly; accept ordering loss only for telemetry |
| Ordering loss | overwrite-on-full FIFOs | never for commands; fine for meters |
| Torn reads | non-atomic multi-word state | SeqLock or double-buffer, never "two atomics that must agree" |
| Stale snapshot references | new snapshot published while RT still holds the old | refcount + deferred free, not just a pointer swap |
| Allocation on grow | `Vec::push` past capacity | preallocate; forbid growth in RT types |
| ABA | index reused after delete | generational keys (§6 of the identity work) |
| MPMC push not wait-free | CAS retry loop under contention | prefer SPSC on the callback side |

### 3.9 What we should adopt

1. **Structure travels by immutable snapshot + atomic swap; values travel by
   ring; telemetry travels by latest-value channel.** Three mechanisms, three
   payload classes — ARCHITECTURE §2.3 already has this taxonomy; formalise it
   as a rule with no exceptions.
2. **Make "no deallocation on the RT thread" structurally impossible**, not
   merely documented. This is the one rule that silently breaks in Rust as the
   codebase grows.
3. **Batch the command flush per block.** One publish point per callback
   boundary.
4. **Degrade, don't block.** If a swap is mid-flight, the RT thread outputs
   silence for that block rather than waiting.
   `LockFreeObject::retainRealTime() -> nullptr` is the model.
5. **SeqLock for RT→control of medium structs** (transport position +
   bar/beat + loop state as one coherent snapshot) instead of a pile of
   independent atomics that can disagree with each other.

**Trade-off.** Snapshot/RCU costs memory (two copies of the graph live at
once) and makes writes expensive — exactly the trade Ardour's header says it
deliberately accepts. It also means *structural* change latency is bounded by
rebuild time, so parameter changes must stay off that path (§4).

---

## 4. Parameters and modulation

### 4.1 CLAP is the reference data model, and we should copy it

The `params` extension header is the best-written parameter spec in the
industry. Verbatim from
[`include/clap/ext/params.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/ext/params.h):

> "The host sees the plugin as an atomic entity; and acts as a controller on
> top of its parameters."

> "There are two options to communicate parameter value changes, and they are
> not concurrent.
> - send automation points during clap_plugin.process()
> - send automation points during clap_plugin_params.flush(), for parameter
>   changes without processing audio"

**The MIDI-CC vs automation conflict**, which every DAW gets wrong at least
once:

> "MIDI CCs are tricky because you may not know when the parameter adjustment
> ends. Also if the host records incoming MIDI CC and parameter change
> automation at the same time, there will be a conflict at playback: MIDI CC
> vs Automation. The parameter automation will always target the same
> parameter because the param_id is stable. The MIDI CC may have a different
> mapping in the future and may result in a different playback.
> When a MIDI CC changes a parameter's value, set the flag
> CLAP_EVENT_DONT_RECORD…"

**Plain value vs knob position** — the choice you must make once, in the file
format:

> "There are two approaches to automations, either you automate the plain
> value, or you automate the knob position. The first option will be robust to
> a range increase, while the second won't be."

**Advice for the host** (i.e. for AURA), verbatim:

> - "store plain values in the document (automation)"
> - "**store modulation amount in plain value delta, not in percentage**"
> - "when you apply a CC mapping, remember the min/max plain values so you can
>   adjust"
> - "do not implement a parameter saving fall back for plugins that don't
>   implement the state extension"

### 4.2 The flag matrix is the answer to "what does modulatable mean"

```c
CLAP_PARAM_IS_AUTOMATABLE                 = 1 << 5,
CLAP_PARAM_IS_AUTOMATABLE_PER_NOTE_ID     = 1 << 6,
CLAP_PARAM_IS_AUTOMATABLE_PER_KEY         = 1 << 7,
CLAP_PARAM_IS_AUTOMATABLE_PER_CHANNEL     = 1 << 8,
CLAP_PARAM_IS_AUTOMATABLE_PER_PORT        = 1 << 9,
CLAP_PARAM_IS_MODULATABLE                 = 1 << 10,
CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID     = 1 << 11,
CLAP_PARAM_IS_MODULATABLE_PER_KEY         = 1 << 12,
CLAP_PARAM_IS_MODULATABLE_PER_CHANNEL     = 1 << 13,
CLAP_PARAM_IS_MODULATABLE_PER_PORT        = 1 << 14,
CLAP_PARAM_REQUIRES_PROCESS               = 1 << 15,
```

plus `IS_STEPPED`, `IS_PERIODIC`, `IS_HIDDEN`, `IS_READONLY`, `IS_BYPASS`,
`IS_ENUM`. `REQUIRES_PROCESS` is subtle and important: *"Any change to this
parameter will affect the plugin output and requires to be done via process()
if the plugin is active. A simple example would be a DC Offset."*

The `cookie` field is the O(1) dispatch trick:

```c
// in clap_plugin_params.get_info():
//    Parameter *p = findParameter(param_id);
//    param_info->cookie = p;
// later, in clap_plugin.process():
//    Parameter *p = (Parameter *)event->cookie;
//    if (!p) [[unlikely]] p = findParameter(event->param_id);
```

### 4.3 Automation and modulation are *different events* with the same targeting tuple

From
[`include/clap/events.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/events.h):

```c
typedef struct clap_event_param_value {
   clap_event_header_t header;
   clap_id param_id;
   void   *cookie;
   int32_t note_id;     // -1 = wildcard
   int16_t port_index;
   int16_t channel;
   int16_t key;
   double  value;       // absolute, plain
} clap_event_param_value_t;

typedef struct clap_event_param_mod {
   clap_event_header_t header;
   clap_id param_id;
   void   *cookie;
   int32_t note_id; int16_t port_index; int16_t channel; int16_t key;
   double  amount;      // modulation amount — a DELTA
} clap_event_param_mod_t;
```

> "Clap addresses notes and voices using the 4-value tuple (port, channel, key,
> note_id)."

Gestures are separate events (`CLAP_EVENT_PARAM_GESTURE_BEGIN` / `_END`) —
which is what makes "one fader drag = one undo step" and "start writing
automation" expressible.

Events are delivered sorted: `clap_process.in_events` is *"Input read-only
event list. The host will deliver these sorted in sample order."*

### 4.4 What a *voice* does with all this — the precise semantics

The Surge team's reference plugin shows the shape
([`clap-saw-demo.cpp`](https://raw.githubusercontent.com/surge-synthesizer/clap-saw-demo/main/src/clap-saw-demo.cpp)):

```cpp
/* CLAP_EVENT_PARAM_MOD provides both monophonic and polyphonic modulation.
 * We do this by seeing which parameter is modulated then adjusting the
 * side-by-side modulation values in a voice. */
case CLAP_EVENT_PARAM_MOD:
{
    auto applyToVoice = [&pevt](auto &v) {
        if (!v.isPlaying()) return;
        switch (pevt->param_id) {
        case paramIds::pmCutoff: v.cutoffMod = pevt->amount; v.recalcFilter(); break;
        ...
```

**Base value + side-by-side per-voice offset**, recombined on read. The full
operational rules are spelled out best by Robbert van der Helm in nih-plug
([`src/midi.rs`](https://raw.githubusercontent.com/robbert-vdh/nih-plug/master/src/midi.rs)),
verbatim:

> - "If a `PolyModulation` event is emitted for the voice, that voice should
>   use the _normalized offset_ contained within the event to compute the
>   voice's modulated value and use that in place of the global value.
>   - This value can be obtained by calling
>     `param.preview_plain(param.normalized_value() + event.normalized_offset)`.
>     These functions automatically clamp the values as necessary."
> - "If a `MonoAutomation` event is emitted for a parameter, then the values or
>   target values (if the parameter uses smoothing) for all voices must be
>   updated. The normalized value from the `MonoAutomation` and the voice's
>   normalized modulation offset must be added and converted back to a plain
>   value… The global value will have already been updated, so this event only
>   serves as a notification to update polyphonic modulation."
> - "When a voice ends… the plugin must send a `VoiceTerminated` to the host to
>   let it know that it can reuse the resources it used to modulate the value."

Plus the smoothing traps, which are real and non-obvious:

> "One caveat with smoothing is that copying the smoother like this only works
> correctly if it last produced a value during the sample before the
> `PolyModulation` event. Otherwise there may still be an audible jump…
> Finally, if the polyphonic modulation happens on the same sample as the
> `NoteOn` event, then the smoothing should not start at the current global
> value. In this case, `reset()` should be called with the voice's modulated
> value."

The host side needs its own voice pool, and CLAP says so
([`ext/voice-info.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/ext/voice-info.h)):

> "This extension indicates the number of voices the synthesizer has. It is
> useful for the host when performing polyphonic modulations, **because the
> host needs its own voice management and should try to follow what the plugin
> is doing**:
> - make the host's voice pool coherent with what the plugin has
> - turn the host's voice management to mono when the plugin is mono"

with `CLAP_VOICE_INFO_SUPPORTS_OVERLAPPING_NOTES` — *"Allows the host to send
overlapping NOTE_ON events. The plugin will then rely upon the note_id to
distinguish between them."*

### 4.5 Bitwig's unified modulation — the UX contract that shapes the data model

Verified from the
[user guide](https://www.bitwig.com/userguide/latest/the_unified_modulation_system/)
and
[Introduction to Modulators](https://www.bitwig.com/learnings/an-introduction-to-modulators-45/):

- **Modulators are devices** living in a modulator panel on *every* device —
  native, and VST/CLAP alike. Unlimited per device.
- **Relative bipolar depth**, not absolute: *"the modulation range is set
  relatively, the range displayed is also relative and does not directly
  correspond to the parameter's values. So you can twist the modulation range
  past the parameter's normal range."*
- **The base value stays live.** *"the modulated parameter's knob can still be
  used, allowing you to easily shift the modulation range"*, and the currently
  modulated value is displayed alongside.
- **Mono (blue) vs per-voice (green)** is a per-modulator toggle:
  *"Monophonic: Generates one control signal applied identically to all
  targets / Polyphonic: Produces unique signals per note event"*.
- **Modulators modulate modulators.** *"All modulator parameters — both those
  present atop the modulator slot and those within the additional parameters
  pane — can themselves be targets of modulations."*

**⊳ Judgment.** The load-bearing design decision is the third: **the base
value and the modulated value are distinct, and both are addressable.** A model
where modulation *writes into* the parameter is unrecoverable — you cannot drag
the knob while it is modulated, you cannot record automation of the base under
a live LFO, and you cannot display "what it would be" vs "what it is". Store
them separately, always.

### 4.6 The parameter *state machine* nobody documents — except CLAP

[`ext/param-indication.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/ext/param-indication.h)
enumerates it exactly:

```c
CLAP_PARAM_INDICATION_AUTOMATION_NONE       = 0,  // no automation for this parameter
CLAP_PARAM_INDICATION_AUTOMATION_PRESENT    = 1,  // automation exists but isn't playing
CLAP_PARAM_INDICATION_AUTOMATION_PLAYING    = 2,
CLAP_PARAM_INDICATION_AUTOMATION_RECORDING  = 3,
CLAP_PARAM_INDICATION_AUTOMATION_OVERRIDING = 4,  // user is touching it, overriding playback
```

plus `set_mapping(param_id, has_mapping, color, label, description)` for "a
physical controller is mapped here".

That five-state enum *is* touch/latch/write automation modes. Model it
explicitly rather than deriving it from booleans scattered across the UI.

### 4.7 The Rust answer to "how do params reach the DSP without allocating"

Firewheel's `Diff`/`Patch` system
([docs.rs](https://docs.rs/firewheel-core/latest/firewheel_core/diff/index.html))
is the most interesting new idea in this area. Verbatim from the module docs:

> "Diffing is the process of comparing a piece of data to some baseline and
> generating events to describe the differences. Patching takes these events
> and applies them to another instance of this data."

> "In typical usage, Diff will be called in non-realtime contexts like game
> logic, whereas Patch will be called directly within audio processors."

Fields are addressed by **indexed path**: for `struct { a, b: (bool, bool) }`,
paths are `[0]`, `[1,0]`, `[1,1]`. Arbitrarily deep, built by `PathBuilder`
during diff.

> "since the paths are built only during Diff, we can traverse them with high
> performance during Patch calls in audio processors."

Derive macros (`firewheel-macros`) generate both. The README calls it *"An
optional data-driven parameter API that is friendly to entity component
systems (ECS)."*

nih-plug takes the complementary approach — a declarative param struct with
**stable string IDs**
([`src/params.rs`](https://raw.githubusercontent.com/robbert-vdh/nih-plug/master/src/params.rs)):

```rust
fn param_map(&self) -> Vec<(String, ParamPtr, String)>;   // (param_id, ptr, group path)
```

> "The derive macro does this for every parameter field marked with
> `#[id = "stable"]`, and it also inlines all fields from nested child `Params`
> structs marked with `#[nested(...)]` while prefixing that group name…"

with `#[nested(id_prefix = "foo")]` (renames `bar` → `foo_bar`),
`#[nested(array)]` (indexes into `bar_1`, `bar_2`, …), and
`#[persist = "key"]` for non-parameter state serialised via Serde. And flags:
`BYPASS`, `NON_AUTOMATABLE`, `HIDDEN`, `HIDE_IN_GENERIC_UI`.

### 4.8 What we should adopt

**The data model.** A parameter is:

```
Param {
  id: ParamId,            // stable u32, never reused, per node
  name, unit, group_path, // schema — needed by MCP, generic UI, and remote controls
  range: Range,           // plain-value min/max + skew/curve
  flags: automatable | modulatable | per_voice(...) | stepped | enum | bypass | requires_process,
  default: plain,
}
```

and a parameter's **effective value** at sample `t` is:

```
plain(t) = clamp(
    range.denormalize(
        normalize(base(t))                 // base(t) = automation curve, or live override if OVERRIDING
        + Σ_r  routing[r].depth * source[r].value(t)      // global modulations, plain-value deltas
        + voice_offset(v)                                  // per-voice, from poly modulation
    ))
```

1. **CLAP's model verbatim** as the internal contract — plain values in the
   document, modulation as plain-value deltas,
   `(port, channel, key, note_id)` targeting with `-1` wildcards, gesture
   begin/end, sorted event lists, `DONT_RECORD` for CC-driven changes. Our CLAP
   adapter then becomes near-trivial, which is exactly what SCALABILITY §2
   already wants.
2. **Base and modulated values are separate and both readable.** Bitwig's core
   UX guarantee.
3. **Modulation routings are first-class objects** with stable IDs
   (`{id, source_node, source_output, target_node, target_param, depth,
   polarity, per_voice}`), living in the project, appearing in the op-log,
   undoable, scriptable, and addressable by an agent. Not a field on the
   parameter.
4. **The five-state automation indication enum**, modelled explicitly.
5. **Structural diff → indexed-path patch** for pushing node parameter state to
   the RT thread. Diff on the control thread, patch on the audio thread, no
   allocation, no per-field plumbing.
6. **Remote-control pages** (CLAP `remote-controls`: sections × pages ×
   8 params) as the *semantic* projection over an opaque plugin's parameter
   list — also the right surface to hand an LLM.
7. **Host-side voice pool** mirroring the plugin's `voice-info`, with
   `VoiceTerminated` handling, before shipping poly-mod.

**Trade-off.** Full per-voice modulation doubles the parameter machinery
(global path + per-voice path) and forces voice identity through the whole
event system. It is also the single feature that differentiates Bitwig from
Ableton. **⊳ Judgment:** build the *data model* with `note_id` in it now (it
costs 4 bytes per event and nothing else), implement per-voice evaluation
later. Retrofitting voice identity into an event system is a rewrite; carrying
an unused field is free.

---

## 5. Time model

### 5.1 Ardour's Temporal library is the reference implementation

Rewritten by Paul Davis 2020–2022. Four ideas, all verified from source.

**(a) Two time domains, one type, one bit.**
[`libs/temporal/temporal/timeline.h`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/temporal/timeline.h),
verbatim:

```cpp
/* A timepos_t designates an absolute position on the global timeline. It is
 * measured since time zero, and it can thus only be positive. It is in one of
 * two TimeDomains: AudioTime (wall time measured using superclock,
 * proportional to sample count) or BeatTime (counting musical ticks, which are
 * subdivided quarternotes or Beats). The ratio between these two is the tempo,
 * which might change over time. Conversion between these time domains is thus
 * non-trivial and will use the global TempoMap.
 *
 * Implemented using a 62 bit positional time value, a flag bit, and a sign bit.
 * ... If the flag bit is set (i.e. ::flagged() is true), the
 * numerical value counts musical ticks; otherwise it counts superclocks.
 */
class LIBTEMPORAL_API timepos_t : public int62_t
```

The comparison operators show why this is clever — same-domain comparisons are
free, cross-domain ones are explicitly named as expensive:

```cpp
bool operator< (timepos_t const & other) const {
    if (is_beats() == other.is_beats()) return val() < other.val();
    return expensive_lt (other);
}
```

**(b) A duration is not a number — it needs an anchor.**

```cpp
class LIBTEMPORAL_API timecnt_t {
   ...
   int62_t   _distance;
   timepos_t _position;   // where the distance is measured FROM
```

Because "four bars" is a different number of samples depending on where it
starts. This is the single most commonly-missed idea in DAW time models.

**(c) Integer-exact units chosen for factorisation.**
[`libs/temporal/temporal/types.h`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/temporal/types.h),
verbatim:

```cpp
/* This defines the smallest division of a "beat".
   The number is intended to have as many integer factors as possible so that
   1/Nth divisions are integer numbers of ticks.
   1920 has many factors, though going up to 3840 gets a couple more.
*/
static const int32_t ticks_per_beat = 1920;
```

`superclock_t` is an int64 counting sub-sample time units per second; the rate
is *session-persisted*
(`node.set_property (X_("superclocks-per-second"), superclock_ticks_per_second())`,
[`tempo.cc:3857`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/tempo.cc))
and a source comment notes it *"should probably be at the global level in the
session file because it is the time unit for anything in the audio time
domain."* The sibling constant shows the selection criterion explicitly:

```cpp
/* big number to allow most (fractional) BPMs to be represented as an integer "super note type per second"
 * It is not required that big_numerator equal superclock_ticks_per_second but since the values in both cases have similar
 * desired properties (many, many factors), it doesn't hurt to use the same number. */
const superclock_t big_numerator = 508032000; // 2^10 * 3^4 * 5^3 * 7^2
```

**⚠ Gap.** The commonly-cited default of 282,240,000 could **not** be confirmed
from source. What is confirmed is that the value is chosen for divisibility by
all common sample rates and is stored per session.

Conversions use integer muldiv, not floating point:

```cpp
static inline superclock_t superclock_to_samples (superclock_t s, int sr) { return PBD::muldiv_floor (s, sr, superclock_ticks_per_second()); }
static inline superclock_t samples_to_superclock (int64_t samples, int sr) { return PBD::muldiv_round (samples, superclock_ticks_per_second(), superclock_t (sr)); }
```

**(d) Tempo is stored as a period, in integers, not as BPM.**
[`temporal/tempo.h`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/temporal/temporal/tempo.h):

```cpp
superclock_t _superclocks_per_note_type;
superclock_t _end_superclocks_per_note_type;
...
Type type() const { return _superclocks_per_note_type == _end_superclocks_per_note_type ? Constant : Ramped; }
bool ramped () const { return _superclocks_per_note_type != _end_superclocks_per_note_type; }
```

BPM is a *derived, lossy view*:
`note_types_per_minute() { return ((double) superclock_ticks_per_second() * 60.0) / (double) _superclocks_per_note_type; }`.
A ramp is simply start-period ≠ end-period.

**(e) The tempo map is RCU'd with a thread-local cached pointer.**
`TempoMap::init()` → `_map_mgr.init(new_map)` → `fetch()`; `TempoMap::update()`
calls `update_thread_tempo_map()`. And Ardour's graph refreshes it per node
with a self-aware comment
([`graph.cc`](https://raw.githubusercontent.com/Ardour/ardour/master/libs/ardour/graph.cc)):

```cpp
/* Update the thread-local tempo map ptr.
 *
 * Doing this here is problematic, since it can result in each thread,
 * using a different tempo-map in a given cycle. And even different maps
 * in the same cycle for different routes.
 */
Temporal::TempoMap::fetch ();
```

**⊳ Judgment.** That comment is a warning, not a pattern. Zrythm does it right:
`GraphScheduler::run_cycle(time_nfo, remaining_preroll_frames, const ITransport&, const TempoMap&)`
passes the map explicitly for the whole cycle, with
`get_tempo_map_for_this_cycle()` available to nodes. **One tempo map per block,
chosen once, immutable for the block.** Do not fetch per node.

### 5.2 Tracktion's tempo sequence: precompute constant-tempo sections

From the ADC 2023 deck source listings, Tracktion's `Sequence` precomputes an
array of `Section`s, each carrying everything needed for O(1) conversion once
you have found the section:

```
Section { bpm, numerator, prevNumerator, denominator, triplets,
          startTime, startBeat, secondsPerBeat, beatsPerSecond,
          ppqAtStart, barNumberOfFirstBar, beatsUntilFirstBar, timeOfFirstBar, key }
```

Conversion is a reverse linear scan:

```cpp
inline BeatPosition toBeats (const std::vector<Sequence::Section>& sections, TimePosition time) {
    for (int i = (int) sections.size(); --i > 0;) {
        auto& it = sections[(size_t) i];
        if (it.startTime <= time) return it.startBeat + (time - it.startTime) * it.beatsPerSecond;
    }
    auto& it = sections[0];
    return it.startBeat + ((time - it.startTime) * it.beatsPerSecond);
}
```

Curved tempo ramps are handled by **subdividing into up to 100 constant
sections**:

```cpp
if (nextTempoValid && (currTempo.curve != -1.0f && currTempo.curve != 1.0f))
    numSubdivisions = static_cast<int> (std::clamp (4.0 * (tempos[tempoIdx].startBeat - currentBeat).inBeats(), 1.0, 100.0));
```

with the curve itself a one-control-point Bezier (`getBezierPoint`,
`getBezierEnds`, `getBezierYFromX` — the last solving a quadratic for `t` given
`x`). Bar numbering and `ppqAtStart` are accumulated forward across sections.
The whole sequence is hashed (`hash_combine`) for change detection.

**⊳ Judgment.** This is the practical answer to "how do you invert a ramped
tempo map exactly": *you don't* — you approximate the ramp with enough
piecewise-constant segments that the error is inaudible, and then every
conversion in both directions is exact linear arithmetic within a segment.
Ardour's alternative (integer supernote arithmetic with `muldiv_round`) is more
exact but much harder. For AURA, Tracktion's approach is the right
cost/benefit.

### 5.3 What CLAP puts on the wire

[`include/clap/events.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/events.h):

```c
typedef struct clap_event_transport {
   clap_event_header_t header;
   uint32_t flags;                  // HAS_TEMPO / HAS_BEATS_TIMELINE / HAS_SECONDS_TIMELINE /
                                    // HAS_TIME_SIGNATURE / IS_PLAYING / IS_RECORDING /
                                    // IS_LOOP_ACTIVE / IS_WITHIN_PRE_ROLL
   clap_beattime song_pos_beats;
   clap_sectime  song_pos_seconds;
   double tempo;
   double tempo_inc;                // tempo increment for each sample and until the next
                                    // time info event
   clap_beattime loop_start_beats,  loop_end_beats;
   clap_sectime  loop_start_seconds, loop_end_seconds;
   clap_beattime bar_start;         // start pos of the current bar
   int32_t       bar_number;        // bar at song pos 0 has the number 0
   uint16_t tsig_num, tsig_denom;
} clap_event_transport_t;
```

with fixed-point time
([`fixedpoint.h`](https://raw.githubusercontent.com/free-audio/clap/main/include/clap/fixedpoint.h)):

```c
static const CLAP_CONSTEXPR int64_t CLAP_BEATTIME_FACTOR = 1LL << 31;
static const CLAP_CONSTEXPR int64_t CLAP_SECTIME_FACTOR  = 1LL << 31;
typedef int64_t clap_beattime;
typedef int64_t clap_sectime;
```

— documented as values that *"will never change."* Note that this is the same
idea as Ardour's superclock: **int64 fixed point, not double.**

And separately on `clap_process`:

```c
// A steady sample time counter.
// ... Set to -1 if not available, otherwise the value must be greater or equal to 0,
// and must be increased by at least `frames_count` for the next call to process.
int64_t steady_time;
```

A monotonic wall-clock-ish counter *independent of song position* — needed
because song position jumps and loops while LFOs and delay lines must not.

**⚠ Gap: VST3 `ProcessContext`** — its field list and limitations were not
verified. Do not cite recollection.

### 5.4 Ableton Link — what it is, and specifically what it is not

From [ableton.github.io/link](https://ableton.github.io/link/), verbatim:

> "Link is different from other approaches to synchronizing electronic
> instruments that you may be familiar with. **It is not designed to
> orchestrate multiple instruments so that they play together in lock-step
> along a shared timeline. In fact, Link-enabled apps each have their own
> independent timelines.** The Link library maintains a temporal relationship
> between these independent timelines…"

> "With Link, any participant can propose a change to the session tempo at any
> time. **No single participant is responsible for maintaining the shared
> session tempo.**"

> "When a session is in a state of beat alignment, an integral value on any
> participant's beat timeline corresponds to an integral value on all other
> participants' beat timelines… For example, beat 1 on one participant's
> timeline might correspond to beat 3 or beat 4 on another's, but it cannot
> correspond to beat 3.5."

> "Link guarantees that session participants with the same quantum value will
> be phase aligned…"

> "As every application handles start and stop commands according to its
> capabilities and quantization, **it is not expected that applications start
> or stop at the same time.**"

So Link is: distributed agreement on (tempo, beat phase modulo quantum,
start/stop *intent*). It is **not** song-position sync, not state sync, not a
control protocol, not a clock master.

Newer: **Link Audio** shares named audio channels between peers, and lands on
the same identity rule as everything else in this document — *"Channel and peer
IDs are persistent identifiers that applications should use for tracking and
referencing specific audio streams, while names provide user-friendly
[display]."*

### 5.5 Loops and split cycles

Zrythm splits the block at loop points
(`process_chunks_after_splitting_at_loop_points`) and carries `buffer_offset_`
so each chunk knows where in the output buffer it lands. AURA's
`transport::crossing(base, frames, loop, point)` (ARCHITECTURE §2.6) is the
same primitive, and the "callback reports, control plane decides" rule around
it is, in my judgment, better factored than either Ardour's or Zrythm's — see
§11.

### 5.6 What we should adopt

1. **Two time domains, one tagged type.** AURA is currently *ticks for
   musical, samples for audio* as two separate concepts. Promote them to a
   single tagged `TimePos` (an `i64` with a domain bit, or a Rust enum — Rust
   gives us this without the bit-packing hack). It removes an entire class of
   "which unit is this field in" bugs and makes the schema self-describing.
2. **Durations carry their anchor.** `TimeCnt { distance, position }`. Not
   optional for musical durations.
3. **Store tempo as an integer period with start/end**, not as a BPM float.
   Ramps become "start ≠ end" for free; round-tripping through the file never
   drifts.
4. **Precompute a section table** (constant-tempo segments with cumulative
   time, beat, ppq, bar number), subdividing curved ramps. Rebuild it whenever
   the tempo map changes; hash it for change detection. Both directions are
   then exact and O(log n) with a binary search.
5. **One tempo map per block**, passed explicitly into the render, immutable
   for the block. Not a thread-local fetch.
6. **A monotonic `steady_time` sample counter separate from song position** —
   for LFO phase, delay lines, and any node that must not jump when the
   playhead does.
7. **Split the cycle at every discontinuity** (loop wrap, locate, tempo change
   if we support sample-accurate tempo), not just at loop ends.
8. **Keep PPQ 960 in the file, but make the internal tick resolution highly
   factorable.** 960 = 2⁶·3·5 is decent; 1920 adds one more factor of 2;
   consider carrying the file PPQ and an internal tick rate separately (Ardour
   does exactly this: `Beats::from_ticks_at_ppqn` converts, with a comment that
   it "is potentially lossy").

**Trade-off.** A tagged time type touches every signature in the codebase and
is miserable to retrofit — which is precisely why it must happen before v2 of
the project format, not after. Debt item **D-02** is already open on this; the
research says widen it from "ticks vs seconds" to "one tagged time type with
anchored durations", and do it in the same pass.

---

## 6. Consolidated recommendations, ordered by "expensive to retrofit"

| # | Decision | Why now | §|
|---|---|---|---|
| 1 | **Property-addressed ops as the single mutation vocabulary** (`Set{object, path, from, to}` + named structural ops) | undo, journal, delta-sync, MCP and future collaboration all become one mechanism; retrofitting is a rewrite of everything | §4, and the separate command/undo dossier |
| 2 | **Tagged time type + anchored durations + integer tempo periods** | touches every signature; must land before project format v2 | §5 |
| 3 | **Three identity tiers, named in code** (`ProjectId` / `Handle` / `Slot`), IDs never reused | agents and undo both hold references across time | §4.8, identity dossier |
| 4 | **`note_id` in the event model now**, poly-mod evaluation later | 4 bytes now vs. an event-system rewrite later | §4.3 |
| 5 | **Refcounted buffer pool, not preassigned indices** (change to current plan) | static assignment breaks the moment the graph goes multicore | §1.4 |
| 6 | **Latency nodes, not time-shift PDC** | Tracktion's dated verdict: time-shift fails on nested send/return | §1.5 |
| 7 | **Stable node IDs + adopt-old-graph handoff** | without it, every graph rebuild is an audible glitch | §1.6 |
| 8 | **Structural no-dealloc-on-RT enforcement** (newtype or `basedrop`) | the Rust-specific footgun that appears as the codebase grows | §3.5 |
| 9 | **Generate MCP/JSON/TS surfaces from one annotated op registry** | the difference between a surface that stays in sync and one that rots | §4.8 |
| 10 | **slotmap arenas for the model** | cheap now; `Rc<RefCell<>>` is a rewrite later | identity dossier |

Things that can safely wait: multicore scheduling (but design the schedule for
it — #5), poly-mod evaluation, silence masks, byte-bounded history, an OSC
surface.

---

## 7. Gaps — do not treat these as settled

1. **rossbencina.com was unreachable all session** (connection refused). The
   canonical RT-audio article is cited from the LWN summary only.
2. **Elk Audio OS / Twine, and ADC talk measurements on core scaling** —
   unverified. No numbers on "how many cores actually help".
3. **Denormals (FTZ/DAZ), xrun detection, RT priority mechanics per OS**
   (`thread_policy_set`, `AvSetMmThreadCharacteristics`, rtkit, PREEMPT_RT) —
   the JUCE/Tracktion *usage* was found, but no primary source on the
   mechanics.
4. **VST3 `ProcessContext`** — field list and limitations unverified.
5. **Ardour's default superclock ticks/second** — confirmed session-persisted
   and chosen for factorability; the specific default value is unconfirmed.
6. **Clip warping / time-stretch interaction with the tempo map** (Ableton warp
   markers, Bitwig, Ardour stretch) — no primary source obtained. The two-level
   session-time→clip-time mapping is judgment, not verified.
7. **Real-time collaboration in actual DAWs** — unverified across the board.
   Figma is the only fully verified comparator.
8. **Bitwig MCP servers, WavTool, any vendor-official MCP integration** —
   unverified.
9. **Dave Rowland's ADC 2017 ValueTrees talk** — video/title/speaker verified,
   slides and transcript not obtained (the architectural argument is
   independently evidenced in tracktion_engine source).
10. The research session's global **WebSearch budget (200 calls) was
    exhausted**; three of four delegated research streams did not return.
    Items 1–8 are closeable in a fresh session with the budget raised.

---

## 8. What this means for AURA

Three items are load-bearing enough to restate on their own.

### 8.1 SCALABILITY §1 is wrong about buffers — change it

`docs/SCALABILITY.md` §1 specifies **"preassigned buffer indices from a buffer
pool"**. That is static assignment, and it is correct *only* for a
single-threaded, fixed-order schedule. The moment the schedule executes on more
than one thread — Stage 2 of our own migration path — execution order is
nondeterministic, and a statically assigned buffer can be read by node A while
node B is writing it. The symptom is intermittent, load-dependent, and will be
mistaken for a plugin bug for weeks.

**Replace it with: a pre-reserved lock-free pool, with per-node
retain/release refcounts**, sized at compile time by walking the schedule
(Tracktion's `reserveAudioBufferPool`). Identical zero-allocation guarantee on
the RT thread, and it survives parallelism with no further change. This is a
one-paragraph edit to SCALABILITY §1 today and a rewrite of the schedule
executor if left until the multicore round.

### 8.2 §2.6 was independently validated — keep it absolute

ARCHITECTURE §2.6 states the rule that the audio callback **reports** that a
boundary was crossed and **never decides what that means**; the control thread
drains `engine_evt` and applies policy, then parks the playhead through the
`SharedRt::park` handshake.

Both reference engines leak policy into the RT thread where we do not. Ardour
refreshes the thread-local tempo map *inside* the graph run, with a source
comment admitting the consequence ("it can result in each thread using a
different tempo-map in a given cycle"). Zrythm halts the engine around graph
mutation and destroys the retired node collection *inside* the critical
section, so every graph edit is a potential dropout. Our seam is better
factored than either, and the research reviewer said so explicitly:

> "One thing AURA already has that most engines got wrong and had to fix later:
> the callback reports, the control plane decides (§2.6). Ardour and Zrythm
> both leak policy into the RT thread. Keep that rule absolute — it is why your
> engine will stay auditable as the feature set grows."

The corollary from §5.6 item 5 tightens it: **one tempo map per block, chosen
once by the control plane, immutable for the block.** Never a per-node fetch.

### 8.3 The AMEV record has no note id

`src-tauri/src/midi/events.rs` defines the 16-byte event record as
`[tick u32][duration u32][kind u8][key u8][velocity u8][channel u8][value f32]`.
The container mechanism is already right — `columnMask` with "old readers skip
unknown columns, never break" is the forward-tolerance lesson correctly learned
— but **there is no stable per-note identity in the record.**

Every one of the following needs one, and all four are on the roadmap:

- per-voice (polyphonic) parameter modulation, which addresses voices by
  `note_id` (§4.3);
- per-note expression, and MPE, for the same reason;
- overlapping note-ons on the same key, which CLAP's
  `SUPPORTS_OVERLAPPING_NOTES` resolves *only* by `note_id` (§4.4);
- undo and agent addressing of individual notes, which today would have to fall
  back to positional indexing — the exact design that produced a decade of
  wrong-note-moved bugs in Zrythm 1.

Adding a note-id column now costs **one bit in `columnMask`** and four bytes per
event. Adding it after projects exist costs a migration of every event chunk
ever written. A per-clip `u32` counter is sufficient — the address
`(ClipId, NoteId)` is globally unique because the clip already carries a UUID —
and it fits the fixed-record discipline the format already uses.

---

*End of dossier. Companions: `01-zrythm.md` (competitive/architectural
analysis), `03-command-undo.md` (mutation vocabulary, undo, identity),
`04-history-and-takes.md` (browsable history, extract-to-take, A/B).*
