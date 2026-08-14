//! Tempo map: the tick <-> sample bijection (debt D-02 paydown; round-2
//! §3.3 moved storage to integer superticks-per-quarter periods).
//!
//! The control plane precomputes a piecewise table (one segment per tempo
//! event) so conversions are O(log n) lookups + a closed-form evaluation.
//! The RT thread NEVER converts — it receives pre-converted sample
//! positions / pre-rendered event blocks from the control thread.

use super::types::{MeterEvent, TempoEvent, TempoPeriodEvent, DEFAULT_PPQ};
use crate::time::{Samples, Ticks, SUPERTICKS_PER_SECOND};

/// Piecewise tick<->sample mapping over integer-period tempo events
/// (round-2 §3.3). Immutable once built; rebuild on any tempo-map edit
/// (tempo edits are structural, i.e. rebuild-and-swap tier, not param
/// tier).
#[derive(Debug, Clone)]
pub struct TempoMap {
    ppq: u32,
    /// Sorted by tick, first entry at tick 0. Invariant enforced by
    /// `from_periods`.
    events: Vec<TempoPeriodEvent>,
    /// events[i] -> absolute sample position of events[i].tick (at `rate`).
    sample_at_event: Vec<f64>,
    rate: u32,
}

impl TempoMap {
    /// Build a map from v3 integer-period events. Requirements: `events`
    /// non-empty, sorted by strictly increasing tick, first at tick 0, all
    /// periods > 0.
    pub fn from_periods(ppq: u32, events: Vec<TempoPeriodEvent>, sample_rate: u32) -> Result<Self, String> {
        if ppq == 0 {
            return Err("ppq must be > 0".into());
        }
        if sample_rate == 0 {
            return Err("sampleRate must be > 0".into());
        }
        if events.is_empty() {
            return Err("tempo map must have at least one entry".into());
        }
        if events[0].tick != 0 {
            return Err("first tempo event must be at tick 0".into());
        }
        for pair in events.windows(2) {
            if pair[1].tick <= pair[0].tick {
                return Err("tempo events must have strictly increasing ticks".into());
            }
        }
        for e in &events {
            if e.period_start == 0 || e.period_end == 0 {
                return Err("tempo period must be > 0".into());
            }
        }
        let mut sample_at_event = Vec::with_capacity(events.len());
        sample_at_event.push(0.0_f64);
        for (i, pair) in events.windows(2).enumerate() {
            let span = segment_sample_span(pair[0], pair[1].tick, ppq, sample_rate);
            sample_at_event.push(sample_at_event[i] + span);
        }
        Ok(Self { ppq, events, sample_at_event, rate: sample_rate })
    }

    /// v1/v2 constructor kept as a thin wrapper (frozen internal API):
    /// quantize each bpm ONCE (`crate::time::period_from_bpm`) into a
    /// constant-period event.
    pub fn new(ppq: u32, events: Vec<TempoEvent>, sample_rate: u32) -> Result<Self, String> {
        let period_events = events
            .iter()
            .map(|e| {
                if !e.bpm.is_finite() || e.bpm <= 0.0 {
                    return Err(format!("invalid bpm: {}", e.bpm));
                }
                let p = crate::time::period_from_bpm(e.bpm);
                Ok(TempoPeriodEvent { tick: e.tick, period_start: p, period_end: p })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Self::from_periods(ppq, period_events, sample_rate)
    }

    /// Trivial one-entry map — the exact semantics of a v1 project
    /// (single global `tempoBpm`). v1 -> v2/v3 migration constructs this.
    pub fn from_v1(tempo_bpm: f64, sample_rate: u32) -> Result<Self, String> {
        Self::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: tempo_bpm }], sample_rate)
    }

    pub fn ppq(&self) -> u32 {
        self.ppq
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// The v3 integer-period events, in order.
    pub fn period_events(&self) -> &[TempoPeriodEvent] {
        &self.events
    }

    /// Old bpm-projected view, for wire compatibility
    /// (`TempoMapState.events`) — derived from period on every call, never
    /// the source of truth.
    pub fn events(&self) -> Vec<TempoEvent> {
        self.events
            .iter()
            .map(|e| TempoEvent { tick: e.tick, bpm: crate::time::bpm_from_period(e.period_start) })
            .collect()
    }

    pub fn tick_to_samples(&self, tick: u64) -> u64 {
        self.tick_to_samples_v3(Ticks(tick)).0
    }

    pub fn samples_to_tick(&self, samples: u64) -> u64 {
        self.samples_to_tick_v3(Samples(samples)).0
    }

    /// Absolute tick -> absolute sample position (rounded to nearest).
    /// Exact closed form: samples-per-tick is linear in tick within a
    /// segment (period is linear in tick per round-2 §3.3's "linear in
    /// period" ramp law), so cumulative samples is the exact trapezoid.
    pub fn tick_to_samples_v3(&self, tick: Ticks) -> Samples {
        let tick = tick.0;
        let i = self.segment_index_for_tick(tick);
        let e = self.events[i];
        let base_samples = self.sample_at_event[i];
        let dticks = tick - e.tick;
        let period_at = self.period_at(i, tick);
        let spt0 = samples_per_tick_at_period(e.period_start, self.ppq, self.rate);
        let spt_at = samples_per_tick_at_period(period_at, self.ppq, self.rate);
        let samples_in_segment = dticks as f64 * (spt0 + spt_at) / 2.0;
        Samples((base_samples + samples_in_segment).round() as u64)
    }

    /// Absolute sample position -> absolute tick (rounded to nearest).
    /// Constant segments invert algebraically; a ramp segment inverts the
    /// exact quadratic (trapezoid) via the quadratic formula.
    pub fn samples_to_tick_v3(&self, samples: Samples) -> Ticks {
        let s = samples.0 as f64;
        let i = match self
            .sample_at_event
            .binary_search_by(|b| b.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Less))
        {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let e = self.events[i];
        let base_samples = self.sample_at_event[i];
        let target = s - base_samples;
        if target <= 0.0 {
            return Ticks(e.tick);
        }
        let next_tick = self.events.get(i + 1).map(|n| n.tick);
        if e.period_start == e.period_end || next_tick.is_none() {
            // Constant segment: exact algebraic inversion.
            let spt = samples_per_tick_at_period(e.period_start, self.ppq, self.rate);
            return Ticks(e.tick + (target / spt).round() as u64);
        }
        // Ramp segment: invert the quadratic
        // samples(d) = d*(spt0 + spt(d))/2, spt linear in d, so this is
        // a*d^2 + b*d - target = 0 with a = slope/2, b = spt0.
        let span = next_tick.unwrap() - e.tick;
        let spt0 = samples_per_tick_at_period(e.period_start, self.ppq, self.rate);
        let spt1 = samples_per_tick_at_period(e.period_end, self.ppq, self.rate);
        let slope = (spt1 - spt0) / span as f64;
        let a = slope / 2.0;
        let b = spt0;
        let d = if a.abs() < 1e-12 {
            target / b
        } else {
            let disc = (b * b + 4.0 * a * target).max(0.0);
            (-b + disc.sqrt()) / (2.0 * a)
        };
        Ticks(e.tick + d.round().max(0.0) as u64)
    }

    fn segment_index_for_tick(&self, tick: u64) -> usize {
        match self.events.binary_search_by(|e| e.tick.cmp(&tick)) {
            Ok(i) => i,
            Err(i) => i - 1, // safe: events[0].tick == 0
        }
    }

    /// Period at `tick`, linearly interpolated within its segment
    /// (constant if the segment has no ramp, or `tick` is at/after the
    /// last event with nothing to ramp toward).
    fn period_at(&self, segment_index: usize, tick: u64) -> u64 {
        let e = self.events[segment_index];
        let next_tick = self.events.get(segment_index + 1).map(|n| n.tick);
        match next_tick {
            Some(nt) if nt > e.tick => {
                let span = nt - e.tick;
                let frac = (tick - e.tick) as f64 / span as f64;
                (e.period_start as f64 + (e.period_end as f64 - e.period_start as f64) * frac).round() as u64
            }
            _ => e.period_start,
        }
    }
}

/// Exact sample span of one segment `[event.tick, next_tick)`. Linear-in-
/// tick period means samples-per-tick is linear in tick, so the trapezoid
/// rule (average of the endpoint rates, times the tick span) is EXACT, not
/// an approximation.
fn segment_sample_span(event: TempoPeriodEvent, next_tick: u64, ppq: u32, rate: u32) -> f64 {
    let dticks = (next_tick - event.tick) as f64;
    let spt0 = samples_per_tick_at_period(event.period_start, ppq, rate);
    let spt1 = samples_per_tick_at_period(event.period_end, ppq, rate);
    dticks * (spt0 + spt1) / 2.0
}

#[inline]
fn samples_per_tick_at_period(period: u64, ppq: u32, rate: u32) -> f64 {
    // seconds per quarter = period / SUPERTICKS_PER_SECOND;
    // per tick = that / ppq; samples = * rate.
    period as f64 / SUPERTICKS_PER_SECOND as f64 * rate as f64 / ppq as f64
}

/// Persisted time-signature map (round-2 §3.3/O-10): bar/beat lookups.
/// Separate from `TempoMap` — bars/beats don't need tempo, and a project
/// can hold BOTH maps independently, changing one without touching the
/// other.
#[derive(Debug, Clone)]
pub struct MeterMap {
    /// Sorted by tick, first entry at tick 0. Invariant enforced by `new`.
    events: Vec<MeterEvent>,
}

impl MeterMap {
    pub fn new(events: Vec<MeterEvent>) -> Result<Self, String> {
        if events.is_empty() {
            return Err("meter map must have at least one entry".into());
        }
        if events[0].tick != 0 {
            return Err("first meter event must be at tick 0".into());
        }
        for pair in events.windows(2) {
            if pair[1].tick <= pair[0].tick {
                return Err("meter events must have strictly increasing ticks".into());
            }
        }
        for e in &events {
            if e.num == 0 || e.den == 0 {
                return Err("meter numerator/denominator must be > 0".into());
            }
        }
        Ok(Self { events })
    }

    /// The `[{0,4,4}]` default (round-2 §3.3).
    pub fn default_map() -> Self {
        Self { events: vec![MeterEvent { tick: 0, num: 4, den: 4 }] }
    }

    pub fn events(&self) -> &[MeterEvent] {
        &self.events
    }

    fn segment_at(&self, tick: u64) -> &MeterEvent {
        let i = match self.events.binary_search_by(|e| e.tick.cmp(&tick)) {
            Ok(i) => i,
            Err(i) => i - 1, // safe: events[0].tick == 0
        };
        &self.events[i]
    }

    /// Ticks spanned by one bar of a `num/den` signature at `ppq`: `num`
    /// beats of `4/den` whole notes each.
    fn bar_ticks(num: u8, den: u8, ppq: u32) -> u64 {
        (num as u64 * ppq as u64 * 4 / den as u64).max(1)
    }

    /// 0-indexed bar number at `tick`, given the project `ppq`. A single
    /// forward pass over the segments that end at-or-before `tick`
    /// accumulates whole bars from each, then adds the bars elapsed inside
    /// the segment containing `tick` — O(n) in the number of signature
    /// changes, no repeated position lookups.
    pub fn bar_at(&self, tick: Ticks, ppq: u32) -> u32 {
        let tick = tick.0;
        let mut bars = 0u32;
        for w in self.events.windows(2) {
            if tick < w[1].tick {
                break;
            }
            bars += ((w[1].tick - w[0].tick) / Self::bar_ticks(w[0].num, w[0].den, ppq)) as u32;
        }
        let e = self.segment_at(tick);
        bars + ((tick - e.tick) / Self::bar_ticks(e.num, e.den, ppq)) as u32
    }

    /// Fractional beat position within the current bar (0.0 = downbeat).
    pub fn beat_at(&self, tick: Ticks, ppq: u32) -> f64 {
        let tick = tick.0;
        let e = self.segment_at(tick);
        let beat_ticks = (ppq as u64 * 4 / e.den as u64).max(1);
        let bar_ticks = (e.num as u64 * beat_ticks).max(1);
        let into_bar = (tick - e.tick) % bar_ticks;
        into_bar as f64 / beat_ticks as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_equivalent_single_entry_map() {
        // 120 bpm @48k, 960 ppq: one quarter note = 0.5 s = 24000 samples.
        let m = TempoMap::from_v1(120.0, 48_000).unwrap();
        assert_eq!(m.tick_to_samples(0), 0);
        assert_eq!(m.tick_to_samples(960), 24_000);
        assert_eq!(m.tick_to_samples(4 * 960), 96_000);
        assert_eq!(m.samples_to_tick(24_000), 960);
    }

    #[test]
    fn multi_segment_conversion_and_roundtrip() {
        // 120 bpm for one bar (4*960 ticks = 96000 samples), then 60 bpm.
        let m = TempoMap::new(
            960,
            vec![
                TempoEvent { tick: 0, bpm: 120.0 },
                TempoEvent { tick: 3840, bpm: 60.0 },
            ],
            48_000,
        )
        .unwrap();
        // At 60 bpm a quarter is 48000 samples.
        assert_eq!(m.tick_to_samples(3840), 96_000);
        assert_eq!(m.tick_to_samples(3840 + 960), 96_000 + 48_000);
        for tick in [0u64, 1, 959, 3839, 3840, 3841, 10_000, 1_000_000] {
            let s = m.tick_to_samples(tick);
            assert_eq!(m.samples_to_tick(s), tick, "roundtrip at tick {tick}");
        }
    }

    #[test]
    fn rejects_malformed_maps() {
        assert!(TempoMap::new(960, vec![], 48_000).is_err());
        assert!(TempoMap::new(
            960,
            vec![TempoEvent { tick: 5, bpm: 120.0 }],
            48_000
        )
        .is_err());
        assert!(TempoMap::new(
            960,
            vec![
                TempoEvent { tick: 0, bpm: 120.0 },
                TempoEvent { tick: 0, bpm: 100.0 }
            ],
            48_000
        )
        .is_err());
        assert!(TempoMap::new(960, vec![TempoEvent { tick: 0, bpm: 0.0 }], 48_000).is_err());
    }

    #[test]
    fn v3_constant_tempo_matches_the_old_bpm_path_exactly() {
        // 120 bpm @48k, 960 ppq: quarter note = 24000 samples — same
        // numbers v1_equivalent_single_entry_map pins for the old API.
        let period = crate::time::period_from_bpm(120.0);
        let m = TempoMap::from_periods(
            960,
            vec![TempoPeriodEvent { tick: 0, period_start: period, period_end: period }],
            48_000,
        )
        .unwrap();
        assert_eq!(m.tick_to_samples_v3(Ticks(0)), Samples(0));
        assert_eq!(m.tick_to_samples_v3(Ticks(960)), Samples(24_000));
        assert_eq!(m.tick_to_samples_v3(Ticks(4 * 960)), Samples(96_000));
        assert_eq!(m.samples_to_tick_v3(Samples(24_000)), Ticks(960));
    }

    #[test]
    fn v3_linear_ramp_hits_the_exact_trapezoid_midpoint() {
        // A ramp from 120bpm to 60bpm across one bar (3840 ticks @960ppq).
        let p_start = crate::time::period_from_bpm(120.0);
        let p_end = crate::time::period_from_bpm(60.0);
        let m = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: p_start, period_end: p_end },
                TempoPeriodEvent { tick: 3840, period_start: p_end, period_end: p_end },
            ],
            48_000,
        )
        .unwrap();
        let spt = |period: u64| period as f64 / SUPERTICKS_PER_SECOND as f64 * 48_000.0 / 960.0;
        // tick 1920 is exactly halfway, so the period there is the
        // arithmetic mean of the endpoints (linear-in-tick ramp).
        let p_mid = p_start + (p_end - p_start) / 2;
        let expected_mid_samples = 1920.0 * (spt(p_start) + spt(p_mid)) / 2.0;
        let got = m.tick_to_samples_v3(Ticks(1920)).0 as f64;
        assert!((got - expected_mid_samples).abs() <= 1.0, "got {got}, expected ~{expected_mid_samples}");
        // Monotonic: end-of-ramp sample position must exceed the midpoint.
        assert!(m.tick_to_samples_v3(Ticks(3840)).0 > m.tick_to_samples_v3(Ticks(1920)).0);
    }

    #[test]
    fn v3_roundtrip_across_a_ramp_and_a_constant_segment() {
        let p120 = crate::time::period_from_bpm(120.0);
        let p90 = crate::time::period_from_bpm(90.0);
        let m = TempoMap::from_periods(
            960,
            vec![
                TempoPeriodEvent { tick: 0, period_start: p120, period_end: p90 },
                TempoPeriodEvent { tick: 3840, period_start: p90, period_end: p90 },
            ],
            48_000,
        )
        .unwrap();
        for tick in [0u64, 1, 500, 3839, 3840, 3841, 10_000] {
            let s = m.tick_to_samples_v3(Ticks(tick));
            let back = m.samples_to_tick_v3(s);
            // Round-trip within 1 tick — the reverse direction inverts
            // the ramp's exact quadratic, not a re-derivation from scratch.
            assert!((back.0 as i64 - tick as i64).abs() <= 1, "tick {tick} -> {s:?} -> {back:?}");
        }
    }

    #[test]
    fn old_bpm_api_is_a_thin_wrapper_and_still_matches_pinned_numbers() {
        let m = TempoMap::new(
            960,
            vec![
                TempoEvent { tick: 0, bpm: 120.0 },
                TempoEvent { tick: 3840, bpm: 60.0 },
            ],
            48_000,
        )
        .unwrap();
        assert_eq!(m.tick_to_samples(3840), 96_000);
        assert_eq!(m.tick_to_samples(3840 + 960), 96_000 + 48_000);
    }

    #[test]
    fn meter_map_default_is_four_four_at_tick_zero() {
        let m = MeterMap::default_map();
        assert_eq!(m.events()[0], MeterEvent { tick: 0, num: 4, den: 4 });
    }

    #[test]
    fn meter_map_computes_bar_and_beat_at_a_constant_four_four() {
        let m = MeterMap::new(vec![MeterEvent { tick: 0, num: 4, den: 4 }]).unwrap();
        let ppq = 960u32;
        // One bar = 4 beats = 4*960 = 3840 ticks at 4/4.
        assert_eq!(m.bar_at(Ticks(0), ppq), 0);
        assert_eq!(m.bar_at(Ticks(3839), ppq), 0);
        assert_eq!(m.bar_at(Ticks(3840), ppq), 1);
        assert_eq!(m.bar_at(Ticks(3840 * 3 + 100), ppq), 3);
        assert!((m.beat_at(Ticks(960), ppq) - 1.0).abs() < 1e-9);
        assert!((m.beat_at(Ticks(480), ppq) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn meter_map_handles_a_signature_change_mid_song() {
        // 4/4 for 2 bars (7680 ticks), then 3/4.
        let m = MeterMap::new(vec![
            MeterEvent { tick: 0, num: 4, den: 4 },
            MeterEvent { tick: 7680, num: 3, den: 4 },
        ])
        .unwrap();
        let ppq = 960u32;
        assert_eq!(m.bar_at(Ticks(7680), ppq), 2, "bar 2 starts exactly at the signature change");
        // One 3/4 bar = 3*960 = 2880 ticks.
        assert_eq!(m.bar_at(Ticks(7680 + 2880), ppq), 3);
    }

    #[test]
    fn meter_map_rejects_malformed_maps() {
        assert!(MeterMap::new(vec![]).is_err());
        assert!(MeterMap::new(vec![MeterEvent { tick: 5, num: 4, den: 4 }]).is_err());
        assert!(MeterMap::new(vec![MeterEvent { tick: 0, num: 0, den: 4 }]).is_err());
    }
}
