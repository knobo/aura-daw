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

Measured on this box (32 tracks, medians, `cargo bench`):

Full path (32 tracks rendered and mixed into the master), medians:

| block | case | `multipass` | `apply_fader_into` | `fused_scalar` | `jit` | `master_mix_only` |
|---|---|---|---|---|---|---|
| 128 | flat | 12.6 µs | **11.6 µs** | 10.8 µs | 7.2 µs | 2.6 µs |
| 128 | ramped | 20.4 µs | **18.3 µs** | 11.4 µs | 7.9 µs | 2.5 µs |
| 128 | ramped+pan | 20.7 µs | **18.4 µs** | 11.5 µs | 8.2 µs | 2.5 µs |
| 512 | flat | 51.1 µs | **50.1 µs** | 41.6 µs | 26.5 µs | 9.9 µs |
| 512 | ramped | 81.2 µs | **71.4 µs** | 42.4 µs | 28.1 µs | 9.7 µs |
| 512 | ramped+pan | 81.4 µs | **76.8 µs** | 42.2 µs | 27.8 µs | 9.7 µs |
| 1024 | flat | 95.9 µs | **86.6 µs** | 82.3 µs | 50.3 µs | 19.7 µs |
| 1024 | ramped | 190.2 µs | **147.2 µs** | 84.2 µs | 55.4 µs | 19.8 µs |
| 1024 | ramped+pan | 160.2 µs | **145.8 µs** | 83.2 µs | 53.9 µs | 19.4 µs |

The fader alone, with `master_mix_only` subtracted — the constant every
contender pays and none can avoid — and the win split into the part that
is the plan and the part that is the code generator:

| block | case | baseline | `jit` | total | of which plan | of which codegen |
|---|---|---|---|---|---|---|
| 128 | flat | 9.0 µs | 4.5 µs | **1.98×** | 1.10× | 1.81× |
| 128 | ramped | 15.8 µs | 5.4 µs | **2.92×** | 1.78× | 1.64× |
| 128 | ramped+pan | 15.9 µs | 5.7 µs | **2.78×** | 1.76× | 1.58× |
| 512 | flat | 40.2 µs | 16.6 µs | **2.42×** | 1.27× | 1.91× |
| 512 | ramped | 61.7 µs | 18.4 µs | **3.35×** | 1.88× | 1.78× |
| 512 | ramped+pan | 67.2 µs | 18.1 µs | **3.71×** | 2.07× | 1.79× |
| 1024 | flat | 66.9 µs | 30.6 µs | **2.18×** | 1.07× | 2.04× |
| 1024 | ramped | 127.4 µs | 35.7 µs | **3.57×** | 1.98× | 1.81× |
| 1024 | ramped+pan | 126.4 µs | 34.5 µs | **3.66×** | 1.98× | 1.85× |

Compiling the whole table costs **190 µs**, once, on the control
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
case in that table is 76.8 µs — **0.7% of one core** for 32 tracks. The
JIT saves ~49 µs of it, which is **0.46% of one core**.

The fader is not where a DAW's CPU goes; plugins are. Nobody should read
`3.7×` as a fix for anything a user can hear today, and this track should
not be merged on the strength of that number. The reason to build the
seam is the insert chain behind it: fusing `[gain, ramp, pan]` is the
smallest honest rehearsal for fusing `[eq, comp, gain, ramp, pan]`
without writing one function per permutation — and the machinery that
makes it safe (the equivalence proof, the allocation counter, the
retire-and-recycle publish) is worth more than the microseconds.

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

**Nothing in `src-tauri/` changes.** Wiring the kernels into
`mixer::render` is a separate, owner-gated step, and the open questions
are listed in [`backlog/jit-engine.md`](backlog/jit-engine.md): cranelift
in the frozen manifest, and a fallback for the Windows release build
(`cranelift-jit` needs a writable executable mapping; that worker builds
`--no-default-features` and has never been tested with one). The kernel
API is shaped so `apply_fader_into` stays as the fallback arm.

One consequence of standing outside the app: **the track is not
ear-checkable.** No code path in AURA reaches this crate, so there is
nothing to listen to. The equivalence tests are the substitute, and they
are the reason the crate is now gated in CI (`.github/workflows/tests.yml`,
job `engine`) — dossier 10's gap 19 noted there was no CI at all when it
was written; there is now, and a proof nothing runs is not a proof.
