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
| Cranelift fusion of gain/ramp/pan | **gap** — `mixer::apply_fader` is one hand-written per-sample loop |

## Where the work lives

A new **standalone crate, `aura-engine/`**, with its own `Cargo.toml` and
its own lockfile — the pattern `benches/bulkbench` and `benches/pdsbench`
already use. Two reasons, and neither is stylistic:

- `src-tauri/Cargo.toml` is a **frozen file**. cranelift alone is ~15
  transitive crates; that is a dependency decision for the owner, not a
  side effect of a task.
- The JIT kernel must be *proven* equal to `mixer::apply_fader` before it
  is allowed anywhere near the callback. A crate that can be benchmarked
  and fuzzed on its own is how that proof gets written.

Nothing in `src-tauri/` is touched by this track. Wiring the kernels into
`mixer::render` is a **separate, owner-gated step** — see below.

## Landed here

1. `sync::TripleBuffer<T>` — wait-free SPSC latest-value publish,
   allocation only in `new`, recycling instead of freeing.
2. `metrics` — an allocation-counting global allocator so a test can
   assert *zero* allocations across an RT call, plus per-block cycle and
   xrun telemetry.
3. `jit` — a Cranelift compiler that fuses `gain · ramp · pan · mute ·
   accumulate` into one native kernel, specialised on the runtime shape of
   the track strip (flat gain vs. ramp, static pan vs. moving, muted,
   mono vs. stereo out).
4. `dsp::reference` — the un-fused multi-pass shape the brief wants
   compared against, and a port of `mixer::apply_fader` as the real
   baseline. Equivalence tests hold the JIT kernel to the port.
5. `benches/kernel.rs` — criterion: multi-pass vs. `apply_fader` vs. JIT.

## Not started — needs an owner decision

- **Wiring into `src-tauri`.** Requires cranelift in the frozen
  `Cargo.toml`, and a fallback path for the Windows build (`cranelift-jit`
  needs a writable executable mapping; the release worker builds with
  `--no-default-features` and has never been tested with one). The kernel
  API is deliberately shaped so `mixer::apply_fader` can stay as the
  fallback arm.
- **Ear-check.** Not ear-checkable while the crate is standalone: no code
  path in the app reaches it.
