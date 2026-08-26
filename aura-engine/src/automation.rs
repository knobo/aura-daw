//! The ramp contract, ported from `src-tauri/src/plugins/automation.rs`.
//!
//! Ported rather than shared because this crate is standalone (see
//! `docs/backlog/jit-engine.md`), and ported **verbatim** on purpose: the
//! baseline this crate benchmarks against has to be the code that actually
//! runs in the app, or the comparison measures the port instead of the
//! change. If the app's interpolation ever changes, this file is the one that
//! has to move with it — `dsp::apply_fader_into`'s doc says so too.

/// One breakpoint on a compiled lane: absolute sample position, value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AbsParamEvent {
    pub sample: u64,
    pub value: f32,
}

/// Interpolate with a precomputed `partition_point` index.
#[inline]
pub fn segment_value(events: &[AbsParamEvent], idx: usize, sample: u64) -> f32 {
    if idx == 0 {
        events[0].value
    } else if idx >= events.len() {
        events[events.len() - 1].value
    } else {
        let a = events[idx - 1];
        let b = events[idx];
        let span = (b.sample - a.sample) as f32;
        let t = (sample - a.sample) as f32 / span;
        a.value + (b.value - a.value) * t
    }
}

/// Value at `sample`: linear between the surrounding breakpoints, holding
/// first/last outside the curve. `None` for an empty curve.
pub fn value_at(events: &[AbsParamEvent], sample: u64) -> Option<f32> {
    if events.is_empty() {
        return None;
    }
    let idx = events.partition_point(|e| e.sample <= sample);
    Some(segment_value(events, idx, sample))
}

/// O(1)-per-frame cursor over a ramp, re-seeding only on a backward jump.
#[derive(Clone, Copy, Debug)]
pub struct RampCursor {
    idx: usize,
    last: u64,
}

impl Default for RampCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl RampCursor {
    pub fn new() -> Self {
        Self { idx: 0, last: u64::MAX }
    }

    #[inline]
    pub fn value(&mut self, events: &[AbsParamEvent], sample: u64) -> Option<f32> {
        if events.is_empty() {
            return None;
        }
        if sample < self.last {
            self.idx = events.partition_point(|e| e.sample <= sample);
        } else {
            while self.idx < events.len() && events[self.idx].sample <= sample {
                self.idx += 1;
            }
        }
        self.last = sample;
        Some(segment_value(events, self.idx, sample))
    }
}
