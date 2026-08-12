//! Transport position math (pure, RT-safe, allocation-free).

/// Loop region snapshot, read from atomics once per callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopSpec {
    pub enabled: bool,
    /// Inclusive start, samples.
    pub start: u64,
    /// Exclusive end, samples. `end <= start` disables the loop.
    pub end: u64,
}

impl LoopSpec {
    pub const OFF: LoopSpec = LoopSpec { enabled: false, start: 0, end: 0 };

    #[inline]
    pub fn active(&self) -> bool {
        self.enabled && self.end > self.start
    }
}

/// Advance `pos` by `frames`, wrapping inside the loop region when the
/// playhead crosses `end`. Positions already past the loop end are unaffected
/// (free-running). Wrapping lands at `start + (overshoot % span)`, which also
/// handles advances larger than the loop span.
#[inline]
pub fn advance(pos: u64, frames: u64, lp: &LoopSpec) -> u64 {
    let new = pos.saturating_add(frames);
    if lp.active() && pos < lp.end && new >= lp.end {
        let span = lp.end - lp.start;
        // `new >= end > start` -> new - start > 0
        lp.start + (new - lp.start) % span
    } else {
        new
    }
}

/// Position of the `i`-th frame of a callback that started at `base`.
#[inline]
pub fn frame_pos(base: u64, i: u64, lp: &LoopSpec) -> u64 {
    advance(base, i, lp)
}

/// Convert samples to seconds (control-plane convenience).
pub fn samples_to_secs(samples: u64, sample_rate: u32) -> f64 {
    samples as f64 / sample_rate.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const LP: LoopSpec = LoopSpec { enabled: true, start: 100, end: 200 };

    #[test]
    fn advances_linearly_without_loop() {
        assert_eq!(advance(0, 128, &LoopSpec::OFF), 128);
        assert_eq!(advance(1000, 0, &LoopSpec::OFF), 1000);
    }

    #[test]
    fn wraps_at_loop_end() {
        // 190 + 20 = 210 -> wraps to 100 + (210-100) % 100 = 110
        assert_eq!(advance(190, 20, &LP), 110);
        // exact hit on end wraps to start
        assert_eq!(advance(190, 10, &LP), 100);
    }

    #[test]
    fn wraps_multiple_spans() {
        // 150 + 500 = 650 -> 100 + 550 % 100 = 150
        assert_eq!(advance(150, 500, &LP), 150);
    }

    #[test]
    fn before_loop_enters_region_and_wraps() {
        // 0 + 250 = 250 crosses end -> 100 + 150 % 100 = 150
        assert_eq!(advance(0, 250, &LP), 150);
        // 0 + 50 stays before the region
        assert_eq!(advance(0, 50, &LP), 50);
    }

    #[test]
    fn past_loop_end_is_free_running() {
        assert_eq!(advance(300, 100, &LP), 400);
    }

    #[test]
    fn degenerate_loop_is_ignored() {
        let bad = LoopSpec { enabled: true, start: 200, end: 200 };
        assert_eq!(advance(150, 100, &bad), 250);
    }

    #[test]
    fn frame_pos_walks_the_loop() {
        let positions: Vec<u64> = (0..6).map(|i| frame_pos(197, i, &LP)).collect();
        assert_eq!(positions, vec![197, 198, 199, 100, 101, 102]);
    }

    #[test]
    fn no_overflow_at_u64_edge() {
        assert_eq!(advance(u64::MAX - 1, 10, &LoopSpec::OFF), u64::MAX);
    }
}
