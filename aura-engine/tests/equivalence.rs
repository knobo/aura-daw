//! The JIT kernel against the loop it is meant to replace.
//!
//! Two claims, and they are deliberately different in strength:
//!
//! * **vs. [`dsp::fused_scalar`] — bit-identical.** Same algorithm, same
//!   operation order; the only difference is who emitted the code. Anything
//!   less than equality here is a codegen bug, so the test says `assert_eq`.
//! * **vs. [`dsp::apply_fader`] — equal within 1e-5 relative.** The plan
//!   replaces per-sample interpolation with an affine form and folds the fader
//!   into the coefficients, which moves the last bits. Except in the flat case,
//!   where it is exact — and that is asserted as equality, because the flat
//!   case is most blocks of most sessions and "we did not change the sound"
//!   should be provable there.
//!
//! What this suite does NOT establish: that the sum-of-squares meter matches
//! bit-for-bit. Four vector lanes summed and then folded is a different
//! addition order from 512 scalar `+=`, and no amount of care makes float
//! addition associative. The tolerance on the meters is 1e-4 absolute.

use aura_engine::automation::{AbsParamEvent, RampCursor};
use aura_engine::dsp::{apply_fader, fused_scalar, Accum};
use aura_engine::jit::{Kernels, Shape};
use aura_engine::strip::{plan, Coef, PanQuad, Strip};

/// Compiled once for the whole suite: `Kernels::compile` is a register
/// allocator run, and the table is read-only afterwards.
/// Per test thread, because `Kernels` is `Send` and deliberately not `Sync`:
/// one reader is the whole contract the triple buffer provides, and a table
/// that were `Sync` would invite two callbacks to share it. Leaked rather than
/// released — `Kernels::release` is `unsafe` for a reason, and a test binary
/// exiting is the one place where leaking is unambiguously correct.
fn kernels() -> &'static Kernels {
    thread_local! {
        static K: &'static Kernels = Box::leak(Box::new(
            Kernels::compile().expect("cranelift could not target this host"),
        ));
    }
    K.with(|k| *k)
}

fn signal(frames: usize) -> Vec<f32> {
    // Two different waveforms in the two channels, so a lane mix-up in the
    // kernel cannot hide behind a symmetric input.
    (0..frames)
        .flat_map(|i| {
            let t = i as f32;
            [(t * 0.031).sin() * 0.9, (t * 0.017).cos() * 0.6]
        })
        .collect()
}

struct Run {
    out: Vec<f32>,
    acc: Accum,
}

fn baseline(s: &Strip<'_>, buf: &[f32], frames: usize, pos: u64, out_ch: usize) -> Run {
    let mut out = vec![0.0; frames * out_ch];
    let mut acc = Accum::default();
    apply_fader(
        s,
        buf,
        frames,
        pos,
        0,
        frames - 1,
        &mut RampCursor::new(),
        &mut out,
        out_ch,
        &mut acc,
    );
    Run { out, acc }
}

fn scalar(s: &Strip<'_>, buf: &[f32], frames: usize, pos: u64, out_ch: usize) -> Run {
    let p = plan(s, pos, frames, 0, frames - 1);
    let mut out = vec![0.0; frames * out_ch];
    let mut acc = Accum::default();
    fused_scalar(&p, buf, 0, &mut out, out_ch, &mut acc);
    Run { out, acc }
}

fn jit(s: &Strip<'_>, buf: &[f32], frames: usize, pos: u64, out_ch: usize) -> Run {
    let p = plan(s, pos, frames, 0, frames - 1);
    let mut out = vec![0.0; frames * out_ch];
    let mut acc = Accum::default();
    assert!(
        kernels().run(&p, buf, 0, &mut out, out_ch, &mut acc),
        "the kernels declined a plan this test expects them to take"
    );
    Run { out, acc }
}

fn assert_samples_close(a: &[f32], b: &[f32], tol: f32, what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: length");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let scale = x.abs().max(y.abs()).max(1.0);
        assert!((x - y).abs() <= tol * scale, "{what}: frame {i}: {x} vs {y}");
    }
}

fn assert_meters_close(a: &Accum, b: &Accum, what: &str) {
    for (name, x, y) in [
        ("pk_l", a.pk_l, b.pk_l),
        ("pk_r", a.pk_r, b.pk_r),
        ("ss_l", a.ss_l, b.ss_l),
        ("ss_r", a.ss_r, b.ss_r),
    ] {
        assert!((x - y).abs() < 1e-4, "{what}: {name}: {x} vs {y}");
    }
}

fn flat_pan() -> PanQuad {
    PanQuad { gl0: 0.7071, gr0: 0.7071, gl1: 0.7071, gr1: 0.7071 }
}

fn moving_pan() -> PanQuad {
    PanQuad { gl0: 0.98, gr0: 0.19, gl1: 0.19, gr1: 0.98 }
}

/// A fader move: hold, dip, recover — with the breakpoints deliberately not
/// on block boundaries.
fn fader_move() -> Vec<AbsParamEvent> {
    vec![
        AbsParamEvent { sample: 0, value: 1.0 },
        AbsParamEvent { sample: 137, value: 0.18 },
        AbsParamEvent { sample: 381, value: 0.93 },
        AbsParamEvent { sample: 900, value: 0.5 },
    ]
}

#[test]
fn the_host_can_be_targeted_at_all() {
    // If this fails, every other test in the file fails for the same reason
    // and none of them says so — worth its own line.
    assert!(Kernels::compile().is_ok());
}

#[test]
fn flat_strips_are_bit_identical_to_the_mixer() {
    let buf = signal(512);
    let s = Strip { gain: 0.62, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 };
    let b = baseline(&s, &buf, 512, 0, 2);
    let j = jit(&s, &buf, 512, 0, 2);
    assert_eq!(b.out, j.out, "the common case must not change the sound at all");
    assert_eq!(b.acc.pk_l, j.acc.pk_l);
    assert_eq!(b.acc.pk_r, j.acc.pk_r);
    // Only the sums reassociate.
    assert_meters_close(&b.acc, &j.acc, "flat meters");
}

#[test]
fn the_jit_matches_the_scalar_plan_bit_for_bit() {
    let buf = signal(512);
    let r = fader_move();
    for (name, s) in [
        ("flat", Strip { gain: 0.5, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 }),
        (
            "ramp only",
            Strip { gain: 0.8, ramp: &r, pan: flat_pan(), audible: true, pdc_delay: 0 },
        ),
        (
            "pan only",
            Strip { gain: 0.8, ramp: &[], pan: moving_pan(), audible: true, pdc_delay: 0 },
        ),
        (
            "ramp and pan",
            Strip { gain: 0.73, ramp: &r, pan: moving_pan(), audible: true, pdc_delay: 0 },
        ),
        (
            "ramp through pdc",
            Strip { gain: 0.73, ramp: &r, pan: moving_pan(), audible: true, pdc_delay: 192 },
        ),
    ] {
        let sc = scalar(&s, &buf, 512, 256, 2);
        let j = jit(&s, &buf, 512, 256, 2);
        assert_eq!(sc.out, j.out, "{name}: cranelift and rustc must agree exactly");
        assert_eq!(sc.acc.pk_l, j.acc.pk_l, "{name}: peak L");
        assert_eq!(sc.acc.pk_r, j.acc.pk_r, "{name}: peak R");
        assert_meters_close(&sc.acc, &j.acc, name);
    }
}

#[test]
fn ramped_and_panned_strips_track_the_mixer_within_rounding() {
    let buf = signal(512);
    let r = fader_move();
    let s = Strip { gain: 0.73, ramp: &r, pan: moving_pan(), audible: true, pdc_delay: 0 };
    let b = baseline(&s, &buf, 512, 0, 2);
    let j = jit(&s, &buf, 512, 0, 2);
    assert_samples_close(&b.out, &j.out, 1e-5, "ramped+panned");
    assert_meters_close(&b.acc, &j.acc, "ramped+panned meters");
}

#[test]
fn odd_block_sizes_render_every_frame() {
    // The kernel handles frame pairs; the tail frame is finished in Rust. An
    // off-by-one there loses the last frame of every block, which is a click
    // at the block rate — audible, and invisible to a test that only uses
    // powers of two.
    let r = fader_move();
    for frames in [1, 2, 3, 7, 63, 127, 129, 511] {
        let buf = signal(frames);
        let s = Strip { gain: 0.9, ramp: &r, pan: moving_pan(), audible: true, pdc_delay: 0 };
        let sc = scalar(&s, &buf, frames, 0, 2);
        let j = jit(&s, &buf, frames, 0, 2);
        assert_eq!(sc.out, j.out, "{frames} frames");
        assert!(
            j.out.iter().any(|&x| x != 0.0) || frames == 0,
            "{frames} frames: nothing was written"
        );
    }
}

#[test]
fn every_segment_of_a_multi_breakpoint_block_is_rendered() {
    // Breakpoints every 100 samples inside a 512-frame block: five stretches,
    // four of them starting at an odd frame offset once PDC shifts them.
    let ramp: Vec<_> = (0..12)
        .map(|n| AbsParamEvent { sample: n * 100 + 37, value: if n % 2 == 0 { 1.0 } else { 0.2 } })
        .collect();
    let buf = signal(512);
    let s = Strip { gain: 0.85, ramp: &ramp, pan: moving_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, 512, 0, 511);
    assert!(p.segments().len() > 1, "the test needs a multi-segment plan");
    assert!(!p.overflowed);
    let sc = scalar(&s, &buf, 512, 0, 2);
    let j = jit(&s, &buf, 512, 0, 2);
    assert_eq!(sc.out, j.out);
    let b = baseline(&s, &buf, 512, 0, 2);
    assert_samples_close(&b.out, &j.out, 1e-5, "multi-segment");
}

#[test]
fn a_muted_strip_writes_nothing() {
    let buf = signal(256);
    let s = Strip { gain: 1.0, ramp: &[], pan: flat_pan(), audible: false, pdc_delay: 0 };
    let b = baseline(&s, &buf, 256, 0, 2);
    let j = jit(&s, &buf, 256, 0, 2);
    assert!(b.out.iter().all(|&x| x == 0.0));
    assert_eq!(b.out, j.out);
    assert_eq!(j.acc, Accum::default());
}

#[test]
fn the_kernels_add_into_the_output_rather_than_overwrite_it() {
    // Every track after the first mixes on top of what is already there. A
    // kernel that stored instead of accumulating would silence every track but
    // the last, which is exactly the kind of bug a single-track test misses.
    let buf = signal(128);
    let s = Strip { gain: 0.5, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, 128, 0, 127);
    let mut out = vec![0.0; 256];
    let mut acc = Accum::default();
    assert!(kernels().run(&p, &buf, 0, &mut out, 2, &mut acc));
    let once = out.clone();
    assert!(kernels().run(&p, &buf, 0, &mut out, 2, &mut acc));
    for (i, (single, twice)) in once.iter().zip(&out).enumerate() {
        assert!((twice - single * 2.0).abs() < 1e-6, "frame {i}: {twice} vs {}", single * 2.0);
    }
}

#[test]
fn a_run_placed_later_in_the_block_lands_at_the_right_offset() {
    // A loop wrap splits a callback block into runs; the second run writes at
    // `pan_index` frames in. Writing at 0 instead would double the first half
    // of the block and drop the second.
    let frames = 64;
    let buf = signal(frames);
    let s = Strip { gain: 1.0, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, frames, frames, 2 * frames - 1);
    let mut out = vec![0.0; 2 * frames * 2];
    let mut acc = Accum::default();
    assert!(kernels().run(&p, &buf, frames, &mut out, 2, &mut acc));
    assert!(out[..frames * 2].iter().all(|&x| x == 0.0), "the first half must be untouched");
    assert!(out[frames * 2..].iter().any(|&x| x != 0.0), "the second half must be written");
}

#[test]
fn the_kernels_decline_what_they_cannot_render() {
    let buf = signal(64);
    let s = Strip { gain: 1.0, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, 64, 0, 63);
    let mut out = vec![0.0; 64];
    let mut acc = Accum::default();
    // Mono output: not a kernel shape, and it must SAY so rather than write
    // stereo pairs into a mono buffer.
    assert!(!kernels().run(&p, &buf, 0, &mut out, 1, &mut acc));
    assert!(out.iter().all(|&x| x == 0.0), "a declined plan must write nothing");

    // Too many breakpoints for the plan: same contract.
    let dense: Vec<_> =
        (0..300).map(|n| AbsParamEvent { sample: n * 2, value: (n % 2) as f32 }).collect();
    let s = Strip { gain: 1.0, ramp: &dense, pan: flat_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, 64, 0, 63);
    assert!(p.overflowed);
    let mut out = vec![0.0; 128];
    assert!(!kernels().run(&p, &buf, 0, &mut out, 2, &mut acc));
    assert!(out.iter().all(|&x| x == 0.0));
}

#[test]
fn shape_selection_follows_the_coefficients() {
    assert_eq!(Shape::of(&Coef::flat(1.0, 0.7, 0.7)), Shape::Flat);
    assert_eq!(
        Shape::of(&Coef { g0: 1.0, dg: 1e-9, gl0: 0.7, dgl: 0.0, gr0: 0.7, dgr: 0.0 }),
        Shape::Affine,
        "any movement at all needs the affine kernel — `is_flat` is exact, not approximate"
    );
}

#[test]
fn a_full_stereo_mix_of_many_strips_agrees_with_the_mixer() {
    // The integration shape: 24 tracks, a mix of automated and static, summed
    // into one output buffer, compared against the same sum through
    // `apply_fader`. This is the number that would change if the JIT path were
    // wired into `mixer::render`.
    let frames = 480;
    let r = fader_move();
    let strips: Vec<Strip<'_>> = (0..24)
        .map(|n| Strip {
            gain: 0.2 + (n as f32) * 0.03,
            ramp: if n % 3 == 0 { &r[..] } else { &[] },
            pan: if n % 2 == 0 { moving_pan() } else { flat_pan() },
            audible: n % 7 != 6,
            pdc_delay: if n % 5 == 0 { 128 } else { 0 },
        })
        .collect();
    let buf = signal(frames);

    let mut out_b = vec![0.0; frames * 2];
    let mut acc_b = Accum::default();
    for s in &strips {
        apply_fader(
            s,
            &buf,
            frames,
            1024,
            0,
            frames - 1,
            &mut RampCursor::new(),
            &mut out_b,
            2,
            &mut acc_b,
        );
    }

    let mut out_j = vec![0.0; frames * 2];
    let mut acc_j = Accum::default();
    for s in &strips {
        let p = plan(s, 1024, frames, 0, frames - 1);
        assert!(kernels().run(&p, &buf, 0, &mut out_j, 2, &mut acc_j));
    }

    assert_samples_close(&out_b, &out_j, 1e-5, "24-track mix");
}
