//! Tempo map: the tick <-> sample bijection (debt D-02 paydown).
//!
//! The control plane precomputes a piecewise-linear table (one segment per
//! tempo event) so conversions are O(log n) lookups + one multiply. The RT
//! thread NEVER converts — it receives pre-converted sample positions /
//! pre-rendered event blocks from the control thread.

use super::types::{TempoEvent, DEFAULT_PPQ};

/// Piecewise tick<->sample mapping. Immutable once built; rebuild on any
/// tempo-map edit (tempo edits are structural, i.e. rebuild-and-swap tier,
/// not param tier).
#[derive(Debug, Clone)]
pub struct TempoMap {
    ppq: u32,
    /// Sorted by tick, first entry at tick 0. Invariant enforced by `new`.
    events: Vec<TempoEvent>,
    /// events[i] -> absolute sample position of events[i].tick (at `rate`).
    sample_at_event: Vec<f64>,
    rate: u32,
}

impl TempoMap {
    /// Build a map. Requirements: `events` non-empty, sorted by strictly
    /// increasing tick, first at tick 0, all bpm finite and > 0.
    pub fn new(ppq: u32, events: Vec<TempoEvent>, sample_rate: u32) -> Result<Self, String> {
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
            if !e.bpm.is_finite() || e.bpm <= 0.0 {
                return Err(format!("invalid bpm: {}", e.bpm));
            }
        }
        let mut sample_at_event = Vec::with_capacity(events.len());
        let mut acc = 0.0_f64;
        sample_at_event.push(0.0);
        for pair in events.windows(2) {
            let dticks = (pair[1].tick - pair[0].tick) as f64;
            acc += dticks * samples_per_tick(pair[0].bpm, ppq, sample_rate);
            sample_at_event.push(acc);
        }
        Ok(Self { ppq, events, sample_at_event, rate: sample_rate })
    }

    /// Trivial one-entry map — the exact semantics of a v1 project
    /// (single global `tempoBpm`). v1 -> v2 migration constructs this.
    pub fn from_v1(tempo_bpm: f64, sample_rate: u32) -> Result<Self, String> {
        Self::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: tempo_bpm }], sample_rate)
    }

    pub fn ppq(&self) -> u32 {
        self.ppq
    }

    pub fn events(&self) -> &[TempoEvent] {
        &self.events
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    /// Absolute tick -> absolute sample position (rounded to nearest).
    pub fn tick_to_samples(&self, tick: u64) -> u64 {
        let i = match self.events.binary_search_by(|e| e.tick.cmp(&tick)) {
            Ok(i) => i,
            Err(i) => i - 1, // safe: events[0].tick == 0
        };
        let e = self.events[i];
        let base = self.sample_at_event[i];
        let s = base + (tick - e.tick) as f64 * samples_per_tick(e.bpm, self.ppq, self.rate);
        s.round() as u64
    }

    /// Absolute sample position -> absolute tick (rounded to nearest).
    pub fn samples_to_tick(&self, samples: u64) -> u64 {
        let s = samples as f64;
        // Find the segment whose start sample is <= s.
        let i = match self
            .sample_at_event
            .binary_search_by(|b| b.partial_cmp(&s).unwrap_or(std::cmp::Ordering::Less))
        {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let e = self.events[i];
        let dt = (s - self.sample_at_event[i]) / samples_per_tick(e.bpm, self.ppq, self.rate);
        e.tick + dt.round() as u64
    }
}

#[inline]
fn samples_per_tick(bpm: f64, ppq: u32, rate: u32) -> f64 {
    // seconds per quarter = 60/bpm; per tick = 60/(bpm*ppq); samples = *rate
    60.0 * rate as f64 / (bpm * ppq as f64)
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
}
