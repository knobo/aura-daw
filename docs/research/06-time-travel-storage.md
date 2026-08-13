# Research 06 — Time-Travel Storage

**Date:** 2026-08-13
**Status:** research dossier, input to the architecture round. Not yet binding.
**Provenance:** literature survey plus **benchmarks run first-hand** on the
machine described in the Measurement Appendix. Every number in this document
was measured, not cited, unless it carries an external attribution.

---

## Why this document exists

The history feature we want — a browsable edit history where the user can
select any past point, hear it, and extract it into a take — has one hard
requirement that decides the whole storage design:

> **Materializing an arbitrary past revision must be cheap enough to do on
> hover.**

Everything else (bounded memory, survives save/reopen, no allocation on the RT
thread) is a constraint. This one is the *feature*. If jumping to a past state
costs a rebuild, the history panel becomes a progress bar and the feature dies
the way Blender's global undo did — see `docs/research/` on Blender's memfile
system, where the measured cost is ~97 % identity invalidation and ~3 % data.

This document answers: what data structure holds the session such that any
retained version is a pointer read, the memory bill stays bounded, and the
audio thread never pays for it.

It also corrects a specification we already wrote. **`docs/SCALABILITY.md` §3
currently specifies a flat copy-on-write rope of event chunks.** That structure
is asymptotically worse than the alternative by a factor that grows with
session size, and the correction is cheap now and expensive after the event
chunk file format ships. See "What this means for AURA".

---

## 1. The recommended storage design

### 1.1 Persistent above the RT boundary, dense and disposable below it

That sentence is the whole organizing rule.

**Persistent (immutable, structurally shared).** The session root is one
`Arc<Session>` per history node. Inside it:

| Data | Structure |
|---|---|
| Track / clip / lane maps | `imbl::HashMap<Id, T>` (HAMT) |
| MIDI + automation events | COW B-tree with dense chunk leaves (§1.2) |
| Plugin state | `Arc<StateBlob>` keyed by BLAKE3, shared by pointer between versions |
| Audio assets | `AssetRef { blake3, path, len }` — references only, never bytes |

**Mutable / non-persistent.** Object *identity* — a generational
`slotmap`-style ID allocator living **outside** the versioned session. The
compiled `Arc<RenderGraph>` the RT thread renders from, which is derived,
disposable, and rebuilt on publish. Audio sample data, in content-addressed
files on disk. All regenerable caches (waveform LODs).

**The identity/data split is the part people get wrong**, so state it
explicitly:

> **The arena is an ID allocator, not a store.**

A `SlotMap` cannot be the versioned container, because cloning one is an O(n)
deep copy. But it is the right *identity* mechanism, and it composes cleanly
with persistent maps keyed by those IDs. Three consequences fall out:

* **Undo never un-allocates an ID.** Undoing a track creation leaves that ID
  unreferenced by the current version, while the history node that created it
  keeps referencing it. No resurrection problem, no ID reuse.
* **Generation counters guarantee a stale ID can never alias a new object** —
  the failure mode that produced Zrythm 1.x's decade of wrong-note-moved bugs.
* **The allocator needs no history of its own.** It is rebuilt from a
  high-water mark at load.

And because IDs are dense `u32`/`u64`, they are exactly what the RT thread
wants (ARCHITECTURE §10.3).

### 1.2 The bulk structure: a COW B-tree over dense chunk leaves

Not `imbl::Vector`. Not a flat chunk list. A **persistent B-tree whose leaves
are dense arrays of ~128 records and whose interior nodes carry summaries**
(count, min/max tick, min/max key) — a `SumTree`, in Zed's vocabulary.

**Chosen parameters: leaf 128, branch 32**, for 16-byte event records.

Three properties, all measured, none of which any off-the-shelf crate gives you
together:

| Property | Measured |
|---|---|
| Sequential scan | **442 µs vs 395 µs for `Vec`** at 10⁶ events — **1.12×** |
| Retained cost per version (point edit) | **4.1 KB** |
| Viewport range query over 1 % of 10⁶ events | **8.8 µs** |

The scan number holds because scanning walks leaves linearly and pointer-chasing
amortizes over 128 contiguous records. The range query is the piano-roll query,
answered *for free* out of the same structure that gives you history, because
interior summaries let you skip whole subtrees. **An `imbl::Vector` cannot do
this at all.**

#### The flat-list correction

A `Vec<Arc<Chunk>>` — the shape most people reach for, and the shape
`SCALABILITY.md` §3 specifies — must copy the whole outer index vector on every
edit. Per-version cost is therefore **Θ(√N)**.

Measured at N = 10⁶ events over 10 000 retained versions:

| Chunk size | Bytes retained per version |
|---|---|
| 64 | 126 160 B |
| **512 (the flat structure's own optimum)** | **23 969 B** |
| 1024 | 24 300 B |
| 4096 | 67 604 B |

The optimum is real — chunk ≈ √(N/2) ≈ 707, cost ≈ 2√(8·16·N) ≈ 24 KB, which
matches the measurement — but it is still 24 KB. **A tree over the same chunks
gets 4.1 KB: 5.8× better, and the gap widens with N**, because the tree is
Θ(log N) and the flat list is Θ(√N).

Independent confirmation from a different domain: **bup** builds a recursive
fanout tree of git tree objects rather than a flat list of chunk hashes, with
the documented result that *"the number of changed git objects is **O(log n)**
where n is the number of chunks"* ([bup
DESIGN](https://github.com/bup/bup/blob/master/DESIGN)). Same finding, arrived
at from backup software rather than from editors.

### 1.3 Where snapshots live, and how a past revision is materialized

**In RAM, one materialized `Arc<Session>` per history node.** There is no
snapshot *cadence* to tune, because there is no replay on the critical path.

```rust
struct HistoryNode {
    parent:   Option<NodeId>,
    children: SmallVec<[NodeId; 2]>,  // branching costs ~6 KB/node, so it is free
    session:  Option<Arc<Session>>,   // None once coarsened to the cold tier
    ops:      OpBatch,                // always retained
    label:    CompactString,          // "Quantize 240 notes"
    at:       Timestamp,
    bytes:    u32,                    // incremental retained cost, drives the budget
}
```

Measured costs of the operations the UI performs:

| Operation | Cost |
|---|---|
| Materialize an arbitrary past state | **7.9 ns** (a pointer read) |
| Read one track out of it | **23.7 µs** |
| Flatten a 20 000-event track into a dense RT-ready `Vec` | **29.3 µs** |

This is the design's central bet, and it is worth stating plainly:

> **The "well under a second" requirement is not a performance problem to be
> solved by clever snapshotting. It is a consequence of never destroying the
> state in the first place.**

Event sourcing with periodic snapshots solves a problem you only have if you
deleted the answer.

Note also that `children: SmallVec<[NodeId; 2]>` makes the history a DAG at
essentially zero cost (~6 KB/node). Branching is free; what it costs is UI
legibility, which is a separate problem with a separate answer (REAPER
annotates the branch point with `(*2)` rather than drawing a graph).

### 1.4 Two layers: the op log and the blob store

They are **separate, separately garbage-collected, and neither is derived from
the other on the critical path.** This is not the split most people assume when
they say "hybrid".

**The op-log layer carries *semantics*.** Every history node retains its
`OpBatch` permanently, even after its materialized session is evicted. The op
log serves the journal (`journal.ndjson`, fsync'd on debounce), crash recovery,
the MCP/agent front door, multi-window sync, and any future collaboration. It is
also the **cold tier**: an evicted node is reconstructible by replaying ops from
the nearest retained ancestor. Measured: **10 000 point-edit ops replay in
17.8 ms** — perfectly adequate for that role, and hopeless as a primary
mechanism for the reasons in §5.

**The blob layer carries *bytes*.** Recorded takes, rendered stems and plugin
state live in `objects/<hh>/<blake3>`, content-addressed and immutable. History
nodes hold references, never bytes.

Deletion is by **reachability from {saved project} ∪ {all retained history
nodes, including abandoned branches}**, plus a **recency veto** — git's rule,
worth copying exactly: any object newer than the prune date survives regardless
of reachability.

Plugin state gets the same content-addressing but is snapshotted **per gesture,
not per op** — a knob drag must not write 2 MB per frame. Between gesture
boundaries, history nodes share the previous `Arc<StateBlob>`.

#### The four-tier blob deletion policy

The question "the user undid a recording — when do we delete the WAV?" has four
tiers, and the first one is the important one:

1. **Live undo of a take: do nothing.** The file stays intact; the history node
   holding it stays alive. Cost: one file. **A musician who undoes a take and
   then realizes bar 3 was the good one must get it back. Non-negotiable.**
2. **History eviction never deletes blobs.** Evicting a node drops its in-memory
   session. Blob deletion is a separate, slower, save-time process.
3. **At save: mark, sweep, but move to trash.** Unreachable blobs go to
   `.aura/trash/` with a timestamp, and the user is told: *"3 unused recordings
   (412 MB) moved to project trash."*
4. **Trash expires** after a grace window (default 14 days) or on explicit
   "Clean Project".

### 1.5 The memory budget model

GIMP's model, whose semantics are exactly what we want: **a hard ceiling in
bytes with a floor in steps that overrides it.**

```
history_min_steps: usize = 100                        // guaranteed, overrides the ceiling
history_max_bytes: usize = min(1 GiB, physical / 8)   // GIMP uses physical/8
```

Each node is charged its *incremental* retained bytes. When the ceiling is
exceeded **and** node count exceeds the floor, evict in this order:

1. Coarsen (drop `session`, keep `ops`) the oldest nodes on **abandoned
   branches** first.
2. Then the oldest on the current branch.
3. Then merge adjacent coarsened nodes into spans.
4. Then drop spans entirely, oldest first.

**Always keep the newest step even if it alone blows the budget** (Blender's
rule — the limit is applied *after* the push, so a push never fails for budget
reasons). **Never let eviction touch blobs.**

#### Coalescing matters more than the budget

A knob drag at 200 ops/s produces 12 000 ops/minute. **A gesture must create
exactly one history node.** Without that rule no budget survives; with it, the
budget is not the binding constraint.

#### What the budget buys

Measured on a 10-million-event, 500-track session:

| Retained versions | Memory |
|---|---|
| 1 000 | ≈ 6 MB |
| 20 000 | 113 MB |
| 50 000 | 294 MB |
| **≈ 85 000** | **512 MB** |

After coalescing, 85 000 retained edit steps is a very long working day.

### 1.6 RT integration, and the deallocation hazard

The RT thread never touches the persistent session. The control plane holds it
behind a `parking_lot::Mutex`, compiles a dense `Arc<RenderGraph>` from a chosen
session version, and publishes it by the existing pointer swap over the
wait-free `rtrb` queue (ARCHITECTURE §2.3). **This is additive to a decision
AURA has already made and already enforces** — the persistent-structure work
slots in behind that boundary without changing the RT contract at all.

**The one real trap is deallocation, not allocation.** If the RT thread drops
the last `Arc` to a retired graph, the refcount cascade runs there. Measured:

| Drop | Cost |
|---|---|
| Newest single version | **45 ns** (harmless) |
| Base tree | **0.60 ms** |
| Entire history, 10⁶ events | **6.32 ms** |
| Entire history, session scale | **83.8 ms** |

Against a **2.6 ms deadline** at 48 kHz / 128 frames, the last figure is up to
**32 buffers of stall**.

> **A janitor thread is mandatory infrastructure, not a nicety.**

Return retired `Arc`s over the existing SPSC queue; keep `basedrop` in reserve
for node and plugin state with less predictable lifetimes. This is the same
conclusion the RT-undo research reached from the correctness direction, and the
same one Ardour reached the hard way — its `rcu.h` carries an `#if 0`'d
"free it now if nobody else holds it" optimization that produced a
heap-use-after-free of the `RouteList`.

---

## 2. Prior art: blob stores and retention

Collected because the blob layer is the part with the most existing practice
and the most expensive failure mode.

**BLAKE3 / Bao — content addressing with verified random access.** Chunk size
is exactly **1024 bytes** (`CHUNK_LEN` in the reference implementation). Bao's
combined-encoding overhead is **~6.25 %** — a measured 1 000 000-byte input
yields a **1 062 472-byte** encoding, or a **62 472-byte** outboard file.
Verified random access costs O(log n) parent nodes. Two spec caveats to carry:
the encoding is **malleable** (multiple encodings can decode to the same input
under the same hash — so **never hash the encoding**), and a decoder *"must not
expose the length to the caller in any way before the final chunk is
validated."* The BLAKE3 paper §7.1 validates the small-chunk instinct directly:
*"verified streaming requires buffering chunks, and incremental updates require
re-hashing chunks, so both of those use cases benefit from a smaller chunk
size."*

**Why git chokes on binaries.** `core.bigFileThreshold` defaults to **512 MiB**;
above it, files are "stored deflated in packfiles, without attempting delta
compression". The sharper statement is bup's:
*"The primary reason git can't handle huge files is that it runs them through
xdelta, which generally means it tries to load the entire contents of a file
into memory at once… xdelta works great for small files and gets amazingly slow
and memory-hungry for large files."*
([bup DESIGN](https://github.com/bup/bup/blob/master/DESIGN))

**Content-defined chunking parameters, if ever revisited** — not recommended for
v1; whole-file content addressing captures essentially all the available dedup
for append-only takes:

| System | Min | Avg | Max |
|---|---|---|---|
| restic | 512 KiB | **1 MiB** | 8 MiB |
| borg | 512 KiB | ~2 MiB | 8 MiB |
| casync/desync | 16 KB | **64 KB** | 256 KB |
| bup | — | 8192 B | 32 KiB |

borg documents the index cost, which is the number that would actually bind:
**~84 bytes of RAM per unique chunk** (`chunk_count * 44` cache +
`chunk_count * 40` index). And note bup's honesty about its own constant:
*"(Why 13 bits? Well, we picked the number at random and… eugh.)"* — nobody's
chunk size is sacred, including ours.

**Perforce `+Sn` is exactly the per-asset bounded-history primitive.**
`+S<n>` (n ∈ 1–10, 16, 32, 64, 128, 256, 512): *"Only the most recent n
revisions are stored. Older revisions are purged from the depot upon submission
of more than n new revisions, **or if you change an existing +Sn file's n to a
number less than its current value**."* And `p4 obliterate` explicitly
**preserves** archive files still referenced by lazy copies — a refcount by
another name.

**Ableton's reachability bug is a warning for our GC root set.** From the Live
manual: *"Live considers any file in the Project which is not referenced by its
Sets, clips, or presets as unused — **even if the file is actively used in other
Projects**."* If AURA ever shares a pool across projects, mark-and-sweep from one
project's history will delete another project's audio. Design the root set
accordingly.

**Pro Tools' "rename to pin" is a zero-UI pinning mechanism worth stealing.**
Auto-created clips are GC-eligible and bulk-deletable; *"To ensure that you keep
any specific auto-created clips, **rename them**. When you name a clip, it is
promoted from being an auto-created clip to a user-defined clip."* Naming a take
makes it exempt. That maps exactly onto "the user undid a recording but had
labelled it first."

Pro Tools' **Compact** also takes a **padding in milliseconds** parameter so
crossfades and future trims survive — the GC keeps a margin around live regions.
Worth copying for any destructive audio reclamation.

**Automerge is the instructive negative example.** Its binary format spec states
*"Automerge stores the full history of changes to the document: this is a large
amount of data but in practice it is very repetitive and amenable to
compression"* — and contains **no provision for pruning at all**. "Never delete,
just compress" works for text ops and fails immediately for 200 MB of audio.
This is the cleanest argument for why the op-log DAG and the refcounted blob
store must be **two separately-GC'd layers**.

---

## 3. The three falsifying benchmarks

Each has a threshold. If the measured value exceeds it, the design is wrong in
the stated way.

### Falsifier 1 — bulk-op amplification (**the design's biggest risk**)

**Statement of the risk.** Every per-version number in this document comes from
*single-point* edits, where copy-on-write touches exactly one leaf and one node
per level. That is the easy case, and it is the only case measured. Real DAW
gestures are not all point edits.

> **A quantize over 100 000 notes rewrites ~780 leaves ≈ 1.6 MB in a single
> history node — roughly 400× the 4.1 KB mean.**

Transpose a pattern, humanize a track, delete 5 000 clips: same shape. If bulk
ops are common in real use, the budget arithmetic collapses — 512 MB buys
hundreds of steps rather than 85 000 — and the whole "retain everything,
materialize in nanoseconds" bet fails.

**The measurement.** On a 10⁷-event / 500-track session, execute a realistic
gesture mix — single-note edit, note insert, note delete, quantize 10 000 notes,
transpose a 50 000-note pattern, delete 5 000 clips, humanize a whole track,
plugin param gesture, plugin state change — and record bytes retained and µs per
op for each.

**Threshold: mean ≤ 8 KB/node, p99 ≤ 2 MB/node.**

**If falsified:** bulk ops stop retaining a materialized snapshot and become
replay-only nodes (store the op plus its inverse; materialize on demand). That
is a genuine, deliberate retreat toward event sourcing for exactly the class of
ops where replay is cheap and deterministic — which, for pure MIDI transforms,
it is. Pair this with an **Emacs-style per-op outer limit** that warns the user
when a single operation blows the budget (`undo-outer-limit` is the only one of
Emacs' three tiers that notifies).

### Falsifier 2 — RT publish and retirement

**The measurement.** With a real audio device at 48 kHz / 128 frames, click 200
random history nodes. Record (a) selection → first buffer rendered from the new
graph, and (b) xrun count, running under `assert_no_alloc` with
`disable_release` turned **off** — its default silently disables the check in
release builds, which makes the whole exercise theatre. **Include the retirement
path, not just the publish path.**

**Threshold: p99 ≤ 100 ms end-to-end, zero xruns, zero allocation assertions.**

**Why it falsifies:** the 7.9 ns materialization is meaningless if compiling and
publishing the render graph — or retiring the old one — stalls the callback.
This is the one interaction that could not be tested in isolation.

**If falsified:** the graph compiler is the bottleneck, not the storage. Make
the compiler incremental — diffing two session versions is cheap precisely
because structural sharing turns "what changed" into a pointer comparison.
**Do not change the storage model in response.**

### Falsifier 3 — save, reopen, and the undone recording

**The measurement.** A 10 GB project with 50 000 history entries, including 20
recorded takes of which 8 were undone. Save, quit, reopen. Then measure (a) time
to materialize the current state, (b) time to materialize a node from the middle
of the history, and (c) redo back to an undone take and **verify the audio
plays**.

**Threshold: (a) ≤ 2 s, (b) ≤ 1 s, (c) must succeed — no exceptions.**

**Why:** (c) is not a performance test. It is the correctness test that the
entire blob-GC policy exists to pass, and it is the one that a naive "sweep
unused pool files at save" implementation fails. **If the history says a take is
reachable and the file is gone, the product has destroyed a musician's work.**

**If falsified on (a)/(b):** persist materialized snapshots at coarse intervals
(~every 500 nodes) alongside the journal. This is the one place where classic
snapshot-cadence tuning genuinely applies.
**If falsified on (c): it does not ship.** There is no acceptable trade.

---

## 4. Measurement appendix

All measured first-hand.

**Environment:** Intel Core i9-14900, Linux 6.8, rustc 1.94.1, `--release` with
`lto = true`, `codegen-units = 1`.

**Record shape:** 16 bytes — `tick: u32`, `kind/key/vel/pad: u8×4`, `dur: u32`,
`val: f32`.

**Reproducible crate:**
`/tmp/claude-1000/-home-knobo-prog-dav/aa81f388-79df-4119-9ec0-23203677c063/scratchpad/pdsbench/`
(binaries: `pdsbench`, `hist`, `btree`, `insert`, `session`).

### 4.1 Read cost, 10⁶ events — the constant-factor penalty, honestly

| Structure | Sequential scan | 10⁶ random indexed reads |
|---|---|---|
| `Vec<Ev>` | 395 µs (1.0×) | 3.21 ms (1.0×) |
| Flat COW rope (512) | 369 µs (0.93×) | 6.53 ms (2.0×) |
| **COW B-tree (128/32)** | **442 µs (1.12×)** | 65.5 ms (20×) |
| `imbl::Vector` | 1 579 µs (4.0×) | 69.8 ms (22×) |
| `rpds::Vector` | 4 833 µs (12.2×) | 43.3 ms (13×) |

**Maps:** `std::HashMap` 10⁶ lookups = **44.9 ms**; `imbl::HashMap` =
**120.5 ms (2.7×)**.

### 4.2 Mutation and snapshot, 10⁶ events

| Structure | One point update | Clone (= snapshot) |
|---|---|---|
| `Vec` (clone + set) | 1.487 ms | 1.280 ms |
| `rpds::Vector` | **283 ns** | ~0 ns |
| `im::Vector` | 1.74 µs | 47.9 ns |
| `imbl::Vector` | 2.44 µs | 48.6 ns |
| `imbl::HashMap` | 2.79 µs | 12.1 ns |
| COW chunk rope (4096) | 3.85 µs | 9.3 ns |

### 4.3 Retained-version memory, 10⁶ events × 10 000 retained versions

| Structure | Bytes per version |
|---|---|
| `Vec` deep copy | 16 080 056 B |
| Flat rope, chunk 64 | 126 160 B |
| **Flat rope, chunk 512 (optimum)** | **23 969 B** |
| Flat rope, chunk 4096 | 67 604 B |
| `imbl::Vector` | 5 935 B |
| `rpds::Vector` | 1 398 B |
| COW B-tree 64/16 | 2 700 B |
| **COW B-tree 128/32** | **4 114 B** |
| COW B-tree 256/32 | 6 053 B |
| COW B-tree 512/32 | 10 109 B |

### 4.4 B-tree tuning sweep

| Leaf / branch | Bytes/version | µs/edit | Scan | Range query |
|---|---|---|---|---|
| 64 / 16 | 2 700 B | 1.16 µs | 630 µs | 11.6 µs |
| **128 / 32 ← chosen** | **4 114 B** | **1.81 µs** | **442 µs** | **8.8 µs** |
| 256 / 32 | 6 053 B | 2.37 µs | 406 µs | 10.0 µs |
| 512 / 32 | 10 109 B | 4.09 µs | 368 µs | 10.0 µs |

### 4.5 Session scale — 500 tracks × 20 000 = 10⁷ events, 160 MB payload

| Measurement | Value |
|---|---|
| Build | 61.9 ms |
| RSS | 166.6 MB = **16.86 bytes/event all-in, only ~5 % over raw payload** |
| Full-session scan (render/export) | 11.0 ms |
| Flatten one track | 29.3 µs |
| 20 000 retained versions | 57.5 ms total, 2.87 µs/edit, 113.3 MB, 5 943 B/version |
| Select a past version | **7.9 ns** |
| Read one track of it | 23.7 µs |
| Drop whole history | 24.7 ms |

At **1000 tracks × 10 000 with 50 000 versions**: 293.8 MB, 6 162 B/version,
3.11 µs/edit, drop 83.8 ms.

**Naive full-copy equivalent: 3.2–8.0 TB.**

### 4.6 Sorted insert — 10⁶ events, 10 000 inserts

Correctness verified: 1 010 000 events, still sorted.

| Approach | Cost |
|---|---|
| `Vec::insert`, mutable, **no history** | 244.23 µs/insert |
| COW B-tree, **all 10 000 versions retained** | **2.13 µs/insert, 3 528 B/version** |

**114× faster — while also retaining every version.**

### 4.7 Drop cascade

| Drop | Cost |
|---|---|
| Newest single version | 45 ns |
| Whole 10 000-version history | 6.32 ms |
| Base tree | 0.60 ms |

### 4.8 Replay (the cold tier)

10 000 point-edit ops replay in **17.8 ms**.

---

## 5. Verdict: persistent structures vs. event sourcing with snapshots

### 5.1 On the constant factor, honestly

The folklore is that persistent structures cost a 2–10× constant factor, so you
should only use them if you genuinely need persistence. **For random indexed
access that folklore is not just true, it is understated: measured 13–22× on
off-the-shelf persistent vectors, and 20× on our own tree.**

And `imbl::HashMap` is **2.7× slower** than `std::HashMap` at 10⁶ entries,
which flatly contradicts the `im` crate's own documentation claiming the two are
"almost neck and neck". That claim does not survive a cold cache at scale.
Anyone who stores notes in an `imbl::Vector` and indexes into it from a UI
hit-test loop will get exactly what they deserve. **Write that constraint into
the module docs, because someone will try.**

The `im` authors are honest about the rest of it, and their sharpest line is the
one that matters: *"if you never clone the data structure, the data inside it is
also never cloned, and in this case it acts just like a mutable data
structure."* You pay the constant factor whether or not you use persistence. So
the question is never "is this slower than `Vec`" — it is **"do I clone often
enough that the amortized cost inverts."**

**But the usual argument does not survive contact with this specific workload,
and it fails in two directions.**

**First: the penalty applies to *random indexing*, and a DAW mostly doesn't do
that.** It does sequential scan (playback, render, export) and range queries
(piano-roll viewport). A chunked structure scans at 0.93–1.12× `Vec` speed and
answers a 1 %-viewport range query over a million events in 8.8 µs.
`imbl::Vector` is 4× slower to scan because its 64-element leaves go through RRB
focus machinery on every access; `rpds::Vector` is 12× slower. **This is why we
should build the tree rather than adopt a crate** — the crates are
general-purpose sequences, and their generality is precisely what costs the scan
performance and denies the summary-based range query.

**Second, and more surprising: for sorted insert into a large event list, the
persistent structure is 114× *faster* than the mutable one, while also retaining
every version for free.** `Vec::insert` at 10⁶ events memmoves ~8 MB per
operation — 244 µs. The COW chunk tree does 2.13 µs. **Inserting and deleting
notes is what a piano roll *is*.** So on the operation that dominates MIDI
editing, there is no trade-off to make: the persistent structure wins outright
and hands you history as a side effect.

### 5.2 Crate recommendations

| Crate | Verdict |
|---|---|
| `im` | **Do not use.** Last published 2022-04-29, effectively unmaintained, carrying an unpatched `OrdSet` soundness advisory (**RUSTSEC-2023-0126**). |
| `imbl` (7.0.1, published 2026-07-18) | **Use for the identity maps.** Maintained fork. |
| `rpds` (1.2.1) | Better *vector* if you only ever do point updates — beat `imbl` by 9× on update (283 ns vs 2.44 µs), because it is a plain bitmapped trie with no RRB relaxation and no focus cache to maintain. |
| The bulk structure | **Ours.** No crate provides scan + summary range query + Θ(log N) versioning together. |

### 5.3 On event sourcing

**Not the primary mechanism.** Three reasons, in increasing order of severity.

**The weakest reason is speed, and it is the reason people usually give, and it
is the wrong one.** Replaying 10 000 point-edit ops into the tree takes
**17.8 ms**. A pure event-sourced design with snapshots every 10 000 ops would
materialize MIDI edits in well under 100 ms. **Event sourcing is not
disqualified by replay speed for data ops.** Anyone who says otherwise hasn't
measured it.

The real reasons are the other two.

**Non-determinism.** A third-party plugin is exactly the "external system"
Fowler warns about: *"if these events cause update messages to be sent to
external systems, then things will go wrong because those external systems don't
know the difference between real processing and replays."* Replaying "set param
17 to 0.6" against a plugin with internal state machines, modulation and
undocumented smoothing does not reliably reconstruct the sound. And the non-data
ops — load a 3-minute WAV, instantiate a plugin and push 2 MB of opaque state,
rebuild the render graph — are not 1.8 µs each; **they dominate.**

**Schema-evolution tax.** History that survives save/reopen across app versions
means every historical op must remain replayable by every future version,
forever. That tax compounds, and it is the thing event-sourced systems are
famous for regretting.

### 5.4 The synthesis

Keep both layers, always, for every history node — and note that this is **not**
the split most people assume when they say "hybrid". The op log is not there to
reconstruct state on demand, and the tree is not there as a periodic
optimization of the log.

> **The op log is retained for semantics, durability and interoperability.
> The structurally-shared tree is retained for materialization.
> Neither is derived from the other on the critical path.**

The log serves the journal, crash recovery, the agent front door, multi-window
sync and collaboration. The tree serves the 7.9 ns jump to any point in history.

**Event sourcing earns its keep as the cold tier** — the mechanism for
reconstructing nodes whose materialized snapshot the budget evicted — where its
latency is acceptable and its determinism problems are bounded by the fact that
plugin state comes from a retained blob rather than from replay.

---

## 6. Accepted trade-offs

* **13–22× slower random indexed access**, mitigated by materializing a dense
  `Vec` (29 µs/track) wherever true random access is needed.
* **~5 % memory overhead** on bulk event data (16.86 bytes/event all-in against
  a 16-byte record).
* **Reopened cold history is slower than live history.**
* **Plugin state is snapshotted per gesture**, so intra-gesture plugin states
  are unrecoverable.
* **No content-defined chunking of audio in v1** — whole-file content addressing
  captures essentially all the available dedup for append-only takes, and CDC's
  complexity should wait for evidence.
* **A janitor thread is mandatory new infrastructure.**
* **`imbl` sits on the critical path** for identity maps — mitigated by the fact
  that it holds only hundreds-to-thousands of entries and could be swapped out
  in a day.

---

## What this means for AURA

**1. `SCALABILITY.md` §3's chunk-rope specification should become a summarising
COW B-tree, and it should change before the event-chunk file format ships.**

§3 currently reads:

> "Per pattern: a **sorted struct-of-arrays chunk list** … immutable chunks of
> ~4k events with a small mutable tail — i.e. a persistent/copy-on-write rope of
> event chunks."

The *reasoning* in §3 is right — cache-linear playback, undo by keeping old
chunk references, range queries from per-chunk `[minTick, maxTick, minKey,
maxKey]` summaries, described there as "an implicit interval tree". The
correction is that **an implicit interval tree should be an explicit one.** Put
the summaries in interior nodes instead of scanning a flat vector of chunks, and
the same design gets Θ(log N) instead of Θ(√N) versioning, 4.1 KB instead of
24 KB per retained version, and an 8.8 µs viewport query instead of a linear
chunk scan. The chunk size also wants to move: **~4 k events is 3–4× worse than
128** on retained bytes.

This is a cheap change now — the AMEV chunk format is a *file* format and the
tree is an *in-memory* structure, so they are separable — and an expensive one
once projects exist on disk with the flat layout assumed by readers.

**2. The RCU boundary AURA already enforces is exactly the right seam**, and
none of this changes the RT contract. The persistent session sits on the control
side of ARCHITECTURE §2.3's prepare-then-pointer-swap rule.

**3. The janitor thread is a new, mandatory piece of infrastructure**, and it is
the same conclusion the RT-undo research reached independently. Retiring a
history is measured at up to 83.8 ms — 32 buffer periods. Nothing about this is
optional.

**4. Note identity is a prerequisite**, and it is tracked elsewhere: none of the
per-version economics work if events are addressed positionally, because a
structural diff between two versions degenerates when identity is an index. The
AMEV `columnMask` mechanism already gives us the additive path.
