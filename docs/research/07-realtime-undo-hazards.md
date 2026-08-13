# Undo, Redo and Time-Travel Against a Hard Real-Time Audio Thread

Research dossier · 2026-08-13 · owned by the architecture round

---

## Why this document exists

AURA is about to grow an op-log, an undo stack, and — if the take/history
design lands — the ability to materialise and *render* an arbitrary past
revision. Every one of those features mutates state that a hard real-time
audio callback is concurrently reading.

The failure modes here are not the ordinary kind. They are silent, they are
timing-dependent, they usually hide in development and fire under load, and
several of them destroy user work rather than merely crashing. Worse, most of
them survive a *perfectly correct* RCU snapshot swap: the snapshot mechanism
solves the memory-safety problem and solves the musical-continuity problem not
at all. Those are separate designs and this document keeps them separate.

Three shipping projects solve this three different ways, and the differences
are instructive rather than arbitrary:

* **Zrythm stops the world.** Every undoable action pauses the engine, sends
  MIDI panic, waits for the in-flight cycle, mutates, rebuilds the graph under
  the engine lock, resumes.
* **Ardour never recreates objects at all.** Its undo model is explicitly
  restricted to changing the state of *existing* objects, paired with an RCU
  plus a lazily-drained "dead wood" list and a butler thread that performs
  deletions on behalf of the RT thread.
* **openDAW** is closest to AURA's architecture — a transactional box graph on
  the main thread, synced forward-only into a Rust engine — and its bug tracker
  is a catalogue of exactly these hazards.

Meanwhile **JUCE** converged independently on the same shape AURA is building,
and its `RenderSequenceExchange` is forty lines that answer the grace-period
question cleanly and *fail-safely*.

### Provenance and confidence

Statements are tagged **[FACT]** (with citation) or **[JUDGMENT]** (analysis,
not sourced). Ardour, Zrythm, openDAW, JUCE, Mixxx, VCV Rack, CLAP, VST3,
`basedrop`, `crossbeam-epoch`, `arc-swap`, `pluginval`, `clap-validator`,
`loom`, `proptest`, `rtsan` and Figma are **verified primary sources with
quotes** — mostly read as source through the GitHub API rather than as
summaries. REAPER, Pro Tools, Logic, Cubase and Bitwig internals are **not
verified** and are flagged as such at every point of use rather than dressed
up. §10 lists every unverified claim in one place.

One caveat inherited from the research pass: quoted code was extracted by a
fetching model. Spot-check exact wording against the linked URL before pasting
a quote anywhere public.

---

## 1. What actually goes wrong when a structural undo happens during playback

### 1.1 The seven failure modes

#### (a) Dangling audio buffers — freed arrays still referenced by the RT thread

The control thread frees the old snapshot while a callback that already loaded
the old pointer is still mid-block.

*Mechanism that prevents it:* RCU discipline — **provided reclamation is gated
on a proven condition, not on hope.**

There is a spectacular cautionary tale here, and it is the single most valuable
artifact in this entire research pass. Ardour's `SerializedRCUManager::update()`
contains this, verbatim **[FACT]** —
[`libs/pbd/pbd/rcu.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/rcu.h):

```cpp
#if 0 // TODO find a good solition here...
        /* if we are not the only user, put the old value into dead_wood.
         * if we are the only user, then it is safe to drop it here.
         */
        if (1 != _current_write_old->use_count ()) {
                _dead_wood.push_back (*_current_write_old);
        }
#else
        /* above use_count() condition is subject to a race condition.
         *
         * Particulalry with JACK2 graph-order callbacks arriving
         * concurrently to processing, which can lead to heap-use-after-free
         * of the RouteList.
         *
         * std::shared_ptr<T>::use_count documetation reads:
         * > In multithreaded environment, the value returned by use_count is approximate
         * > (typical implementations use a memory_order_relaxed load).
         */
        _dead_wood.push_back (*_current_write_old);
#endif
```

Ardour tried the obvious optimisation — *"if nobody else holds it, free it
right now"* — and it produced a **heap-use-after-free of the `RouteList`**
under JACK2's concurrent graph-order callbacks, because `use_count()` is an
approximate relaxed load. The fix was to stop being clever and *always* retire.

**[JUDGMENT] — the reachability insight.** Note *why* the same `use_count()`
check is safe a few lines away, in `write_copy()`'s dead-wood scan: once an
object is in `_dead_wood`, `managed_object` no longer points at it, so no
reader can obtain a *new* reference, and the count is monotonically
non-increasing — observing 1 is **durable**. In `update()` the object was still
reachable, so a reader could be mid-`reader()` and the count could go *up*.

> **Reachability is the invariant that makes a refcount observation
> meaningful.** Encode that in your types, not in a comment.

#### (b) Plugin instances destroyed while the RT thread still holds them

Not merely a use-after-free risk. For CLAP it is a **spec violation with
defined preconditions**: `destroy()` is `[main-thread & !active]` and *"It is
required to deactivate the plugin prior to this call"*; `deactivate()` is
`[main-thread & active]`; `process()` is `[audio-thread & active &
processing]`. **[FACT]** —
[`clap/include/clap/plugin.h`](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h).

*Mechanism:* a plugin instance's teardown is a three-step, two-thread protocol,
not a refcount reaching zero. **`Drop` is the wrong tool.** **[JUDGMENT]**

#### (c) Voices left ringing / MIDI notes stuck on

Note-on played from the old material; the undo removes the region; the note-off
never arrives. **This is the most user-visible symptom and it survives a
perfectly correct RCU swap** — it is not a memory-safety problem at all.

*Mechanism, as actually shipped:* Zrythm's `AudioEngine::wait_for_pause()`
calls `panic_all()` (MIDI panic) **before** requesting the pause, and then —
after `run_` is cleared and the processing lock has been taken and released —
runs **one more one-sample cycle**, commented `/* run 1 more time to flush
panic messages */`. **[FACT]** —
[`src/dsp/engine.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/engine.cpp).

That extra cycle is the instructive part: **enqueuing the panic is not enough.**
The graph needs a cycle to *emit* it, and that cycle must come *after* normal
processing stops, or the panic races the material still playing.

#### (d) Automation cursors pointing into freed arrays

An automation cursor is RT-owned mutable state indexing into control-owned
immutable storage. Undo replaces the storage; the index is meaningless or out
of bounds.

*Mechanism:* never let RT-side cursors be raw indices into snapshot-owned
memory. Carry the cursor as a **time value** and re-derive per block, or
version-stamp the snapshot and force a re-seek on change.

openDAW takes the strong form: *"Runtime modulation (automation playback,
MIDI/CC control) is computed on a **transient layer**, never written into
boxes"* **[FACT]** —
[`plans/wasm-audio/04-architecture.md`](https://github.com/andremichelle/openDAW/blob/main/plans/wasm-audio/04-architecture.md).
The transient layer is derived, so it can be rebuilt from a position; the boxes
are truth, so they can be swapped.

#### (e) Sample-position discontinuity

Zrythm is honest about this. `resume()` does
`transport_.move_playhead (transport_.playhead_ticks_before_pause (), false)`
then re-requests roll **[FACT]**
([engine.cpp](https://github.com/zrythm/zrythm/blob/master/src/dsp/engine.cpp))
— undo during playback is *stop, edit, seek back, restart*. The playhead does
not drift, but the audio has a hole in it.

#### (f) Non-deterministic ordering between the undo and the next callback

Two sub-cases; only one is about threading.

**The threading one:** if an undo produces *more than one* published snapshot
("remove plugin", then "reconnect ports"), the audio thread can render a block
from the intermediate state. **One undo step publishes exactly one snapshot.**

**The ordering one** is subtler, and openDAW hit it in production. Their
forward-only sync stream carries `new` / `update-primitive` / `update-pointer`
/ `delete` tasks to the Rust engine:

> "`primitiveType` carries the field's codec captured at emission time: a task
> stream is forward-only and self-contained, so serialization must never
> re-resolve the field against a live graph that a later task in the same batch
> may have deleted (e.g. **undo trims a region, then unstages it** — #287)."

**[FACT]** —
[`packages/lib/box/src/sync.ts`](https://github.com/andremichelle/openDAW/blob/main/packages/lib/box/src/sync.ts).

And the companion production crash:

> "`BoxGraph.#rollback` replayed the **raw, un-optimized** transaction updates
> in reverse, with deferred pointer updates appended at the **end** of the list
> — out of chronological order… the reverse replay inverts a pointer update of
> a box that is already unstaged → the exact production panic `Could not find
> PointerField at <uuid>/2`"

**[FACT]** —
[`errors/P2-undo-rollback-pointerfield-missing.md`](https://github.com/andremichelle/openDAW/blob/main/errors/P2-undo-rollback-pointerfield-missing.md).
Their fix collapsed create+delete pairs via `optimizeUpdates` before replaying,
because *"phantom create+delete pairs net to nothing; replaying them raw is
what resurrected edges."*

**[JUDGMENT]** This is the most transferable lesson here for a snapshot
architecture:

> **An undo delta must be self-contained and order-correct at emission time.**
> Anything resolved lazily against a graph the rest of the delta is mutating is
> a bomb that fires only on undo, and only sometimes.

#### (g) Latency-compensation invalidation

Removing a plugin changes graph latency, which changes every delay line.
Zrythm treats latency as a first-class recalculation: `recalc_graph(soft=true)`
exists purely to `update_latencies()` under the engine lock **[FACT]**
([`graph_dispatcher.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/graph_dispatcher.cpp)).

### 1.2 Ardour's alternative: do not create the hazard

Ardour's answer to most of §1.1 is architectural refusal. Paul Davis, on why
deleting a track is not undoable **[FACT]** —
[discourse.ardour.org, "Why is track deletion not part of undo history?"](https://discourse.ardour.org/t/88782):

> "Our undo/redo model only operates on existing objects, it does not delete or
> recreate objects."
>
> "Undo/Redo is generally a lightweight operation that just changes the state
> of *existing* objects, rather than delete/create them."
>
> "Keeping one around just so that you can 'undo' out of its deletion makes it
> a bit pointless to have deleted it in the first place."
>
> "That leaves the undo operation as actually recreating the track in its
> previous state, which is totally at odds with undo/redo as a state changing
> operation for components that make up a session."

Correspondingly, `UndoTransaction::undo()` reverse-iterates a command list
calling `->undo()` on `MementoCommand`/`StatefulDiffCommand` objects over XML
state of *existing* `Stateful` objects **[FACT]**
([`libs/pbd/undo.cc`](https://github.com/Ardour/ardour/blob/master/libs/pbd/undo.cc)).
Grepping `libs/ardour/route.cc` finds `MementoCommand<AutomationList>` for
automation edits but **no undo command emitted by
`Route::remove_processor()`** — plugin add/remove is not on the undo stack.
**[FACT, by source inspection]**

**[JUDGMENT]** For an FL-Studio-scale ambition this is not an acceptable
answer, but it correctly locates the cost. Ardour did not *fail* to implement
structural undo; it decided the price was wrong for its object model. AURA is
choosing to pay it, so it should know what it is buying.

---

## 2. Deallocation discipline

### 2.1 The rule, stated precisely

> **The audio thread must never be the *last owner* of anything whose
> destructor is not `nonblocking`.**

This is stricter than "must not allocate". A `Drop` chain can free thousands of
nodes, unmap a sample file, close an FD, join a thread, or call a plugin's
`destroy()`. The question is not "does this allocate" but **"who runs the
destructor, and what does it transitively do."**

### 2.2 The canonical rule lists, verbatim

Ross Bencina, *Real-time audio programming 101: time waits for nothing*
**[FACT]** —
[rossbencina.com](http://www.rossbencina.com/code/real-time-audio-programming-101-time-waits-for-nothing)
(HTTP-only; mirrored discussion at [LWN](https://lwn.net/Articles/452630/)):

> "Don't allocate or deallocate memory / Don't lock a mutex / Don't read or
> write to the filesystem or otherwise perform i/o. (In case there's any doubt,
> this includes things like calling printf or NSLog, or GUI APIs.) / Don't call
> OS functions that may block waiting for something / Don't execute any code
> that has unpredictable or poor worst-case timing behavior / Don't call any
> code that does or may do any of the above / Don't call any code that you
> don't trust to follow these rules"

and, on structure:

> "For tasks with unbounded execution time such as plugin loading, AudioMulch
> performs them in the UI thread and then sends the results to the audio
> callback when they're ready"

Timur Doumler, CppCon 2021, slide 16 **[FACT]** —
[slides PDF](https://cppcon.digital-medium.co.uk/wp-content/uploads/2021/09/talk.pdf):

> "don't call anything that might block *(non-deterministic execution time +
> priority inversion!)* • don't try to acquire a mutex • **don't allocate /
> deallocate memory** • don't do any I/O • don't interact with the thread
> scheduler • don't do any other system calls • don't call any 3rdparty code if
> you don't know what it's doing • don't use algorithms with > O(1) complexity
> • don't use algorithms with *amortised* O(1) complexity"

And in *Using locks in real-time audio processing, safely*, the RCU shape
stated directly **[FACT]** —
[timur.audio](https://timur.audio/using-locks-in-real-time-audio-processing-safely):

> "Instead of modifying the data structure in-place, the message thread peels
> off a copy that contains the modification, while the audio thread still looks
> at the previous version for however long it needs to."

He also warns against `std::mutex` on the audio thread *"not even with
`try_lock()`"*, because the destructor's `unlock()` may enter the kernel to
wake waiters.

### 2.3 The classic Rust bug, and why it hides in development

```rust
// RT thread
let snapshot: Arc<Graph> = self.current.load();   // clone -> refcount 2
render(&snapshot);
// snapshot dropped here.
```

If the control thread swapped and dropped its `Arc` *between* the load and the
drop, the RT thread's drop is the last one, and `Graph`'s entire destructor
runs inside the audio callback.

**[JUDGMENT]** Two Rust-specific sharpenings:

1. **The drop is recursive and data-dependent.** The last `Arc<GraphSnapshot>`
   drops every `Vec`, every boxed `dyn Processor`, every plugin instance it
   transitively owns — and plugin destructors take locks and join threads. It
   is not "one `free()`, ~100 ns"; it is an unbounded destructor chain inside a
   1–10 ms deadline.
2. **Which thread loses the race is nondeterministic, and the control thread
   usually wins.** So the bug hides in development and fires under load.

`assert_no_alloc` exists for exactly this and explicitly covers
*de*allocation **[FACT]** —
[docs.rs/assert_no_alloc](https://docs.rs/assert_no_alloc/latest/assert_no_alloc/):

> "This crate provides a custom allocator that allows to temporarily disable
> memory (de)allocations for a thread. If a (de)allocation is attempted anyway,
> the program will abort or print a warning."
>
> "Allocation and deallocation can take unpredictable amounts of time, and thus
> can *sometimes* lead to audible glitches… Debugging such problems can be
> hard, because it is difficult to reproduce such problems consistently."

It even ships a `PermitDrop` wrapper because *"Objects that deallocate upon
`Drop` can be wrapped."*

### 2.4 The Rust primitives, and where each stops short

| Crate | Cited guarantee | Where it leaves you exposed |
|---|---|---|
| **`rtrb`** | "Reading from and writing into the ring buffer is lock-free and wait-free." "A fixed-capacity buffer is allocated on construction. After that, no more memory is allocated (unless the type `T` does that internally)." **[FACT]** — [docs.rs](https://docs.rs/rtrb/) | Fixed capacity → the return channel can fill. `T`'s drop still runs wherever you pop it. |
| **`triple_buffer`** | "nonblocking, and more precisely bounded wait-free"; "allocates memory on initialization only, rather than on every update"; explicitly contrasts itself with "RCU primitives" that allocate per update. **[FACT]** — [docs.rs](https://docs.rs/triple_buffer/) | "Only works in single-producer, single-consumer scenarios"; "Consumer only has access to the latest state". Recycling instead of freeing is a real strength, but 3× memory and no multi-reader. |
| **`arc-swap`** | "All the read operations are always lock-free. Most of the time, they are actually wait-free"; guarantees "at least `usize::MAX / 4` wait-free accesses in between waits"; each thread has a limited number of fast borrow slots ("currently 8") after which "the algorithm reverts to a slower path"; "If the content gets changed, all existing Guards are promoted to contain an owned instance." **[FACT]** — [limitations](https://docs.rs/arc-swap/latest/arc_swap/docs/limitations/index.html), [performance](https://docs.rs/arc-swap/latest/arc_swap/docs/performance/index.html) | **`arc-swap` does not defer drops.** The promoted owned `Arc`'s decrement happens wherever you drop it. It solves *publication*, not *reclamation*. |
| **`farbot`** | `RealtimeObject<T, RealtimeObjectOptions>` for "sharing data of type T between non-realtime threads and a single realtime thread", via `ScopedAccess` parameterised by `ThreadType`. "The `fifo` will never lock nor block." **[FACT]** — [github.com/hogliux/farbot](https://github.com/hogliux/farbot) | README does not state which thread destroys retired values; you must derive it from `RealtimeObjectOptions`. `docs.rs/farbot` 404s. |

**[JUDGMENT]** For an RCU DAW the honest combination is arc-swap-style
*publication* **plus** basedrop-style *deferred reclamation* — or
`basedrop::SharedCell`, which is both in one type.

### 2.5 `basedrop` — the purpose-built answer, in detail

Docs: [docs.rs/basedrop](https://docs.rs/basedrop/latest/basedrop/) · Repo:
[micahrj/basedrop](https://github.com/micahrj/basedrop) (raw paths still
resolve under `glowcoil/basedrop`) · Author's write-up:
[micahjohnston.com/posts/basedrop](https://micahjohnston.com/posts/basedrop/).

Crate-level, from `lib.rs` — note `#![no_std]` **[FACT]**
([`src/lib.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/lib.rs)):

> "Memory-management tools for real-time audio and other latency-critical
> scenarios. `Owned` and `Shared` are smart pointers analogous to `Box` and
> `Arc` which add their contents to a queue for deferred collection when
> dropped. `Collector` is used to process the drop queue… `SharedCell`
> implements a mutable memory location holding a `Shared` pointer that can be
> used by multiple readers and writers in a thread-safe manner."

**The mechanism.** Every allocation is a `Node<T>` whose `#[repr(C)]` header
holds a **union**: while live it stores the collector pointer; once retired the
same word is reinterpreted as the MPSC queue's `next` pointer. **No allocation
is needed at retirement time, because the queue node *is* the object.**
**[FACT]** —
[`src/collector.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/collector.rs):

```rust
#[repr(C)]
struct NodeHeader { link: NodeLink, drop: unsafe fn(*mut NodeHeader) }
#[repr(C)]
union NodeLink { collector: *mut CollectorInner, next: ManuallyDrop<AtomicPtr<NodeHeader>> }
```

What the audio thread actually runs — a Vyukov intrusive MPSC push, a swap plus
a store, no CAS loop, no allocation, no syscall:

```rust
pub unsafe fn queue_drop(node: *mut Node<T>) {
    let collector = (*node).header.link.collector;
    (*node).header.link.next = ManuallyDrop::new(AtomicPtr::new(core::ptr::null_mut()));
    let tail = (*collector).tail.swap(node as *mut NodeHeader, Ordering::AcqRel);
    (*tail).link.next.store(node as *mut NodeHeader, Ordering::Release);
}
```

And `Shared`'s `Drop` **[FACT]** —
[`src/shared.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/shared.rs):

```rust
impl<T> Drop for Shared<T> {
    fn drop(&mut self) { unsafe {
        let count = self.node.as_ref().data.count.fetch_sub(1, Ordering::Release);
        if count == 1 { fence(Ordering::Acquire); Node::queue_drop(self.node.as_ptr()); }
    } }
}
```

Dropping the last `Shared` on the audio thread performs one `fetch_sub`, one
fence, one swap, one store. **Constant time, wait-free, allocation-free.** The
doc comment: *"When a `Shared<T>`'s reference count goes to zero, its contents
are added to the drop queue of the `Collector` whose `Handle` it was originally
allocated with. As the collector may be on another thread, contents are
required to be `Send + 'static`."*

`Collector::collect()` is `&mut self` and `Collector` is **`Send` but not
`Sync`** — one collector thread, by construction. The actual `free` happens in
`collect_one()` via `((*head).drop)(head)`. **[FACT]**

`SharedCell` is the RCU cell, and this is what answers the grace-period
question **[FACT]** —
[`src/shared_cell.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/shared_cell.rs):

```rust
pub fn get(&self) -> Shared<T> {
    self.readers.fetch_add(1, Ordering::SeqCst);
    let shared = Shared { node: NonNull::new_unchecked(self.node.load(Ordering::SeqCst)), .. };
    let copy = shared.clone();
    core::mem::forget(shared);
    self.readers.fetch_sub(1, Ordering::Relaxed);
    copy
}

pub fn replace(&self, value: Shared<T>) -> Shared<T> {
    let node = value.node.as_ptr(); core::mem::forget(value);
    let old = self.node.swap(node, Ordering::AcqRel);
    while self.readers.load(Ordering::Relaxed) != 0 {}   // writer spins — a few instructions
    fence(Ordering::Acquire);
    Shared { node: NonNull::new_unchecked(old), .. }
}
```

**Read that carefully.** The `readers` counter is held only across *pointer
fetch + refcount increment*, **not** across the audio callback. The writer's
spin is bounded by a few instructions on a reader, not by a buffer period. Once
`get()` returns, the snapshot is kept alive by the ordinary refcount and the
audio thread may hold it **arbitrarily long** — the writer does not wait for
that.

> **There is no grace-period detection in the classical sense. Reclamation is
> purely refcount-driven, deferred through the collector.**

The author's own framing **[FACT]** —
[micahjohnston.com](https://micahjohnston.com/posts/basedrop/):

> "if a large number of allocations are being transferred to and from the audio
> thread, the fixed-capacity channel for returning allocations can fill up."
>
> "Basedrop's solution is to replace the fixed-capacity ring buffer for
> returning allocations with an MPSC linked-list queue whose nodes are created
> at allocation time… When the audio thread is ready to release a piece of
> memory for reclamation, the corresponding node can be pushed onto the queue
> in an allocation-free, wait-free operation."

Stated limitations: *"Basedrop doesn't currently support dynamically sized
types, like `Owned<[T]>` or `Owned<dyn Trait>`"* and *"`Shared<T>` doesn't
currently support weak references for cyclic data structures the way `Arc<T>`
does."* Future work he names includes *"the RCU pattern… epoch-based
reclamation, and quiescent state-based reclamation."* The issue tracker has two
open, non-correctness issues (`Owned<T: ?Sized>`, `Debug` impls) and no closed
ones. **[FACT]**

#### Caveats not in the docs **[JUDGMENT]**

* **The consumer can transiently miss items.** `queue_drop`'s tail swap and
  `next` store are two operations; a producer preempted between them leaves
  `collect_one` seeing `next == null`, and it returns `false` rather than
  spinning. Harmless, but `collect()` is not "drain everything now".
* **Unbounded growth if you forget to poll.** Nothing bounds the queue.
  `alloc_count()` is the monitoring hook and should feed a watchdog.
* **`SharedCell::replace` spins on the control thread** with relaxed loads and
  no backoff. The window is ~3 instructions, but a spin loop that can be
  descheduled mid-iteration can burn a timeslice on an oversubscribed machine.
* **`Collector` leaks on drop** unless `try_cleanup()` succeeds: *"If a
  `Collector` is dropped, it will leak all associated allocations as well as
  its internal data structures."* Matters for tests and teardown paths.
* **`Handle` must be threaded to every allocation site.** That ergonomic tax is
  why [`audio-garbage-collector`](https://docs.rs/audio-garbage-collector)
  exists — it wraps basedrop with a global GC thread, but note its own caveat:
  *"Collection is based on polling the queue every 100ms by default… If
  references are created and dropped very frequently this strategy is not
  adequate."* **[FACT]**

#### One clarification that inverts an intuition

`basedrop::Shared<T>`'s deferral covers `T`'s **entire drop glue**, because
`drop_node::<T>` runs on the collector thread. So interior `Arc`s **inside** a
`Shared` snapshot are fine — the whole tree is destroyed on the collector.

> The danger is interior `Arc`s that the audio thread clones and drops
> **independently** of the snapshot. That is the rule to lint for.

**[JUDGMENT, derived from collector.rs]**

### 2.6 Why `crossbeam-epoch` is a trap

**`crossbeam-epoch` is not RT-safe on the reader side.** Four independent
pieces of source evidence **[FACT]**:

1. **First pin on a thread allocates.** `Local::register` does
   `Owned::new(Self { … })` — a heap allocation.
   ([`internal.rs`](https://raw.githubusercontent.com/crossbeam-rs/crossbeam/master/crossbeam-epoch/src/internal.rs))
2. **Pinning periodically runs garbage collection on the pinning thread.**
   `const PINNINGS_BETWEEN_COLLECT: usize = 128;` and
   `if count.0 % Self::PINNINGS_BETWEEN_COLLECT == 0 { self.global().collect(&guard); }`.
   Every 128th `pin()` on the audio thread would run destructors of arbitrary
   retired objects. **This alone disqualifies it.**
3. **Deferring allocates once the local bag overflows.** `MAX_OBJECTS = 64`;
   overflow calls `push_bag`, and `Queue::push` does `Owned::new(Node { … })`.
   ([`sync/queue.rs`](https://raw.githubusercontent.com/crossbeam-rs/crossbeam/master/crossbeam-epoch/src/sync/queue.rs))
4. **Closures larger than 3 words are boxed.** `Deferred` is documented as *"A
   `FnOnce()` that is stored inline if small, or otherwise boxed on the heap."*
   ([`deferred.rs`](https://raw.githubusercontent.com/crossbeam-rs/crossbeam/master/crossbeam-epoch/src/deferred.rs))

### 2.7 Reclamation schemes compared

| Scheme | RT-thread cost per read | RT cost at retirement | Memory bound | Failure mode |
|---|---|---|---|---|
| **Retired list drained by control thread** (basedrop, Ardour dead wood) | refcount `fetch_add` + brief reader-count guard; wait-free | `fetch_sub` + tail swap + store; wait-free, no alloc | unbounded queue | leak-until-collected; needs a watchdog |
| **Epoch-based** (crossbeam-epoch) | `pin()` allocates on first pin, runs GC every 128 pins | `defer_destroy` may allocate | bounded bag then global spill | **not RT-safe**; a stalled pinned reader stalls *all* reclamation globally |
| **Hazard pointers** | store pointer + fence; wait-free, no alloc if slots preallocated | retire → scan hazard array, O(threads × hazards) | bounded by retire threshold | the scan must be kept off the RT thread |
| **Stop-the-world** (Zrythm, VCV Rack) | zero | zero | zero | audio thread blocks → dropout on every edit |
| **Try-lock + swap + poller** (JUCE) | one try-lock | zero (swap only) | one pending snapshot | publication delayed a callback; frees up to 500 ms late |

**[JUDGMENT]** For AURA's access pattern the retired-list scheme is right and
epoch reclamation is a trap. Epoch's value is amortising away per-read refcount
traffic — but a DAW takes *one* snapshot per callback, not millions per second.
You would be paying epoch's RT-unsafety for an optimisation you do not need.
Hazard pointers are RT-safe on the read side but buy nothing over a single
refcount at ~1 read/callback. A refcounted snapshot published by atomic swap,
retired onto a wait-free intrusive queue drained by the control thread, is both
simplest and strictly safest — which is presumably why Ardour, basedrop and
JUCE converged on it independently.

### 2.8 Grace-period detection: two distinct problems

Conflating these is the classic bug — it is precisely what Ardour's `#if 0`
block was.

* **(A) The publication race** — between the control thread's swap and a reader
  that loaded the old pointer but has not yet incremented its refcount. A
  handful of instructions.
* **(B) The reclamation question** — when is it safe to run the retired
  snapshot's destructor? Unbounded; the audio thread may hold the snapshot
  across many callbacks.

How each project solves **(A)** **[FACT]**:

| Project | Mechanism |
|---|---|
| basedrop `SharedCell` | `readers` counter around fetch+clone; `replace()` swaps then spins `while readers != 0` |
| Ardour `SerializedRCUManager` | `_active_reads` counter; `update()` CASes then `boost::detail::yield` spin |
| JUCE | Sidesteps it — `SpinLock` + audio-thread `ScopedTryLock`, skip on contention |
| Mixxx | Sidesteps it — no shared snapshot; typed SPSC messages |

How each solves **(B)** **[FACT]**:

| Project | Grace-period proof | Who frees |
|---|---|---|
| basedrop | **None needed** — refcounting *is* the proof; the RT drop enqueues the node | control thread, `Collector::collect()` |
| Ardour RCU | polled `use_count() == 1` on `_dead_wood`, scanned at the next `write_copy()` — safe only because the object is already unreachable | writer thread, lazily |
| Ardour graph chain | runtime `in_process_thread()` check → push a deleter onto the butler's MPMC queue | butler thread |
| JUCE | **adoption flag** `isNew`, cleared by the audio thread on swap; timer frees only when it observes `!isNew` | message thread, 500 ms timer |
| Mixxx | **explicit acknowledgement message** carrying `request_id` | main thread |
| Zrythm | **mutual exclusion** — engine stopped or locked | control thread, RT blocked |

#### The three options for AURA, assessed **[JUDGMENT]**

**Option 1 — refcount, no grace period (basedrop). RECOMMENDED.** The audio
thread may hold the snapshot indefinitely; the control thread never waits on
it. RT cost is one `fetch_sub` + one MPSC push on the final drop. Least
coupled: the control thread needs to know nothing about callback cadence, and
it works when the audio device is stopped mid-edit.

**Option 2 — per-callback generation counter.** Wait-free both sides, no
refcount. **But it has a fail-unsafe mode nobody in the surveyed projects
accepted:** if the audio thread stops calling back (device change, xrun, stop,
suspend), a naive `generation > tag + 1` test never fires and memory is never
reclaimed — and the moment you add a timeout to fix that, you have created a
use-after-free. Note that JUCE's `isNew` flag is the *safe* variant of the same
idea: a frozen flag means "not yet adopted" → **do not free**. That is
fail-safe in the correct direction. If you want a generation counter, build it
in the `isNew` shape.

**Option 3 — acknowledgement over an SPSC queue (Mixxx).** The most explicit
and easiest to reason about, and it composes with the command channel. Cost:
the RT thread pushes per retirement, so the return queue must not be able to
fill — which is exactly Micah Johnston's objection to bounded channels and the
reason basedrop uses an unbounded intrusive list. Size against the worst case
or use an intrusive queue.

### 2.9 What real projects do

#### Ardour — three distinct mechanisms

**1. RCU + dead wood** for collection-shaped state. From `rcu.h`'s own
comments **[FACT]**: *"Any existing users of the value returned by `reader()`
can continue to use their copy even as a `write_copy()`/`update()` takes
place."* … *"The class maintains a lock-protected 'dead wood' list of old value
of `*managed_object`… If the list is the last instance of a `shared_ptr<T>`
that references the object (determined by `shared_ptr::use_count()`) then we
erase it from the list, thus deleting the object it points to. This is lazy
destruction."* … *"we do not care how slow the `write_copy()`/`update()`
operations are."*

Readers bracket their `shared_ptr` copy with `_active_reads`
fetch_add/fetch_sub; `update()` CASes then spins
`for (unsigned i = 0; active_read(); ++i) boost::detail::yield(i);` with the
comment *"wait until there are no active readers. This ensures that any
references to the old value have been fully copied into a new shared_ptr, and
thus have had their reference count incremented."*

Used as `SerializedRCUManager<RouteList> routes;`,
`SerializedRCUManager<BundleList> _bundles;`,
`SerializedRCUManager<IOPlugList> _io_plugins;` with
`get_routes() { return routes.reader (); }` **[FACT]** —
[`session.h`](https://github.com/Ardour/ardour/blob/master/libs/ardour/ardour/session.h).
`Session::destroy()` ends with `/* clear out any pending dead wood from RCU
managed objects */ routes.flush (); _bundles.flush (); _io_plugins.flush ();`
**[FACT]** —
[`session.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/session.cc).

**[JUDGMENT]** Note the consequence: outside teardown, dead wood is only
reclaimed on the *next* `write_copy()`, so a retired route list can linger
until the user next edits the graph. Cheap, and deliberate.

**2. `rt_safe_delete` + the butler thread** — a runtime-checked, per-object
deferred free **[FACT]** —
[`rt_safe_delete.h`](https://github.com/Ardour/ardour/blob/master/libs/ardour/ardour/rt_safe_delete.h):

```cpp
template <class C>
void rt_safe_delete (ARDOUR::Session* s, C* gc) {
    if (s->deletion_in_progress () || !s->engine ().in_process_thread ()) { delete gc; return; }
    if (!s->butler ()->delegate (sigc::bind ([] (C* p) { delete p; }, gc))) { delete gc; return; }
}
```

with `Butler::delegate` pushing onto
`PBD::MPMCQueue<sigc::slot<void>> _delegated_work` and `summon()`ing the
butler, which drains it in `process_delegated_work()`
([`butler.h`](https://github.com/Ardour/ardour/blob/master/libs/ardour/ardour/butler.h),
[`butler.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/butler.cc)).

And the use site is **exactly AURA's problem statement** —
`Session::rechain_process_graph` **[FACT]**:

```cpp
/* However, the graph-chain may be in use (session process), and the last reference
 * be helf by the process-callback. So we delegate deletion to the butler thread. */
_graph_chain = std::shared_ptr<GraphChain> (new GraphChain (g, edges),
                 std::bind (&rt_safe_delete<GraphChain>, this, _1));
```

A **custom `shared_ptr` deleter** reroutes the free to the butler. That is the
C++ analogue of basedrop's `Drop`, with one advantage (no deferral cost when
the drop already happens on a safe thread) and one risk **[JUDGMENT]**: the
`sigc::slot` copy into the queue may itself heap-allocate on the RT thread.

**3. RT-side try-lock with graceful degradation.** The audio path takes a
reader try-lock and, on failure, silences or skips the route rather than
blocking: `PBD::RWLock::ReaderLock lm (_processor_lock, PBD::RWLock::TryLock);
if (!lm.locked()) { bufs.silence (nframes, 0); ... return; }` and, in
roll/no_roll, `if (!lm.locked()) { return 0; }` **[FACT]** —
[`route.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/route.cc).

Ardour also has a fourth pattern: `Route::apply_processor_changes_rt()`,
annotated `__attribute__((annotate("realtime")))`, applies *pending*
meter-point / reorder / listen changes **from the RT thread** via a writer
try-lock — cheap topology tweaks staged as atomics, no control-thread
rendezvous at all. **[FACT]**

Also note the destructor-reentrancy hazard, called out in
`Route::clear_processors`: `/* drop references w/o process-lock (I/O procs may
re-take it in ~IO() */ old_list.clear ();`, and `processor->drop_references ()`
deliberately placed *after* the lock scope closes in `remove_processor`.
**[FACT]**

#### JUCE — the closest published analogue to what AURA is building

`RenderSequenceExchange` in `juce_AudioProcessorGraph.cpp` (tag 8.0.4) is forty
lines and worth reading whole **[FACT]** —
[source](https://github.com/juce-framework/JUCE/blob/8.0.4/modules/juce_audio_processors/processors/juce_AudioProcessorGraph.cpp):

```cpp
void set (std::unique_ptr<RenderSequence>&& next) {
    const SpinLock::ScopedLockType lock (mutex);
    mainThreadState = std::move (next);
    isNew = true;
}

/*  Call from the audio thread only. */
void updateAudioThreadState() {
    const SpinLock::ScopedTryLockType lock (mutex);
    if (lock.isLocked() && isNew) {
        // Swap pointers rather than assigning to avoid calling delete here
        std::swap (mainThreadState, audioThreadState);
        isNew = false;
    }
}

void timerCallback() override {          // 500ms timer on the message thread
    const SpinLock::ScopedLockType lock (mutex);
    if (! isNew)
        mainThreadState.reset();
}
```

The audio thread **try-locks** (never blocks; on contention it keeps running
the old sequence), **swaps rather than assigns** — explicitly *"to avoid
calling delete here"* — pushing its previous sequence back to the message
thread, and clears `isNew`. The message thread's timer frees `mainThreadState`
**only when it observes `!isNew`**. That flag is the grace-period proof, and
crucially it is **fail-safe**: a frozen flag means "not yet adopted" → do not
free.

Historically, JUCE users hit exactly the bug AURA is guarding against. On the
forum thread *"AudioProcessorGraph crashes after removing node"*, the
workaround was deferred deletion — *"an AsyncUpdater to remove Nodes. Instead
of removing nodes straight away from a separate thread, I queue them and use
`triggerAsyncUpdate()` to do the real removing"* — alongside the blunt *"I
think it would be nice to add a sentence to the AudioProcessorGraph description
about thread safety of the class. Which is more or less non-existent."*
**[FACT]** —
[forum.juce.com/t/15035](https://forum.juce.com/t/audioprocessorgraph-crashes-after-removing-node/15035).
Those threads are JUCE-4-era; `RenderSequenceExchange` is the modern
convergence.

#### Mixxx — explicit request/response, main thread does all allocation

The class comment on `EffectsMessenger` is the crispest statement of the rule
found anywhere **[FACT]** —
[`src/effects/effectsmessenger.h`](https://github.com/mixxxdj/mixxx/blob/main/src/effects/effectsmessenger.h):

> "EffectsMessenger sends EffectsRequests from the main thread and receives
> EffectsResponses from the audio thread. This allows memory allocation and
> deallocation on the heap, which is slow, to be done in the main thread to
> avoid blocking the audio thread and causing audible glitches. All of
> EffectsMessenger's methods are called on the main thread."

Transport is `MessagePipe<Sender, Receiver>` over `rigtorp::SPSCQueue`,
non-blocking, with copy ops deleted to preserve the SPSC invariant **[FACT]**.
The deferred free is driven by acknowledgement **[FACT]** —
[`effectsmessenger.cpp`](https://github.com/mixxxdj/mixxx/blob/main/src/effects/effectsmessenger.cpp):

```cpp
void EffectsMessenger::processEffectsResponses() {
    EffectsResponse response;
    while (m_requestPipe.readMessage(&response)) {
        ... EffectsRequest* pRequest = it.value();
        collectGarbage(pRequest);        // deletes pEffect / pChain
        delete pRequest;
        ...
    }
}
```

The engine's response for `request_id` **is** the receipt that the engine is
done with the object.

#### Zrythm — a negative result worth recording

There is **no** `free_later` / idle-deferred-free mechanism in Zrythm, in
either the C or the C++ tree. `inc/utils/objects.h` (v1) contains only
immediate-free helpers; the similarly-named `object_free_w_func_and_null` is
just `if (_obj) { _func (_obj); _obj = NULL; }`. **[FACT, by source
inspection]**

Instead: v1 halts the engine (`g_atomic_int_set (&AUDIO_ENGINE->run, 0)` then
waits for the cycle) around `router_recalc_graph`; master wraps the rebuild in
`run_function_with_engine_lock_`, and
`GraphScheduler::rechain_from_node_collection` calls `release_node_resources()`
and then `graph_nodes_ = std::move (nodes)` — **the old collection is destroyed
inside the critical section, potentially while the audio thread is blocked.**
**[FACT]** —
[`graph_dispatcher.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/graph_dispatcher.cpp),
[`graph_scheduler.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/graph_scheduler.cpp).
Zrythm master has adopted `[[clang::nonblocking]]` on some RT-critical methods
**[FACT]**.

#### VCV Rack — the honest counterexample

`Engine::addModule` / `removeModule` take `std::lock_guard<SharedMutex>` on the
same mutex `stepBlock()` holds, with the comment *"Writers lock when mutating
the engine's state or stepping the block."* **[FACT]** —
[`src/engine/Engine.cpp`](https://github.com/VCVRack/Rack/blob/v2/src/engine/Engine.cpp).
Adding or removing a module blocks the audio thread. **[JUDGMENT]** Rack gets
away with it because module add/remove is rare and its audience tolerates a
click. Not a model for a DAW editing during playback.

---

## 3. Plugin lifecycle vs undo

### 3.1 The hard constraint

You cannot choose freely; the formats constrain you.

**CLAP** — `destroy()` is `[main-thread & !active]`, requires prior
`deactivate()` which is `[main-thread & active]`; `stop_processing()` is
`[audio-thread & active & processing]`. **[FACT]** —
[`plugin.h`](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h).

**VST3** — `setProcessing()` *"will be called before any process calls start
with true and after with false"*, should do *"only light operation (no memory
allocation or big setup reconfiguration)"*, and *"setProcessing (false) may be
called after setProcessing (true) without any process calls"*. **[FACT]** —
[`IAudioProcessor`](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IAudioProcessor.html).

**[JUDGMENT]** So the snapshot must not own plugin instances in the
`Drop`-equals-`destroy` sense. Hold a **handle** (an id, or a
`Shared<PluginSlot>` whose drop only returns the slot to a control-thread
pool). Instantiation and destruction live in a control-thread registry with an
explicit state machine; undo manipulates *references* into it.

### 3.2 The three options and their real costs

**Option A — keep the instance alive but unrouted.** Undo of "delete plugin" is
a pure graph edit: instant, sample-accurate, and the plugin's internal state
(reverb tail, LFO phase, learned MIDI mappings, oversampling buffers) is
bit-exact because it never went anywhere. Costs: RAM held indefinitely, and the
risk that the plugin holds an exclusive resource — CLAP's
`CLAP_STATE_CONTEXT_FOR_DUPLICATE` exists precisely because plugins may hold
*"limited resources or external hardware connections"* **[FACT]** —
[`state-context.h`](https://github.com/free-audio/clap/blob/main/include/clap/ext/state-context.h).

**Option B — destroy and re-instantiate from a saved state blob.** Cheap in
RAM; three problems: undo is no longer instant, no longer *reliable*, and
silently loses anything save/load does not cover.

How unreliable? pluginval's own suite answers. Its "Plugin state restoration"
test saves state, randomises a parameter, restores, and asserts the parameter
came back — with `const auto tolaratedDiff = 0.1f`. The **exact** binary
round-trip assertion (*"Returned state differs from that set by host"*) is
gated behind `if (strictnessLevel >= 8)`. **[FACT]** —
[`Source/tests/BasicTests.cpp`](https://github.com/Tracktion/pluginval/blob/develop/Source/tests/BasicTests.cpp).

**[JUDGMENT]** A validator shipping a **10 %-of-full-scale default tolerance**,
with byte-exactness as a **strictness-8 opt-in**, is telling you plainly that
real plugins do not round-trip state faithfully. clap-validator says the same
by different means: its `state-reproducibility-binary` test is documented as
something projects may want to switch off in config **[FACT]** —
[README](https://github.com/free-audio/clap-validator/blob/master/README.md).

What CLAP actually guarantees: `save()` *"Saves the plugin state into stream.
Returns true if the state was correctly saved. [main-thread]"*, the mirror for
`load()`, plus `mark_dirty()` **[FACT]** —
[`state.h`](https://github.com/free-audio/clap/blob/main/include/clap/ext/state.h).
**[JUDGMENT]** That is a *stream protocol* and a *thread*. It is not
determinism, byte-stability, version compatibility, or a promise that
everything audible is captured. Delay lines, reverb tails, filter memory, LFO
phase, RNG seeds, oversampler history and per-voice envelopes are typically not
in the blob by design.

**Option C — hybrid with a timeout.** Keep the instance alive and unrouted for
a bounded window; beyond it, serialize and destroy.

**[JUDGMENT]** This is what to build, with the fallback made **visible**:
outside the window the undo entry reads *"restore plugin (from saved state)"*.
Silent lossy undo is worse than honest lossy undo. Bound the window by **bytes
as well as time** — a 4 GB sampler should not be held for 30 seconds because a
20 KB EQ's policy said so.

### 3.3 What the projects actually do

* **Ardour:** plugin add/remove is not on the undo stack **[FACT, by source
  inspection]**, consistent with Paul Davis's *"our undo/redo model only
  operates on existing objects"* **[FACT]**.
* **Zrythm:** clones plugin *settings* into the action
  (`setting_ = utils::clone_unique (*setting)`), snapshots the selection, and
  saves/restores the whole port-connection manager around each action
  (`port_connections_before_` / `port_connections_after_` →
  `reset_connections_from_other`) **[FACT]** —
  [`mixer_selections_action.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/mixer_selections_action.cpp),
  [`undoable_action.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/undoable_action.cpp).
  It also carries plugin **state files** in undo actions, with a known gap — in
  `UndoManager::do_or_undo_action`, when the redo stack overflows:

  ```cpp
  /* TODO create functions to delete unnecessary files held by the action
   * (eg, something that calls plugin_delete_state_files()) */
  ```

  **[FACT]** —
  [`undo_manager.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/undo_manager.cpp).
  **[JUDGMENT]** A shipping example of the §4 problem: undo history owns files,
  and trimming the history leaks them.
* **REAPER:** widely described as storing whole-project undo states with a
  configurable memory limit. **Unverified in this session.**
* **Bitwig / Live / Cubase / Pro Tools:** **unverified.** Better a gap than a
  manufactured citation.

---

## 4. Undo of things that are not model state

### 4.1 The general rule

**[JUDGMENT]** An operation belongs in the undo stack **iff** its effect is
fully described by a reversible transformation of the project model. Once the
effect includes a filesystem write, a device reconfiguration, a network call,
or a change in the physical world, undo restores the *model* but not the
*effect*, and pretending otherwise is a lie in the UI.

The useful decomposition is **model / artifact / world**:

| Tier | Examples | Undoable? |
|---|---|---|
| **Model** | regions, notes, routing, parameter values | fully |
| **Artifact** | a recorded WAV, a rendered stem, a frozen track, an AI-generated clip | the *reference* is undoable; the artifact has its own lifecycle |
| **World** | sample rate, audio device, buffer size, control-surface state, files outside the project folder | never |

Keeping the artifact is what makes redo cheap and deterministic.

### 4.2 Recording

The convention: **undo removes the region, the audio file survives on disk.**
Ableton documents that *"Recorded samples are stored with the current Set's
Project folder, under Samples/Recorded"* and that a loop recording can be
"unrolled" by *"repeatedly using the Edit menu's Undo command"* **[FACT]** —
[Ableton manual, Recording New Clips](https://www.ableton.com/en/manual/recording-new-clips/).
The manual does not state whether the file is deleted on undo; **[JUDGMENT]**
the near-universal behaviour is that it is not, with a separate clean-up pass
reclaiming orphans. Copy that separation.

openDAW is the instructive counter-example: deleting an `AudioFileBox` deletes
the physical OPFS file — but only for samples tracked as user-created, only
after `RuntimeNotifier.approve()` confirmation, with an opt-in
`auto-delete-orphaned-samples` preference (default `false`), and the
tracked-UUID set deliberately **not persisted** with the project. **[FACT]** —
[`plans/done/obsolete-sample.md`](https://github.com/andremichelle/openDAW/blob/main/plans/done/obsolete-sample.md).

**[JUDGMENT]** Note the shape: they coupled deletion to box removal, then
needed a confirmation dialog and a default-off preference to make it tolerable
— because box removal also happens on *undo of an import*. If you couple them
at all: **never delete on undo**, only on an explicit cleanup command, and
never delete a file any point in the undo history still references.

### 4.3 The RT-side rule during recording

Ardour enforces it at the source **[FACT]** —
[`route.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/route.cc):

```cpp
if (need_process_lock && _disk_writer && _disk_writer->record_enabled () && _session.actively_recording ()) {
    return -1;
}
```

Structural change on a record-armed, actively-recording route is **refused** —
not queued, not deferred. **[JUDGMENT]** Right default, and it should be a hard
rule: a take is a real-time capture of a performance, and a graph edit mid-take
can corrupt it in ways the user cannot inspect until the take is over.

### 4.4 Rendering / bounce / freeze

**[JUDGMENT]** Artifact-producing. Make the *replacement of live material by
the rendered artifact* undoable and cache the artifact so redo is instant. Do
not make the render itself undoable, and do not delete the artifact on undo.

### 4.5 Sample rate, buffer size, device changes

**[JUDGMENT]** Not undoable. These change the meaning of every sample position
and every plugin's internal state.

Zrythm's design implicitly agrees: `UndoableAction` stores `sample_rate_` and
`frames_per_tick_` at construction, and `init_loaded()` **rescales** by
`engine_sample_rate / sample_rate_` when the stack is reloaded **[FACT]** —
[`undoable_action.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/undoable_action.cpp).
Sample rate is an **ambient property the undo stack adapts to**, not one it can
change.

### 4.6 AI generation jobs

**[JUDGMENT]** This maps directly onto AURA's existing job/agent machinery.
Treat a generation job as:

1. **a job producing an artifact** — not undoable, cancellable; and
2. **an insertion of that artifact into the project** — fully undoable.

Undo of the insertion must not cancel the job, must not delete the artifact,
and redo must **reuse** the artifact rather than regenerate (otherwise redo is
nondeterministic and possibly billable). If a job is still running when the
user undoes its placeholder: remove the placeholder from the model, let the job
finish, and either land it into the redo entry or park it in a "generated,
unplaced" bin. Cancelling on undo is defensible but must be explicit in the UI.

### 4.7 The "cannot be undone" convention

An attempt at the Ardour manual's cleanup page returned 404s on both URLs; no
wording is quoted here that could not be verified.

Verified: openDAW gates its only destructive filesystem operation behind
explicit approval plus a default-off preference **[FACT]**, and Zrythm's
redo-overflow path is flagged in-source as needing explicit file deletion
**[FACT]**.

**[JUDGMENT]** The convention, as it should be implemented: destructive
operations are

* **(a)** confirmed by a modal naming the irreversibility in plain language,
* **(b)** a **barrier or truncation of the undo stack** rather than sitting
  silently on top of it, and
* **(c)** offered alongside a non-destructive alternative where one exists.

The barrier matters: if "clean up unused sources" leaves the stack intact, a
user can undo *past* it into a state referencing deleted files.

---

## 5. Undo during playback and during recording

### 5.1 The shipping positions

**Zrythm: allowed, engine stops.** **[FACT]** —
[`undoable_action.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/undoable_action.cpp),
[`undoable_action.h`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/undoable_action.h):

```cpp
EngineState state{};
if (needs_pause ()) {
    /* stop engine and give it some time to stop running */
    AUDIO_ENGINE->wait_for_pause (state, false, true);
}
...
perform ? perform_impl () : undo_impl ();
...
if (needs_pause ()) { AUDIO_ENGINE->resume (state); }
```

with `virtual bool needs_pause () const { return true; }`.

So Zrythm has **exactly the safe/unsafe classification this section is about** —
`needs_pause()`, overridable per action type — but it is **opt-*out*,
defaulting to "assume unsafe."** Undo also asserts it is off the DSP thread
(`z_return_if_fail (ROUTER->is_processing_thread () == false)`) and serialises
actions behind `action_sem_` **[FACT]**.

**Ardour: allowed for state changes; structural changes refused during
recording and otherwise taking the global process lock.**
`Route::remove_processor` asserts
`assert (!AudioEngine::instance()->process_lock().trylock());` with
`/* Caller must hold process lock */`, and refuses outright while actively
recording **[FACT]**. Cheap topology tweaks are instead staged as atomics and
applied by the audio thread itself in `apply_processor_changes_rt()`
**[FACT]**.

**openDAW: allowed, no engine stop.** Main thread owns the box graph;
mutations sync one-way, forward-only, into the Rust engine which *"reads the
box graph read-only"* **[FACT]** —
[`04-architecture.md`](https://github.com/andremichelle/openDAW/blob/main/plans/wasm-audio/04-architecture.md).
Updates arrive over `WASM_SYNC_CHANNEL` and are executed via
`applyUpdates(bytes)` in the `AudioWorkletProcessor` **[FACT]** —
[`processor.ts`](https://github.com/andremichelle/openDAW/blob/main/packages/studio/core-wasm/src/processor.ts).

**[JUDGMENT]** Worklet port callbacks run on the audio thread *between* render
quanta — so openDAW applies undo deltas **on the audio thread**, serialised by
the worklet's run-loop rather than by a lock. Elegant (no lock, no grace
period, no torn state) but it puts a variable-cost decode-and-mutate inside the
audio budget; tolerable only because the worklet is not hard-RT and the deltas
are small.

### 5.2 Is there a safe class?

**[JUDGMENT]** Yes, but "mixer vs structural" is not quite the right cut. The
discriminator is:

> **Does the change alter the identity or lifetime of anything the RT thread
> holds, or the shape of the graph?**

**Safe while rolling** (no pause, no reinstantiation, ramp the change):
continuous parameter values; mute/solo/arm (they are gain changes); automation
*values* on lanes that already exist; region property edits that do not change
what is sounding; adding a node not yet connected to anything audible.

**Unsafe while rolling** (needs a rendezvous, a crossfade, or refusal):
removing/reordering nodes in the active path; anything changing reported
latency; anything changing channel counts or buffer sizes; plugin
instantiation/destruction; editing a region that is currently sounding;
anything at all while a record pass is capturing that route.

**And a third class: musically unsafe but technically safe.** Deleting a region
twenty bars ahead is structurally identical to deleting the one under the
playhead, but only one is audible. **[JUDGMENT]** With snapshots you can
exploit this — classify by *distance from the playhead* and let far-future
edits publish immediately while near-playhead edits go through §6's machinery.

### 5.3 What users expect

**[JUDGMENT, low confidence — no forum verification this session]** Users
expect undo to work during playback and expect playback not to stop. Zrythm's
pause-seek-resume is audibly a stutter; that is a known rough edge, not a
target. The modern bar: mixer undo during playback inaudible, structural undo
at worst a short crossfade, undo during recording blocked with a clear message.

---

## 6. Sample-clean state switching

### 6.1 What the RCU swap gives you, and what it does not

**[JUDGMENT]** An atomic snapshot swap at a buffer boundary gives exactly one
guarantee: **no torn state.** Block N renders entirely from A, block N+1
entirely from B.

Everything else is still broken: signal discontinuity (a step has energy across
the whole spectrum); plugin internal state divergence (B's reverb tail is
empty); voice state (notes sounding in A have no counterpart in B); latency
change (the boundary is not even time-aligned); and buffer contents (delay
lines, oversampler history, filter memory all reset).

### 6.2 The four techniques

**(a) Crossfade at the swap.** Render both snapshots for 5–20 ms and
equal-power crossfade. The only technique that handles *arbitrary*
discontinuity, including plugin-state divergence, because it never assumes the
two signals are related. Cost: ~2× CPU for the fade window.

**[JUDGMENT]** For a snapshot architecture this is the natural primitive and it
composes beautifully: the RT thread holds *two* snapshot pointers plus a fade
position, then drops the old one into the deferred-free queue. Zrythm's source
has an aspirational note in exactly this direction — its fade-out path is
`#if 0`'d with `// TODO (or use graph cross-fade)` **[FACT]**
([`engine.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/engine.cpp)).

**(b) Wait for a musical boundary.** Ableton's Session View is the reference:
*"The Clip Quantization chooser lets you adjust an onset timing correction for
clip triggering,"* with "Global" syncing to the master quantization value and
"None" disabling **[FACT]** —
[Ableton manual, Launching Clips](https://www.ableton.com/en/manual/launching-clips/).

**[JUDGMENT]** This is a *musical* solution, not a *click* solution — a bar
line is not a zero crossing, and a sustained pad still clicks across it. Its
value is making the switch feel intentional. **Quantize *when* the crossfade
starts; do not use it instead of one.**

**(c) Zero-crossing alignment.** Useful for a single monophonic signal in a
sample editor. **[JUDGMENT] Useless for a mix:** there is no common zero
crossing across L/R let alone a full arrangement, and even at one the
*derivative* is discontinuous. Do not build on it.

**(d) Pre-roll the incoming version.** Render B into a null sink before the
switch so its reverbs fill, filters settle, LFOs reach correct phase, and PDC
delay lines prime. Then crossfade — and because both signals have converged,
the fade can be very short and near-inaudible.

**[JUDGMENT]** Highest quality, and exactly what a snapshot architecture is
good at, because snapshot B is a complete independently-renderable object.
Cost: 2× CPU for the pre-roll, which must cover the longest tail. For A/B-ing
two mix versions, 1–2 s of pre-roll plus a 20 ms crossfade would be
indistinguishable from a hard cut on most material. **Caveat:** pre-roll
converges only *deterministic* state; plugins with free-running randomness
never converge, and neither CLAP nor VST3 gives you a way to force them to.

### 6.3 What products do, and the gap

**[FACT]** Ableton quantizes clip launches to a musical grid. **[FACT]** VST3's
`setProcessing()` is documented as the hook where a plugin *"could be used to
reset some buffers (like Delay line or Reverb)"* — the format's own model of
"this plugin's tail is now discarded".

**[JUDGMENT / unverified]** Plugin A/B compare buttons are near-universal;
whole-*project* A/B during playback is rare to nonexistent in mainstream DAWs,
and specific implementations could not be verified this session.

> The inference is that this is a genuine gap, and a snapshot-per-undo-state
> architecture is unusually well-placed to fill it: **"A/B any two points in
> history, crossfaded, during playback" falls out of the architecture rather
> than being bolted on.** Treat that as a headline capability, not a footnote.

### 6.4 What is achievable

**[JUDGMENT]** Snapshot swap + dual render + crossfade gives *click-free*
switching in all cases and *musically continuous* switching in most. It cannot
give *bit-identical continuity* across a structural change, because the two
graphs genuinely have different state. **Set the goal at "the user cannot hear
the seam."**

---

## 7. Testing

### 7.1 The oracle: deterministic offline rendering

**[JUDGMENT]** The highest-value test in the whole domain. Make the engine
renderable offline, deterministically, from a snapshot + start position +
length, with no dependence on wall-clock or device. Then:

```
render(S₀, 0..N) == render(undo(apply(S₀, op)), 0..N)   // bit-identical
```

If that holds over a large generated corpus of `op`, undo is correct in the
only sense a user cares about. openDAW has the matching artifact
(`packages/app/wasm/src/perf/offline-render.ts` **[FACT, file exists]**) and
notes that Rust-side serialization *"exists just for round-trip tests"*
**[FACT]**.

*Caveat:* this requires the DSP to be deterministic. Seed every RNG from the
snapshot; render single-threaded offline, or separately prove the parallel
scheduler is order-independent.

### 7.2 The Figma invariant

Adopt it verbatim. Figma's stated principle **[FACT]** —
[How Figma's multiplayer technology works](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/):

> "if you undo a lot, copy something, and redo back to the present (a common
> operation), the document should not change."

and the mechanism:

> "an undo operation modifies redo history at the time of the undo, and
> likewise a redo operation modifies undo history at the time of the redo."

**[JUDGMENT]** The second quote is the non-obvious part, and it generalises
past multiplayer. In a DAW the "other participant" is the *engine* and the
user's *non-undoable* actions: recording wrote a file, an AI job landed a clip,
a plugin's internal state moved on. **Undo and redo must be rebased against
what actually happened.** openDAW already handles the failure case this way —
on a failed inverse it rolls the applied steps forward, restores the history
index, and reports *"History changed by another participant."* **[FACT]** —
[`editing.ts`](https://github.com/andremichelle/openDAW/blob/main/packages/lib/box/src/editing.ts).

### 7.3 The thirteen tests

**T1 — Undo round-trip, structural equality.** `proptest`: random op sequence,
apply all, undo all, assert structural equality and byte-identical canonical
serialization. Use `proptest-state-machine`'s `ReferenceStateMachine` +
`StateMachineTest` split for shrinking to a minimal failing sequence **[FACT]**
— [docs.rs](https://docs.rs/proptest-state-machine/latest/proptest_state_machine/).

**T2 — Undo round-trip, audible equality.** Same sequence, assert
`render(S_before) == render(S_after_undo)` sample-for-sample. Catches
everything T1 misses: canonicalisation bugs, ordering bugs, and any state not
in the serialization.

**T3 — Figma invariant.** Apply K ops, undo all K, perform a *read-only*
operation, redo all K, assert document and render unchanged. Stronger variant:
after undoing all K, apply a *new* op, and assert the redo stack behaves per
the documented policy with no dangling references.

**T4 — Interleaved undo/redo at random depths.** `undo × a; redo × b; undo × c;
…` then "redo to the top", assert byte-identical to the starting state. This is
where index-management bugs live — openDAW's `#historyIndex` /
`#savedHistoryIndex` (with `-1` meaning "the saved state was spliced away")
shows the care required **[FACT]**.

**T5 — Delta self-containment (the openDAW #287 test).** Serialize the undo
delta, then **mutate the graph arbitrarily**, then apply the delta to a pristine
copy of the pre-state. It must succeed. If it cannot, the delta is resolving
something lazily. Direct port of a documented production bug **[FACT]**.

**T6 — Transaction abort integrity.** Inject a panic partway through applying
an op; assert the graph is exactly as before. Copy openDAW's regression suite
test-for-test: *"rollback survives a box with deferred pointer created and
deleted in the aborted transaction"*, *"recovers when a box constructor throws
mid-transaction"*, *"restores deleted boxes with resolved pointers when a
transaction aborts"* **[FACT]**.

**T7 — RT-safety assertion under undo storm.** Run the engine on a real or
simulated callback while a control thread hammers undo/redo, with the callback
instrumented:

* `rtsan-standalone-rs`: mark the callback `#[nonblocking]`, run with
  `RTSAN_ENABLE=1`; it intercepts `malloc`, `free`, `pthread_mutex_lock` and
  prints a stack trace. Linux/macOS/iOS **[FACT]** —
  [rtsan-standalone-rs](https://github.com/realtime-sanitizer/rtsan-standalone-rs),
  [rtsan](https://github.com/realtime-sanitizer/rtsan).
* `assert_no_alloc` for the cheap always-on CI variant, with `PermitDrop` for
  anything legitimately deferred **[FACT]**.

**[JUDGMENT]** T7 is the *only* test that catches the "last `Arc` drop landed
on the audio thread" bug, because it is timing-dependent and silent. **Make it
a gate.** Both Ardour (`__attribute__((annotate("realtime")))`) and Zrythm
(`[[clang::nonblocking]]`) have adopted the compile-time cousin of this
**[FACT]**.

**T8 — Grace-period / reclamation model check.** `loom` for the publish/retire
protocol. It *"runs tests many times over, and each time a different thread
scheduling will be used"*, models C11, and requires its replacement types —
*"any code that does not use loom's replacement types is invisible to loom."*
Known limit: *"It is not possible for loom to completely model all the
interleavings that relaxed memory ordering allows"* **[FACT]** —
[docs.rs/loom](https://docs.rs/loom/latest/loom/). **[JUDGMENT]** Isolate the
swap + retire logic into a tiny loom-swappable module; the full engine will not
fit the state space.

**T9 — Stuck-note / silence-tail assertions.** After any structural undo during
playback, assert from the rendered output that no note remains sounding without
a corresponding note-on in the new snapshot, and that within M ms the output
converges to a from-scratch render of the new snapshot at that position. This
is the test that forces you to discover Zrythm's `panic_all()` + flush cycle.

**T10 — Op-stream fuzzing.** Fuzz the *serialized* delta, not just the API.
clap-validator is the model: a *"built-in multi-process fuzzer that can run the
plugin through a series of random parameter changes, note on/off events, and
transport changes while checking for crashes, hangs, and spec-compliance
issues"*, with `--reproduce <seed>` **[FACT]** —
[README](https://github.com/free-audio/clap-validator/blob/master/README.md).
Copy the seed-reproduction ergonomics.

**T11 — Live mirror checksum (cheapest high-value test).** openDAW's
`Synchronization` interface carries `checksum(value: Int8Array): Promise<void>`
beside `sendUpdates`, and the Rust side implements a rolling 32-byte XOR
checksum described as *"used to validate the mirror after a transaction"*
**[FACT]** —
[`sync.ts`](https://github.com/andremichelle/openDAW/blob/main/packages/lib/box/src/sync.ts),
[`checksum.rs`](https://github.com/andremichelle/openDAW/blob/main/crates/boxgraph/src/checksum.rs).
**[JUDGMENT]** In AURA's architecture the "mirror" is the published snapshot:
after every publish (debug/CI always, release sampled), have the RT side
checksum what it is actually rendering from and compare. An always-on
invariant, not a test — it turns "undo desynced the engine" from a mystery into
a localised assertion.

**T12 — Deferred-free accounting.** Assert `collector.alloc_count()` returns to
baseline after a full apply/undo cycle plus a `collect()`. Assert it never
grows monotonically under a sustained undo storm. This is the leak test for the
one failure mode basedrop *does* have.

**T13 — Plugin state round-trip characterisation.** Not a test of your code but
of your plugin population: `save → randomise → load → compare` per plugin,
recorded, to decide per-plugin whether Option A (keep alive) is mandatory.
Calibrate expectations against pluginval's 0.1 default tolerance and
strictness-8 exactness gate **[FACT]**.

---

## 8. The checklist — forty rules

### Ownership and deallocation

1. The audio thread must never run a destructor. Not "must not allocate" —
   must not *destruct*. Use `basedrop::Shared`/`SharedCell` so
   refcount-to-zero enqueues instead of freeing.
2. Pump the collector from one designated control thread on a fixed cadence and
   after every publish. `Collector` is `Send` but not `Sync` — one collector,
   by construction.
3. Watchdog `alloc_count()`. The drop queue is unbounded by design; a monotonic
   rise is a stalled collector.
4. No `Arc<T>` that the audio thread clones and drops *independently of the
   snapshot*. Interior `Arc`s inside a `Shared` snapshot are fine — the whole
   drop glue runs on the collector. Enforce with a newtype and a lint, not
   discipline.
5. Never destroy an object while holding a lock the audio thread takes.
   Ardour's `~IO()` re-takes the process lock — destructors have re-entrancy
   hazards, not just timing hazards.
6. **Never shortcut retirement with "if nobody else holds it, free it now."**
   `use_count()` is an approximate relaxed load. That exact optimisation
   produced a heap-use-after-free of Ardour's `RouteList`.
7. Never make the control thread spin-wait on the audio thread's *callback*. A
   few-instruction reader-guard spin is fine; waiting for a buffer period is
   not.
8. If you use a generation counter for grace-period detection, build it in
   JUCE's `isNew` shape — a frozen flag must mean "don't free", never "safe to
   free".
9. Do not use `crossbeam-epoch` on the read side: first pin allocates, every
   128th pin runs GC, bag overflow allocates, and >3-word closures box.
10. Nothing in an undo entry may own a file, an FD, a device handle, or a
    plugin instance's lifetime by `Drop`. Undo entries hold ids into a
    control-thread registry.
11. Call `try_cleanup()` on teardown paths and in tests, or the collector leaks
    everything.

### Publishing

12. One undo step publishes exactly one snapshot. The audio thread must never
    observe an intermediate state.
13. Undo deltas are self-contained and order-correct at emission time. Never
    resolve a field, address, or type against a graph a later part of the same
    delta mutates.
14. Collapse phantom create+delete pairs before replaying a delta in either
    direction.
15. RT-side cursors are times, not indices. Anything derived from the snapshot
    must be re-derivable from a position after a swap.
16. Runtime modulation lives in a transient layer, never written back into the
    model.

### Plugins

17. Plugin destruction follows the format's state machine (`stop_processing` on
    audio → `deactivate` on main → `destroy` on main), never a refcount.
18. Undo of "delete plugin" keeps the instance alive within a window bounded by
    bytes *and* time. Outside it, fall back to a state blob — and say so in the
    UI.
19. Never assume a state blob round-trips. Measure it per plugin; treat
    exactness as a property you observe, not one you rely on.

### Transport

20. Refuse structural edits on a route that is actively recording. Refuse,
    don't defer.
21. Classify every action as playback-safe or playback-unsafe, and **default to
    unsafe**. (Zrythm's `needs_pause()` defaults to `true` — copy the default,
    not the pause.)
22. Never stop the transport to perform an undo. Publish a new snapshot
    instead.
23. Any change altering reported latency re-primes PDC as part of the same
    publish.
24. MIDI panic plus a flush cycle before any change that can orphan a note-off
    — or, better, diff the sounding-note set across snapshots and emit exactly
    the orphaned note-offs.

### Continuity

25. No hard cut in an audible signal path. Crossfade (5–20 ms, equal-power)
    whenever the swap can change the output.
26. For A/B and large structural swaps, pre-roll the incoming snapshot to
    convergence, then crossfade short.
27. Quantize *when* the fade starts to a musical boundary; never use the
    boundary as a substitute for the fade.
28. Do not build on zero-crossing detection.

### Non-model state

29. Model / artifact / world. Only model state is undoable.
30. Undo never deletes a file. Cleanup is an explicit, confirmed, separate
    command.
31. Cleanup refuses to delete anything referenced by any live point in the undo
    history — or truncates the history at the destructive operation.
32. Trimming the undo stack must release the artifacts it owned. (Zrythm has a
    live TODO here; don't inherit it.)
33. Sample rate, buffer size and device changes are never undoable, and every
    history entry records the rate it was authored at.
34. Generation jobs: the placement is undoable, the job and its artifact are
    not. Redo reuses the artifact; it never regenerates.

### Testing

35. Offline deterministic render is the oracle:
    `render(S) == render(undo(apply(S, op)))`, byte-identical, over a generated
    corpus.
36. The Figma invariant is an acceptance test: undo a lot, do something
    read-only, redo to the present — nothing changed.
37. An RT-safety sanitizer runs over the audio callback while a control thread
    hammers undo/redo, and it is a CI gate.
38. `loom` covers the publish/retire protocol in isolation.
39. The snapshot the RT thread is rendering is checksummed against the control
    thread's expectation after every publish, always on in debug.
40. Every fuzz failure is reproducible from a printed seed.

---

## 9. Sources

**Ardour** —
[`libs/pbd/pbd/rcu.h`](https://github.com/Ardour/ardour/blob/master/libs/pbd/pbd/rcu.h) ·
[`libs/ardour/route.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/route.cc) ·
[`libs/pbd/undo.cc`](https://github.com/Ardour/ardour/blob/master/libs/pbd/undo.cc) ·
[`libs/ardour/ardour/session.h`](https://github.com/Ardour/ardour/blob/master/libs/ardour/ardour/session.h) ·
[`libs/ardour/session.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/session.cc) ·
[`rt_safe_delete.h`](https://github.com/Ardour/ardour/blob/master/libs/ardour/ardour/rt_safe_delete.h) ·
[`butler.h`](https://github.com/Ardour/ardour/blob/master/libs/ardour/ardour/butler.h) ·
[`butler.cc`](https://github.com/Ardour/ardour/blob/master/libs/ardour/butler.cc) ·
[`gtk2_ardour/processor_box.cc`](https://github.com/Ardour/ardour/blob/master/gtk2_ardour/processor_box.cc) ·
[Discourse: "Why is track deletion not part of undo history?"](https://discourse.ardour.org/t/88782)

**Zrythm** —
[`undoable_action.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/undoable_action.cpp) ·
[`undo_manager.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/undo_manager.cpp) ·
[`mixer_selections_action.cpp`](https://github.com/zrythm/zrythm/blob/master/src/gui/backend/legacy_actions/mixer_selections_action.cpp) ·
[`src/dsp/engine.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/engine.cpp) ·
[`graph_dispatcher.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/graph_dispatcher.cpp) ·
[`graph_scheduler.cpp`](https://github.com/zrythm/zrythm/blob/master/src/dsp/graph_scheduler.cpp) ·
[v1 `objects.h`](https://raw.githubusercontent.com/zrythm/zrythm/v1/inc/utils/objects.h) ·
[v1 `router.c`](https://raw.githubusercontent.com/zrythm/zrythm/v1/src/dsp/router.c)

**openDAW** —
[`editing.ts`](https://github.com/andremichelle/openDAW/blob/main/packages/lib/box/src/editing.ts) ·
[`sync.ts`](https://github.com/andremichelle/openDAW/blob/main/packages/lib/box/src/sync.ts) ·
[`lib/box/README.md`](https://github.com/andremichelle/openDAW/blob/main/packages/lib/box/README.md) ·
[`docs/graph.md`](https://github.com/andremichelle/openDAW/blob/main/docs/graph.md) ·
[`P2-undo-rollback-pointerfield-missing.md`](https://github.com/andremichelle/openDAW/blob/main/errors/P2-undo-rollback-pointerfield-missing.md) ·
[`plans/wasm-audio/04-architecture.md`](https://github.com/andremichelle/openDAW/blob/main/plans/wasm-audio/04-architecture.md) ·
[`crates/boxgraph/src/checksum.rs`](https://github.com/andremichelle/openDAW/blob/main/crates/boxgraph/src/checksum.rs) ·
[`core-wasm/src/processor.ts`](https://github.com/andremichelle/openDAW/blob/main/packages/studio/core-wasm/src/processor.ts) ·
[`plans/done/obsolete-sample.md`](https://github.com/andremichelle/openDAW/blob/main/plans/done/obsolete-sample.md)

**JUCE / Mixxx / VCV Rack** —
[`juce_AudioProcessorGraph.cpp` @ 8.0.4](https://github.com/juce-framework/JUCE/blob/8.0.4/modules/juce_audio_processors/processors/juce_AudioProcessorGraph.cpp) ·
[forum 15035](https://forum.juce.com/t/audioprocessorgraph-crashes-after-removing-node/15035) ·
[forum 13123](https://forum.juce.com/t/keeping-an-audioprocessor-alive-when-deleting-the-corresponding-audioprocessorgraph-node/13123) ·
[`effectsmessenger.h`](https://github.com/mixxxdj/mixxx/blob/main/src/effects/effectsmessenger.h) ·
[`effectsmessenger.cpp`](https://github.com/mixxxdj/mixxx/blob/main/src/effects/effectsmessenger.cpp) ·
[`engineeffectsmanager.h`](https://github.com/mixxxdj/mixxx/blob/main/src/engine/effects/engineeffectsmanager.h) ·
[`messagepipe.h`](https://github.com/mixxxdj/mixxx/blob/main/src/util/messagepipe.h) ·
[VCV `Engine.cpp`](https://github.com/VCVRack/Rack/blob/v2/src/engine/Engine.cpp)

**Plugin formats** —
[CLAP `plugin.h`](https://github.com/free-audio/clap/blob/main/include/clap/plugin.h) ·
[CLAP `ext/state.h`](https://github.com/free-audio/clap/blob/main/include/clap/ext/state.h) ·
[CLAP `ext/state-context.h`](https://github.com/free-audio/clap/blob/main/include/clap/ext/state-context.h) ·
[VST3 `IAudioProcessor`](https://steinbergmedia.github.io/vst3_doc/vstinterfaces/classSteinberg_1_1Vst_1_1IAudioProcessor.html)

**Rust crates** —
[basedrop docs](https://docs.rs/basedrop/latest/basedrop/) ·
[`lib.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/lib.rs) ·
[`collector.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/collector.rs) ·
[`shared.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/shared.rs) ·
[`shared_cell.rs`](https://raw.githubusercontent.com/glowcoil/basedrop/master/src/shared_cell.rs) ·
[repo](https://github.com/micahrj/basedrop) ·
[audio-garbage-collector](https://docs.rs/audio-garbage-collector) ·
[arc-swap limitations](https://docs.rs/arc-swap/latest/arc_swap/docs/limitations/index.html) /
[performance](https://docs.rs/arc-swap/latest/arc_swap/docs/performance/index.html) ·
[rtrb](https://docs.rs/rtrb/) ·
[triple_buffer](https://docs.rs/triple_buffer/) ·
[farbot](https://github.com/hogliux/farbot) ·
[assert_no_alloc](https://docs.rs/assert_no_alloc/) ·
[crossbeam-epoch](https://docs.rs/crossbeam-epoch/) /
[`internal.rs`](https://raw.githubusercontent.com/crossbeam-rs/crossbeam/master/crossbeam-epoch/src/internal.rs) /
[`sync/queue.rs`](https://raw.githubusercontent.com/crossbeam-rs/crossbeam/master/crossbeam-epoch/src/sync/queue.rs) /
[`deferred.rs`](https://raw.githubusercontent.com/crossbeam-rs/crossbeam/master/crossbeam-epoch/src/deferred.rs) ·
[loom](https://docs.rs/loom/) ·
[proptest-state-machine](https://docs.rs/proptest-state-machine/)

**RT-programming references** —
[Micah Johnston, "Basedrop: A garbage collector for real-time audio in Rust"](https://micahjohnston.com/posts/basedrop/) ·
[Ross Bencina, "Real-time audio programming 101"](http://www.rossbencina.com/code/real-time-audio-programming-101-time-waits-for-nothing)
([LWN mirror](https://lwn.net/Articles/452630/)) ·
[Timur Doumler, CppCon 2021 slides](https://cppcon.digital-medium.co.uk/wp-content/uploads/2021/09/talk.pdf) /
[video](https://www.youtube.com/watch?v=Tof5pRedskI) /
["Using locks in real-time audio processing, safely"](https://timur.audio/using-locks-in-real-time-audio-processing-safely) ·
[Renn-Giles & Rowland, "Real-time 101" I](https://www.youtube.com/watch?v=Q0vrQFyAdWI) /
[II](https://www.youtube.com/watch?v=PoZAo2Vikbo) /
[slides](https://github.com/drowaudio/presentations) ·
[RealtimeSanitizer](https://github.com/realtime-sanitizer/rtsan) /
[rtsan-standalone-rs](https://github.com/realtime-sanitizer/rtsan-standalone-rs)

**Testing / other** —
[pluginval `BasicTests.cpp`](https://github.com/Tracktion/pluginval/blob/develop/Source/tests/BasicTests.cpp) /
[README](https://github.com/Tracktion/pluginval) ·
[clap-validator README](https://github.com/free-audio/clap-validator/blob/master/README.md) ·
[Figma multiplayer](https://www.figma.com/blog/how-figmas-multiplayer-technology-works/) ·
[Ableton: Launching Clips](https://www.ableton.com/en/manual/launching-clips/) /
[Recording New Clips](https://www.ableton.com/en/manual/recording-new-clips/)

---

## 10. Not verified — do not cite as fact

* REAPER's undo internals and its memory limit.
* Pro Tools / Logic / Cubase / Studio One / Bitwig undo-during-playback and
  non-undoable-operation conventions.
* Ardour manual wording on session clean-up (both URLs 404'd).
* Specific commercial plugin A/B implementations.
* ardour-dev mailing-list threads on RCU.
* A rendered Mixxx wiki page stating the "engine never allocates" rule as
  prose (the source-level class comment *is* verified).
* Whether whole-project A/B during playback exists in any mainstream DAW —
  believed to be a genuine gap, unconfirmed.
* User expectations in §5.3 — no forum verification this session.

Also confirmed **negative**: Zrythm has no `free_later` / idle-deferred-free
mechanism, in either the C or the C++ tree.

---

## 11. What this means for AURA

**The RCU transport is already ahead of the reference implementation.** Zrythm
— the project this research round set out to learn from — has *no deferred-free
mechanism at all*. It halts the engine around every graph rebuild and destroys
the retired node collection **inside the critical section, while the audio
thread is blocked**. That is simple and correct, and it converts every graph
edit into a potential dropout. AURA's `GraphPtr` + retire-queue discipline
(ARCHITECTURE §2.3) already buys what Zrythm pays for with a stutter, and that
should be stated plainly in the architecture doc rather than left implicit.

**The gap is the janitor.** What AURA has today is a *bounded* return path: the
callback adopts a new graph only when `retire_tx.slots() > 0`, and the retired
`GraphPtr` goes back for the control thread to drop. That is correct for the
current one-graph-at-a-time model. It does not survive the take/history design,
where the RT thread will hold snapshot references of unpredictable lifetime,
and where an A/B crossfade means holding *two* snapshots at once. Rule 1 and
Rule 3 of §8 are the ones AURA does not yet satisfy structurally: nothing
prevents an `Arc` interior to a snapshot from having its last clone dropped on
the audio thread, and there is no `alloc_count()`-shaped watchdog.

Concretely, the near-term work this dossier implies:

1. **Adopt a collector.** `basedrop::SharedCell<GraphSnapshot>` for
   publication, `Shared`/`Owned` for anything the RT thread can hold last,
   `collect()` pumped from the existing engine control thread. Keep the current
   `rtrb` retire queue — it composes; the collector covers the cases the queue
   cannot bound.
2. **Make Rule 4 mechanical, not cultural.** A newtype whose `Drop` is
   `unreachable!()` when called on the RT thread, plus a clippy
   `disallowed-types` entry for bare `Arc` in RT-reachable crates. §2.5's
   clarification is the subtle part and belongs in the module docs: interior
   `Arc`s *inside* a `Shared` snapshot are fine; independently-cloned ones are
   not.
3. **Land T7 as a CI gate before the op-log ships**, not after. It is the only
   test that catches the class of bug this document is mostly about, and both
   Ardour and Zrythm have adopted its compile-time cousin.
4. **Classify actions with `needs_pause()`'s default, and none of its
   mechanism.** Copy Zrythm's opt-out-defaults-to-unsafe discipline; publish a
   new snapshot rather than pausing (Rule 22).
5. **Note-orphan handling belongs in the boundary seam.** ARCHITECTURE §2.6
   already establishes that the callback reports and the control plane decides.
   Diffing the sounding-note set across snapshots and emitting exactly the
   orphaned note-offs is a better answer than a blanket MIDI panic, and it fits
   that seam without widening it.

And the opportunity, stated once: **whole-project A/B during playback appears
to be a genuine gap in the market, and a snapshot-per-history-point
architecture gets it for the cost of a second snapshot pointer and a fade
position on the RT thread.** Every other DAW would have to bolt it on. AURA
would be declining to throw it away.
