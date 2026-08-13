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

/// Did a callback of `frames` frames starting at `base` carry the playhead
/// across `point`? Returns the exact sample the crossing lands on.
///
/// This is the general "the playhead reached a marked position" primitive:
/// auto-stop passes the song end, but punch-in/out, markers and follow
/// actions are the same question with a different point. It answers only
/// WHERE — never what to do about it, which is the control plane's call
/// (ARCHITECTURE §2.1: no policy on the RT thread).
///
/// `point == 0` means "no point set" and never fires, mirroring how
/// `LoopSpec::active` treats a degenerate region. A block that wraps inside
/// an active loop cannot cross a point beyond the loop end — the playhead
/// never gets there.
#[inline]
pub fn crossing(base: u64, frames: u64, lp: &LoopSpec, point: u64) -> Option<u64> {
    if point == 0 || frames == 0 || base >= point {
        return None;
    }
    let reached = advance(base, frames, lp);
    // An active loop that wrapped this block turned the playhead back before
    // it could reach anything past `lp.end`.
    if lp.active() && base < lp.end && reached < base {
        return None;
    }
    (reached >= point).then_some(point)
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

    // -- crossing (timeline boundary detection) --------------------------

    #[test]
    fn crossing_fires_once_on_the_block_that_reaches_the_point() {
        // 900 + 128 = 1028 >= 1000
        assert_eq!(crossing(900, 128, &LoopSpec::OFF, 1000), Some(1000));
        // the block before it does not
        assert_eq!(crossing(700, 128, &LoopSpec::OFF, 1000), None);
        // and no block after it can: `base >= point` is already past
        assert_eq!(crossing(1028, 128, &LoopSpec::OFF, 1000), None);
        assert_eq!(crossing(1000, 128, &LoopSpec::OFF, 1000), None);
    }

    #[test]
    fn crossing_lands_exactly_on_the_point_not_the_block_edge() {
        // The playhead must park ON the boundary, whatever the buffer size.
        for frames in [1u64, 64, 128, 512, 4096] {
            assert_eq!(crossing(999, frames, &LoopSpec::OFF, 1000), Some(1000));
        }
    }

    #[test]
    fn point_zero_means_no_point() {
        // An empty project has no end; playing it must not fire at sample 0.
        assert_eq!(crossing(0, 128, &LoopSpec::OFF, 0), None);
        assert_eq!(crossing(5000, 128, &LoopSpec::OFF, 0), None);
    }

    #[test]
    fn empty_block_crosses_nothing() {
        assert_eq!(crossing(900, 0, &LoopSpec::OFF, 1000), None);
    }

    #[test]
    fn an_active_loop_never_reaches_a_point_past_its_end() {
        // LP wraps 100..200; the song end at 1000 is unreachable while looping.
        assert_eq!(crossing(190, 20, &LP, 1000), None);
        assert_eq!(crossing(150, 500, &LP, 1000), None);
    }

    #[test]
    fn a_point_inside_the_loop_still_fires_before_the_wrap() {
        // Crossing 150 on the way to the wrap at 200 is a real crossing.
        assert_eq!(crossing(140, 20, &LP, 150), Some(150));
    }

    #[test]
    fn a_disabled_loop_does_not_block_the_crossing() {
        let off = LoopSpec { enabled: false, start: 100, end: 200 };
        assert_eq!(crossing(900, 128, &off, 1000), Some(1000));
    }

    #[test]
    fn crossing_past_a_free_running_loop_still_fires() {
        // Playhead already past the loop region: free-running, so a later
        // point is reachable.
        assert_eq!(crossing(900, 128, &LP, 1000), Some(1000));
    }
}
