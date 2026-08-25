# Backlog — JIT-fused fader, triple buffer, RT bench harness

Source brief: [`jit.md`](../../jit.md) (owner canvas, 2026-08-24). Audit
against the current engine: [`../GAP_ANALYSIS.md`](../GAP_ANALYSIS.md).

The brief asks for six things. Three of them already exist in this repo
under different names, one is declined, and two are real gaps. The
argument for each is in `GAP_ANALYSIS.md`; the short form:

| Brief item | Verdict |
|---|---|
| Lock-free structural handoff | **exists** — rtrb ownership transfer of `GraphPtr` (`audio/rt.rs`, `audio/engine.rs`) |
| Sample-accurate scheduler | **exists** — superticks + `TempoMap` (`time.rs`) and `midi/schedule.rs`'s absolute-sample event slicing |
| Carla C-FFI host bridge | **declined** — CLAP (clack) and LV2 (livi) are hosted in-process already; Carla would be a third host and a new IPC boundary |
| `TripleBuffer<T>` | **gap** — nothing publishes a whole preallocated snapshot without allocating |
| `assert_no_alloc` + criterion | **gap** — the four RT rules are prose; nothing enforces them (dossier 10 gap 19) |
| Cranelift fusion of gain/ramp/pan | **gap** — `mixer::apply_fader_into` is one hand-written per-sample loop |

## Where the work lives

A new **standalone crate, `aura-engine/`**, with its own `Cargo.toml` and
its own lockfile — the pattern `benches/bulkbench` and `benches/pdsbench`
already use. Two reasons, and neither is stylistic:

- `src-tauri/Cargo.toml` is a **frozen file**. Cranelift is **34**
  transitive crates for the library alone (`cargo tree -e normal`; 118
  packages in the lockfile once criterion is counted). That is a dependency
  decision for the owner, not a side effect of a task.
- The JIT kernel must be *proven* equal to `mixer::apply_fader_into` before
  it is allowed anywhere near the callback. A crate that can be tested and
  benchmarked on its own is how that proof gets written.

Nothing in `src-tauri/` is touched by this track. Wiring the kernels into
`mixer::render` is a **separate, owner-gated step** — see below.

## Landed here

1. `sync::TripleBuffer<T>` — wait-free SPSC latest-value publish,
   allocation only in `new`, recycling instead of freeing.
2. `metrics` — an allocation-counting global allocator so a test can
   assert *zero* allocations across an RT call, plus per-block cycle and
   xrun telemetry.
3. `jit` — a Cranelift compiler that fuses `gain · ramp · pan · mute` and
   the meter fold into one native kernel, two frames per iteration,
   specialised on the runtime shape of the strip: `Shape::Flat` where gain
   and pan both hold still, `Shape::Affine` where either moves. A silent
   strip needs no kernel at all.

   Output channel count used to be a third case. Plan G2 removed it: the
   fader writes contiguous stereo into a post-fader buffer, so the master's
   channel count is `mixer::mix_post_into`'s problem, and the kernel stores
   instead of read-modify-writing. The merge with `main` mid-track is what
   surfaced that — the baseline port had to follow the mixer, and the
   kernel got simpler for it.
4. `dsp` — three ways to run one strip over one block: `multipass` (the
   un-fused shape the brief wants compared against), `apply_fader_into` (a
   verbatim port of the mixer's own loop — **the** baseline), and
   `fused_scalar` (the same plan as the JIT, compiled by rustc). Three, not
   two, so a speedup can be attributed to the algorithm or to the code
   generator instead of being credited to whichever is being sold.
   Equivalence tests hold the JIT kernel to the port.
5. `benches/kernel.rs` — criterion: multi-pass vs. `apply_fader_into` vs.
   `fused_scalar` vs. JIT, plus the master-mix pass alone so a reader can
   subtract the constant every contender pays.
6. CI job `engine` in `.github/workflows/tests.yml` — clippy `-D warnings`,
   the tests, and a bench compile. The crate is outside the workspace the
   `rust` job builds, so without this nothing would ever run the
   equivalence proof again. (It earned its keep immediately: clippy's
   deny-by-default `approx_constant` caught the centre-pan gain spelled
   `0.7071` where the mixer's own tests use `FRAC_1_SQRT_2`.)

## Found on the way, not fixed here

Two notes from the audit (§3.1 of `GAP_ANALYSIS.md`), neither a rule
violation and neither touched, because both live in `src-tauri/`:

- **`mixer.rs:626` and `:1000` do `graph.params.clone()` every block** —
  an `Arc` refcount up and back down on the RT thread, bought purely to
  get a split borrow past the borrow checker. Safe (the graph owns a
  strong reference for the whole block) and free of allocation or locks,
  so the four rules hold. Passing the table by shared reference from a
  caller that split the borrow one level up removes both atomics.
- **`Kernels::run`'s `#[must_use]` is load-bearing.** Plan G2 made the
  post-fader buffer an OVERWRITE target, so a caller that ignored a
  `false` return would not emit silence — it would ship the previous
  block's audio to the master and to every send tapping that strip. Worth
  knowing before the wiring step below.

## Not started — needs an owner decision

- **Wiring into `src-tauri`.** Requires cranelift in the frozen
  `Cargo.toml`, and a fallback path for the Windows build (`cranelift-jit`
  needs a writable executable mapping; the release worker builds with
  `--no-default-features` and has never been tested with one). The kernel
  API is deliberately shaped so `mixer::apply_fader_into` can stay as the
  fallback arm.
- **Ear-check.** Not ear-checkable while the crate is standalone: no code
  path in the app reaches it.
