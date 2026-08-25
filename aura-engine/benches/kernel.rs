//! What the fusion actually costs and buys.
//!
//! Four contenders, and the middle two are the point:
//!
//! | | what it measures |
//! |---|---|
//! | `multipass` | the un-fused node-graph shape `jit.md` asks to be compared against |
//! | `apply_fader_into` | **the baseline** — a verbatim port of what the app runs today |
//! | `fused_scalar` | the plan, compiled by rustc: how much of the win is the *algorithm* |
//! | `jit` | the plan, compiled by cranelift into SSE: how much is the *code generator* |
//!
//! Every variant renders into a post-fader buffer and then mixes that into
//! the master through `mix_post_into`, which is the shape `mixer::render_impl`
//! has had since Plan G2 — the fader's own loop no longer touches the master.
//!
//! Reporting `jit` against `multipass` alone would be dishonest — it credits
//! the JIT with beating a straw man. Reporting it against `apply_fader_into` is the
//! real claim, and reporting `fused_scalar` next to it is what stops that
//! claim from being mis-attributed.
//!
//! Planning is INSIDE the measured region for both fused variants, because it
//! is per-block work the audio thread has to do. Kernel compilation is
//! outside, because it happens once on the control thread.

use aura_engine::automation::{AbsParamEvent, RampCursor};
use aura_engine::dsp::{apply_fader_into, fused_scalar, mix_post_into, multipass, Accum};
use aura_engine::jit::Kernels;
use aura_engine::strip::{plan, PanQuad, Strip};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

const TRACKS: usize = 32;

/// Centre pan, equal-power — the −3 dB the app's `mixer::pan_gains(0.0)`
/// returns. Spelled as the constant rather than `0.7071` so the pan law here
/// is the same value the mixer's own tests assert.
const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn signal(frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|i| [(i as f32 * 0.031).sin() * 0.9, (i as f32 * 0.017).cos() * 0.6])
        .collect()
}

/// A fader move with breakpoints inside every window the bench renders, so the
/// ramped cases really do walk several stretches per block.
fn fader_move() -> Vec<AbsParamEvent> {
    (0..64).map(|n| AbsParamEvent { sample: n * 397, value: if n % 2 == 0 { 1.0 } else { 0.3 } }).collect()
}

fn flat_pan() -> PanQuad {
    PanQuad { gl0: CENTRE, gr0: CENTRE, gl1: CENTRE, gr1: CENTRE }
}

fn moving_pan() -> PanQuad {
    PanQuad { gl0: 0.98, gr0: 0.19, gl1: 0.19, gr1: 0.98 }
}

/// The three strip shapes worth separating: what most tracks do, what an
/// automated track does, and what a track being panned while automated does.
fn strips<'a>(case: &str, ramp: &'a [AbsParamEvent]) -> Vec<Strip<'a>> {
    (0..TRACKS)
        .map(|n| {
            let gain = 0.3 + n as f32 * 0.01;
            match case {
                "flat" => Strip { gain, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 },
                "ramped" => Strip { gain, ramp, pan: flat_pan(), audible: true, pdc_delay: 0 },
                _ => Strip { gain, ramp, pan: moving_pan(), audible: true, pdc_delay: 0 },
            }
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let kernels = Kernels::compile().expect("cranelift could not target this host");
    let ramp = fader_move();

    for case in ["flat", "ramped", "ramped+pan"] {
        for frames in [128usize, 512, 1024] {
            let all = strips(case, &ramp);
            let buf = signal(frames);
            let pos = 4096u64;
            let last = frames - 1;

            let mut group = c.benchmark_group(format!("{case}/{frames}"));
            // One "element" is one rendered frame of one track — the unit the
            // engine's cost actually scales in.
            group.throughput(Throughput::Elements((frames * TRACKS) as u64));

            let mut post = vec![0.0; frames * 2];
            let mut master = vec![0.0; frames * 2];

            group.bench_function(BenchmarkId::new("multipass", frames), |b| {
                b.iter(|| {
                    master.fill(0.0);
                    let mut acc = Accum::default();
                    for s in &all {
                        multipass(s, &buf, frames, pos, 0, last, &mut post, &mut acc);
                        mix_post_into(&mut master, 0, 2, &post, frames);
                    }
                    black_box(acc);
                })
            });

            group.bench_function(BenchmarkId::new("apply_fader_into", frames), |b| {
                b.iter(|| {
                    master.fill(0.0);
                    let mut acc = Accum::default();
                    for s in &all {
                        apply_fader_into(
                            s,
                            &buf,
                            frames,
                            pos,
                            0,
                            last,
                            &mut RampCursor::new(),
                            &mut post,
                            &mut acc,
                        );
                        mix_post_into(&mut master, 0, 2, &post, frames);
                    }
                    black_box(acc);
                })
            });

            group.bench_function(BenchmarkId::new("fused_scalar", frames), |b| {
                b.iter(|| {
                    master.fill(0.0);
                    let mut acc = Accum::default();
                    for s in &all {
                        let p = plan(s, pos, frames, 0, last);
                        fused_scalar(&p, &buf, &mut post, &mut acc);
                        mix_post_into(&mut master, 0, 2, &post, frames);
                    }
                    black_box(acc);
                })
            });

            group.bench_function(BenchmarkId::new("jit", frames), |b| {
                b.iter(|| {
                    master.fill(0.0);
                    let mut acc = Accum::default();
                    for s in &all {
                        let p = plan(s, pos, frames, 0, last);
                        assert!(kernels.run(&p, &buf, &mut post, &mut acc));
                        mix_post_into(&mut master, 0, 2, &post, frames);
                    }
                    black_box(acc);
                })
            });

            // The master-mix pass on its own. All four contenders pay it and
            // none of them can avoid it, so a speedup quoted on the full path
            // is diluted by a constant — this is the number that lets a reader
            // subtract it instead of guessing.
            group.bench_function(BenchmarkId::new("master_mix_only", frames), |b| {
                b.iter(|| {
                    master.fill(0.0);
                    for _ in 0..TRACKS {
                        mix_post_into(&mut master, 0, 2, &post, frames);
                    }
                    black_box(master[0]);
                })
            });

            group.finish();
        }
    }
}

/// What compiling the table costs, so "once on the control thread" is a number
/// rather than a hope. A graph rebuild that had to wait on this would be a
/// visible hitch, which is why nothing in the design lets it.
fn bench_compile(c: &mut Criterion) {
    c.bench_function("Kernels::compile", |b| {
        b.iter(|| {
            let k = Kernels::compile().expect("cranelift could not target this host");
            // Leak deliberately: `release` is unsafe, and a benchmark that
            // freed pages would be timing the unmap too.
            black_box(&k);
            std::mem::forget(k);
        })
    });
}

criterion_group!(benches, bench, bench_compile);
criterion_main!(benches);
