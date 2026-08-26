//! The JIT kernel against the loop it is meant to replace.
//!
//! Two claims, and they are deliberately different in strength:
//!
//! * **vs. [`dsp::fused_scalar`] — bit-identical.** Same algorithm, same
//!   operation order; the only difference is who emitted the code. Anything
//!   less than equality here is a codegen bug, so the test says `assert_eq`.
//! * **vs. [`dsp::apply_fader_into`] — equal within 1e-5 relative.** The plan
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
use aura_engine::dsp::{apply_fader_into, fused_scalar, mix_post_into, Accum};
use aura_engine::jit::{Kernels, Shape};
use aura_engine::strip::{plan, Coef, PanQuad, Strip};

/// Centre pan, equal-power — the −3 dB the app's `mixer::pan_gains(0.0)`
/// returns. Spelled as the constant rather than `0.7071` so the pan law here
/// is the same value the mixer's own tests assert.
const CENTRE: f32 = std::f32::consts::FRAC_1_SQRT_2;

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
    post: Vec<f32>,
    acc: Accum,
}

/// Every run starts from a recognisably dirty `post`, because the buffer is an
/// OVERWRITE target: a path that skips a frame would match a zeroed buffer by
/// luck and leak the previous run's audio in production.
fn dirty(frames: usize) -> Vec<f32> {
    vec![-7.5; frames * 2]
}

fn baseline(s: &Strip<'_>, buf: &[f32], frames: usize, pos: u64) -> Run {
    let mut post = dirty(frames);
    let mut acc = Accum::default();
    apply_fader_into(
        s,
        buf,
        frames,
        pos,
        0,
        frames - 1,
        &mut RampCursor::new(),
        &mut post,
        &mut acc,
    );
    Run { post, acc }
}

fn scalar(s: &Strip<'_>, buf: &[f32], frames: usize, pos: u64) -> Run {
    let p = plan(s, pos, frames, 0, frames - 1);
    let mut post = dirty(frames);
    let mut acc = Accum::default();
    assert!(
        fused_scalar(&p, buf, &mut post, &mut acc),
        "the scalar plan declined a plan this test expects it to take"
    );
    Run { post, acc }
}

fn jit(s: &Strip<'_>, buf: &[f32], frames: usize, pos: u64) -> Run {
    let p = plan(s, pos, frames, 0, frames - 1);
    let mut post = dirty(frames);
    let mut acc = Accum::default();
    assert!(
        kernels().run(&p, buf, &mut post, &mut acc),
        "the kernels declined a plan this test expects them to take"
    );
    Run { post, acc }
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
    PanQuad { gl0: CENTRE, gr0: CENTRE, gl1: CENTRE, gr1: CENTRE }
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
    let b = baseline(&s, &buf, 512, 0);
    let j = jit(&s, &buf, 512, 0);
    assert_eq!(b.post, j.post, "the common case must not change the sound at all");
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
        let sc = scalar(&s, &buf, 512, 256);
        let j = jit(&s, &buf, 512, 256);
        assert_eq!(sc.post, j.post, "{name}: cranelift and rustc must agree exactly");
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
    let b = baseline(&s, &buf, 512, 0);
    let j = jit(&s, &buf, 512, 0);
    assert_samples_close(&b.post, &j.post, 1e-5, "ramped+panned");
    assert_meters_close(&b.acc, &j.acc, "ramped+panned meters");
}

#[test]
fn a_pdc_delay_larger_than_the_position_holds_the_lane_at_zero() {
    // Regression, found in review. The lane position is
    // `pos + i - pdc_delay` SATURATING PER SAMPLE, so while `pos + i` is
    // still below `pdc_delay` every frame reads lane sample 0 and the gain
    // is flat. Clamping the run's START instead let the plan ramp through a
    // region the mixer holds still: 48% of gain wrong at frame 192.
    //
    // This is not a corner case. `pos` is the absolute timeline position,
    // so it fires on every block at the top of a session and after every
    // loop wrap to a low start, on any latency-compensated track.
    let r = vec![
        AbsParamEvent { sample: 0, value: 1.0 },
        AbsParamEvent { sample: 400, value: 0.0 },
    ];
    for (pos, pdc) in [(0u64, 192u64), (0, 512), (50, 192), (191, 192), (192, 192)] {
        let buf = signal(512);
        let s = Strip { gain: 1.0, ramp: &r, pan: flat_pan(), audible: true, pdc_delay: pdc };
        let b = baseline(&s, &buf, 512, pos);
        let j = jit(&s, &buf, 512, pos);
        assert_samples_close(&b.post, &j.post, 1e-5, &format!("pos={pos} pdc={pdc}"));
    }
}

#[test]
fn a_long_automation_lane_does_not_change_the_result() {
    // Companion to the perf fix: the breakpoint cursor is now seeded by
    // binary search and only walked forward. Correctness has to be identical
    // to the baseline no matter how far into the lane the block sits — a
    // mis-seeded cursor would read the wrong pair of breakpoints and be
    // silently wrong rather than slow.
    let r: Vec<AbsParamEvent> = (0..12_000)
        .map(|n| AbsParamEvent { sample: n * 100, value: if n % 2 == 0 { 1.0 } else { 0.3 } })
        .collect();
    let buf = signal(512);
    for pos in [0u64, 100, 150, 50_000, 599_000, 1_199_000] {
        let s = Strip { gain: 0.8, ramp: &r, pan: moving_pan(), audible: true, pdc_delay: 0 };
        let b = baseline(&s, &buf, 512, pos);
        let j = jit(&s, &buf, 512, pos);
        assert_samples_close(&b.post, &j.post, 1e-5, &format!("pos={pos} on a 12k-point lane"));
    }
}

#[test]
fn odd_block_sizes_render_every_frame() {
    // The kernel handles frame pairs; the tail frame is finished in Rust. An
    // off-by-one there loses the last frame of every block, which is a click
    // at the block rate — audible, and invisible to a test that only uses
    // powers of two.
    // EVERY odd size from 1 to 511, plus a few even ones for symmetry — the
    // claim in `docs/GAP_ANALYSIS.md` is exactly this, and it used to be a
    // hand-picked list of eight (two of them even) that did not match it.
    let r = fader_move();
    for frames in (1..512).step_by(2).chain([2, 128, 512]) {
        let buf = signal(frames);
        let s = Strip { gain: 0.9, ramp: &r, pan: moving_pan(), audible: true, pdc_delay: 0 };
        let sc = scalar(&s, &buf, frames, 0);
        let j = jit(&s, &buf, frames, 0);
        assert_eq!(sc.post, j.post, "{frames} frames");
        assert!(
            j.post.iter().all(|&x| x != -7.5),
            "{frames} frames: a frame was left at its dirty value"
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
    let sc = scalar(&s, &buf, 512, 0);
    let j = jit(&s, &buf, 512, 0);
    assert_eq!(sc.post, j.post);
    let b = baseline(&s, &buf, 512, 0);
    assert_samples_close(&b.post, &j.post, 1e-5, "multi-segment");
}

#[test]
fn a_muted_strip_clears_the_post_buffer() {
    // Not "writes nothing" — `post` is read after the fader by the sends and
    // by routing, so a muted strip that skipped it would send the previous
    // run's audio to the master once per block.
    let buf = signal(256);
    let s = Strip { gain: 1.0, ramp: &[], pan: flat_pan(), audible: false, pdc_delay: 0 };
    let b = baseline(&s, &buf, 256, 0);
    let j = jit(&s, &buf, 256, 0);
    assert!(b.post.iter().all(|&x| x == 0.0), "the baseline writes silence");
    assert_eq!(b.post, j.post);
    assert_eq!(j.acc, Accum::default());
}

#[test]
fn the_kernels_overwrite_the_post_buffer_rather_than_accumulate() {
    // The direction this assertion points in flipped with Plan G2. The fader
    // used to add into the shared master, so accumulating was the contract;
    // now it fills a per-run buffer that routing reads, so ADDING would double
    // every track on the second block it rendered.
    let buf = signal(128);
    let s = Strip { gain: 0.5, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, 128, 0, 127);
    let mut post = vec![-7.5; 256];
    let mut acc = Accum::default();
    assert!(kernels().run(&p, &buf, &mut post, &mut acc));
    let once = post.clone();
    assert!(kernels().run(&p, &buf, &mut post, &mut acc));
    assert_eq!(once, post, "a second run must produce the same samples, not twice them");
}

#[test]
fn pan_index_moves_the_pan_and_not_the_output_index() {
    // A loop wrap splits a callback block into runs. The second run's audio
    // starts at index 0 of its OWN post buffer, but its pan must continue from
    // where the first run left off — the mixer keeps `f` (block-relative) and
    // the buffer index (run-relative) apart, and so must the plan.
    let frames = 64;
    let buf = signal(frames);
    let s = Strip { gain: 1.0, ramp: &[], pan: moving_pan(), audible: true, pdc_delay: 0 };

    // Same run, planned as if it were the first half of a 128-frame block and
    // then as if it were the second half.
    let first = plan(&s, 0, frames, 0, 2 * frames - 1);
    let second = plan(&s, 0, frames, frames, 2 * frames - 1);
    let mut post_a = dirty(frames);
    let mut post_b = dirty(frames);
    let mut acc = Accum::default();
    assert!(kernels().run(&first, &buf, &mut post_a, &mut acc));
    assert!(kernels().run(&second, &buf, &mut post_b, &mut acc));

    assert!(post_a.iter().all(|&x| x != -7.5), "the first run filled its buffer from 0");
    assert!(post_b.iter().all(|&x| x != -7.5), "so did the second");
    assert_ne!(post_a, post_b, "the pan must have moved between the two halves");
    // And against the mixer, for the half that is easy to get wrong.
    let mut post_ref = dirty(frames);
    let mut acc_ref = Accum::default();
    apply_fader_into(
        &s,
        &buf,
        frames,
        0,
        frames,
        2 * frames - 1,
        &mut RampCursor::new(),
        &mut post_ref,
        &mut acc_ref,
    );
    assert_samples_close(&post_ref, &post_b, 1e-5, "second half of a wrapped block");
}

#[test]
fn the_kernels_decline_what_they_cannot_render() {
    let buf = signal(64);
    // Too many breakpoints for the plan to hold: the kernels must SAY so,
    // because the alternative is a post buffer left at whatever it held.
    let dense: Vec<_> =
        (0..300).map(|n| AbsParamEvent { sample: n * 2, value: (n % 2) as f32 }).collect();
    let s = Strip { gain: 1.0, ramp: &dense, pan: flat_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, 64, 0, 63);
    assert!(p.overflowed);
    let mut post = vec![-7.5; 128];
    let mut acc = Accum::default();
    assert!(!kernels().run(&p, &buf, &mut post, &mut acc));
    assert!(post.iter().all(|&x| x == -7.5), "a declined plan must not touch the buffer");

    // A mono master used to be declined too. It is not the fader's business
    // any more — `mix_post_into` downmixes, after the kernel has run.
    let s = Strip { gain: 1.0, ramp: &[], pan: flat_pan(), audible: true, pdc_delay: 0 };
    let p = plan(&s, 0, 64, 0, 63);
    let mut post = vec![0.0; 128];
    assert!(kernels().run(&p, &buf, &mut post, &mut acc));
    let mut mono = vec![0.0; 64];
    mix_post_into(&mut mono, 0, 1, &post, 64);
    assert!(mono.iter().any(|&x| x != 0.0));
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
    // The integration shape: 24 tracks, a mix of automated and static, each
    // through the fader into its own post buffer and then into the master —
    // the same two steps `mixer::render_impl` takes. This is the number that
    // would change if the JIT path were wired in.
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

    let mut master_b = vec![0.0; frames * 2];
    let mut post = dirty(frames);
    let mut acc_b = Accum::default();
    for s in &strips {
        apply_fader_into(
            s,
            &buf,
            frames,
            1024,
            0,
            frames - 1,
            &mut RampCursor::new(),
            &mut post,
            &mut acc_b,
        );
        mix_post_into(&mut master_b, 0, 2, &post, frames);
    }

    let mut master_j = vec![0.0; frames * 2];
    let mut post = dirty(frames);
    let mut acc_j = Accum::default();
    for s in &strips {
        let p = plan(s, 1024, frames, 0, frames - 1);
        assert!(kernels().run(&p, &buf, &mut post, &mut acc_j));
        mix_post_into(&mut master_j, 0, 2, &post, frames);
    }

    assert_samples_close(&master_b, &master_j, 1e-5, "24-track mix");
}
