//! Can this machine JIT at all, and what does it cost?
//!
//! `cargo run --release --example jit-probe`
//!
//! Worth having as its own binary because the answer is a **platform**
//! question, not a code one: `cranelift-jit` needs a writable-then-executable
//! mapping, and the one build this project ships that has never been near one
//! is the Windows release worker (it builds with `--no-default-features` and
//! has no audio device to test against either). Before any of this is wired
//! into `src-tauri`, this is the program to run on the target.

use std::time::Instant;

/// Centre pan, equal-power — the −3 dB the app's `mixer::pan_gains(0.0)`
/// returns. Spelled as the constant rather than `0.7071` so the pan law here
/// is the same value the mixer's own tests assert.
const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn main() {
    let started = Instant::now();
    match aura_engine::jit::Kernels::compile() {
        Ok(k) => {
            println!("cranelift targeted this host in {:?}", started.elapsed());
            // Render one block through the flat kernel, so the report is
            // "compiled AND ran" rather than "compiled".
            let frames = 256usize;
            let buf: Vec<f32> = (0..frames * 2).map(|i| (i as f32 * 0.05).sin()).collect();
            let s = aura_engine::strip::Strip {
                gain: 0.8,
                ramp: &[],
                pan: aura_engine::strip::PanQuad {
                    gl0: CENTRE,
                    gr0: CENTRE,
                    gl1: CENTRE,
                    gr1: CENTRE,
                },
                audible: true,
                pdc_delay: 0,
            };
            let plan = aura_engine::strip::plan(&s, 0, frames, 0, frames - 1);
            let mut post = vec![0.0f32; frames * 2];
            let mut acc = aura_engine::dsp::Accum::default();
            let ran = k.run(&plan, &buf, &mut post, &mut acc);
            println!("kernel ran: {ran}, peak L {:.4} R {:.4}", acc.pk_l, acc.pk_r);
            std::mem::forget(k);
        }
        Err(e) => {
            println!("NOT SUPPORTED on this host: {e}");
            std::process::exit(1);
        }
    }
}
