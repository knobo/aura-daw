//! AURA RT engine primitives.
//!
//! A standalone crate, deliberately outside the Tauri app: `src-tauri`'s
//! manifest is frozen, and a Cranelift JIT has no business near an audio
//! callback until it has been proven equal to the loop it replaces. Everything
//! here is built to make that proof possible —
//!
//! * [`sync`] — `TripleBuffer<T>`: latest-value publish where no `T` is ever
//!   constructed, dropped, or reference-counted by the consumer.
//! * [`metrics`] — an allocation counter that turns "no allocation on the
//!   audio thread" into a test, and per-block latency/xrun telemetry.
//! * [`strip`] — one track strip's fader work for one block, as data: the
//!   automation breakpoints cut the block into straight-line stretches.
//! * [`dsp`] — the baseline (a verbatim port of `mixer::apply_fader_into`), the
//!   un-fused multi-pass shape, and the plan run as scalar Rust.
//! * [`jit`] — Cranelift kernels for the same plan, specialised per shape.
//!
//! ## The RT contract this crate is written under
//!
//! Copied from `ARCHITECTURE.md` §2.1, and the reason most of the code looks
//! the way it does: **no allocation, no locks, no blocking, no I/O** on the
//! audio thread. Everything that would break one of those happens when a plan
//! is built or a kernel is compiled, on the control thread, and what reaches
//! the callback is a pointer to finished code and a `Copy` plan.

pub mod automation;
pub mod dsp;
pub mod jit;
pub mod metrics;
pub mod strip;
pub mod sync;
