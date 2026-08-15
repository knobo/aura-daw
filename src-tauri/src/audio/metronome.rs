//! Click track: a compiled beat schedule (control thread) and an RT-safe
//! mixer that stamps a short decaying sine on each hit.
//!
//! The RT thread never does tempo math (D-02 / `tempo.rs`): it receives
//! absolute sample positions, the same contract as live-node note events.

use crate::midi::tempo::{MeterMap, TempoMap};
use crate::midi::types::DEFAULT_PPQ;
use crate::time::Ticks;

/// One compiled click. `accent` is the downbeat of a bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Click {
    pub sample: u64,
    pub accent: bool,
}

/// Walk every beat in `[0, end_samples]` (inclusive of a click that lands
/// exactly on the end) and emit a click. Empty / degenerate maps yield no
/// clicks rather than a panic — a missing tempo map must not take the
/// engine down.
pub fn schedule(tempo: &TempoMap, meter: &MeterMap, end_samples: u64) -> Vec<Click> {
    if end_samples == 0 {
        return Vec::new();
    }
    let ppq = tempo.ppq();
    let end_tick = tempo.samples_to_tick(end_samples);
    let mut clicks = Vec::new();
    let mut tick = 0u64;
    // Hard cap: a 1-tick beat at a huge end would be millions of clicks.
    // 32 clicks/sec for an hour of audio is already generous.
    let cap = 32 * 60 * 60;
    while tick <= end_tick && clicks.len() < cap {
        let beat = meter.beat_at(Ticks(tick), ppq);
        let accent = beat < 1e-6;
        clicks.push(Click { sample: tempo.tick_to_samples(tick), accent });
        let beat_ticks = beat_ticks_at(meter, tick, ppq);
        let next = tick.saturating_add(beat_ticks);
        if next <= tick {
            break;
        }
        tick = next;
    }
    clicks
}

/// How many engine samples `bars` of the meter at `at_sample` occupy.
pub fn count_in_samples(tempo: &TempoMap, meter: &MeterMap, at_sample: u64, bars: u8) -> u64 {
    if bars == 0 {
        return 0;
    }
    let ppq = tempo.ppq();
    let at_tick = tempo.samples_to_tick(at_sample);
    let bar = bar_ticks_at(meter, at_tick, ppq);
    let later = at_tick.saturating_add(bar.saturating_mul(bars as u64));
    tempo.tick_to_samples(later).saturating_sub(tempo.tick_to_samples(at_tick))
}

/// Samples in one beat of the meter at `at_sample` — the count-in click
/// period, so a 1-bar 4/4 count-in is four evenly spaced clicks.
pub fn beat_samples_at(tempo: &TempoMap, meter: &MeterMap, at_sample: u64) -> u64 {
    let ppq = tempo.ppq();
    let at_tick = tempo.samples_to_tick(at_sample);
    let beat = beat_ticks_at(meter, at_tick, ppq);
    tempo.tick_to_samples(at_tick.saturating_add(beat)).saturating_sub(tempo.tick_to_samples(at_tick))
}

fn beat_ticks_at(meter: &MeterMap, tick: u64, ppq: u32) -> u64 {
    // beat_at uses den: one beat = ppq * 4 / den ticks.
    // Reconstruct from the same formula without exposing segment_at.
    let e = meter.events().iter().rev().find(|e| e.tick <= tick).unwrap_or(&meter.events()[0]);
    (ppq as u64 * 4 / e.den as u64).max(1)
}

fn bar_ticks_at(meter: &MeterMap, tick: u64, ppq: u32) -> u64 {
    let e = meter.events().iter().rev().find(|e| e.tick <= tick).unwrap_or(&meter.events()[0]);
    (e.num as u64 * ppq as u64 * 4 / e.den as u64).max(1)
}

/// Mix compiled song clicks that fall inside this block into `out`.
/// RT-safe: binary search + a short fixed-length sine, no allocation.
pub fn mix_clicks(
    out: &mut [f32],
    channels: usize,
    base_pos: u64,
    clicks: &[Click],
    gain: f32,
    sample_rate: u32,
) {
    if clicks.is_empty() || gain <= 0.0 {
        return;
    }
    let ch = channels.max(1);
    let frames = out.len() / ch;
    if frames == 0 {
        return;
    }
    let end = base_pos.saturating_add(frames as u64);
    let start_i = clicks.partition_point(|c| c.sample.saturating_add(click_len(sample_rate)) <= base_pos);
    for click in clicks[start_i..].iter() {
        if click.sample >= end {
            break;
        }
        stamp_click(out, ch, base_pos, frames as u64, click.sample, click.accent, gain, sample_rate);
    }
}

/// Mix count-in clicks from an elapsed-sample clock (playhead frozen).
/// Beat 0 of each bar is accented; `beats_per_bar` is the current meter.
pub fn mix_count_in(
    out: &mut [f32],
    channels: usize,
    elapsed: u64,
    beat_samples: u64,
    beats_per_bar: u8,
    gain: f32,
    sample_rate: u32,
) {
    if beat_samples == 0 || gain <= 0.0 {
        return;
    }
    let ch = channels.max(1);
    let frames = out.len() / ch;
    if frames == 0 {
        return;
    }
    let first = elapsed / beat_samples;
    let last = (elapsed + frames as u64) / beat_samples;
    let bar = beats_per_bar.max(1) as u64;
    for n in first..=last {
        let at = n.saturating_mul(beat_samples);
        if at < elapsed || at >= elapsed + frames as u64 {
            continue;
        }
        let accent = n % bar == 0;
        stamp_click(out, ch, elapsed, frames as u64, at, accent, gain, sample_rate);
    }
}

fn click_len(rate: u32) -> u64 {
    (rate as u64 / 80).max(32) // ~12.5 ms
}

fn stamp_click(
    out: &mut [f32],
    ch: usize,
    base: u64,
    frames: u64,
    at: u64,
    accent: bool,
    gain: f32,
    rate: u32,
) {
    let len = click_len(rate);
    let freq = if accent { 2000.0f32 } else { 1000.0 };
    let amp = if accent { gain } else { gain * 0.65 };
    let decay = (rate as f32) * 0.006;
    let two_pi = std::f32::consts::TAU;
    for k in 0..len {
        let abs = at + k;
        if abs < base {
            continue;
        }
        let i = abs - base;
        if i >= frames {
            break;
        }
        let env = (-(k as f32) / decay).exp();
        let s = (two_pi * freq * k as f32 / rate as f32).sin() * env * amp;
        let o = i as usize * ch;
        out[o] += s;
        if ch > 1 {
            out[o + 1] += s;
        }
    }
}

/// Default click schedule for tests / empty projects: 4/4 at `bpm`.
pub fn schedule_constant(bpm: f64, sample_rate: u32, end_samples: u64) -> Vec<Click> {
    let tempo = TempoMap::from_v1(bpm, sample_rate).expect("valid bpm");
    let meter = MeterMap::default_map();
    let _ = DEFAULT_PPQ;
    schedule(&tempo, &meter, end_samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::types::{MeterEvent, TempoEvent};

    #[test]
    fn four_four_at_120_places_a_click_every_quarter_and_accents_the_bar() {
        // 120 bpm, 48 kHz: one quarter = 24_000 samples. One bar = 96_000.
        let clicks = schedule_constant(120.0, 48_000, 96_000);
        assert_eq!(clicks.len(), 5, "beats 0,1,2,3 and the end downbeat: {clicks:?}");
        assert!(clicks[0].accent);
        assert!(!clicks[1].accent && !clicks[2].accent && !clicks[3].accent);
        assert!(clicks[4].accent);
        assert_eq!(clicks[1].sample, 24_000);
        assert_eq!(clicks[4].sample, 96_000);
    }

    #[test]
    fn three_four_accents_every_third_beat() {
        let tempo = TempoMap::new(960, vec![TempoEvent { tick: 0, bpm: 120.0 }], 48_000).unwrap();
        let meter = MeterMap::new(vec![MeterEvent { tick: 0, num: 3, den: 4 }]).unwrap();
        let clicks = schedule(&tempo, &meter, 72_000); // 3 beats
        assert_eq!(clicks.iter().filter(|c| c.accent).count(), 2, "{clicks:?}");
        assert_eq!(clicks.len(), 4);
    }

    #[test]
    fn count_in_two_bars_at_120_is_eight_beats() {
        let tempo = TempoMap::from_v1(120.0, 48_000).unwrap();
        let meter = MeterMap::default_map();
        assert_eq!(count_in_samples(&tempo, &meter, 0, 2), 192_000);
        assert_eq!(beat_samples_at(&tempo, &meter, 0), 24_000);
        assert_eq!(count_in_samples(&tempo, &meter, 0, 0), 0);
    }

    #[test]
    fn mix_clicks_stamps_a_nonzero_transient_on_the_downbeat() {
        let clicks = [Click { sample: 0, accent: true }];
        let mut out = vec![0.0f32; 256 * 2];
        mix_clicks(&mut out, 2, 0, &clicks, 0.5, 48_000);
        assert!(out.iter().any(|s| s.abs() > 0.1), "accent click must be audible");
        assert_eq!(out[0], out[1], "click is centered");
    }

    #[test]
    fn mix_count_in_accents_bar_starts_only() {
        let mut out = vec![0.0f32; 48_000 * 2]; // one second, one beat at 120
        mix_count_in(&mut out, 2, 0, 24_000, 4, 0.4, 48_000);
        let peak0 = out[..64].iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        let beat1 = 24_000 * 2;
        let peak1 = out[beat1..beat1 + 64].iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!(peak0 > 0.1, "beat 0 is an accent, peak={peak0}");
        assert!(peak1 > 0.05, "beat 1 clicks, peak={peak1}");
        assert!(peak0 > peak1, "downbeat louder than the off-beat");
    }

    #[test]
    fn empty_end_or_zero_gain_is_a_no_op() {
        assert!(schedule_constant(120.0, 48_000, 0).is_empty());
        let mut out = vec![0.0f32; 64];
        mix_clicks(&mut out, 1, 0, &[Click { sample: 0, accent: true }], 0.0, 48_000);
        assert!(out.iter().all(|s| *s == 0.0));
    }
}
