//! Precomputed constant-tempo segments (round-2 §3.4): cumulative
//! superticks/samples/beat/bar per segment, ramps subdivided under a
//! versioned, property-tested error bound.

use super::tempo::{MeterMap, TempoMap};
use super::types::TempoPeriodEvent;
use crate::time::{Ticks, SUPERTICKS_PER_SECOND};

/// A versioned constant, next to the schema version per round-2 §3.4
/// ("the subdivision rule is format semantics and is versioned").
pub const RULE_VERSION: u32 = 1;

// REVIEW: round-2 doesn't name a subdivision cap; this is a doubling-
// runaway guard for pathological ramps (extreme bpm swings over very few
// ticks). No fixture in this plan's test corpus gets anywhere near it —
// flagged in case a future round's ramp UI needs a tighter or looser cap.
const MAX_SUBDIVISIONS: usize = 4096;
const ERROR_BOUND_SAMPLES: f64 = 64.0;
const PROBES_PER_SUBSEGMENT: u64 = 32;

/// One constant-tempo segment. A genuinely constant-tempo `TempoPeriodEvent`
/// becomes exactly one `Section`; a ramp event is subdivided into several.
#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub start_tick: u64,
    pub start_sample: u64,
    pub start_beat: f64,
    pub start_bar: u32,
    /// Constant period across this section (exact for a genuinely
    /// constant-tempo event; the midpoint-period approximation of a ramp
    /// sub-segment otherwise — bounded by `ERROR_BOUND_SAMPLES`).
    pub period: u64,
}

#[derive(Debug, Clone)]
pub struct SectionTable {
    ppq: u32,
    rate: u32,
    sections: Vec<Section>,
}

impl SectionTable {
    pub fn build(tempo: &TempoMap, meter: &MeterMap) -> Self {
        let mut sections = Vec::new();
        let events = tempo.period_events();
        for (i, e) in events.iter().enumerate() {
            let next_tick = events.get(i + 1).map(|n| n.tick);
            match next_tick {
                Some(nt) if e.period_start != e.period_end => {
                    subdivide_ramp(tempo, meter, *e, nt, &mut sections);
                }
                _ => sections.push(section_at(tempo, meter, e.tick, e.period_start)),
            }
        }
        Self { ppq: tempo.ppq(), rate: tempo.sample_rate(), sections }
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// Suffix-only rebuild (round-2 §3.4): keep every section strictly
    /// before `from_tick`, recompute the rest.
    pub fn rebuild_from(&mut self, tempo: &TempoMap, meter: &MeterMap, from_tick: u64) {
        let fresh = Self::build(tempo, meter);
        self.sections.retain(|s| s.start_tick < from_tick);
        self.sections.extend(fresh.sections.into_iter().filter(|s| s.start_tick >= from_tick));
        self.ppq = fresh.ppq;
        self.rate = fresh.rate;
    }

    /// Piecewise-constant sample lookup against the table — what a
    /// renderer consumes, NOT the exact bijection (bounded by
    /// `ERROR_BOUND_SAMPLES` vs `TempoMap::tick_to_samples_v3`).
    pub fn sample_at_tick(&self, tick: u64) -> u64 {
        if self.sections.is_empty() {
            return 0;
        }
        let i = match self.sections.binary_search_by(|s| s.start_tick.cmp(&tick)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let s = &self.sections[i];
        let spt = samples_per_tick_at_period(s.period, self.ppq, self.rate);
        (s.start_sample as f64 + (tick - s.start_tick) as f64 * spt).round() as u64
    }
}

fn section_at(tempo: &TempoMap, meter: &MeterMap, tick: u64, period: u64) -> Section {
    let ppq = tempo.ppq();
    Section {
        start_tick: tick,
        start_sample: tempo.tick_to_samples_v3(Ticks(tick)).0,
        start_beat: meter.beat_at(Ticks(tick), ppq),
        start_bar: meter.bar_at(Ticks(tick), ppq),
        period,
    }
}

/// Period at `tick`, linearly interpolated across `[event.tick, next_tick)`
/// (matches `TempoMap`'s own ramp law — round-2 §3.3).
fn period_at_linear(event: TempoPeriodEvent, next_tick: u64, tick: u64) -> u64 {
    let span = (next_tick - event.tick) as f64;
    let frac = (tick - event.tick) as f64 / span;
    (event.period_start as f64 + (event.period_end as f64 - event.period_start as f64) * frac).round() as u64
}

#[inline]
fn samples_per_tick_at_period(period: u64, ppq: u32, rate: u32) -> f64 {
    period as f64 / SUPERTICKS_PER_SECOND as f64 * rate as f64 / ppq as f64
}

/// Subdivide `[event.tick, next_tick)` into equal-tick sub-segments,
/// doubling the count from 1 until every probe point's deviation from the
/// exact bijection (`TempoMap::tick_to_samples_v3`, which is exact for a
/// linear-in-period ramp — Task 3) is `< ERROR_BOUND_SAMPLES`.
fn subdivide_ramp(
    tempo: &TempoMap,
    meter: &MeterMap,
    event: TempoPeriodEvent,
    next_tick: u64,
    out: &mut Vec<Section>,
) {
    let mut n = 1usize;
    while n < MAX_SUBDIVISIONS && !within_bound(tempo, event, next_tick, n) {
        n *= 2;
    }
    let span = next_tick - event.tick;
    for k in 0..n as u64 {
        let sub_tick = event.tick + span * k / n as u64;
        let sub_next = event.tick + span * (k + 1) / n as u64;
        let midpoint = (sub_tick + sub_next) / 2;
        let period_mid = period_at_linear(event, next_tick, midpoint);
        out.push(section_at(tempo, meter, sub_tick, period_mid));
    }
}

/// Whether subdividing `[event.tick, next_tick)` into `n` equal-tick
/// sub-segments (each approximated by its midpoint period) stays within
/// `ERROR_BOUND_SAMPLES` of the exact bijection at every one of
/// `PROBES_PER_SUBSEGMENT` evenly-spaced probe points per sub-segment.
fn within_bound(tempo: &TempoMap, event: TempoPeriodEvent, next_tick: u64, n: usize) -> bool {
    let span = next_tick - event.tick;
    let ppq = tempo.ppq();
    let rate = tempo.sample_rate();
    for k in 0..n as u64 {
        let sub_tick = event.tick + span * k / n as u64;
        let sub_next = event.tick + span * (k + 1) / n as u64;
        let midpoint = (sub_tick + sub_next) / 2;
        let period_mid = period_at_linear(event, next_tick, midpoint);
        let approx_start = tempo.tick_to_samples_v3(Ticks(sub_tick)).0 as f64;
        let spt = samples_per_tick_at_period(period_mid, ppq, rate);
        for p in 0..PROBES_PER_SUBSEGMENT {
            let t = sub_tick + (sub_next - sub_tick) * p / PROBES_PER_SUBSEGMENT;
            let exact = tempo.tick_to_samples_v3(Ticks(t)).0 as f64;
            let approx = approx_start + (t - sub_tick) as f64 * spt;
            if (exact - approx).abs() >= ERROR_BOUND_SAMPLES {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::tempo::{MeterMap, TempoMap};
    use crate::midi::types::TempoPeriodEvent;

    #[test]
    fn constant_tempo_produces_exactly_one_section() {
        let period = crate::time::period_from_bpm(120.0);
        let tempo = TempoMap::from_periods(
            960,
            vec![TempoPeriodEvent { tick: 0, period_start: period, period_end: period }],
            48_000,
        )
        .unwrap();
        let meter = MeterMap::default_map();
        let table = SectionTable::build(&tempo, &meter);
        assert_eq!(table.sections().len(), 1);
        assert_eq!(table.sections()[0].start_tick, 0);
        assert_eq!(table.sections()[0].start_sample, 0);
        assert_eq!(table.sections()[0].start_bar, 0);
    }

    #[test]
    fn ramp_subdivision_stays_within_the_64_sample_bound() {
        // A steep ramp: 60bpm -> 240bpm across one bar, 48kHz — exercises
        // the worst-case curvature this project supports.
        let p_start = crate::time::period_from_bpm(60.0);
        let p_end = crate::time::period_from_bpm(240.0);
        let tempo = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: p_start, period_end: p_end },
                TempoPeriodEvent { tick: 3840, period_start: p_end, period_end: p_end },
            ],
            48_000,
        )
        .unwrap();
        let meter = MeterMap::default_map();
        let table = SectionTable::build(&tempo, &meter);
        assert!(table.sections().len() > 1, "a steep ramp must subdivide");
        // Probe 200 points across the ramp span; the piecewise-constant
        // reconstruction must stay within 64 samples of the exact bijection
        // everywhere, not just at sub-segment boundaries.
        for k in 0..=200u64 {
            let tick = k * 3840 / 200;
            let exact = tempo.tick_to_samples_v3(Ticks(tick)).0 as i64;
            let approx = table.sample_at_tick(tick) as i64;
            assert!((exact - approx).abs() < 64, "tick {tick}: exact={exact} approx={approx}");
        }
    }

    #[test]
    fn a_gentle_ramp_needs_few_or_no_subdivisions() {
        // A one-bpm drift across a single bar (3840 ticks) is nearly
        // constant-tempo over that short a span; the subdivision rule must
        // not over-split it. (Contrast with the steep-ramp test above,
        // which covers the same tick span but a 4x bpm swing and DOES
        // subdivide — the accumulated quadratic error of a ramp scales
        // with the SQUARE of the tick span, so span matters as much as
        // curvature; a drift this gentle over a short span needs at most
        // one or two doublings.)
        let p_start = crate::time::period_from_bpm(120.0);
        let p_end = crate::time::period_from_bpm(121.0);
        let tempo = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: p_start, period_end: p_end },
                TempoPeriodEvent { tick: 3840, period_start: p_end, period_end: p_end },
            ],
            48_000,
        )
        .unwrap();
        let meter = MeterMap::default_map();
        let table = SectionTable::build(&tempo, &meter);
        assert!(
            table.sections().len() <= 4,
            "gentle ramp should not over-subdivide: got {}",
            table.sections().len()
        );
    }

    #[test]
    fn suffix_rebuild_only_touches_segments_at_or_after_the_edit_tick() {
        let period = crate::time::period_from_bpm(120.0);
        let tempo = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: period, period_end: period },
                TempoPeriodEvent { tick: 7680, period_start: period, period_end: period },
            ],
            48_000,
        )
        .unwrap();
        let meter = MeterMap::default_map();
        let mut table = SectionTable::build(&tempo, &meter);
        let prefix_before = table.sections()[0].clone();
        table.rebuild_from(&tempo, &meter, 7680);
        assert_eq!(table.sections()[0], prefix_before, "segment before the edit tick is untouched");
    }
}
