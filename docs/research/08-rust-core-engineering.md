# AURA — Rust Core Engineering

Structuring a large, long-lived Rust core for a DAW: architectural style,
crate decomposition, mechanical boundary enforcement, IPC codegen, testing,
and documentation that survives handover.

> **Status:** research input, 2026-08-13. Not normative. Feeds the
> architecture round that follows; conclusions here become binding only when
> they land in `docs/ARCHITECTURE.md` or `docs/SCALABILITY.md`.
> **Scope:** the software-engineering layer, not the DSP layer. Graph
> compilation, the parameter model and the time model are separate documents.

---

## Why this document exists

AURA is one `aura` crate, roughly 28k lines across
`src-tauri/src/{audio,midi,control,plugins,mcp,sidecars}`, with hand-written
JSON schemas in `docs/ipc-schemas/` and ownership expressed as a comment
block in `docs/ARCHITECTURE.md` §0. That arrangement was correct for a
four-agent parallel sprint. It will not survive a multi-year project edited
by rotating LLM contributors, because **every rule it depends on is enforced
by reading rather than by building.**

The strategic target is an FL Studio-class DAW. That is a decade-scale
codebase with a hard real-time thread in the middle of it, edited by
contributors who cannot hold the whole thing in context. The question this
document answers is not "what is the elegant architecture" but:

> **Which boundaries can be made impossible to cross by accident, and what
> does it cost to make them that way today?**

Everything below is organised around that question. Where a rule cannot be
mechanically enforced, it is marked as such, because an unenforced rule in a
repository edited by LLMs has a half-life of about three pull requests.

### Provenance

Produced by an Opus research agent during the 2026-08-13 architecture round,
against the tree at `f632580`. Primary sources were fetched directly:
crate listings from the repositories, lint and manifest semantics from the
Cargo and Clippy references, tooling versions from crates.io as of the
research date.

**Convention used throughout:** claims carry their source URL inline.
Blocks marked **⊳ Judgment** are engineering opinion, not sourced fact.
Blocks marked **⚠ Gap** are things that could not be verified and must not be
treated as settled.

---

## 1. The three regions

### 1.1 The dichotomy that hides the RT plane

Gary Bernhardt's *Boundaries*
([destroyallsoftware.com/talks/boundaries](https://www.destroyallsoftware.com/talks/boundaries))
proposes a **functional core** of immutable values and copy-on-write logic,
wrapped by an **imperative shell** that does I/O and drives the core,
communicating across the boundary in primitive values. The core gets many
fast unit tests; the shell gets few integration tests. Bernhardt describes it
as ports-and-adapters "on a lower level: dealing with functions, not
services."

**⊳ Judgment.** Functional core / imperative shell is the right spine for a
DAW, and the two-region framing is wrong in a way that will actively mislead
contributors. A DAW has a third region that is neither.

| Region | Character | Test surface | Allowed to |
|---|---|---|---|
| **Pure core** — project model, musical time, graph compilation, op application, undo inverses | Immutable in, immutable out. Deterministic. No clock, no I/O. | Unit + property tests, thousands, milliseconds | allocate freely, panic on programmer error |
| **RT executor** — the compiled schedule, node `process()`, param smoothing, transport advance | *Imperative*, mutation-heavy, in-place buffer writes. But **bounded, allocation-free, lock-free, panic-free**, and *deterministic given (schedule, params, input)* | offline block-render harness; golden output; RT-safety sanitizers | mutate preallocated memory only |
| **Imperative shell** — devices, files, plugins, sidecars, IPC, MCP, OS | Nondeterministic, fallible, async | few integration tests, smoke tests | everything |

Bernhardt's dichotomy collapses regions 2 and 3 into "imperative", and that
collapse is exactly the mistake that kills DAW codebases: someone puts a
`log::info!` or a `HashMap` lookup in the executor because "the shell is
allowed to do that."

The RT executor is *functionally pure at block granularity* —
`(schedule, params, in_buffers, position) → (out_buffers, next_state)` — but
implemented imperatively for cache and allocation reasons.

**⊳ Judgment.** Name this as a first-class region in the docs (call it the
**RT plane**), with its own rule list, its own crate, and its own lint
profile. Everything in §5 hangs off that decision.

### 1.2 Bencina's transitivity argument

Ross Bencina's *Real-time audio programming 101: time waits for nothing*
([rossbencina.com](http://www.rossbencina.com/code/real-time-audio-programming-101-time-waits-for-nothing),
mirrored at [LWN](https://lwn.net/Articles/452630/)) states the governing
rule:

> **"If you don't know how long it will take, don't do it."**

His enumerated glitch sources:

1. **Blocking** — mutexes, semaphores, waiting on another thread or process,
   disk, sockets. "Not only do you want to avoid directly writing code that
   blocks, it is critical that you avoid calling 3rd-party or operating
   system code that could block internally."
2. **Poor worst-case complexity** — average-case-optimised algorithms are the
   wrong tool. "It's usually better to use an algorithm that spreads the load
   across many samples/callbacks, even if it ends up burning a few more
   cycles overall."
3. **Locking**, for three separate reasons: priority inversion — the
   low-priority GUI thread holding the lock can be preempted by anything;
   unbounded critical sections; and scheduler paranoia — "avoid any kind of
   interaction with the OS thread scheduler."
4. **Memory allocation** — the allocator may lock, may ask the OS for pages,
   may page to disk. "Pre-allocate all your data. Only perform dynamic
   allocation in a non-real-time thread where it isn't time-critical."

The load-bearing sentence for *this* document is inside reason 3:

> "any code path inside a critical section that's shared with the real-time
> audio thread would have to follow all of the rules we're outlining here.
> That's asking for a lot of discipline from you and your fellow developers.
> **It would be easy for bugs to creep in.**"

**⊳ Judgment — this is the whole argument for a crate boundary.** Bencina's
own justification is a *software-engineering* argument, not a performance
one. The RT contract is **transitive**: it infects every function reachable
from the callback. A contract that propagates through the call graph cannot
be enforced by a comment at the top of one file. It has to be enforced at a
boundary the compiler understands — and in Rust that boundary is the crate,
with `#![no_std]` making the forbidden operations *unnameable* rather than
merely discouraged.

AURA's ARCHITECTURE §2.1 already states the four rules correctly. What is
missing is that nothing in the build enforces them.

### 1.3 What the reference implementations do

The three-region shape is what the mature systems actually converged on.

**Ardour** ([ardour.org/transport_threading.html](https://ardour.org/transport_threading.html))
— three threads: UI, realtime/process, butler/transport. UI calls
`Session::request_*` which enqueues into an "immediate" event list; the
process thread picks it up and runs `Session::process_event()`.

> "For many events, the work of satisfying the request can be split into two
> parts: one that can be done within realtime constraints, and another that
> may take an indeterminate amount of time."

The indeterminate half is queued to the butler via a `post_transport_work`
bitset, and — critically — "when entering the JACK process callback, the
value of `post_transport_work` is checked. If any bits are set… the process
callback will exit."

**Meadowlark** ([MVP_DESIGN_DOC.md](https://github.com/fwcd/meadowlark/blob/main/MVP_DESIGN_DOC.md))
— GUI-thread state is persistent/copy-on-write: "it first clones the data,
modifies that clone, and then pushes that new 'version' onto the pointer".
The result is a **"Compiled Schedule" sent to the RT thread via a
`SharedCell`**, available "at the top of the next process loop"; deallocation
is deferred to a `basedrop` collector thread; all GUI events carry
`MusicalTime`, converted to `SampleTime` on the RT side via a `TempoMap`.

**Firewheel** ([DESIGN_DOC.md](https://raw.githubusercontent.com/BillyDM/Firewheel/main/DESIGN_DOC.md))
— main thread owns the graph, compiles it to a schedule, pushes events over
"a realtime-safe message channel"; "realtime constraints (no mutexes!)";
nodes return a `ProcessStatus` declaring which outputs are silent so upstream
work can be skipped.

**Zrythm** ([deepwiki.com/zrythm/zrythm](https://deepwiki.com/zrythm/zrythm))
— an Engine owning the audio thread, a `GraphBuilder` constructing the
processing graph and a `GraphDispatcher` executing it with parallel
processing. Its feature page ([zrythm.org/en/features.html](https://www.zrythm.org/en/features.html))
documents "undoable user actions with serializable undo history".

AURA's §10 constraint 1 already mandates the prepare-then-pointer-swap
discipline. Four independent projects made the same decision. **Keep it
absolute.**

---

## 2. Architectural styles, judged

### 2.1 The verdict table

| Style | Verdict | Where it belongs | Failure mode if misapplied |
|---|---|---|---|
| **Functional core / imperative shell** | **Good fit — adopt as the spine**, with the three-region correction | whole app | two-region framing lets people treat the RT executor as "just imperative code" |
| **Hexagonal / ports & adapters** | **Good fit for the control plane; dangerous inside the RT plane at fine granularity** | devices, plugin formats, persistence, IPC, MCP, sidecars | ports at *sample* granularity; `dyn` in the inner sample loop |
| **Clean / Onion** | **Partial — take the dependency rule, drop the ceremony** | dependency direction only | "UseCase" classes per operation; "entities are objects with behaviour" fights data-oriented layout |
| **ECS** | **Bad fit as a framework; good as a data layout** | data layout in the RT plane; possibly the UI scene | `World` mutation and archetype moves allocate; no RT guarantees |
| **Actor model** | **Partial — you already have a constrained one** | control plane, sidecar supervision, disk I/O | cannot own the audio callback; actor frameworks bring executors, allocation, unbounded mailboxes |
| **Event sourcing / CQRS** | **Partial — yes as an op-log for undo and IPC; no as the persistence model** | op envelope, undo/redo, MCP↔UI convergence | O(history) project load; audio blobs don't fit an event store |

### 2.2 Hexagonal — and the block-granular port rule

Cockburn's stated intent
([alistair.cockburn.us/hexagonal-architecture](https://alistair.cockburn.us/hexagonal-architecture/)):

> "Allow an application to equally be driven by users, programs, automated
> test or batch scripts, and to be developed and tested in isolation from its
> eventual run-time devices and databases,"

with the structural rule that

> "code pertaining to the inside part should not leak into the outside part."

**⊳ Judgment.** This is exactly AURA's situation: two drivers already exist
(Tauri IPC and MCP) and a third is wanted (an offline test harness). The
`control::ControlPlane` seam (ARCHITECTURE §11) is a port in Cockburn's sense
and is the single best structural decision already in the codebase.

The thing to get right is **where the port sits relative to the block
boundary**:

- A `dyn Processor::process(&mut self, buf: &mut [f32])` call happens **once
  per node per block**. At 48 kHz with a 128-frame buffer that is one
  indirect call per 2.6 ms per node. Even 500 nodes is 500 vtable dispatches
  per 2.6 ms. Irrelevant.
- A `dyn` call **per sample** costs you the inlining of the whole DSP kernel.
  Dynamic dispatch's real cost is that "the indirection prevents inlining",
  not the vtable load itself
  ([Possible Rust: Enum or Trait Object](https://www.possiblerust.com/guide/enum-or-trait-object)).

> **Rule: ports are block-granular. Anything inside a `process()` body is
> monomorphized or `match`ed, never `dyn`.**

That single sentence makes hexagonal and hard-RT compatible.

### 2.3 Clean / Onion

**⊳ Judgment.** The Dependency Rule — source dependencies point inward only —
is the valuable 10%. The rest imports an OO cost model Rust does not share
and produces the `AddTrackUseCase` / `RemoveTrackUseCase` sprawl that an LLM
will happily generate forever.

"Entities are rich objects" also directly conflicts with what an audio engine
needs: dense arrays, slot indices, struct-of-arrays. AURA's §10 constraint 3
(dense slot indices on the RT thread, UUID↔slot mapping control-thread-only)
is the *right* call and is anti-Clean-Architecture in spirit.

**Keep the rule, refuse the layering vocabulary.**

### 2.4 ECS

Bevy is 59 crates with `bevy_ecs` usable standalone. Archetypal ECS allocates
in chunks and moves entities between archetypes when component sets change
([Unity Entities docs](https://docs.unity3d.com/Packages/com.unity.entities@0.51/manual/ecs_core.html)
describe the canonical model). Firewheel — written by the Meadowlark author —
offers "an optional data-driven parameter API that is friendly to entity
component systems (ECS)" but is itself not an ECS, and its design doc
explicitly notes that "the needs of game audio engines and DAW audio engines
are in conflict."

**⊳ Judgment — do not run an ECS on the audio thread.** Archetype moves
allocate; query iteration is a general mechanism with no worst-case bound you
control; commands are deferred and flushed at unpredictable points; and no
ECS gives an allocation-free guarantee.

What to steal is the **data layout**: dense `SlotId` indices, generational
handles for the control plane, struct-of-arrays for per-track parameters, and
"components" as separate parallel arrays rather than fat structs. That is
data-oriented design, and it is free.

**⚠ Gap.** No DAW was found using ECS for its session model.

### 2.5 Actor model

**⊳ Judgment.** You cannot make the audio callback an actor — it does not
have a mailbox loop you control; the driver calls *you*, on its schedule,
with a deadline.

What you have instead is a **fixed, statically known set of threads connected
by SPSC rings** — an actor system with the dynamism removed, and the removal
is the point. Adopt actor *discipline* (one owner per piece of state,
communication by message, no shared mutable state) without an actor
*framework*. Ardour's butler is exactly this.

Tokio is fine for sidecars, MCP and file I/O — AURA already depends on it —
but the audio path must never touch a work-stealing executor, and no
`tokio::spawn` may appear in a crate the RT plane can reach.

### 2.6 Event sourcing / CQRS

Event sourcing stores immutable events and reconstructs state by replay;
undo/redo falls out naturally. Zrythm ships "serializable undo history" over
undoable actions — the command pattern, not event sourcing.

**⊳ Judgment.** Split the question:

- **As the undo/redo and IPC mechanism: yes, and AURA has already designed
  it** — the op-log / `op-envelope.schema.json` in SCALABILITY §5 plus
  ARCHITECTURE §10 rule 9 ("do not implement undo/redo ad hoc… the prototype
  ships without undo rather than with a throwaway one"). That decision is
  correct and rare; hold it.
- **As the persistence mechanism: no.** A DAW must open a 3 GB project in
  time proportional to its *state*, not its *history*. Audio and plugin state
  are large opaque blobs that do not belong in an event log. Recording
  produces continuous data that is not a "domain event".
- **The CQRS read/write split maps beautifully onto the engine**: the write
  model is the editable project (control thread, `Store`), the read model is
  the compiled schedule (RT thread), and the "projection" is the graph
  compiler. Say it that way in the docs — it makes the pointer-swap rule feel
  inevitable rather than arbitrary.
- **One benefit specific to AURA:** with two front doors plus AI agents
  mutating the session, an op log is the only sane way to get convergence, an
  audit trail of what the agent did, and a "reject this op by policy" hook.
  The `mcp/policy.rs` gate belongs *at the op layer*, not per-tool.

---

## 3. SOLID in Rust

There is broad agreement that SOLID applies to Rust but must be translated,
not transliterated — e.g.
[Rust Design Patterns: Design principles](https://rust-unofficial.github.io/patterns/additional_resources/design-principles.html)
lists SOLID alongside DRY/KISS/YAGNI, while community write-ups warn that
"over-applying OOP principles can lead to non-idiomatic Rust code"
([Level Up Coding](https://levelup.gitconnected.com/is-applying-solid-principles-in-rust-a-good-practice-dc7eaf0d2270)).

**⊳ Judgment — the honest position:** three of the five principles are
load-bearing (SRP, ISP, DIP), one is a design decision with a real cost model
(OCP), and one is about contracts rather than subtyping (LSP).

### 3.1 SRP — the unit is the crate, not the struct

matklad's [Large Rust Workspaces](https://matklad.github.io/2021/08/22/large-rust-workspaces.html):
a flat `crates/` directory (good to ~1M LOC, because "even comparatively
large lists are easier to understand at a glance than even small trees"); one
crate per directory with matching names; the workspace root is a **virtual
manifest** ("putting the main crate into the root pollutes the root with
`src/`, requires passing `--workspace` to every Cargo command"); internal
crates get `version = "0.0.0"`; keep `src/` even for one-file crates.

rust-analyzer's [style guide](https://rust-analyzer.github.io/book/contributing/style.html)
classifies changes by blast radius, and treats *a new dependency edge* — "you
add a `pub use` reexport from another crate or you add a new line to the
`[dependencies]` section" — as its own review category:

> **"Adding an innocent-looking `pub use` is a very simple way to break
> encapsulation, keep an eye on it!"**

**⊳ Judgment.** SRP at module level is advice; at crate level it is enforced
by the compiler, because **Cargo's crate graph is acyclic and module graphs
are not**. Modules give you privacy; crates give you privacy *plus a
direction that cannot be violated even by accident*. For a codebase edited by
parallel agents, that difference is the whole ballgame.

### 3.2 ISP — small traits, sealed where they are contracts

Rust API Guidelines C-SEALED
([future-proofing](https://rust-lang.github.io/api-guidelines/future-proofing.html)):
a trait with a private supertrait can only be implemented inside the defining
crate, which lets you add methods later without a breaking change.

```rust
pub trait Processor: private::Sealed {
    fn process(&mut self, ctx: &ProcCtx, io: &mut Io) -> ProcessStatus;
    fn latency_samples(&self) -> u32 { 0 }
}
mod private { pub trait Sealed {} }
```

C-STRUCT-PRIVATE: public fields are a permanent commitment and prevent
invariant maintenance. rust-analyzer's style guide is blunter:

> "If a field can have any value without breaking invariants, make the field
> public. Conversely, if there is an invariant, document it, enforce it in
> the 'constructor' function, make the field private, and provide a getter.
> **Never provide setters**."

with the rationale worth carving into ARCHITECTURE.md:

> "Non-local code properties degrade under change, privacy makes invariant
> local."

**⊳ Judgment — applied to a DAW:** resist the One Big Node Trait. `Processor`
(block processing) should be separate from `HasParams` (parameter
declaration), `HasLatency`, `Stateful` (save/restore), `HasGui`. A sine
oscillator implements one; a CLAP plugin implements five. AURA's `plugins/`
module already discovered this shape via CLAP's own extension model; clack
mirrors it in crates. Copy that: **extensions are optional traits, queried
once at graph-compile time and resolved to a concrete vtable or enum in the
schedule**, never queried per block.

### 3.3 OCP and the expression problem

The decision criterion from
[Possible Rust](https://www.possiblerust.com/guide/enum-or-trait-object):

> "If the need for delegation is only internal, meaning you control all the
> variants… you're likely better off with an enum. It's faster, subject to
> fewer rules… and makes it easy to see a list of all the variants which may
> exist."

Trait objects when "the need for delegation is exposed externally". Enums are
exhaustive (the compiler proves you handled every case) and monomorphic;
trait objects are open but require dyn-compatibility and defeat inlining.

**⊳ Judgment — decision table for AURA:**

| Concept | Choose | Why |
|---|---|---|
| `EngineCmd`, `RtEvent`, `Op` (the op-log) | **`enum`, POD, `Copy` where possible** | closed set you own; exhaustive `match` is the safety property; must cross a ring buffer as plain bytes; adding an *operation* (validate, invert, serialize, apply) is the frequent change, adding a variant is the rare one |
| DSP nodes inside the schedule | **`enum NodeKind` for built-ins + one `Hosted(Box<dyn HostedProcessor>)` variant** | built-ins get inlined and stay allocation-free; third-party plugins are genuinely open-world. The standard escape hatch: closed enum with one open variant |
| Audio device backends (cpal/JACK/ASIO/CoreAudio) | **`trait` + one adapter crate per backend, chosen once at startup** | Cockburn port; cost paid once per stream, not per block; matches §10 constraint 6 |
| Plugin formats (CLAP/LV2/VST3) | **`trait PluginFormat` + `dyn`** | open-ended, resolved at scan time |
| Persistence / project versions | **`enum ProjectFile { V1(..), V2(..) }` + migration functions** | exhaustiveness is the point; you must never silently fail to migrate |
| MCP tools | **`enum` of tool names + schema, generated** | closed, and must be enumerable for the policy gate |

Supporting facts: `#[non_exhaustive]` on public enums lets you add variants
without a semver break (relevant only if you publish crates);
[`enum_dispatch`](https://docs.rs/enum_dispatch) mechanically converts a
trait into an enum when you want trait syntax with static dispatch.

> **Rule: new *kinds* of thing → trait. New *operations* on things → enum. If
> unsure, use an enum — converting enum→trait later is mechanical,
> trait→enum is not, because by then downstream code depends on the
> openness.**

### 3.4 LSP — trait contracts, not subtyping

Rust has no inheritance, so LSP shows up as **contract violations in trait
impls**. The real hazards:

- **Panicking impls of infallible-looking traits.** An `impl Processor` that
  can panic breaks the RT contract for every caller. Enforcement, not
  documentation, is the answer (§5.5).
- **Inconsistent `PartialOrd`/`Ord`/`Eq`/`Hash`.** `Hash` must agree with
  `Eq`; `Ord` must be a total order. Break these and `HashMap`/`BTreeMap`
  behave nondeterministically — a nightmare in a system whose test strategy
  is determinism. Relevant to AURA: `f32` param values, `Tick`, and any
  `NoteId` used as a map key.
- **`Deref` polymorphism** — listed as an anti-pattern in the Rust Design
  Patterns book
  ([anti_patterns/deref](https://rust-unofficial.github.io/patterns/anti_patterns/deref.html)):
  `Deref` "is designed for the implementation of custom pointer types", and
  abusing it to fake inheritance breaks trait resolution, generics and
  privacy. **LLM contributors produce this pattern often when wrapping engine
  handles. Ban it in review.**
- **Dyn-compatibility** (formerly "object safety"): generic methods,
  `Self`-returning methods and `where Self: Sized` interact with `dyn`.
  **⊳ Judgment:** for port traits, keep them dyn-compatible on purpose so the
  test double can be a `Box<dyn …>`; for RT traits, don't care — they are
  monomorphized.

### 3.5 DIP without a container

**⊳ Judgment.** Rust needs no DI framework, and adding one (`shaku`,
`waiter_di`) buys runtime failure modes in exchange for compile-time ones you
already had. The idiomatic forms, in increasing order of cost:

```rust
// 1. Generic parameter — zero cost, best for hot paths and the shell's
//    own composition root.
pub struct ControlPlane<D: AudioBackend, S: ProjectStore> { device: D, store: S }

// 2. &dyn in a function signature — zero storage cost, one-shot calls.
pub fn export(project: &Project, sink: &mut dyn SampleSink) -> Result<()>;

// 3. Box<dyn>/Arc<dyn> field — for genuinely runtime-chosen implementations
//    (which audio backend? which plugin format?). Chosen once at startup.
pub struct App { backend: Box<dyn AudioBackend> }
```

The "container" is `main()` — a composition root that constructs concrete
adapters and hands them to the core. Prefer form 1 in `aura-core`/`aura-ops`
(so tests instantiate with in-memory fakes and monomorphize away), and form 3
only in `aura-app` where the choice is genuinely dynamic.

If a trait exists solely to enable mocking and has exactly one production
impl and one test impl, that is a smell — prefer making the function take
*data* instead of a *dependency*. That is the functional-core move, and it is
cheaper than DIP.

**⊳ Judgment — the anti-pattern to name explicitly for agents:** *trait per
struct*. An LLM asked to make code testable will reflexively extract a trait
for every struct. That inflates the public surface, kills inlining and
destroys the "few, meaningful ports" property.

> **Rule: a trait must have either ≥2 real implementations, or be a published
> extension point, or not exist.**

---

## 4. Crate and module decomposition

### 4.1 What real projects do

Crate lists fetched from the repositories:

| Project | Crates | Shape |
|---|---|---|
| **Helix** | `helix-core`, `helix-view`, `helix-term`, `helix-tui`, `helix-lsp`, `helix-lsp-types`, `helix-dap`, `helix-dap-types`, `helix-loader`, `helix-vcs`, `helix-event`, `helix-parsec`, `helix-stdx` | Textbook strict layering: `core` (text/ropes/syntax, no UI) → `view` (editor state, no rendering) → `term` (the binary). Protocol *types* split from protocol *clients* (`-lsp-types` vs `-lsp`). |
| **rust-analyzer** | ~25, in explicit layers | `parser`/`syntax`/`base-db` → `hir-expand`/`hir-def`/`hir-ty` → `hir` (facade) → `ide*` → `rust-analyzer` (bin). |
| **Bevy** | 59 under `crates/` (`bevy_ecs`, `bevy_app`, `bevy_render`, `bevy_asset`, `bevy_reflect`, `bevy_math`, `bevy_platform`, `bevy_ptr`, `bevy_tasks`, `bevy_internal`, …) | Facade pattern: the `bevy` crate re-exports `bevy_internal`, which aggregates the rest; sub-crates usable standalone; explicit compile-time/binary-size rationale. |
| **Zed** | **242** under `crates/` | Extreme fine-grained split — `gpui` split further into `gpui_macos`/`gpui_linux`/`gpui_windows`/`gpui_wgpu`/`gpui_web`; `language` vs `language_core`; `settings` vs `settings_content` vs `settings_json`; per-provider LLM crates. |
| **Symphonia** | `symphonia-core`, `-common`, `-metadata`, `-format-{ogg,isomp4,mkv,riff,caf}`, `-codec-{pcm,aac,vorbis,alac,adpcm,opus,wavpack}`, `-bundle-{mp3,flac}`, plus a `symphonia` facade | **Registry/plugin pattern**: `-core` defines the traits + registry, each format/codec is a leaf crate, the facade wires them by feature flag. |
| **dasp** | `dasp_sample`, `dasp_frame`, `dasp_slice`, `dasp_ring_buffer`, `dasp_interpolate`, `dasp_signal`, `dasp_envelope`, `dasp_graph`, `dasp_window`, `dasp_peak`, `dasp_rms` (all 0.11.0) | Maximally fine — each is one concept, `no_std`-capable, so embedded/RT users take only what they need. |
| **nih-plug** | `nih_plug` + `nih_plug_derive` + `nih_plug_{egui,iced,vizia}` + `nih_plug_xtask` + `cargo_nih_plug` | Core is GUI-agnostic; each GUI toolkit is a separate crate; build tooling is its own crate. |
| **clack** | `clack-common`, `clack-host`, `clack-plugin`, `clack-extensions` (all 0.1.1) | Host and plugin sides split so a host does not compile plugin-side code; extensions isolated. |
| **Firewheel** | `firewheel-core`, `firewheel-graph`, `firewheel-nodes`, `firewheel` facade, `firewheel-pool` | Node library separated from graph engine separated from shared types. |

Sources: [Bevy crates/](https://github.com/bevyengine/bevy/tree/main/crates) ·
[Bevy crate organization](https://deepwiki.com/bevyengine/bevy/1.2-project-structure) ·
[Helix](https://github.com/helix-editor/helix) ·
[Zed crates/](https://github.com/zed-industries/zed/tree/main/crates) ·
[Symphonia](https://github.com/pdeljanov/Symphonia) ·
[dasp](https://github.com/RustAudio/dasp) ·
[nih-plug](https://github.com/robbert-vdh/nih-plug) ·
[clack](https://github.com/prokopyl/clack) ·
[Firewheel](https://github.com/BillyDM/Firewheel).

### 4.2 What the split buys, and honestly costs

**Buys:**

- **Acyclicity.** Cargo's package graph is a DAG; a cycle between crates is a
  hard error (dev-dependency cycles are the only exception). Module graphs
  inside a crate have no such restriction.
- **Compilation.** Crates are the unit of codegen and of
  parallelism/pipelining; Bevy's stated rationale for the split includes
  reducing binary size and compile time by letting users drop unused crates.
- **Encapsulation.** `pub(crate)` stops at the crate boundary; across a crate
  boundary you must be `pub`, which makes API growth visible in the diff —
  which is why rust-analyzer treats a new `pub use` as a reviewable
  architectural event.

**⊳ Judgment — costs, stated honestly:**

- Cross-crate refactors touch more files and more `Cargo.toml`s.
- You lose `pub(crate)` as a "visible to my whole subsystem" tool; you gain
  the discipline of deciding what is public.
- Feature unification across a workspace can silently enable features in a
  dependency for all members — mitigate with `resolver = "3"` (edition 2024)
  and, if it bites, [`cargo-hakari`](https://crates.io/crates/cargo-hakari)
  (0.9.38) for deliberate workspace-hack unification.
- Too-fine a split too early (Zed's 242) has real navigation cost.
  **Start at ~10–14 crates, not 40.**

> **The heuristic that held across all these projects: a crate boundary is
> justified when it enforces a *direction* or excludes a *capability*.**
> "Fewer files per crate" is not a reason. "This crate must not be able to
> allocate / must not know about Tauri / must not depend on the filesystem"
> is.

### 4.3 The proposed AURA layout

```
aura/
├── Cargo.toml                 # [workspace] members, [workspace.lints], [workspace.dependencies]
├── deny.toml                  # dependency policy (§5.3)
├── clippy.toml                # global bans (§5.2)
├── crates/
│   ├── aura-core/             # L0  pure domain: ids, Tick, TempoMap, Project, Track, Clip,
│   │                          #     MidiEvent, ParamId, gain law, tick<->sample bijection.
│   │                          #     NO tokio, NO tauri, NO cpal, NO fs. #![forbid(unsafe_code)]
│   ├── aura-rt/               # L0  RT primitives: #![no_std] + libm. Buffers, Slot ids,
│   │                          #     ParamTable, ring types, ProcCtx, ProcessStatus,
│   │                          #     the `Processor` trait. Cannot name std::sync, String, HashMap.
│   ├── aura-dsp/              # L1  pure block-processing kernels (mixer, gain/pan, filters,
│   │                          #     sampler voice, time-stretch). depends: aura-rt (+ core types)
│   ├── aura-engine/           # L1  Schedule type + executor + transport + meters.
│   │                          #     depends: aura-rt, aura-dsp. NO device, NO plugin API.
│   ├── aura-graph/            # L1  PURE graph compiler: Project -> Schedule. PDC, ordering,
│   │                          #     slot assignment, cycle detection. depends: aura-core, aura-engine
│   ├── aura-ops/              # L1  Op enum, validation, apply, INVERSE (undo), batching.
│   │                          #     depends: aura-core. The only writer of the model.
│   ├── aura-persist/          # L2  project file format, versioning, migrations, autosave.
│   ├── aura-ipc/              # L2  wire DTOs + ts-rs + schemars derives + event names.
│   │                          #     depends: aura-core (conversions only). No logic.
│   ├── aura-backend-cpal/     # L2  adapter: implements aura-engine's AudioBackend port
│   ├── aura-plugins/          # L2  adapter: CLAP (clack) + LV2 (livi) hosting; ALL unsafe here
│   ├── aura-media/            # L2  adapter: hound/symphonia/flacenc decode+encode, waveform tiles
│   ├── aura-sidecars/         # L2  adapter: process supervision, job queue
│   ├── aura-app/              # L3  THE SHELL. Store, ControlPlane, EngineHandle, job scheduler,
│   │                          #     op application + undo stack, snapshot publication.
│   ├── aura-mcp/              # L4  adapter: rmcp server + policy gate over aura-app
│   ├── aura-tauri/            # L4  adapter: the ONLY crate that names `tauri`. Commands, events.
│   ├── aura-testkit/          # dev only: offline host, fake clock, golden-wav harness, RT asserts
│   └── xtask/                 # codegen (bindings, schemas), CI tasks, arch checks
└── src/, index.html, ...      # Svelte UI unchanged
```

Notes on specific choices:

- **`aura-rt` as `#![no_std]`** is the highest-leverage single decision in
  this document. It is not a performance measure — it makes the RT rules
  *unstateable to violate*. An agent literally cannot write
  `std::sync::Mutex::lock` or `println!` in a `no_std` crate; the compiler
  rejects it. Use `libm` for transcendentals and `heapless`/fixed arrays for
  storage. Anything needing `alloc` goes behind a `prepare` feature used only
  from the control thread.
- **`aura-graph` separate from `aura-engine`** turns the most complex logic
  (PDC, ordering, latency, slot assignment) into a **pure function you can
  property-test in microseconds without audio**. The single biggest
  testability win available.
- **`aura-ipc` DTOs separate from `aura-core` model types** costs
  `From`/`TryFrom` boilerplate but decouples wire evolution from model
  evolution. Given that the schemas are *frozen* and the model must grow
  (SCALABILITY §3–§4), this is the correct trade. Helix does the same with
  `helix-lsp-types` vs `helix-lsp`.
- **All `unsafe` concentrated in `aura-plugins`** (plus the ring internals if
  hand-written), so every other crate carries `#![forbid(unsafe_code)]` and
  Miri has one target.
- **Keep it flat**, per matklad: `crates/<name>/`, directory name == crate
  name, `version = "0.0.0"`, virtual root manifest.

### 4.4 The dependency rule

> **Dependencies point strictly downhill L0→L4. `aura-core` and `aura-rt`
> depend on no AURA crate. No L≤2 crate may name `tauri`, `tokio`, `rmcp` or
> `cpal`. And no crate reachable from `aura-engine`'s process path may name
> `std::sync`, `String`, `HashMap` or `log`.**

---

## 5. Enforcing boundaries mechanically

This is the section that matters most for a codebase edited by parallel
agents.

> **⊳ Judgment — the governing principle: every rule in ARCHITECTURE.md must
> name its enforcer on the same line, or be explicitly marked
> `(unenforced — judgment)`.**

### 5.1 Tier 1 — the compiler (free, unbypassable)

| Mechanism | What it stops |
|---|---|
| **Crate graph acyclicity** | any upward dependency. Not a lint — a build error. |
| **`#![no_std]` on `aura-rt`/`aura-dsp`** | allocation, `std::sync`, `println!`, `HashMap`, `String`, file I/O — all unnameable. |
| **`#![forbid(unsafe_code)]` everywhere except `aura-plugins`** | silent `unsafe` creep. `forbid` (not `deny`) cannot be locally `allow`ed. |
| **Private fields + no setters** (rust-analyzer style rule) | invariant erosion. |
| **Sealed traits** (C-SEALED) on `Processor`, `Op` | downstream/agent-authored impls that violate contracts. |

### 5.2 Tier 2 — lints

Cargo's `[lints]` table is "respected as of Cargo 1.74"
([Cargo manifest reference](https://doc.rust-lang.org/cargo/reference/manifest.html#the-lints-section)),
with `level` ∈ {forbid, deny, warn, allow} and a `priority` integer to let
specific lints override group settings.

```toml
# aura/Cargo.toml
[workspace.lints.rust]
unsafe_code            = "forbid"
unreachable_pub        = "deny"     # catches accidental API surface growth
missing_docs           = "warn"

[workspace.lints.clippy]
all                    = { level = "deny", priority = -1 }
disallowed_types       = "deny"
disallowed_methods     = "deny"
disallowed_macros      = "deny"
```

```toml
# crates/aura-core/Cargo.toml (and every member)
[lints]
workspace = true
```

Clippy's `disallowed-types` / `disallowed-methods` / `disallowed-macros` take
`{ path, reason, replacement, allow-invalid }` entries in `clippy.toml`
([lint configuration](https://doc.rust-lang.org/clippy/lint_configuration.html)):

```toml
# clippy.toml (workspace root)
disallowed-types = [
  { path = "std::sync::Mutex",  reason = "RT plane: use atomics or SPSC rings; control plane: parking_lot" },
  { path = "std::sync::RwLock", reason = "same" },
]
disallowed-methods = [
  { path = "std::option::Option::unwrap", reason = "use expect() with an invariant message" },
  { path = "std::time::Instant::now",     reason = "no wall clock in the core; take time as a parameter" },
]
disallowed-macros = [
  { path = "std::println", replacement = "log::info!" },
  { path = "std::dbg",     reason = "no debug output in committed code" },
]
```

**⚠ Gap / verify.** Clippy resolves `clippy.toml` from the crate directory
upward, so per-crate overrides are possible when linting per package — but
this is not clearly documented. **Do not rely on per-crate `clippy.toml` for
the RT rules.** Use `no_std` for those (Tier 1) and the workspace
`clippy.toml` for the universal bans. Additionally for RT crates:
`clippy::indexing_slicing`, `clippy::unwrap_used`, `clippy::expect_used`,
`clippy::panic`, `clippy::exit`.

### 5.3 Tier 3 — dependency policy

[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) (0.20.2) has a
`bans` section whose `wrappers` field allows "specific crates to have a
direct dependency on the banned crate but denies all transitive dependencies
on it"
([bans config docs](https://embarkstudios.github.io/cargo-deny/checks/bans/cfg.html)).
**This is precisely the "only crate X may depend on Y" rule, and it applies
to your own workspace crates too.**

```toml
# deny.toml — the architecture, expressed as policy
[bans]
multiple-versions = "warn"
wildcards         = "deny"
deny = [
  { name = "tauri",      wrappers = ["aura-tauri"] },
  { name = "rmcp",       wrappers = ["aura-mcp"] },
  { name = "cpal",       wrappers = ["aura-backend-cpal"] },
  { name = "clack-host", wrappers = ["aura-plugins"] },
  { name = "livi",       wrappers = ["aura-plugins"] },
  { name = "symphonia",  wrappers = ["aura-media"] },
  { name = "hound",      wrappers = ["aura-media"] },
  { name = "tokio",      wrappers = ["aura-app", "aura-sidecars", "aura-mcp", "aura-tauri"] },
]

[advisories]
[licenses]
```

Run `cargo deny check` in CI. One command, and the layering violation an
agent would otherwise introduce becomes a red build with a readable message.

Supporting tools: `cargo-udeps` / `cargo-machete` (unused deps),
`cargo-audit` / `cargo deny check advisories` (CVEs).

### 5.4 Tier 4 — architecture tests

There is no ArchUnit for Rust. **⊳ Judgment: write the 40-line equivalent** —
genuinely better than a framework, because the rules are explicit and
readable.

```rust
// crates/xtask/tests/architecture.rs
// RULE: dependencies point downhill only. This list IS the architecture.
const ALLOWED: &[(&str, &[&str])] = &[
    ("aura-core",    &[]),
    ("aura-rt",      &[]),
    ("aura-dsp",     &["aura-rt", "aura-core"]),
    ("aura-engine",  &["aura-rt", "aura-dsp", "aura-core"]),
    ("aura-graph",   &["aura-core", "aura-engine"]),
    ("aura-ops",     &["aura-core"]),
    ("aura-persist", &["aura-core"]),
    ("aura-ipc",     &["aura-core"]),
    ("aura-app",     &["aura-core","aura-ops","aura-graph","aura-engine","aura-persist",
                       "aura-ipc","aura-plugins","aura-media","aura-sidecars","aura-backend-cpal"]),
    ("aura-mcp",     &["aura-app","aura-ipc","aura-core"]),
    ("aura-tauri",   &["aura-app","aura-ipc","aura-core","aura-mcp"]),
];

#[test]
fn workspace_dependencies_point_downhill() {
    let md = cargo_metadata::MetadataCommand::new().exec().unwrap();
    for pkg in md.workspace_packages() {
        let allowed = ALLOWED.iter().find(|(n, _)| *n == pkg.name)
            .unwrap_or_else(|| panic!("new crate `{}` must be added to ALLOWED", pkg.name)).1;
        for dep in &pkg.dependencies {
            if dep.name.starts_with("aura-") && !allowed.contains(&dep.name.as_str()) {
                panic!("ARCHITECTURE VIOLATION: {} -> {} is not allowed.\n\
                        See docs/ARCHITECTURE.md §Dependency rule. If this is intended, \
                        write an ADR and update ALLOWED.", pkg.name, dep.name);
            }
        }
    }
}
```

**The panic message is doing real work**: it tells the next agent *what
document to read and what process to follow*. Make every enforcement failure
message do that.

[`guppy`](https://github.com/guppy-rs/guppy) (0.17.26) is the heavier option
— a Rust query API over `cargo metadata`, built for exactly this (its origin
project categorised every workspace package as production or test-only and
enforced that test-only crates never reach production builds). Use it if the
naive version gets unwieldy — e.g. when transitive, feature-aware queries are
needed.

[`cargo-modules`](https://github.com/regexident/cargo-modules) (0.27.0)
prints a crate's internal module structure as a tree and its internal
dependencies as a DOT graph, supports `--acyclic` for cycle detection and
`--deny` on the `orphans` command for CI. **⊳ Judgment:** use it for
*intra*-crate hygiene (module cycles inside `aura-app`), not as the primary
boundary tool — crates + cargo-deny are stronger.

`cargo-public-api` (0.52.0) diffs a crate's public API between commits.
**⊳ Judgment:** run it on `aura-core`, `aura-ipc` and `aura-rt` only, and
commit the API snapshot — an agent widening the core's surface then shows up
as a reviewable diff, which is the mechanical version of rust-analyzer's
"keep an eye on `pub use`". `cargo-semver-checks` (0.50.0) is only worth it
if you publish crates; for an internal workspace, skip it.

### 5.5 Tier 5 — RT-safety enforcement (the important one)

**RealtimeSanitizer (RTSan)** is upstream in LLVM from v20; it is a runtime
sanitizer that raises errors on real-time violations — `malloc`, `free`,
`pthread_mutex_lock` and system calls — inside functions marked
`[[clang::nonblocking]]`, compiled with `-fsanitize=realtime`. It came out of
the audio community (Chris Apple / David Trevelyan, presented at
[ADC 2024](https://conference.audio.dev/session/2024/llvms-real-time-safety-revolution-tools-for-modern-audio-development/)).

**It has a Rust binding**: `rtsan-standalone`
(**0.3.0** on [crates.io](https://crates.io/crates/rtsan-standalone);
[realtime-sanitizer/rtsan](https://github.com/realtime-sanitizer/rtsan);
walkthrough at [steck.tech](https://steck.tech/posts/rtsan-in-rust/)). You
mark functions `#[nonblocking]`, non-RT-safe ones `#[blocking]`, and get
`#[no_sanitize_realtime]` and `scoped_disabler` as escape hatches. It is
gated behind a feature flag so the macros are zero-cost when disabled. Known
caveat: mutex behaviour differs by platform (Linux futex may avoid a syscall
in the uncontended case).

**⊳ Judgment — this is the single most valuable tool in this document for
AURA.** It converts the RT contract from prose into a test failure:

```rust
// crates/aura-engine/src/executor.rs
#[cfg_attr(feature = "rtsan", rtsan_standalone::nonblocking)]
pub fn process_block(&mut self, ctx: &ProcCtx, io: &mut Io) { /* ... */ }
```

plus a CI job: `cargo test -p aura-engine -p aura-dsp --features rtsan`
running the offline harness for a few thousand blocks with a scripted op
stream. Any agent that sneaks a `Vec::push`, a `format!`, a `log::info!` or a
`HashMap` into the executor fails that job **by name and line**.

**Backup/complement:** `assert_no_alloc` (1.1.2, last published 2021,
[Windfisch/rust-assert-no-alloc](https://github.com/Windfisch/rust-assert-no-alloc))
is a global allocator wrapper that aborts or warns on (de)allocation inside a
marked scope; the maintained fork used by nih-plug is
[BillyDM/nice-assert-no-alloc](https://github.com/BillyDM/nice-assert-no-alloc)
(updated as recently as 2026). **⊳ Judgment:** use `nice-assert-no-alloc` as
the always-on debug-build guard (it needs no LLVM 20 toolchain and catches
the most common violation), and RTSan as the thorough CI check.

**`no-panic`** (0.1.37, [dtolnay/no-panic](https://github.com/dtolnay/no-panic)):
`#[no_panic]` uses a link-time trick — if the compiler cannot prove the
function does not panic, you get a linker error
`ERROR[no-panic]: detected panic in function`. Requirements: needs
optimization (`opt-level = 1` in dev, or LTO + `codegen-units = 1` in
release), does not work with `panic = "abort"`, does not fire under
`cargo check`, no const fns, and library-only builds skip it because there is
no link step.

**⊳ Judgment — the honest assessment.** `no_panic` is worth applying to a
*small* set of leaf DSP functions, not to the whole executor — the ergonomics
degrade fast, because every slice index and every arithmetic op that could
overflow blocks the proof. Higher-value alternative for the same goal:
`clippy::indexing_slicing` plus `get()`/`get_mut()` in RT code, and
`panic = "abort"` in release so a violated invariant is a crash you will
actually see rather than an unwind through the driver's callback (unwinding
across an FFI callback boundary is UB-adjacent anyway).

### 5.6 Tier 6 — process and agent containment

- **`CODEOWNERS`** mapping `crates/aura-core`, `crates/aura-rt`, `deny.toml`
  and `docs/adr/` to the architect role — the modern replacement for the
  "FROZEN FILE" comment header, and one that GitHub enforces.
- **Per-crate `AGENTS.md`/`CLAUDE.md`** (5–15 lines): this crate's one
  responsibility, its allowed dependencies, its RT status, and the one thing
  not to do. Agents reliably read the file next to the code they are editing;
  they read a 766-line root doc less reliably.
- **CI gate order** (fail fast, cheapest first): `cargo fmt --check` →
  `cargo clippy --workspace --all-targets` → `cargo deny check` → arch test →
  `cargo nextest run` → bindings-drift check (§6.3) → `--features rtsan` job
  → `miri` on `aura-plugins` (nightly, allowed to be slow).

---

## 6. Keeping the schema-defined IPC surface honest

### 6.1 The 2026 landscape

| Option | Latest | Status |
|---|---|---|
| **ts-rs** | **12.0.1**, June 2026 | Stable, actively released (49 versions). Derive `TS`; `#[ts(export)]` generates a **test** that writes bindings to disk on `cargo test`; `TS_RS_EXPORT_DIR` configures output; `serde-compat` (default) honours `rename`, `rename_all`, `tag`, `content`, `untagged`, `skip`, `flatten`, `default`; MSRV 1.88. Caveat: `skip_serializing`/`skip_serializing_if` are only correct in combination with `#[serde(default)]`. ([docs.rs/ts-rs](https://docs.rs/ts-rs)) |
| **specta / tauri-specta** | **2.0.0-rc.25**, May | **Still a release candidate after years.** rc.21 (Jan 2026) shipped broken against then-current Tauri, requiring a git patch until [tauri#12371](https://github.com/tauri-apps/tauri/pull/12371) landed; release notes advise pinning with `=` because Tauri updates break it. ([releases](https://github.com/specta-rs/tauri-specta/releases)) |
| **schemars** | **1.2.2** | Stable 1.x. JSON Schema generation from serde types. |
| **taurpc** | 0.8.2 | Smaller ecosystem, trait-based RPC over Tauri. |
| **tauri-bindgen** | — | [tauri-apps/tauri-bindgen](https://github.com/tauri-apps/tauri-bindgen), WIT-based, experimental. |

### 6.2 Recommendation

> **⊳ Judgment: use `ts-rs` for the TypeScript surface, `schemars` for the
> MCP tool surface, serde as the single source of truth, and delete the
> hand-written `docs/ipc-schemas/*.json` in favour of generated artifacts.**

Reasoning:

1. **tauri-specta's ergonomics are genuinely nicer** (it types commands and
   events, not just structs) **but its risk profile is wrong for a
   multi-year project.** A perpetual RC that has already been broken by a
   Tauri point release, on the critical path of the only UI transport, edited
   by agents who cannot debug a macro-expansion failure — that is a bad
   trade. Revisit when 2.0.0 goes stable.
2. **`schemars` is needed regardless.** MCP tools require JSON Schema for
   their input schemas; `rmcp` consumes them. So JSON Schema generation is
   not optional — it is a load-bearing part of the AI front door. Deriving
   both `TS` and `JsonSchema` on the same DTO types in `aura-ipc` gives *one*
   source of truth feeding *two* consumers.
3. **The current `docs/ipc-schemas/*.json` are hand-written and therefore
   already drifting** (18 schema files against a moving Rust surface;
   ARCHITECTURE records the accept/emit asymmetry as debt D-06 in prose
   rather than as a check). Hand-written schemas in a repo edited by agents
   are a liability: they look authoritative and nothing verifies them.

### 6.3 The three checks that make it honest

```
# 1. DRIFT: bindings are generated artifacts, committed, and CI asserts currency.
cargo test -p aura-ipc                      # ts-rs writes ./bindings
cargo run -p xtask -- gen-schemas           # schemars writes docs/ipc-schemas/
git diff --exit-code bindings/ docs/ipc-schemas/
```

```rust
// 2. ROUND-TRIP: ts-rs describes the type; serde performs it. They can disagree.
//    Prove they don't, through the real serde path, for every DTO.
proptest! {
    #[test]
    fn op_envelope_roundtrips(v in any::<OpEnvelope>()) {
        let s = serde_json::to_string(&v).unwrap();
        prop_assert_eq!(v, serde_json::from_str::<OpEnvelope>(&s).unwrap());
    }
}
```

```rust
// 3. TOLERANCE: readers must ignore unknown fields (ARCHITECTURE §10.7 / D-06),
//    so assert it rather than describing it.
#[test] fn project_reader_preserves_unknown_fields() { /* inject a field, load, save, assert kept */ }
```

Plus: run `tsc --noEmit` in CI so the generated `.d.ts` actually type-checks
against the Svelte code — that closes the loop from Rust type change →
frontend compile error, which is the property being bought.

**⊳ Judgment.** Keep the **names** frozen as today (commands, events, op
kinds) — that discipline is correct and rare — but let the *shapes* be
generated. **Frozen names + generated shapes** is the combination that
survives.

---

## 7. Testing a real-time system

### 7.1 The harness is the primary test surface

> **⊳ Judgment — the thesis: the primary test surface is not the audio
> thread. It is a deterministic, single-threaded, offline harness that runs
> the *same* code the audio thread runs.**

If that harness exists, everything else is cheap; if it does not, you will
write flaky integration tests forever. AURA already has `audio/offline.rs` —
promote it to `aura-testkit` and make it the centre of gravity.

```rust
// aura-testkit: one thread, no audio device, no wall clock, no sleeping.
pub struct OfflineHost { app: App, engine: Engine, block: usize, pos: u64 }
impl OfflineHost {
    pub fn op(&mut self, op: Op) -> Result<()>;             // control-plane mutation
    pub fn render(&mut self, blocks: usize) -> Vec<f32>;    // drains rings, runs process_block
    pub fn drain_events(&mut self) -> Vec<RtEvent>;         // RT -> control, synchronously
}
```

Two invariants make this work, and both are *architectural* decisions, not
testing ones:

- **No wall clock below `aura-app`.** Time enters as
  `ProcCtx { sample_pos, tempo, sample_rate }`. Ban `Instant::now` /
  `SystemTime::now` in `aura-core`/`-rt`/`-dsp`/`-engine` via
  `clippy.toml disallowed-methods` (§5.2). **This *is* the fake clock** — you
  do not need a clock abstraction, you need to not have a clock.
- **Ring draining is a function, not a thread.** The harness calls the same
  drain logic the real control loop calls, so control↔RT interaction is
  tested at exact block boundaries, deterministically.

### 7.2 The seven test patterns, in order of value

**1. Block-size invariance (property test).** Render the same project at
block sizes 1, 17, 64, 128, 512, 1024 and assert **bit-identical** output.

**⊳ Judgment — this is the highest-value test in a DAW.** It catches: state
carried across blocks incorrectly, off-by-one in event scheduling, filters
reset at block boundaries, parameter smoothing tied to block rate, loop-wrap
bugs, and anything that indexes by block instead of by sample. One test,
enormous coverage. AURA's loop-region and auto-stop-at-end features are
exactly the kind of code it protects.

**2. Golden renders.** Golden test vectors are stored known-input/known-output
pairs used to pin DSP behaviour so any deviation is caught as a regression
([GopherTrunk](https://gophertrunk.org/reference/golden-test-vectors/)); the
technique is standard in plugin work, e.g. rendering a fixed scenario in
REAPER against a reference render
([JUCE forum](https://forum.juce.com/t/automated-testing-with-reaper-on-macos/65905)).
**⊳ Judgment:** store short (1–3 s) f32 renders, compare with an explicit
tolerance rather than a hash — a hash gives "it changed" with no diagnosis,
and cross-platform float differences (FMA contraction, denormal handling,
libm) will make hashes flaky. Compare max abs error and, for anything with a
filter or reverb, an RMS-of-difference threshold. Pin denormal handling
(FTZ/DAZ) explicitly, and record which platform produced the golden.

**3. Round-trip / algebraic properties (`proptest` 1.11).** The pure crates
make these nearly free:

- `tick → sample → tick` is the identity across arbitrary tempo maps
  (ARCHITECTURE §13's bijection claim — assert it, don't assert it in prose).
- `apply(op); apply(op.inverse())` == original project, for arbitrary `Op`
  and arbitrary project. **This is the undo test, and it is the reason to
  build undo as inverse ops in a pure crate.**
- `compile(project)` is deterministic and acyclic; recompiling after a no-op
  edit yields an equal schedule.
- Serialize→deserialize→serialize is a fixed point for `Project` at every
  schema version, including migrations
  (`v1 → v2 → render` == `v1 → render`).

**4. Impulse-response latency test.** Send a unit impulse through the graph;
assert the output impulse lands at exactly `reported_latency_samples`.
**⊳ Judgment:** this is the only reliable way to test PDC, and it becomes
essential the moment plugins are in the graph (§10 constraint 5 already
requires every processor to report latency — this is the test that makes that
requirement real).

**5. RT-safety assertions in tests.** As §5.5: a `--features rtsan` job
renders thousands of blocks while a scripted op stream mutates the graph
(add/remove track, swap plugin, seek, loop-wrap), asserting no allocation,
lock or syscall. Include the *nasty* transitions — graph swap under playback,
plugin removal while its voices are ringing — since that is where allocation
sneaks in.

**6. Output sanity assertions, always on.** After every render in every test:
no NaN, no Inf, `|x| <= ceiling`. Costs nothing, catches uninitialised
buffers, denormal explosions and feedback loops.

**7. Adversarial / fuzz layer.**
[`clap-validator`](https://github.com/free-audio/clap-validator) runs plugins
through an automatic test suite plus "a multi-process fuzzer that can run the
plugin through a series of random parameter changes, note on/off events, and
transport changes while checking for crashes, hangs, and spec-compliance
issues", running tests in separate processes so a crash is data rather than a
lost run. **⊳ Judgment:** steal the whole design for the *host* side — an
out-of-process fuzz runner driving `OfflineHost` with random op streams and
asserting invariants. And run clap-validator itself against the plugins you
host, so you can distinguish "our host is wrong" from "the plugin is wrong",
which will otherwise consume days.

**Supporting tools:** `cargo-nextest` (process-per-test isolation, which
matters because RT tests can abort), `insta` 1.48 (snapshot the compiled
`Schedule` as text — an excellent, human-readable regression surface for the
graph compiler), `criterion` for worst-case-per-block benchmarks
(**⊳ Judgment: track the *max*, not the mean** — Bencina's point 2 in
benchmark form).

---

## 8. Documentation that survives handover to an LLM

### 8.1 The rules worth copying

matklad's [ARCHITECTURE.md](https://matklad.github.io/2021/02/06/ARCHITECTURE.md.html)
argument:

> "it takes 2x more time to write a patch if you are unfamiliar with the
> project, but it takes **10x** more time to figure out where you should
> change the code."

Prescription: a bird's-eye problem statement; a **codemap** ("a map of a
country, not an atlas of maps of its states") naming important
files/modules/types *without links* so readers use symbol search; explicit
**architectural invariants** — "often, important invariants are expressed as
an absence of something"; layer boundaries. Prohibitions: no implementation
detail, nothing that changes frequently, keep it short "because every
recurring contributor will have to read it."

rust-analyzer's own
[architecture.md](https://rust-analyzer.github.io/book/contributing/architecture.html)
is the worked example, and its invariants are stated exactly as absences:

> "Base-db knows nothing about Cargo"
> "Files are represented with opaque `FileId`; there's no operation to get
> `std::path::Path`"
> "[the binary] is the only crate that knows about LSP and JSON
> serialization"
> "Core parts (ide/hir) don't interact with outside world and thus can't
> fail."

Nygard's ADR template — Title, Status (proposed/accepted/rejected/deprecated/
superseded), Context, Decision, Consequences — collected at
[adr.github.io](https://adr.github.io/) and
[joelparkerhenderson/architecture-decision-record](https://github.com/joelparkerhenderson/architecture-decision-record).

rust-analyzer's style guide mandates **one sentence per line** in Markdown,
"to improve editability and readability of diffs."

### 8.2 The mandated document set

**⊳ Judgment.** Opinionated, and deliberately small — a docs tree an agent
cannot read in one context window is a docs tree that does not exist.

| # | Document | Rule |
|---|---|---|
| 1 | **`docs/ARCHITECTURE.md`** — codemap + invariants. **Hard cap ~400 lines.** | Every invariant is phrased as an absence ("`aura-core` cannot name `tokio`") and **names its enforcer inline** (`— enforced by deny.toml`) or is tagged `(unenforced — judgment)`. |
| 2 | **`docs/adr/NNNN-slug.md`** — Nygard format, append-only. | Superseded ADRs get `Status: superseded by ADR-0031`, never deletion. **Any change to a frozen name, a crate boundary, `deny.toml`, or the RT rules requires an ADR in the same PR.** |
| 3 | **Per-crate `//!` module doc** — ≤15 lines. | States: the one responsibility, the allowed dependencies, the **RT status** (`RT: forbidden` / `RT: safe` / `RT: hot path`), and the one thing you must not do here. |
| 4 | **Per-crate `AGENTS.md`** (or `CLAUDE.md`) | Same content as #3, aimed at the tool rather than the reader, plus "if you need X, write an ADR / report it; do not edit `deny.toml`." |
| 5 | **`crates/xtask/tests/architecture.rs`** | The dependency rule as executable code. Its panic messages cite ARCHITECTURE.md sections. |
| 6 | **`docs/SCALABILITY.md`** — the debt register (D-01…D-12). | Keep. Unusually good practice and rarely done; it is what stops an agent from "fixing" a known deliberate limitation. |
| 7 | **`docs/history/`** | Phase plans (`PHASE2-PLAN.md`, `PHASE3-PLAN.md`) move here on completion. Superseded plans in the live docs tree actively mislead agents, who cannot tell "current" from "historical" without dates. |

### 8.3 What actually works when the next contributor is an LLM

**⊳ Judgment**, in order of effect:

1. **Enforcement > prose.** An agent that violates a lint sees the failure in
   its own loop and fixes it. An agent that violates a paragraph in a
   766-line doc ships it. Every hour spent moving a rule from #1 into #4/#5
   is worth ten hours of doc polish.
2. **Locality > completeness.** A 10-line `//!` doc at the top of the file
   being edited beats a perfect central document. Agents read what is
   adjacent.
3. **Error messages are documentation.**
   `panic!("ARCHITECTURE VIOLATION: … see docs/ARCHITECTURE.md §3; if
   intended, write an ADR")` is read at exactly the moment it is needed, by
   exactly the party who needs it. Invest in failure text.
4. **Absences over descriptions.** "`aura-rt` is `no_std`" tells an agent
   more than three paragraphs about real-time safety, because it is
   checkable and unambiguous.
5. **Rationale prevents re-litigation.** Without ADRs, each new agent
   re-derives (and often reverses) decisions. The Consequences section is the
   part that stops the reversal — it pre-answers "why not just X?"
6. **One sentence per line in Markdown.** Trivial, but it makes doc diffs
   reviewable, which makes doc updates actually happen.
7. **Date every document, and delete freely.** Stale docs are worse than
   missing docs for an LLM, which has no way to discount them.

---

## 9. The recommended skeleton

### 9.1 Dependency graph

```
                         ┌───────────────┐   ┌───────────────┐
      L4  adapters       │  aura-tauri   │   │   aura-mcp    │
          (front doors)  │ (only crate   │   │ (rmcp+policy) │
                         │ naming tauri) │   └───────┬───────┘
                         └───────┬───────┘           │
                                 └──────────┬────────┘
                                            ▼
      L3  imperative shell             ┌──────────┐
                                       │ aura-app │  Store · ControlPlane · undo stack ·
                                       └────┬─────┘  EngineHandle · job scheduler
             ┌──────────────┬───────────────┼──────────────┬─────────────────┐
             ▼              ▼               ▼              ▼                 ▼
      L2  ┌─────────┐ ┌───────────┐ ┌──────────────┐ ┌───────────┐ ┌───────────────┐
          │aura-    │ │aura-      │ │aura-backend- │ │aura-media │ │aura-sidecars  │
          │persist  │ │plugins    │ │cpal          │ │           │ │               │
          │         │ │(ALL unsafe│ │              │ │symphonia/ │ │ demucs/whisper│
          │         │ │ clack/livi│ │              │ │hound/flac │ │               │
          └────┬────┘ └─────┬─────┘ └──────┬───────┘ └─────┬─────┘ └───────┬───────┘
               │            │              │               │               │
               │      ┌─────┴──────┐       │               │               │
               │      │ aura-ipc   │◄──────┴───────────────┴───────────────┘
               │      │ DTOs+ts-rs │
               │      │ +schemars  │
               │      └─────┬──────┘
               ▼            ▼
      L1  ┌──────────┐ ┌──────────┐  ┌──────────────┐   ┌───────────┐
          │ aura-ops │ │aura-graph│  │ aura-engine  │◄──│ aura-dsp  │
          │ Op·apply │ │ compiler │─►│ Schedule ·   │   │  kernels  │
          │ ·inverse │ │  (PURE)  │  │ executor ·   │   └─────┬─────┘
          └────┬─────┘ └────┬─────┘  │ transport    │         │
               │            │        └──────┬───────┘         │
               ▼            ▼               ▼                 ▼
      L0  ┌──────────────────────┐     ┌──────────────────────────┐
          │      aura-core       │     │        aura-rt           │
          │ model · Tick ·       │     │ #![no_std] Buffers ·      │
          │ TempoMap · pure math │     │ SlotId · ParamTable ·     │
          │ #![forbid(unsafe)]   │     │ rings · Processor trait   │
          └──────────────────────┘     └──────────────────────────┘

      aura-testkit (dev-dependency only)  ──► OfflineHost over aura-app + aura-engine
      xtask                               ──► codegen · arch tests · CI
```

### 9.2 Crate responsibilities

| Crate | Responsibility | RT status | May not name |
|---|---|---|---|
| `aura-core` | Project model, musical time, pure math. The vocabulary of the domain. | RT: forbidden (control-side data) | any AURA crate, tokio, tauri, fs |
| `aura-rt` | RT primitives: buffers, slot ids, param table, ring types, `Processor` trait. **`#![no_std]`** | RT: hot path | std |
| `aura-dsp` | Stateless-per-block DSP kernels. Mixer, gain/pan, filters, sampler voice, stretch. | RT: hot path | std collections, alloc on the process path |
| `aura-engine` | `Schedule` + executor + transport + meters + RT event emission. | RT: hot path | devices, plugin APIs, fs |
| `aura-graph` | **Pure** `Project → Schedule` compiler: ordering, PDC, slot assignment, cycle check. | RT: forbidden | anything imperative |
| `aura-ops` | `Op` enum, validation, `apply`, `inverse` (undo), batching. Single writer of the model. | RT: forbidden | adapters |
| `aura-persist` | Project file format, versioning, migration, autosave/crash recovery. | RT: forbidden | engine, adapters |
| `aura-ipc` | Wire DTOs + `ts-rs` + `schemars` + frozen command/event/op names. No logic. | RT: forbidden | everything but core |
| `aura-backend-cpal` | Adapter: device enumeration, stream setup, xrun/latency reporting. | boundary | model, ops |
| `aura-plugins` | Adapter: CLAP (clack) + LV2 (livi) hosting, scanning, state. **All `unsafe` lives here.** | boundary | model, ops |
| `aura-media` | Adapter: decode/encode (symphonia/hound/flacenc), waveform tiles. | RT: forbidden | engine |
| `aura-sidecars` | Adapter: process supervision, job queue, model runners. | RT: forbidden | engine |
| `aura-app` | **The shell.** Owns Store, applies ops, publishes snapshots, drives everything. | RT: forbidden | tauri, rmcp |
| `aura-mcp` | Front door: MCP tools + policy gate, expressed as ops. | RT: forbidden | tauri, engine internals |
| `aura-tauri` | Front door: Tauri commands/events/channels. **The only crate naming `tauri`.** | RT: forbidden | engine internals |
| `aura-testkit` | `OfflineHost`, fake clock, golden-WAV harness, RT assertions, op-stream fuzzer. | dev only | — |
| `xtask` | Bindings/schema codegen, arch tests, CI orchestration. | dev only | — |

### 9.3 The three rules that must never be broken

> ### RULE 1 — The RT plane allocates nothing, locks nothing, blocks on nothing, and panics never.
>
> *Scope:* `aura-rt`, `aura-dsp`, `aura-engine`'s process path, and everything
> reachable from `process_block`.
>
> *Corollaries:* structural change reaches the RT thread only as an
> atomically swapped, pre-compiled `Schedule` pointer; the retired schedule
> is published back for the control thread to drop; parameter changes go
> through the preallocated param table, never a graph rebuild; identifiers on
> the RT side are dense `SlotId`s, never UUIDs or strings.
>
> *Enforced by:* `aura-rt` is `#![no_std]` (violations are compile errors) ·
> `clippy.toml disallowed-types/methods/macros` · `nice-assert-no-alloc` in
> debug builds · a CI job running the offline harness under
> `rtsan-standalone` `#[nonblocking]` · `panic = "abort"` in release.
>
> *Rationale:* Bencina — "any code path inside a critical section that's
> shared with the real-time audio thread would have to follow all of the
> rules we're outlining here… It would be easy for bugs to creep in." The
> contract is transitive, so it must be a *crate* boundary.

> ### RULE 2 — Dependencies point downhill only, and `aura-core`/`aura-rt` name nothing of ours.
>
> *Corollaries:* exactly one crate may name `tauri`; one may name `rmcp`; one
> may name `cpal`; one may name a plugin SDK; `unsafe` exists in exactly one
> crate. Adding any workspace dependency edge is an architectural change
> requiring an ADR.
>
> *Enforced by:* Cargo's acyclic crate graph ·
> `deny.toml [bans] deny = [{ name = "tauri", wrappers = ["aura-tauri"] }, …]` ·
> `xtask/tests/architecture.rs` allowlist · `unreachable_pub` +
> `cargo-public-api` snapshots on `aura-core`/`aura-ipc`/`aura-rt`.
>
> *Rationale:* rust-analyzer — "Adding an innocent-looking `pub use` is a
> very simple way to break encapsulation." Crates make the direction
> unviolable; modules only make it aspirational.

> ### RULE 3 — One writer, one op vocabulary, one generated wire schema.
>
> *Corollaries:* every mutation — from the UI, from MCP, from a test — is an
> `Op` validated and applied by `aura-ops`; undo is `Op::inverse`, never an
> ad-hoc UI-local stack; policy gating happens once, at the op layer, not per
> front door; command/event/op **names** are frozen and change only via ADR;
> DTO **shapes** are generated from Rust by `ts-rs` + `schemars` and CI fails
> on drift; no schema file is ever hand-edited.
>
> *Rationale:* Cockburn — "allow an application to equally be driven by
> users, programs, automated test or batch scripts." AURA already has three
> drivers. One op vocabulary is what makes the third one (tests) free, and
> what makes an AI agent's actions auditable and reversible.

### 9.4 Migration order from the current single crate

**⊳ Judgment.** Do not do a big-bang split. Extract in dependency order,
lowest first, one crate per PR, each with its arch-test entry and its
`deny.toml` line added in the same PR:

1. **`aura-core`** — pure types out of `audio/types.rs`, `midi/types.rs`,
   `midi/tempo.rs`. Biggest immediate testability win, zero risk.
2. **`aura-rt` (`no_std`) + `aura-dsp`** — this is where Rule 1 becomes real.
   Land the `rtsan` CI job with it.
3. **`aura-ipc` + ts-rs/schemars codegen**; retire the hand-written
   `docs/ipc-schemas/`.
4. **`aura-graph`** — extract the schedule build from `audio/engine.rs` as a
   pure function; add block-size-invariance and impulse-latency tests.
5. **`aura-ops`** — define `Op`, port the existing `control/ops.rs` surface,
   add `inverse`, unlock undo (currently correctly deferred per §10 rule 9).
6. **`aura-plugins`, `aura-backend-cpal`, `aura-media`, `aura-sidecars`** —
   mechanical, mostly moves.
7. **`aura-app`, then `aura-tauri`/`aura-mcp` last** — the shell shrinks as
   the layers below absorb logic.

---

## 10. What this means for AURA

**The single most consequential finding is that AURA's rules are correct and
its enforcement is absent.** ARCHITECTURE §2.1's four RT rules, §10's
thirteen constraints, §11's control-plane-as-single-seam rule and CONTRIBUTING's
"reviewers will hold the line here" are all sound engineering. None of them
is checked by anything the build runs. There is no `.github/workflows`, no
allocator hook, no lint configuration, no dependency policy, and no
architecture test.

That was a reasonable trade for a four-agent sprint with a human orchestrator
arbitrating every merge. It is not a viable trade for a decade.

**Concretely, and in priority order:**

1. **`aura-rt` as `#![no_std]`** converts §2.1 from prose into compile
   errors. Nothing else in this document has that leverage.
2. **`rtsan-standalone` in CI** converts the transitive half of the RT
   contract — the part `no_std` cannot see, because it lives in
   `aura-engine`'s process path — into a named, line-numbered test failure.
3. **`deny.toml` with `wrappers`** expresses "only `aura-tauri` may name
   `tauri`" as policy the build enforces, in six lines.
4. **The 40-line architecture test** makes the dependency rule executable,
   and its panic message becomes the documentation an agent reads at exactly
   the moment it matters.
5. **Generated schemas** retire eighteen hand-maintained JSON files that look
   authoritative and are verified by nothing.

**And the ownership convention specifically.**

ARCHITECTURE §0's *"FROZEN files — nobody edits these"* header, with its
instruction to "state the request in your final report" rather than editing,
worked — it was the right mechanism for four agents building in parallel over
days, arbitrated by an orchestrator who read every report.

**It will not survive years, because it depends on every agent choosing to
obey a comment.** A comment cannot fail a build. A new contributor — human or
model — who has not read §0, or who has read it and rationalised an
exception, produces a merge that looks fine and silently erodes the boundary.
That is the erosion mode this whole document exists to prevent.

Replace it, at migration step 1, with the three mechanisms that cannot be
rationalised away:

- **`CODEOWNERS`** — GitHub enforces review by the architect role on
  `crates/aura-core`, `crates/aura-rt`, `deny.toml`, `clippy.toml` and
  `docs/adr/`. The frozen-file list becomes a path list the platform honours.
- **`deny.toml`** — the "which crate may depend on what" half of the frozen
  rule becomes a policy file, checked by `cargo deny check` in CI.
- **`xtask/tests/architecture.rs`** — the dependency direction becomes a test
  that fails with a message naming the document and the process.

Keep the *doctrine* of §0 — single ownership per zone, changes to shared
surface go through the architect, no "just this once". Move its *enforcement*
out of prose and into the build. That is the one change that determines
whether the architecture in this document is still true in three years.
