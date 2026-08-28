# Gap analysis — `jit.md` against the engine as it stands

Source brief: [`../jit.md`](../jit.md) (owner canvas, 2026-08-24). Task
list and outcome: [`backlog/jit-engine.md`](backlog/jit-engine.md).

The brief describes a target architecture (`aura_engine/` with `sync/`,
`jit/`, `timeline/`, `host/`, `process/`, `metrics/`) and a six-item
execution queue. This is the audit it asks for as item 1: what of that
already exists here under another name, what is genuinely missing, and
what should not be built.

The short answer is that the brief was written against a smaller engine
than the one in the repo. Three of its six items are already implemented
and load-bearing; one would be a regression; two are real gaps, and those
two are what this track builds.

| # | Brief item | Verdict |
|---|---|---|
| 1 | Audit & report | this file |
| 2 | Lock-free `TripleBuffer<T>` | **gap — built** |
| 3 | `assert_no_alloc` + criterion harness | **gap — built** (not with `assert_no_alloc`; see §3) |
| 4 | Cranelift fusion of `[Gain, Ramp, Pan]` | **gap — built** |
| 5 | Sample-accurate scheduler | **exists** — do not rebuild (§5) |
| 6 | Carla C-FFI host bridge | **declined** (§6) |

## 1. What the brief did not know was already here

### 1.1 Lock-free structural handoff

The brief's `sync/` module is asked for as new work. The engine already
does structural handoff without locks, and not casually:

- `rtrb` is the project's primary RT primitive by written policy
  (`ARCHITECTURE.md` §2.2), with the queue inventory in §2.3.
- A graph swap is an **ownership transfer**, not a shared pointer.
  `audio/rt.rs:819` `GraphPtr(*mut RtGraph)` moves a `Box` through an
  rtrb queue; the callback takes it with `into_box()` and pushes the
  retired graph back so the **drop happens control-side**
  (`audio/engine.rs:513`, the `while self.retire_tx.slots() > 0` loop).
  The callback will not adopt a new graph unless the retire queue has
  room for the old one — the deallocation is designed out rather than
  hoped away.

So the brief's `ArcSwap` suggestion would be a step back here: the last
release of a retired value can land on the RT thread, which is precisely
what the `GraphPtr` dance exists to prevent. `arc-swap` is not in
`src-tauri/Cargo.toml` and should stay out of the callback path.

What is *not* covered is a different shape — see §2.

### 1.2 Sample-accurate timing

The brief's `timeline/` module (beat/sample converters, sample-precise
MIDI queue) exists, with a stronger contract than "sample-accurate":

- `time.rs:43` — **superticks**, `SUPERTICKS_PER_SECOND = 508_032_000`,
  chosen so common tempi divide exactly (`period_from_bpm(60.0) ==
  SUPERTICKS_PER_SECOND` is asserted). Timing is integer, not float
  seconds.
- `midi/schedule.rs` — ticks become **absolute engine samples on the
  control thread only** (`clip_events`, `clip_notes`,
  `clip_drive_events`), sorted, offs-before-ons at equal positions.
  Nothing tick-shaped crosses onto the RT thread; the callback slices a
  presorted `[pos, pos+run)` window into per-block offsets
  (`ARCHITECTURE.md` §15.1).

Rebuilding this would mean two timing models in one engine, which is the
one thing a DAW cannot afford.

## 2. Gap: a whole-snapshot publish (`TripleBuffer<T>`)

The rtrb transfer above is a **queue**: every version the writer pushes
is one the reader walks. That is right for graph swaps (each generation
matters) and wrong for *latest-value* state, where the reader wants only
the newest and the writer must never block or free.

Nothing in the engine publishes a whole preallocated snapshot with
latest-value semantics. `sync::TripleBuffer<T>` fills that:

- Wait-free SPSC, one `AtomicUsize` handoff.
- Allocation **only in `new`** — three `T`s up front.
- The consumer never constructs, drops, or refcounts a `T`. The writer
  gets retired slots back and **recycles** them instead of freeing.

That recycling is not a tidiness point: it is what makes the JIT usable
at all. `cranelift_jit::JITModule` leaks its executable pages on drop by
design, and `free_memory` is `unsafe` — discharging its safety condition
needs a *provably retired* table, on the control thread. The triple
buffer is where that proof comes from (`tests/publish.rs`).

**Which is also its limitation, and §8 acts on it.** That is the only
concrete consumer this primitive has. If the JIT does not ship — the
recommendation below — `TripleBuffer<T>` is correct, tested code with no
call site: a sound answer to a question nothing in AURA is currently
asking. The engine's *existing* RT seams are already right for the
problems it does have (rtrb ownership transfer for graph swaps, atomics
for params), which §3 confirms by audit. Nobody should read this section
as "AURA has a dropout problem that a triple buffer fixes". It does
not.

## 3. Gap: nothing enforces the RT rules

The four rules are prose — `ARCHITECTURE.md` §2.1, §10 rules 1–6,
`CONTRIBUTING.md` §1 — and dossier 10 records the consequence as gap 19:
"Nothing enforces the RT rules mechanically ... The four non-negotiable
rules are prose."

The audit's own reading is that the rules are in fact **kept**. Dossier
10 §1.1's summary still holds after Plan G2: on the RT thread there are
atomic loads/stores, rtrb push/pop, one `Box::from_raw`/`into_raw`
pointer move, fixed-size stack POD, and node `process` calls. Scanning
`audio/mixer.rs` for `lock()`, `Vec::new`, `vec![`, `push`, `format!`,
`to_string`, `collect`, `Box::new` returns hits **only inside
`#[cfg(test)]`**, plus the two noted in §3.1. There is no `eprintln!`,
`println!`, or `log::`/`tracing::` call anywhere on the path.

Kept by discipline is still unenforced, so this crate makes it a test:

- `metrics::AllocCounter` — a counting global allocator, so "no
  allocation on the audio thread" is asserted rather than reviewed.
  `tests/no_alloc.rs` proves 64 blocks × 32 tracks touch it **zero**
  times, and its first test asserts `counting_is_active()` so the guard
  cannot pass vacuously.
- `metrics::Telemetry` — per-block load against the block's own deadline
  (`frames / sample_rate`). This is a different xrun class from
  `SharedRt::xruns`, which counts *ring overflows*, not late renders.
- `benches/kernel.rs` — criterion, per §5 of the brief.

**Deviation from the brief:** `assert_no_alloc` is not used. It was last
published in 2021, and it can only *trip* on an allocation, not count
them — so it cannot express "the guard is armed", which is the assertion
that stops a no-alloc test from being decorative. `AllocCounter` is ~60
lines and adds no dependency.

**Deviation from the brief:** cranelift is pinned at **0.134**, not the
`0.110` the brief lists. 0.110 predates the `cranelift-jit` API used
here; 0.135 requires rustc 1.95 and this toolchain is 1.94.1. 0.134 is
the newest line that builds.

### 3.1 Two findings on the RT path

Neither is a rule violation. Both are recorded because an audit that
reports only "clean" is not worth reading.

1. **`audio/mixer.rs:626` and `:1000` — `graph.params.clone()` per
   block.** An `Arc<ParamTable>` refcount increment and a matching
   decrement on the RT thread, every block, for a **borrow-checker
   workaround**: the comment says `render(g, &g.params, ...)` does not
   borrow-check, so the clone buys a split borrow. No allocation and no
   lock, so the four rules hold, and the drop cannot deallocate because
   the graph itself owns a strong reference for the whole block.

   It is worth writing down anyway, on two counts. The safety of that
   drop is an *invariant about the caller* (the parent outlives the
   clone), not a property of the code at the callsite — and "it doesn't
   borrow-check" is a reason that only holds given a signature we chose.
   Passing the table by shared reference from a caller that split the
   borrow one level up removes both atomics. Small, and out of scope for
   this track: it touches `src-tauri`.

2. **`Kernels::run` returns `#[must_use]` and *must* be honoured.** Since
   Plan G2 the post-fader buffer is an **overwrite** target, so a caller
   that ignored a `false` return would not render silence — it would ship
   the *previous* block's audio to the master and to every send tapping
   that strip. The kernel declines exactly one case (`plan.overflowed`:
   more automation breakpoints in the block than the plan holds) and the
   scalar path is the fallback. This is a property of the API this crate
   hands over, and the reason `run`'s `#[must_use]` message says what it
   says.

## 4. Gap: the fused kernel, and what it is actually worth

`mixer::apply_fader_into` is one hand-written per-sample loop: a ramp
cursor lookup, a pan lerp *including a divide*, a branch on mute, meter
folding, two stores. The brief asks for `[Gain, Ramp, Pan]` fused by
Cranelift.

The kernel this crate generates is **not** `apply_fader_into`
JIT-compiled. `strip::plan` is what made it possible: between two
automation breakpoints, gain and pan are straight lines, so the
per-sample lookup, the divide and the branch collapse into six
coefficients, and what is left vectorises — two frames per iteration, no
branch, no lookup, no divide.

Measured on **one Intel Core i9-14900 core at boost** (32 tracks, medians,
`cargo bench`, `target/criterion` wiped first). Naming the CPU is not
boilerplate: it is close to the fastest single-thread machine available, so
every absolute figure below is a best case and the percentages are the
smallest they will ever be. §4.1 says what changes on a weaker one.

Full path (32 tracks rendered and mixed into the master), medians:

| block | case | `multipass` | `apply_fader_into` | `fused_scalar` | `jit` | `master_mix_only` |
|---|---|---|---|---|---|---|
| 128 | flat | 14.1 µs | **12.8 µs** | 11.9 µs | 7.8 µs | 2.8 µs |
| 128 | ramped | 22.8 µs | **20.1 µs** | 14.6 µs | 9.4 µs | 2.8 µs |
| 128 | ramped+pan | 22.8 µs | **20.3 µs** | 13.0 µs | 9.2 µs | 2.7 µs |
| 128 | long-lane | 25.0 µs | **22.5 µs** | 15.6 µs | 11.7 µs | 2.9 µs |
| 512 | flat | 55.5 µs | **49.1 µs** | 45.0 µs | 28.8 µs | 10.1 µs |
| 512 | ramped | 90.5 µs | **81.2 µs** | 46.7 µs | 32.2 µs | 9.9 µs |
| 512 | ramped+pan | 90.3 µs | **81.9 µs** | 47.1 µs | 32.7 µs | 10.8 µs |
| 512 | long-lane | 98.3 µs | **89.1 µs** | 56.7 µs | 41.2 µs | 10.7 µs |
| 1024 | flat | 106.0 µs | **97.8 µs** | 89.8 µs | 57.1 µs | 19.5 µs |
| 1024 | ramped | 176.8 µs | **165.4 µs** | 92.1 µs | 61.1 µs | 19.5 µs |
| 1024 | ramped+pan | 183.7 µs | **164.4 µs** | 92.1 µs | 64.9 µs | 20.5 µs |
| 1024 | long-lane | 190.8 µs | **177.5 µs** | 110.5 µs | 80.3 µs | 21.1 µs |

`long-lane` is a 12 000-breakpoint automation lane rendered near its end —
a lane written across a whole song rather than a few seconds. It exists
because its absence hid a bug; see §4.2.

The fader alone, with `master_mix_only` subtracted — the constant every
contender pays and none can avoid — and the win split into the part that
is the plan and the part that is the code generator:

| block | case | baseline | `jit` | total | of which plan | of which codegen |
|---|---|---|---|---|---|---|
| 128 | flat | 10.0 µs | 5.1 µs | **1.99×** | 1.10× | 1.80× |
| 128 | ramped | 17.3 µs | 6.7 µs | **2.60×** | 1.46× | 1.78× |
| 128 | ramped+pan | 17.6 µs | 6.5 µs | **2.71×** | 1.72× | 1.58× |
| 128 | long-lane | 19.6 µs | 8.8 µs | **2.23×** | 1.54× | 1.44× |
| 512 | flat | 39.0 µs | 18.6 µs | **2.09×** | 1.12× | 1.87× |
| 512 | ramped | 71.3 µs | 22.3 µs | **3.20×** | 1.94× | 1.65× |
| 512 | ramped+pan | 71.2 µs | 21.9 µs | **3.25×** | 1.96× | 1.66× |
| 512 | long-lane | 78.4 µs | 30.4 µs | **2.58×** | 1.71× | 1.51× |
| 1024 | flat | 78.4 µs | 37.6 µs | **2.08×** | 1.12× | 1.87× |
| 1024 | ramped | 145.9 µs | 41.6 µs | **3.51×** | 2.01× | 1.75× |
| 1024 | ramped+pan | 143.9 µs | 44.4 µs | **3.24×** | 2.01× | 1.61× |
| 1024 | long-lane | 156.4 µs | 59.3 µs | **2.64×** | 1.75× | 1.51× |

Compiling the whole table costs **234 µs**, once, on the control
thread; nothing in the design lets a graph rebuild wait on it.

Reported five ways on purpose. `multipass` is the un-fused node-graph
shape the brief asks to be compared against — quoting the JIT against
that alone would credit it with beating a straw man. `apply_fader_into`
is the real baseline, because it is what the app runs today.
`fused_scalar` is the same plan compiled by rustc, which splits the win
into the part that is the **algorithm** and the part that is the **code
generator**. `master_mix_only` is the pass every contender pays and none
can avoid, so a reader can subtract it instead of guessing.

What the split says, and it is the most useful thing in the table: on an
**automated** block the win is about evenly divided — the plan is ~1.9–2.1×
of it and the code generator ~1.6–1.9×. On a **flat** block the plan is worth
almost nothing (1.07–1.27×) and essentially the whole 2× is the code
generator. That is the expected shape rather than a surprise: with nothing
moving there are no per-sample lookups or divides for the plan to collapse,
so all that is left to win is the vectorisation. It also means the two halves
are separable — the plan alone, run as scalar Rust, is already most of the
gain on the blocks that have automation on them, and it needs no JIT at all.

One instruction was worth ~2× on its own during development: `fmax` for the
peak meter lowers to a multi-instruction IEEE/WASM NaN dance on x86, and a
compare-and-blend computes the same value for every sample a peak meter
deals in.

Equivalence, which is the actual claim:

- vs. `fused_scalar`: **bit-identical**, per sample, across flat, ramped,
  panned, PDC-shifted and multi-segment blocks, and every odd block size
  from 1 to 511. Anything less would be a codegen bug, so the test
  asserts equality, not a tolerance.
- vs. `apply_fader_into`: **bit-identical in the flat case** — no ramp,
  static pan, which is most blocks of most sessions — and within 1e-5
  relative once a ramp is moving, because the affine form reassociates
  the multiply. Stated rather than buried: **no version of this can be
  validated by byte-comparing an existing bounce.**
- Meters agree to 1e-4. Four vector lanes reduced at the end is a
  different addition order from 512 scalar `+=`, and no amount of care
  makes float addition associative.

**The caveat this report has to carry, and it is the headline.** At 512
frames and 48 kHz the block deadline is 10.67 ms. The baseline's worst
case in that table is 89.1 µs — **0.8% of one i9-14900 core** for 32
tracks. The JIT saves ~48 µs of it, which is **0.45% of that core**.

The fader is not where a DAW's CPU goes; plugins are. Nobody should read
`3.7×` as a fix for anything a user can hear today, and this track should
not be merged on the strength of that number. The reason to build the
seam is the insert chain behind it: fusing `[gain, ramp, pan]` is the
smallest honest rehearsal for fusing `[eq, comp, gain, ramp, pan]`
without writing one function per permutation — and the machinery that
makes it safe (the equivalence proof, the allocation counter, the
retire-and-recycle publish) is worth more than the microseconds.

### 4.1 What changes on a weaker CPU

Worth separating, because the intuition "this must matter more on a slow
machine" is half right and the half that is right does not credit the JIT.

**Clock speed changes nothing about the ratios.** If every instruction is
N× slower, baseline and kernel both scale by N: 3.7× stays 3.7× and 0.7%
of a core stays 0.7% of a core. What changes is the *absolute* headroom,
and only in the sense that everything else got slower too.

**Microarchitecture can change the ratios, and plausibly in this
direction:**

- **In-order or narrowly out-of-order cores** (Cortex-A53, older Atom)
  cannot extract the instruction-level parallelism that makes the scalar
  baseline tolerable on a wide core. Explicit 4-lane SIMD hands them
  parallelism they do not have to find, so vectorisation typically wins
  *more* there, not less.
- **The per-sample divide** in the baseline's pan lerp (`i as f32 /
  last as f32`) is the single most microarchitecture-sensitive thing in
  that loop; small cores have proportionally much weaker dividers. The
  **plan** removes it outright — which is the `fused_scalar` column, and
  needs no JIT at all.
- **Near the deadline, cost stops being linear.** A machine at 80% load
  does not experience 0.46% of a core as 0.46% of anything; it is either
  under the deadline or it xruns. Savings are worth more the closer you
  already are to the cliff.

**Two things pull the other way, and they are specific:**

- **The kernel is fixed at 128-bit vectors** (`types::F32X4`), so it
  neither needs nor exploits anything above SSE2/NEON. Good for
  portability — the win is available on any machine — but it also means
  there is no "better CPU, better kernel" effect to lose.
- **The `fcmp`+`bitselect` peak meter is an x86 tuning.** It exists
  because cranelift's `fmax` lowers to a multi-instruction IEEE/NaN
  sequence on x86. aarch64 has `FMAXNM` as a single instruction, so on
  ARM the hand-rolled form may be a *pessimisation*. The archetypal weak
  machine is ARM, and this has not been measured there.

**Where that leaves it:** on a weak machine the honest recommendation is
probably `fused_scalar`, not the JIT — it is most of the win on the blocks
that have automation, it is plain rustc, and it costs neither 34 crates
nor a writable executable mapping. The JIT's case rests on the insert
chain (§4), not on slow hardware.

**The lock-free work is a different argument, and a stronger one.** It
does not scale with clock speed at all; it scales with **core scarcity**.
Priority inversion requires the control thread to be descheduled while
holding something the callback needs. On a 32-core box it almost always
has a spare core and rarely is; on a 2-core machine the two threads
genuinely compete, so the probability rises sharply. The same goes for
allocation: on a memory-constrained machine `malloc` is far likelier to
hit a slow path. So `TripleBuffer` and the zero-allocation guarantee are
worth *more* on small hardware, while the JIT's speedup is worth roughly
the same proportion everywhere. These two halves of the track should not
be justified with one sentence.

### 4.2 The benchmark was measuring a fixture smaller than production

Recorded because it inverted this section's conclusion once already, and
because the failure is a class of mistake rather than a typo.

`benches/kernel.rs` used a 64-breakpoint automation lane. `TrackRamps::gain`
is compiled **session-wide** at graph rebuild (`engine.rs`,
`compile_gain_ramps`), so a real lane is thousands of breakpoints long, and
`strip::plan` located a block's breakpoints with `ramp.iter().find(...)` — a
linear scan from the start of the whole lane, per segment, per block. Cost
was O(the session), not O(the block).

Measured, single strip, 512 frames, rendered near the lane's end:

| lane | `apply_fader_into` | `plan` + `fused_scalar` | |
|---|---|---|---|
| 64 points | 2.06 µs | 1.29 µs | 1.60× faster |
| 6 000 | 1.94 µs | 10.85 µs | **5.60× slower** |
| 12 000 | 1.89 µs | 21.13 µs | **11.20× slower** |
| 48 000 | 1.84 µs | 73.25 µs | **39.84× slower** |

So the headline "3.35× faster" was true of the benchmark and false of any
real session with a long fader move on it. The benchmark could not see it,
which is the actual defect: **a fixture smaller than production is not a
measurement of production.**

Fixed by seeding the breakpoint index with `partition_point` once and only
walking it forward — the same shape `RampCursor` already used. Now flat in
lane length (1.35–1.84× faster at every size measured), and the `long-lane`
case above is a permanent benchmark so the regression cannot return
quietly.

## 5. Declined: rebuilding the scheduler

See §1.2. Superticks and `TempoMap` already give integer, exactly
divisible timing, and `midi/schedule.rs` already produces absolute-sample
events on the control thread. A second scheduler would not be more
accurate; it would be a second answer to "what sample is this beat".

## 6. Declined: the Carla C-FFI bridge

The brief's `host/` module is a Carla bridge. AURA already hosts plugins
**in-process**: CLAP through `clack-host` (`src-tauri/Cargo.toml:83`) and
LV2 through `livi` (`:92`), behind the live-node seam of
`ARCHITECTURE.md` §15.1, with `AudioProcessor::process` running under the
§2.1 contract.

Adding Carla would mean a third host and — the real cost — a **new IPC
boundary** in the audio path, for formats that are already loaded
directly. Plugin *isolation* is a live question here
(`docs/research/09-plugin-isolation-and-ui-scaling.md`), and if
out-of-process hosting is the answer it should be decided as isolation
architecture on its own terms, not arrive as a side effect of a JIT
task.

## 7. Where this landed, and what it deliberately did not touch

The work is a **standalone crate**, `aura-engine/`, with its own
`Cargo.toml` and lockfile — the pattern `benches/bulkbench` and
`benches/pdsbench` already use. Two reasons, neither stylistic:

- `src-tauri/Cargo.toml` is a **frozen file**. Cranelift is **34**
  transitive crates for the library alone (`cargo tree -e normal`; 118
  packages in the lockfile once criterion is counted). That is a
  dependency decision for the owner, not a side effect of a task.
- The kernel has to be *proven* equal to `apply_fader_into` before it
  goes near a callback. A crate that can be tested and benchmarked alone
  is how that proof gets written.

**Nothing in `src-tauri/` changes**, and §8 recommends it stays that
way for the JIT specifically. The kernel API is shaped so
`apply_fader_into` remains the fallback arm if that is ever revisited.

One consequence of standing outside the app: **the track is not
ear-checkable.** No code path in AURA reaches this crate, so there is
nothing to listen to. The equivalence tests are the substitute, and they
are the reason the crate is gated in CI
(`.github/workflows/engine.yml`, gated on `paths: aura-engine/**`) —
dossier 10's gap 19 noted there was no CI at all when it was written;
there is now, and a proof nothing runs is not a proof.

## 8. Recommendation

The brief asked for a JIT. This report's recommendation is **not to ship
one**, stated explicitly rather than left as an open question the next
agent has to re-litigate.

### 8.1 Cranelift into `src-tauri`: no

Three reasons, in order of weight.

**The marginal gain is ~0.2% of one core.** The JIT is 1.65x over
`fused_scalar` (§4), and `fused_scalar` is itself most of the win. 1.65x
of 0.8% of an i9-14900 core is what shipping cranelift would buy, on the
*fader* — which §4 already establishes is not where a DAW spends CPU.

**macOS makes it expensive, and this is read from the source rather than
assumed.** `cranelift-jit` 0.134 obtains pages with
`MmapMut::map_anon`/`region::alloc` and flips them W->X with
`region::protect(READ_EXECUTE)` — plain `mprotect`/`VirtualProtect`.
Grepping the whole cranelift 0.134 tree for `MAP_JIT`,
`pthread_jit_write_protect_np`, or any Apple JIT-entitlement handling
returns **nothing**. Notarization requires the hardened runtime, and
without the sanctioned `MAP_JIT` path that means shipping
`com.apple.security.cs.allow-unsigned-executable-memory` — the broad
entitlement, not the narrow `com.apple.security.cs.allow-jit`. *Not
verified on Apple hardware: the absence of `MAP_JIT` is a fact, the
entitlement consequence is inference from it.*

**Windows works but invites a support burden.** `VirtualProtect` is
fine, but code generated and made executable at runtime is exactly what
EDR and antivirus heuristics flag, and a DAW is not a process users
expect to behave like a loader. The release worker also builds
`--no-default-features` and has never been exercised with a writable
executable mapping.

AURA targets Windows today and macOS later. A per-platform, permanent
cost for 0.2% of one core is a bad trade, and waiting does not improve
it.

**What would reopen it:** profiling that shows the *insert chain* is a
real bottleneck. Fusing `[eq, comp, gain, ramp, pan]` without writing one
function per permutation is a genuine case for runtime codegen in a way
that fusing `[gain, ramp, pan]` is not. That case needs measurements
nobody has taken, and it should be reconsidered on all three platforms at
once.

### 8.2 The plan into `src-tauri`: optional, and cheap

`strip::plan` + `dsp::fused_scalar` is the half of this track worth
having, if any:

- **1.94x on automated blocks**, and it needs **no new dependency at
  all** — `jit/mod.rs` is the only file in the crate that touches
  cranelift; `strip.rs`, `dsp.rs`, `automation.rs` and `sync/` are pure
  `std`. So `src-tauri/Cargo.toml` stays frozen *literally*, which was
  the whole reason this crate stands outside the app.
- Identical on every platform. No W^X, no entitlement, no EDR surface.
- Already proven bit-identical to `apply_fader_into` in the flat case and
  within 1e-5 once a ramp moves (§4).

Sold honestly: it saves ~0.4% of one core on this machine, more on a
weaker one (§4.1). That is ~900 lines into `src-tauri` for something no
user will hear. Cheap, low-risk, not urgent, and **not** a reason to
merge on its own.

### 8.3 What this track actually earned

The parts with value on the day they land are the ones that are not
code: this report (three of the brief's six items already existed; the
Carla bridge is declined) and the four entries added to
[`TRAPS.md`](TRAPS.md). Both exist to stop someone spending a week
building the wrong thing.

The crate itself is worth keeping — CI is path-gated so it costs nothing
per unrelated PR, and the equivalence proof is done if the plan is ever
wanted — but it should be understood as a **proving ground, not a shipped
component**.

### 8.4 The honest next step

This track optimised the fader. **The fader is not where the CPU goes;
plugins are.** That was written as a caveat in §4 when it should have
been the conclusion. Before any further engine performance work, profile
a real session under a realistic plugin load and find out where the time
actually is. Everything above is a well-measured answer to a question
that was probably not the important one.

**Done — see [§9](#9-the-measurement-84-asked-for).** The premise held up
in direction and not in emphasis: plugin DSP is 28% of a 32-track block,
and AURA's own code is 52%.

## 9. The measurement §8.4 asked for

Done. `src-tauri/tests/plugin_load_profile.rs`, on this machine
(i9-14900, Linux, 48 kHz, 512-frame blocks — a **10.67 ms** deadline).

```sh
AURA_PROFILE_PLUGINS=1 cargo test --release \
    --test plugin_load_profile -- --nocapture
```

The session: `n` MIDI tracks, one in four carrying a hosted **Surge XT**,
the rest on AURA's built-in `PolySynth`; **ZamComp + Calf Equalizer 5
Band** as inserts on every strip; every track sending at −12 dB into one
**Calf Reverb** return. So `n = 32` means 32 strips, 8 hosted
instruments, and 66 insert slots.

### 9.1 Where a block's time actually goes

Medians over 2000 timed blocks after 64 warm-up blocks, and then the
**median of three whole runs** — a single run is not enough, see §9.2.

| tracks | slots | block | plugin DSP | host overhead | instruments | AURA mixer/sends |
|---:|---:|---:|---:|---:|---:|---:|
| 4 | 10 | 142 µs | 60 µs (42%) | 29 µs (20%) | 22 µs (16%) | 31 µs (22%) |
| 16 | 34 | 460 µs | 154 µs (33%) | 96 µs (21%) | 85 µs (19%) | 125 µs (27%) |
| 32 | 66 | 918 µs | 255 µs (28%) | 227 µs (25%) | 189 µs (21%) | 247 µs (27%) |

**§8.4's premise is right in direction and wrong in emphasis.** Plugins
are indeed the largest single thing — but at realistic scale they are not
dominant, and **the biggest addressable line in the table is ours.**

At 32 tracks, actual plugin DSP is **28%** of the block. AURA's own code —
the per-insert host path plus the mixer, sends and PDC — is **52%**.

Note the shape: **host overhead grows as a share (20% → 21% → 25%) while
plugin DSP shrinks (42% → 33% → 28%).** Per slot, DSP falls with scale
(6.0 → 4.5 → 3.9 µs) as caches warm and the work amortises; the slot cost
does not. The bigger the session, the more of it is plumbing.

### 9.2 The number to act on: ~3 µs per insert slot

The split between "the plugin computing" and "us calling it" needs no
profiler. The harness renders a fourth pass, `cheap_fx`, with insert
chains of exactly the same length filled with `Audio Gain (Stereo)` —
one multiply per sample. Everything paid to *call* a plugin is paid
identically in both; what is missing is the arithmetic.

`cheap_fx − no_inserts` is therefore the cost of the slot itself:
**2.8–3.4 µs per insert**, across 10, 34 and 66 slots. That is a quarter
of a 32-track block spent on buffer copies, param flushes, event
conversion and `Replace`-mode plumbing rather than on DSP.

**Which column to trust.** Run-to-run spread on this machine is 5–7% per
cell, and the columns do not inherit it equally:

- `cheap_fx − no_inserts` (host overhead) subtracts two middling numbers
  and is **stable** — 2.83, 2.90, 3.43 µs per slot across the sweep.
- `full − cheap_fx` (plugin DSP) subtracts the **two largest** numbers in
  the table, so it carries both their noise. One run during this work put
  it at 391 µs where the median of three says 255 µs. Treat that column
  as ±20% and never quote it from a single run.

This is why §9.1's table is a median of three runs and not one, and it is
the reason the actionable claim rests on the host-overhead column.

Read honestly, that figure is *"the cost of having an insert slot at
all"* — it includes the LV2 `run()` call machinery and the plugin's own
port reads, not only AURA's code. It is not a 227 µs saving sitting
there for the taking. But it is the only line in the table that is both
large and ours, and it scales with a number users increase freely.

### 9.3 There is no performance crisis

32 tracks, 8 hosted instruments and 66 plugin instances use **8.6% of the
deadline**. The p95 is 1.38 ms and the worst observed block 2.09 ms,
against 10.67 ms. Nothing measured here is audible to anyone.

Extrapolating the measured slope (~25 µs per track carrying two inserts,
plus a small fixed cost) puts the ceiling near **400 tracks** on this CPU,
or roughly 200 with the headroom an RT thread should keep. That is an
extrapolation from three points and untested past 32 — but it says where
the uncertainty is, and it is not "what does the time go to". It is
"which machine is the floor": §4.1's reasoning about weaker cores applies
unchanged, and a laptop 4× slower would meet that ceiling inside the
range of sessions people actually build. **That, not a flamegraph, is the
measurement worth taking next.**

This is the same caveat §4 carried about the JIT, now confirmed from the
other side: the engine is not close to its budget, and performance work
on it should be justified by a session that actually breaks rather than
by a ratio. The fader the JIT track optimised lives inside the "AURA
mixer/sends" column — 27% of a block that is itself 9% of the deadline.

### 9.4 What these numbers do not cover

- **One machine, one CPU.** i9-14900, and the figures above are the
  median of three runs taken after PR #118's `MixNode` refactor landed.
  §4.1's reasoning about weaker cores applies unchanged.
- **One plugin set.** The DSP column is ZamComp + Calf EQ + Calf Reverb +
  Surge XT and nothing else; a convolution reverb or a lookahead limiter
  would move it. The *host overhead* column is the portable one — it is
  the same work whatever the plugin does.
- **Offline render, not the live callback.** The harness drives
  `mixer::render` through `audio::offline::build_graph`, which is the
  same compiled graph and the same insert chains the engine uses
  (`bus::compile_routing`, shared since PR #109), but it does not include
  the cpal callback, the ring buffer, or the meter path.
- **No automation moving.** Every ramp is flat. §4's table says a moving
  ramp roughly doubles the fader's own cost, which is inside the smallest
  column here.
- **No plugin editor open.** GUI idle callbacks are a known cost
  (`TRAPS.md` on Carla's `extension_data` spam) and are not measured.

### 9.5 If someone wants the flamegraph anyway

The subtraction says how much; only a profiler says which function. It
needs a sysctl this machine does not have set:

```sh
sudo sysctl kernel.perf_event_paranoid=1        # 4 by default here
cargo test --release --test plugin_load_profile --no-run   # note the path
AURA_PROFILE_PLUGINS=1 perf record -g --call-graph dwarf \
    target/release/deps/plugin_load_profile-<hash> where_the_block_time --nocapture
perf report --stdio --sort dso,symbol | head -60
```

That is the way to find out *which* part of the 3.4 µs is a `memcpy` and
which is a param flush — the next step if §9.2 is ever worth acting on.

### 9.6 Checking this later

§9 is a snapshot. `scripts/perf-check.sh` is how you find out whether it
still holds:

```sh
scripts/perf-check.sh --measure                 # what does this machine do
scripts/perf-check.sh --budget 520              # exit 0 under, 1 over, 125 unjudgeable
```

It defaults to the `bare` column — AURA's own mixer, fader, sends and
built-in synth — because that needs no plugins installed and is the code
we actually write. `--run full` measures the whole session instead, and
needs the catalogue in §9's session description.

The exit codes are the point: they let `git bisect run` drive it, so a
regression that already landed is a search rather than an investigation.
`docs/STANDING-CONSTRAINTS.md` §Performance has the recipe, and the
script's `--help` has the caveats — chiefly that a single number is not
evidence. Ten invocations over unchanged code on the development machine
read 388–408 µs and one batch of four read 260–275, for a reason never
identified. Compare `main` and your branch in one sitting; do not compare
either against the table in §9.1, which was measured in a different
process shape.

**What `bare` does not cover:** with no inserts there is no insert chain
and no PDC. A regression in `insert.rs` or `pdc.rs` needs `--run full`.
