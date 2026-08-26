//! What one track strip does to one run of samples, described as data.
//!
//! `mixer::apply_fader_into` decides this per SAMPLE: it asks the ramp cursor for a
//! value, lerps the pan, branches on mute, and does it 512 times. Every one of
//! those decisions is the same for long stretches of the block, and the stretch
//! boundaries are known before the loop starts — they are the automation
//! breakpoints.
//!
//! So the shape is: **decide once per stretch, then run straight-line code.**
//! [`Plan`] is that decision, [`Coef`] is the straight-line form (gain and pan
//! as affine functions of the frame index), and both the scalar reference in
//! [`crate::dsp`] and the JIT kernels in [`crate::jit`] consume it. Building a
//! plan is bounded, branchy, integer-ish work; running one is a flat multiply
//! chain. That split is what makes the inner loop vectorisable at all.

use crate::automation::{value_at, AbsParamEvent};

/// Gain and pan over one stretch of a block, as affine functions of the frame
/// index `i` counted from the stretch's own start:
///
/// ```text
/// g(i)  = g0  + dg  * i
/// gl(i) = gl0 + dgl * i
/// gr(i) = gr0 + dgr * i
/// out_l += in_l * g(i) * gl(i)
/// ```
///
/// The multiply order is `(in * g) * gl`, matching `apply_fader_into` exactly, so
/// the flat case (`dg == dgl == dgr == 0`) is **bit-identical** to today's
/// mixer rather than merely close.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Coef {
    pub g0: f32,
    pub dg: f32,
    pub gl0: f32,
    pub dgl: f32,
    pub gr0: f32,
    pub dgr: f32,
}

impl Coef {
    /// Constant gain and pan — no automation anywhere in this stretch.
    pub fn flat(g: f32, gl: f32, gr: f32) -> Self {
        Self { g0: g, dg: 0.0, gl0: gl, dgl: 0.0, gr0: gr, dgr: 0.0 }
    }

    /// Whether every coefficient is constant. This is the fact the JIT
    /// specialises on hardest: a flat stretch needs no index at all.
    #[inline]
    pub fn is_flat(&self) -> bool {
        self.dg == 0.0 && self.dgl == 0.0 && self.dgr == 0.0
    }

    /// The same coefficients re-based `n` frames later, so a stretch can be
    /// split without re-deriving it from the lane.
    #[inline]
    pub fn advanced(&self, n: usize) -> Self {
        let n = n as f32;
        Self {
            g0: self.g0 + self.dg * n,
            gl0: self.gl0 + self.dgl * n,
            gr0: self.gr0 + self.dgr * n,
            ..*self
        }
    }
}

/// One straight-line stretch of a block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    /// Frame offset within the run.
    pub offset: usize,
    pub frames: usize,
    pub coef: Coef,
}

/// How many stretches one block may be cut into before the plan gives up.
///
/// 16 stretches inside a 512-frame block is an automation point every 32
/// samples. The UI cannot draw that; a dense MIDI-CC import could. So the
/// bound is not a guess about typical content, it is what keeps `Plan`
/// **fixed-size** — a plan that allocated would be useless on the thread it
/// exists for.
pub const MAX_SEGMENTS: usize = 16;

/// A block's worth of stretches. Fixed size, `Copy`, no allocation.
#[derive(Clone, Copy, Debug)]
pub struct Plan {
    segments: [Segment; MAX_SEGMENTS],
    len: usize,
    /// The block had more breakpoints than [`MAX_SEGMENTS`] can hold.
    ///
    /// NOT an error and NOT a truncation: the caller must fall back to the
    /// per-sample path ([`crate::dsp::apply_fader_into`]) for this block, which is
    /// always correct and merely slower. Silently dropping breakpoints would
    /// make automation stop moving on exactly the busiest lanes.
    pub overflowed: bool,
    /// The strip is inaudible this block (muted, or not audible under a solo
    /// elsewhere). Segments are empty and there is nothing to run.
    pub silent: bool,
}

impl Plan {
    pub fn segments(&self) -> &[Segment] {
        &self.segments[..self.len]
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when the whole block is one constant-coefficient stretch — the
    /// common case, and the one with the cheapest kernel.
    pub fn is_flat(&self) -> bool {
        self.len == 1 && self.segments[0].coef.is_flat()
    }

    fn push(&mut self, seg: Segment) -> bool {
        if self.len == MAX_SEGMENTS {
            self.overflowed = true;
            return false;
        }
        self.segments[self.len] = seg;
        self.len += 1;
        true
    }
}

/// Everything about a strip's fader for one run, in the terms
/// `mixer::apply_fader_into` already takes.
#[derive(Clone, Copy, Debug)]
pub struct Strip<'a> {
    /// Fader gain (the atomic `ParamTable::gain`).
    pub gain: f32,
    /// Compiled track-gain ramp; empty means "parameter untouched", and the
    /// neutral multiplier 1.0 applies.
    pub ramp: &'a [AbsParamEvent],
    /// Pan gains at the run's first and last frame, as
    /// `mixer::pan_gain_quad` resolves them.
    pub pan: PanQuad,
    /// False when the strip is muted or silenced by another track's solo.
    pub audible: bool,
    /// This strip's PDC delay: ramps are read at `pos + i - pdc_delay`, so an
    /// automated move on a latency-compensated track is heard at the right
    /// time. Carried through the plan because it shifts *which* breakpoints
    /// land inside the block.
    pub pdc_delay: u64,
}

/// Pan gains at the run's first and last frame; `lerp_pan` interpolates
/// between them over `pan_last + 1` frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PanQuad {
    pub gl0: f32,
    pub gr0: f32,
    pub gl1: f32,
    pub gr1: f32,
}

impl PanQuad {
    pub fn flat(gl: f32, gr: f32) -> Self {
        Self { gl0: gl, gr0: gr, gl1: gl, gr1: gr }
    }

    #[inline]
    pub fn is_static(&self) -> bool {
        self.gl0 == self.gl1 && self.gr0 == self.gr1
    }
}

/// Cut a run into straight-line stretches.
///
/// `pos` is the absolute sample position of the run's first frame; `frames` is
/// its length; `pan_index` is the frame index the pan lerp counts from
/// (`f` in `apply_fader_into`) and `pan_last` its denominator — the two are the
/// block's, not the run's, because pan interpolates across the whole callback
/// block while a loop wrap can split it into several runs.
///
/// Bounded and allocation-free: one binary search to find the first
/// breakpoint, then a single forward walk over the breakpoints that
/// fall inside the run, and a fixed-size result.
pub fn plan(strip: &Strip<'_>, pos: u64, frames: usize, pan_index: usize, pan_last: usize) -> Plan {
    let mut out = Plan {
        segments: [Segment { offset: 0, frames: 0, coef: Coef::default() }; MAX_SEGMENTS],
        len: 0,
        overflowed: false,
        silent: !strip.audible,
    };
    if !strip.audible || frames == 0 {
        return out;
    }

    // Pan first: affine over the whole block, so its slope is the same in
    // every stretch and only the base has to be re-based per stretch.
    let (dgl, dgr) = if pan_last == 0 || strip.pan.is_static() {
        (0.0, 0.0)
    } else {
        let d = pan_last as f32;
        ((strip.pan.gl1 - strip.pan.gl0) / d, (strip.pan.gr1 - strip.pan.gr0) / d)
    };
    let pan_base = pan_index as f32;
    let gl_at_run = strip.pan.gl0 + dgl * pan_base;
    let gr_at_run = strip.pan.gr0 + dgr * pan_base;

    if strip.ramp.is_empty() {
        // No ramp: `apply_fader_into`'s `unwrap_or(1.0)` means the fader value is
        // the whole gain, exactly.
        out.push(Segment {
            offset: 0,
            frames,
            coef: Coef {
                g0: strip.gain,
                dg: 0.0,
                gl0: gl_at_run,
                dgl,
                gr0: gr_at_run,
                dgr,
            },
        });
        return out;
    }

    // Ramp domain: frame `i` reads the lane at `pos + i - pdc_delay`, and the
    // subtraction SATURATES PER SAMPLE — `dsp::apply_fader_into` does
    // `pos.saturating_add(i).saturating_sub(pdc_delay)` inside the loop, not
    // once outside it. Clamping the run's start instead is a real difference
    // whenever `pdc_delay > pos`, which is every block at the top of a session
    // and after any loop wrap to a low start, on any latency-compensated
    // track: the lane stands still while the plan would ramp through it.
    // Measured at `pos=0, pdc_delay=192`: 0.48 of gain, 48% wrong.
    let lane_at = |o: usize| pos.saturating_add(o as u64).saturating_sub(strip.pdc_delay);
    let read_to = lane_at(frames); // exclusive, in lane samples

    let mut offset = 0usize;
    // The held prefix, where every frame reads lane sample 0 and the gain is
    // therefore FLAT. `+ 1` because the lane is still 0 at the frame where
    // `pos + i == pdc_delay`, not just before it.
    if strip.pdc_delay > pos {
        let held = ((strip.pdc_delay - pos) as usize).saturating_add(1).min(frames);
        let v0 = value_at(strip.ramp, 0).unwrap_or(1.0);
        let ok = out.push(Segment {
            offset: 0,
            frames: held,
            coef: Coef {
                g0: strip.gain * v0,
                dg: 0.0,
                gl0: gl_at_run,
                dgl,
                gr0: gr_at_run,
                dgr,
            },
        });
        if !ok || held >= frames {
            return out;
        }
        offset = held;
    }

    // Cursor over the breakpoints, seeded ONCE by binary search and then only
    // advanced. `iter().find(...)` here was O(the whole session's lane) per
    // segment per block — `TrackRamps::gain` is compiled session-wide at
    // graph rebuild, so on a 48 000-point lane the plan measured 40x SLOWER
    // than the baseline it replaces, degrading linearly with lane length.
    // The benchmark used 64 points and could not see it.
    let mut brk = strip.ramp.partition_point(|e| e.sample <= lane_at(offset));
    while offset < frames {
        let seg_start_sample = lane_at(offset);
        // The next breakpoint strictly after this stretch's first sample ends
        // it: from there on the interpolation uses a different pair of
        // points, so the affine form changes.
        while brk < strip.ramp.len() && strip.ramp[brk].sample <= seg_start_sample {
            brk += 1;
        }
        let next_break = strip
            .ramp
            .get(brk)
            .map(|e| e.sample)
            .filter(|s| *s < read_to)
            .unwrap_or(read_to);
        let seg_frames = (next_break - seg_start_sample) as usize;
        let seg_frames = seg_frames.min(frames - offset).max(1);

        let v0 = value_at(strip.ramp, seg_start_sample).unwrap_or(1.0);
        // Slope from the stretch's own endpoints: inside a stretch the lane
        // is a straight line by construction, so two samples define it. A
        // one-frame stretch has no slope to measure — and needs none.
        let dg = if seg_frames > 1 {
            let v1 = value_at(strip.ramp, seg_start_sample + (seg_frames - 1) as u64).unwrap_or(1.0);
            (v1 - v0) / (seg_frames - 1) as f32
        } else {
            0.0
        };

        let base = offset as f32;
        let ok = out.push(Segment {
            offset,
            frames: seg_frames,
            coef: Coef {
                g0: strip.gain * v0,
                dg: strip.gain * dg,
                gl0: gl_at_run + dgl * base,
                dgl,
                gr0: gr_at_run + dgr * base,
                dgr,
            },
        });
        if !ok {
            return out;
        }
        offset += seg_frames;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip<'a>(ramp: &'a [AbsParamEvent], gain: f32) -> Strip<'a> {
        Strip {
            gain,
            ramp,
            pan: PanQuad { gl0: 0.7, gr0: 0.7, gl1: 0.7, gr1: 0.7 },
            audible: true,
            pdc_delay: 0,
        }
    }

    #[test]
    fn no_ramp_and_static_pan_is_one_flat_segment() {
        let p = plan(&strip(&[], 0.5), 0, 512, 0, 511);
        assert_eq!(p.segments().len(), 1);
        assert!(p.is_flat());
        let c = p.segments()[0].coef;
        assert_eq!(c.g0, 0.5, "the fader value reaches the kernel untouched");
        assert_eq!((c.dg, c.dgl, c.dgr), (0.0, 0.0, 0.0));
    }

    #[test]
    fn moving_pan_is_not_flat_even_without_a_ramp() {
        let mut s = strip(&[], 1.0);
        s.pan = PanQuad { gl0: 1.0, gr0: 0.0, gl1: 0.0, gr1: 1.0 };
        let p = plan(&s, 0, 512, 0, 511);
        assert_eq!(p.segments().len(), 1);
        assert!(!p.is_flat());
        let c = p.segments()[0].coef;
        assert!((c.dgl + 1.0 / 511.0).abs() < 1e-9, "dgl {}", c.dgl);
    }

    #[test]
    fn a_breakpoint_inside_the_block_splits_it() {
        let ramp =
            [AbsParamEvent { sample: 0, value: 0.0 }, AbsParamEvent { sample: 256, value: 1.0 },
             AbsParamEvent { sample: 512, value: 0.0 }];
        let p = plan(&strip(&ramp, 1.0), 0, 512, 0, 511);
        let segs = p.segments();
        assert_eq!(segs.len(), 2, "{segs:?}");
        assert_eq!((segs[0].offset, segs[0].frames), (0, 256));
        assert_eq!((segs[1].offset, segs[1].frames), (256, 256));
        // Rising then falling: the slopes must have opposite signs.
        assert!(segs[0].coef.dg > 0.0 && segs[1].coef.dg < 0.0, "{segs:?}");
    }

    #[test]
    fn segments_tile_the_run_exactly() {
        // A breakpoint every 37 samples — deliberately not a divisor.
        let ramp: Vec<_> = (0..14)
            .map(|n| AbsParamEvent { sample: n * 37, value: (n % 3) as f32 / 2.0 })
            .collect();
        let p = plan(&strip(&ramp, 1.0), 0, 480, 0, 479);
        assert!(!p.overflowed, "13 breakpoints must fit in {MAX_SEGMENTS}");
        let mut expect = 0usize;
        for s in p.segments() {
            assert_eq!(s.offset, expect, "gap or overlap at {expect}");
            expect += s.frames;
        }
        assert_eq!(expect, 480, "the plan must cover every frame exactly once");
    }

    #[test]
    fn too_many_breakpoints_overflow_rather_than_truncate() {
        // One point every 4 samples: far past MAX_SEGMENTS.
        let ramp: Vec<_> = (0..200)
            .map(|n| AbsParamEvent { sample: n * 4, value: (n % 2) as f32 })
            .collect();
        let p = plan(&strip(&ramp, 1.0), 0, 512, 0, 511);
        assert!(p.overflowed, "the caller has to know to use the scalar path");
        assert!(p.segments().len() <= MAX_SEGMENTS);
    }

    #[test]
    fn a_muted_strip_plans_nothing() {
        let mut s = strip(&[], 1.0);
        s.audible = false;
        let p = plan(&s, 0, 512, 0, 511);
        assert!(p.silent && p.is_empty());
    }

    #[test]
    fn pdc_shifts_which_breakpoints_land_in_the_block() {
        let ramp =
            [AbsParamEvent { sample: 0, value: 0.0 }, AbsParamEvent { sample: 300, value: 1.0 }];
        // Reading at pos 256 with no delay: the 300 breakpoint is inside.
        let a = plan(&strip(&ramp, 1.0), 256, 128, 0, 127);
        assert_eq!(a.segments().len(), 2);
        // With 256 samples of PDC the run reads 0..128 — before it.
        let mut s = strip(&ramp, 1.0);
        s.pdc_delay = 256;
        let b = plan(&s, 256, 128, 0, 127);
        assert_eq!(b.segments().len(), 1);
    }
}
