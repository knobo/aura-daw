//! Three ways to run one track strip over one block, so the JIT has something
//! honest to be compared against.
//!
//! * [`apply_fader`] — a verbatim port of `mixer::apply_fader`. **The
//!   baseline.** Any speedup claim in this crate is against this function,
//!   because this is the code that runs in the app today.
//! * [`multipass`] — the un-fused shape `jit.md` asks to be compared: gain,
//!   ramp, pan and accumulate as four separate passes over the buffer. This is
//!   what a naive node graph does, and it is the number a JIT flatters itself
//!   against.
//! * [`fused_scalar`] — the [`crate::strip::Plan`] run as straight-line scalar
//!   Rust. **The control.** It has the JIT's algorithm and none of its
//!   machinery, so `fused_scalar` vs. `apply_fader` measures the *plan*, and
//!   the JIT vs. `fused_scalar` measures the *code generation*. Reporting only
//!   "JIT vs. baseline" would credit the compiler with the algorithm's win.

use crate::automation::RampCursor;
use crate::strip::{Plan, Strip};

/// Peak and sum-of-squares per channel — `mixer::TrackAccum`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Accum {
    pub pk_l: f32,
    pub pk_r: f32,
    pub ss_l: f32,
    pub ss_r: f32,
}

impl Accum {
    #[inline]
    pub fn fold(&mut self, l: f32, r: f32) {
        self.pk_l = self.pk_l.max(l.abs());
        self.pk_r = self.pk_r.max(r.abs());
        self.ss_l += l * l;
        self.ss_r += r * r;
    }
}

/// `mixer::lerp_pan`.
#[inline]
fn lerp_pan(gl0: f32, gr0: f32, gl1: f32, gr1: f32, i: usize, last: usize) -> (f32, f32) {
    if last == 0 {
        return (gl0, gr0);
    }
    let t = i as f32 / last as f32;
    (gl0 + (gl1 - gl0) * t, gr0 + (gr1 - gr0) * t)
}

/// `mixer::mix_out`.
#[inline]
fn mix_out(out: &mut [f32], frame: usize, out_ch: usize, l: f32, r: f32) {
    let o = frame * out_ch;
    if out_ch >= 2 {
        out[o] += l;
        out[o + 1] += r;
    } else {
        out[o] += 0.5 * (l + r);
    }
}

/// **The baseline.** Verbatim port of `mixer::apply_fader`: per-sample ramp
/// lookup, per-sample pan lerp (including its per-sample divide), a branch on
/// mute, meters, and the mix into `out`.
///
/// Kept structurally identical to the original — same order of operations,
/// same `unwrap_or(1.0)`, same `saturating_*` on the ramp position. If the
/// mixer's loop changes, this must change with it or the benchmark stops
/// meaning anything.
#[allow(clippy::too_many_arguments)]
pub fn apply_fader(
    strip: &Strip<'_>,
    buf: &[f32],
    frames: usize,
    pos: u64,
    pan_index: usize,
    pan_last: usize,
    cursor: &mut RampCursor,
    out: &mut [f32],
    out_ch: usize,
    acc: &mut Accum,
) {
    for i in 0..frames {
        let g = strip.gain
            * cursor
                .value(
                    strip.ramp,
                    pos.saturating_add(i as u64).saturating_sub(strip.pdc_delay),
                )
                .unwrap_or(1.0);
        let (gl, gr) = lerp_pan(
            strip.pan.gl0,
            strip.pan.gr0,
            strip.pan.gl1,
            strip.pan.gr1,
            pan_index + i,
            pan_last,
        );
        let mut l = buf[i * 2] * g * gl;
        let mut r = buf[i * 2 + 1] * g * gr;
        if !strip.audible {
            l = 0.0;
            r = 0.0;
        }
        acc.fold(l, r);
        mix_out(out, pan_index + i, out_ch, l, r);
    }
}

/// The un-fused shape: four passes over `scratch`, one per stage.
///
/// `scratch` must hold `2 * frames` samples and is clobbered. Provided by the
/// caller precisely because a node graph that allocated its own edge buffers
/// per block would be disqualified before the timing started — so the
/// comparison is against a multi-pass graph that is *already* RT-legal, not a
/// straw man.
#[allow(clippy::too_many_arguments)]
pub fn multipass(
    strip: &Strip<'_>,
    buf: &[f32],
    frames: usize,
    pos: u64,
    pan_index: usize,
    pan_last: usize,
    scratch: &mut [f32],
    out: &mut [f32],
    out_ch: usize,
    acc: &mut Accum,
) {
    let n = frames * 2;
    if !strip.audible {
        // A muted strip still folds zeros and adds nothing — same observable
        // result as the baseline's branch, without pretending the pass is free.
        for i in 0..frames {
            acc.fold(0.0, 0.0);
            mix_out(out, pan_index + i, out_ch, 0.0, 0.0);
        }
        return;
    }

    // Pass 1 — gain.
    for i in 0..n {
        scratch[i] = buf[i] * strip.gain;
    }
    // Pass 2 — the automation ramp.
    if !strip.ramp.is_empty() {
        let mut cursor = RampCursor::new();
        for i in 0..frames {
            let v = cursor
                .value(strip.ramp, pos.saturating_add(i as u64).saturating_sub(strip.pdc_delay))
                .unwrap_or(1.0);
            scratch[i * 2] *= v;
            scratch[i * 2 + 1] *= v;
        }
    }
    // Pass 3 — pan.
    for i in 0..frames {
        let (gl, gr) = lerp_pan(
            strip.pan.gl0,
            strip.pan.gr0,
            strip.pan.gl1,
            strip.pan.gr1,
            pan_index + i,
            pan_last,
        );
        scratch[i * 2] *= gl;
        scratch[i * 2 + 1] *= gr;
    }
    // Pass 4 — meters and the mix into the output.
    for i in 0..frames {
        let (l, r) = (scratch[i * 2], scratch[i * 2 + 1]);
        acc.fold(l, r);
        mix_out(out, pan_index + i, out_ch, l, r);
    }
}

/// **The control.** The plan run as straight-line scalar Rust: same algorithm
/// as the JIT kernels, compiled by rustc.
///
/// `out` is indexed from `pan_index` exactly as the baseline does, so the two
/// write the same frames.
pub fn fused_scalar(
    plan: &Plan,
    buf: &[f32],
    pan_index: usize,
    out: &mut [f32],
    out_ch: usize,
    acc: &mut Accum,
) {
    if plan.silent {
        // Folding zeros cannot move a peak (`max` against a non-negative
        // running value) and adds nothing to the sums, and mixing zeros adds
        // nothing to the output. So the silent strip is genuinely a no-op —
        // the baseline's `l = 0.0; r = 0.0` costs a full pass to reach the
        // same place.
        return;
    }
    for seg in plan.segments() {
        let c = seg.coef;
        for i in 0..seg.frames {
            let fi = i as f32;
            let g = c.g0 + c.dg * fi;
            let frame = seg.offset + i;
            let l = buf[frame * 2] * g * (c.gl0 + c.dgl * fi);
            let r = buf[frame * 2 + 1] * g * (c.gr0 + c.dgr * fi);
            acc.fold(l, r);
            mix_out(out, pan_index + frame, out_ch, l, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::AbsParamEvent;
    use crate::strip::{plan, PanQuad};

    /// A ramp shaped like a real fader move: down, up, hold.
    fn ramp() -> Vec<AbsParamEvent> {
        vec![
            AbsParamEvent { sample: 0, value: 1.0 },
            AbsParamEvent { sample: 200, value: 0.25 },
            AbsParamEvent { sample: 400, value: 0.9 },
        ]
    }

    fn noise(frames: usize) -> Vec<f32> {
        // Deterministic, and not a constant — a constant input hides index
        // bugs, because every frame looks like every other frame.
        (0..frames * 2)
            .map(|i| ((i as f32 * 0.37).sin() * 0.8))
            .collect()
    }

    fn run3(strip: &Strip<'_>, frames: usize, pos: u64) -> [(Vec<f32>, Accum); 3] {
        let buf = noise(frames);
        let pan_last = frames - 1;

        let mut out_a = vec![0.0; frames * 2];
        let mut acc_a = Accum::default();
        apply_fader(
            strip,
            &buf,
            frames,
            pos,
            0,
            pan_last,
            &mut RampCursor::new(),
            &mut out_a,
            2,
            &mut acc_a,
        );

        let mut out_b = vec![0.0; frames * 2];
        let mut acc_b = Accum::default();
        let mut scratch = vec![0.0; frames * 2];
        multipass(strip, &buf, frames, pos, 0, pan_last, &mut scratch, &mut out_b, 2, &mut acc_b);

        let p = plan(strip, pos, frames, 0, pan_last);
        let mut out_c = vec![0.0; frames * 2];
        let mut acc_c = Accum::default();
        fused_scalar(&p, &buf, 0, &mut out_c, 2, &mut acc_c);

        [(out_a, acc_a), (out_b, acc_b), (out_c, acc_c)]
    }

    fn assert_close(a: &[f32], b: &[f32], tol: f32, what: &str) {
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            assert!(
                (x - y).abs() <= tol * x.abs().max(1.0),
                "{what}: frame {i} differs: {x} vs {y}"
            );
        }
    }

    #[test]
    fn multipass_matches_the_baseline_within_rounding() {
        // Same interpolation, same values — but splitting the work into
        // passes REASSOCIATES the multiply: the baseline computes
        // `in * (gain * ramp) * pan`, four passes compute
        // `((in * gain) * ramp) * pan`. Float multiplication is not
        // associative, so the last bit moves. Worth stating rather than
        // asserting away: it is the reason no version of this can be
        // byte-compared against an existing bounce.
        let r = ramp();
        let s = Strip {
            gain: 0.7,
            ramp: &r,
            pan: PanQuad { gl0: 0.9, gr0: 0.3, gl1: 0.2, gr1: 0.95 },
            audible: true,
            pdc_delay: 0,
        };
        let [(a, aa), (b, ab), _] = run3(&s, 512, 128);
        assert_close(&a, &b, 1e-6, "multipass vs baseline");
        assert!((aa.ss_l - ab.ss_l).abs() < 1e-4, "{aa:?} vs {ab:?}");
    }

    #[test]
    fn the_flat_case_is_bit_identical_across_all_three() {
        // No ramp, static pan: the plan's affine form degenerates to exactly
        // the baseline's constants, so this must be equality, not tolerance.
        let s = Strip {
            gain: 0.6,
            ramp: &[],
            pan: PanQuad { gl0: 0.7, gr0: 0.7, gl1: 0.7, gr1: 0.7 },
            audible: true,
            pdc_delay: 0,
        };
        let [(a, aa), (b, ab), (c, ac)] = run3(&s, 512, 0);
        assert_eq!(a, b);
        assert_eq!(a, c, "the flat plan must reproduce the mixer exactly");
        assert_eq!((aa, ab), (ac, ac));
    }

    #[test]
    fn the_ramped_case_matches_the_baseline_within_rounding() {
        // The plan replaces per-sample interpolation with an affine form and
        // folds the fader into the coefficients, so equality is not
        // available — 1e-5 relative is ~0.0001 dB, ~100 dB below anything
        // audible.
        let r = ramp();
        let s = Strip {
            gain: 0.7,
            ramp: &r,
            pan: PanQuad { gl0: 0.9, gr0: 0.3, gl1: 0.2, gr1: 0.95 },
            audible: true,
            pdc_delay: 0,
        };
        let [(a, aa), _, (c, ac)] = run3(&s, 512, 0);
        assert_close(&a, &c, 1e-5, "fused vs baseline");
        assert!((aa.pk_l - ac.pk_l).abs() < 1e-5, "{aa:?} vs {ac:?}");
        assert!((aa.ss_r - ac.ss_r).abs() < 1e-4, "{aa:?} vs {ac:?}");
    }

    #[test]
    fn a_muted_strip_leaves_the_output_and_the_meters_alone() {
        let s = Strip {
            gain: 1.0,
            ramp: &[],
            pan: PanQuad { gl0: 0.7, gr0: 0.7, gl1: 0.7, gr1: 0.7 },
            audible: false,
            pdc_delay: 0,
        };
        let [(a, aa), (b, ab), (c, ac)] = run3(&s, 256, 0);
        assert!(a.iter().all(|&x| x == 0.0));
        assert_eq!(a, b);
        assert_eq!(a, c, "the no-op fast path must be observably a no-op");
        assert_eq!((aa, ab), (Accum::default(), Accum::default()));
        assert_eq!(ac, Accum::default());
    }

    #[test]
    fn a_ramp_read_through_pdc_agrees_across_the_three() {
        let r = ramp();
        let s = Strip {
            gain: 0.5,
            ramp: &r,
            pan: PanQuad { gl0: 0.7, gr0: 0.7, gl1: 0.7, gr1: 0.7 },
            audible: true,
            pdc_delay: 192,
        };
        let [(a, _), (b, _), (c, _)] = run3(&s, 512, 256);
        assert_eq!(a, b);
        assert_close(&a, &c, 1e-5, "pdc-shifted ramp");
    }

    #[test]
    fn mono_output_downmixes_the_same_way() {
        let s = Strip {
            gain: 0.8,
            ramp: &[],
            pan: PanQuad { gl0: 1.0, gr0: 0.2, gl1: 1.0, gr1: 0.2 },
            audible: true,
            pdc_delay: 0,
        };
        let frames = 128;
        let buf = noise(frames);
        let p = plan(&s, 0, frames, 0, frames - 1);

        let mut out_a = vec![0.0; frames];
        let mut acc_a = Accum::default();
        apply_fader(
            &s,
            &buf,
            frames,
            0,
            0,
            frames - 1,
            &mut RampCursor::new(),
            &mut out_a,
            1,
            &mut acc_a,
        );
        let mut out_c = vec![0.0; frames];
        let mut acc_c = Accum::default();
        fused_scalar(&p, &buf, 0, &mut out_c, 1, &mut acc_c);
        assert_eq!(out_a, out_c);
        assert_eq!(acc_a, acc_c);
    }
}
