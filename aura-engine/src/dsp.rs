//! Three ways to run one track strip over one block, so the JIT has something
//! honest to be compared against.
//!
//! * [`apply_fader_into`] — a verbatim port of `mixer::apply_fader_into`. **The
//!   baseline.** Any speedup claim in this crate is against this function,
//!   because this is the code that runs in the app today.
//! * [`multipass`] — the un-fused shape `jit.md` asks to be compared: gain,
//!   ramp, pan and accumulate as four separate passes over the buffer. This is
//!   what a naive node graph does, and it is the number a JIT flatters itself
//!   against.
//! * [`fused_scalar`] — the [`crate::strip::Plan`] run as straight-line scalar
//!   Rust. **The control.** It has the JIT's algorithm and none of its
//!   machinery, so `fused_scalar` vs. `apply_fader_into` measures the *plan*, and
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

/// `mixer::mix_post_into` — add a stereo post-fader run into the `out_ch`-wide
/// master.
///
/// Ported because it is what the fader stopped doing. Plan G2 (bus tracks and
/// sends) split the strip in two: the fader now writes a contiguous stereo
/// **post-fader buffer**, and routing — master, a bus, a send tap, the output
/// PDC — reads that buffer afterwards. Which means the channel-count and
/// accumulate-vs-overwrite questions left the fader entirely, and the kernel
/// this crate generates got simpler for it: contiguous stereo stores, no
/// read-modify-write, and no reason to refuse a mono device.
pub fn mix_post_into(out: &mut [f32], f: usize, out_ch: usize, post: &[f32], run: usize) {
    for i in 0..run {
        mix_out(out, f + i, out_ch, post[i * 2], post[i * 2 + 1]);
    }
}

/// **The baseline.** Verbatim port of `mixer::apply_fader_into`: per-sample
/// ramp lookup, per-sample pan lerp (including its per-sample divide), a
/// branch on mute, meters, and the write into the post-fader buffer.
///
/// Kept structurally identical to the original — same order of operations,
/// same `unwrap_or(1.0)`, same `saturating_*` on the ramp position. If the
/// mixer's loop changes, this must change with it or the benchmark stops
/// meaning anything. (It already has once: Plan G2 turned the accumulate into
/// `out` into a write into `post`.)
///
/// `post` is the run's own buffer, indexed from 0 — while the pan lerp counts
/// from `pan_index`, which is the frame's position in the whole callback
/// block. The two indices are genuinely different and the mixer keeps them
/// apart the same way.
#[allow(clippy::too_many_arguments)]
pub fn apply_fader_into(
    strip: &Strip<'_>,
    buf: &[f32],
    frames: usize,
    pos: u64,
    pan_index: usize,
    pan_last: usize,
    cursor: &mut RampCursor,
    post: &mut [f32],
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
        post[i * 2] = l;
        post[i * 2 + 1] = r;
    }
}

/// The un-fused shape: four passes over `post`, one per stage.
///
/// No caller-provided scratch any more, and not for tidiness: Plan G2 gave the
/// fader a post-fader buffer of its own, so the multi-pass shape can stage its
/// work in the destination. That removes the last excuse a naive graph had for
/// allocating per block, which makes it a fairer opponent rather than a weaker
/// one.
#[allow(clippy::too_many_arguments)]
pub fn multipass(
    strip: &Strip<'_>,
    buf: &[f32],
    frames: usize,
    pos: u64,
    pan_index: usize,
    pan_last: usize,
    post: &mut [f32],
    acc: &mut Accum,
) {
    let n = frames * 2;
    if !strip.audible {
        // A muted strip still folds zeros and still has to CLEAR the post
        // buffer: it is an overwrite target that sends and routing read
        // afterwards, so skipping it would leak the previous run's audio into
        // this one.
        post[..n].fill(0.0);
        for _ in 0..frames {
            acc.fold(0.0, 0.0);
        }
        return;
    }

    // Pass 1 — gain.
    for i in 0..n {
        post[i] = buf[i] * strip.gain;
    }
    // Pass 2 — the automation ramp.
    if !strip.ramp.is_empty() {
        let mut cursor = RampCursor::new();
        for i in 0..frames {
            let v = cursor
                .value(strip.ramp, pos.saturating_add(i as u64).saturating_sub(strip.pdc_delay))
                .unwrap_or(1.0);
            post[i * 2] *= v;
            post[i * 2 + 1] *= v;
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
        post[i * 2] *= gl;
        post[i * 2 + 1] *= gr;
    }
    // Pass 4 — meters.
    for i in 0..frames {
        acc.fold(post[i * 2], post[i * 2 + 1]);
    }
}

/// **The control.** The plan run as straight-line scalar Rust: same algorithm
/// as the JIT kernels, compiled by rustc.
pub fn fused_scalar(plan: &Plan, buf: &[f32], post: &mut [f32], acc: &mut Accum) {
    if plan.silent {
        // Not a no-op, and this is the one place the Plan G2 merge changed a
        // conclusion: `post` is an overwrite target read afterwards by the
        // sends and the routing, so a muted strip has to clear it. Folding
        // zeros into the meters, on the other hand, genuinely cannot move a
        // peak (`max` against a non-negative running value) or a sum.
        post.fill(0.0);
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
            post[frame * 2] = l;
            post[frame * 2 + 1] = r;
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
        (0..frames * 2).map(|i| (i as f32 * 0.37).sin() * 0.8).collect()
    }

    /// The same run through all three implementations. `post` starts as a
    /// recognisable non-zero pattern in every case, so a path that forgets to
    /// write a frame is caught instead of matching a zeroed buffer by luck.
    fn run3(strip: &Strip<'_>, frames: usize, pos: u64) -> [(Vec<f32>, Accum); 3] {
        let buf = noise(frames);
        let pan_last = frames - 1;
        let dirty = vec![-7.5f32; frames * 2];

        let mut post_a = dirty.clone();
        let mut acc_a = Accum::default();
        apply_fader_into(
            strip,
            &buf,
            frames,
            pos,
            0,
            pan_last,
            &mut RampCursor::new(),
            &mut post_a,
            &mut acc_a,
        );

        let mut post_b = dirty.clone();
        let mut acc_b = Accum::default();
        multipass(strip, &buf, frames, pos, 0, pan_last, &mut post_b, &mut acc_b);

        let p = plan(strip, pos, frames, 0, pan_last);
        let mut post_c = dirty;
        let mut acc_c = Accum::default();
        fused_scalar(&p, &buf, &mut post_c, &mut acc_c);

        [(post_a, acc_a), (post_b, acc_b), (post_c, acc_c)]
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
    fn a_muted_strip_clears_the_post_buffer_rather_than_skipping_it() {
        // The Plan G2 seam makes this load-bearing: `post` is read afterwards
        // by the sends and by routing, so a muted strip that left the buffer
        // alone would leak the PREVIOUS run's audio into this one — a
        // once-per-block burst of stale signal on a track the user muted.
        let s = Strip {
            gain: 1.0,
            ramp: &[],
            pan: PanQuad { gl0: 0.7, gr0: 0.7, gl1: 0.7, gr1: 0.7 },
            audible: false,
            pdc_delay: 0,
        };
        let [(a, aa), (b, ab), (c, ac)] = run3(&s, 256, 0);
        assert!(a.iter().all(|&x| x == 0.0), "the baseline writes silence");
        assert_eq!(a, b);
        assert_eq!(a, c, "the fused path must clear, not skip");
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
        assert_close(&a, &b, 1e-6, "pdc-shifted ramp, multipass");
        assert_close(&a, &c, 1e-5, "pdc-shifted ramp, fused");
    }

    #[test]
    fn mixing_a_post_run_into_a_mono_master_downmixes() {
        // The channel-count question now lives here rather than in the fader,
        // which is why the kernels no longer have to care about it.
        let post = [1.0f32, 0.5, -0.25, 0.75];
        let mut mono = vec![0.0f32; 2];
        mix_post_into(&mut mono, 0, 1, &post, 2);
        assert_eq!(mono, vec![0.75, 0.25]);

        let mut stereo = vec![10.0f32; 4];
        mix_post_into(&mut stereo, 0, 2, &post, 2);
        assert_eq!(stereo, vec![11.0, 10.5, 9.75, 10.75], "the master ACCUMULATES");
    }
}
