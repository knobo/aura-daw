//! MIDI output: transport sync (clock/Start/Stop/Continue/SPP) and note-out
//! to external gear. Runs entirely off `SharedRt` atomics + a document
//! snapshot on its own non-RT thread — see ruling 3: NOTHING here touches
//! `audio/engine.rs`.

use crate::midi::tempo::TempoMap;

/// One outbound MIDI message. POD so the clock and the note scheduler share
/// one output buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutMsg {
    pub bytes: [u8; 3],
    pub len: usize,
}

impl OutMsg {
    pub fn one(b: u8) -> Self {
        Self { bytes: [b, 0, 0], len: 1 }
    }

    pub fn three(a: u8, b: u8, c: u8) -> Self {
        Self { bytes: [a, b, c], len: 3 }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMsg {
    Clock,
    Start,
    Continue,
    Stop,
    SongPosition(u16),
}

impl ClockMsg {
    /// 0xF8 clock, 0xFA start, 0xFB continue, 0xFC stop,
    /// 0xF2 lsb msb song position (14-bit, 7 bits per byte).
    pub fn to_out(self) -> OutMsg {
        match self {
            ClockMsg::Clock => OutMsg::one(0xF8),
            ClockMsg::Start => OutMsg::one(0xFA),
            ClockMsg::Continue => OutMsg::one(0xFB),
            ClockMsg::Stop => OutMsg::one(0xFC),
            ClockMsg::SongPosition(spp) => {
                let lsb = (spp & 0x7F) as u8;
                let msb = ((spp >> 7) & 0x7F) as u8;
                OutMsg::three(0xF2, lsb, msb)
            }
        }
    }
}

/// MIDI clock is 24 pulses per quarter note, always — independent of ppq.
pub const PULSES_PER_QUARTER: u64 = 24;
/// Most clock pulses emitted in one tick; a bigger gap means the thread was
/// starved or the transport jumped, and is re-anchored instead of flooded.
pub const MAX_PULSE_BURST: u64 = 24;
/// Position deviation (in samples) that counts as a transport JUMP rather
/// than ordinary block quantization: 20 ms at the engine rate.
pub fn drift_tolerance(rate: u32) -> u64 {
    (rate as u64) / 50
}

pub fn pulse_of_tick(tick: u64, ppq: u32) -> u64 {
    (tick * PULSES_PER_QUARTER) / (ppq as u64)
}

/// SPP counts MIDI beats (sixteenth notes) = 6 clock pulses; 14-bit range.
pub fn song_position_of_pulse(pulse: u64) -> u16 {
    let spp = pulse / 6;
    spp.min(16_383) as u16
}

/// What the driver reads each tick.
pub struct ClockInput<'a> {
    pub now_micros: u64,
    pub playing: bool,
    pub position: u64,
    pub rate: u32,
    pub map: &'a TempoMap,
}

/// Anchor point: the (sample, wall-clock-micros) pair interpolation is
/// computed from between observed position updates.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    sample: u64,
    micros: u64,
}

#[derive(Default)]
pub struct ClockEngine {
    running: bool,
    anchor: Option<Anchor>,
    last_pulse: u64,
    pulses_sent: u64,
    resyncs: u64,
    estimated_sample: u64,
}

impl ClockEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append this tick's messages. Pure with respect to `now_micros` — the
    /// caller owns the clock, so tests drive it deterministically.
    pub fn step(&mut self, input: ClockInput<'_>, out: &mut Vec<ClockMsg>) {
        let ClockInput { now_micros, playing, position, rate, map } = input;

        // Clause 1: not playing.
        if !playing {
            if self.running {
                out.push(ClockMsg::Stop);
                self.anchor = None;
                self.running = false;
            }
            return;
        }

        // Clause 2: transport start.
        if !self.running {
            self.anchor = Some(Anchor { sample: position, micros: now_micros });
            self.estimated_sample = position;
            let pulse = pulse_of_tick(map.samples_to_tick(position), map.ppq());
            self.last_pulse = pulse;
            if pulse == 0 {
                out.push(ClockMsg::Start);
            } else {
                out.push(ClockMsg::SongPosition(song_position_of_pulse(pulse)));
                out.push(ClockMsg::Continue);
            }
            self.running = true;
            return;
        }

        // Clause 3: running — interpolate, detect jumps.
        let anchor = self.anchor.expect("running implies an anchor");
        let elapsed_us = now_micros.saturating_sub(anchor.micros);
        let est = anchor.sample + (elapsed_us * rate as u64) / 1_000_000;

        if position.abs_diff(est) > drift_tolerance(rate) {
            out.push(ClockMsg::Stop);
            self.anchor = Some(Anchor { sample: position, micros: now_micros });
            self.estimated_sample = position;
            let pulse = pulse_of_tick(map.samples_to_tick(position), map.ppq());
            self.last_pulse = pulse;
            out.push(ClockMsg::SongPosition(song_position_of_pulse(pulse)));
            out.push(ClockMsg::Continue);
            self.resyncs += 1;
            return;
        }

        self.estimated_sample = est;

        // Clause 4: pulse emission with burst clamp.
        let pulse = pulse_of_tick(map.samples_to_tick(est), map.ppq());
        let n = pulse.saturating_sub(self.last_pulse);
        if n > MAX_PULSE_BURST {
            self.last_pulse = pulse;
        } else {
            for _ in 0..n {
                out.push(ClockMsg::Clock);
            }
            self.last_pulse = pulse;
            self.pulses_sent += n;
        }
    }

    /// The interpolated sample position this tick resolved to (Task 8's note
    /// scheduler advances over the same window, from ONE anchor).
    pub fn estimated_sample(&self) -> u64 {
        self.estimated_sample
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn pulses_sent(&self) -> u64 {
        self.pulses_sent
    }

    pub fn resyncs(&self) -> u64 {
        self.resyncs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::types::{DEFAULT_PPQ, TempoEvent};

    fn map120() -> TempoMap {
        TempoMap::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: 120.0 }], 48_000).unwrap()
    }

    fn step(e: &mut ClockEngine, now_us: u64, playing: bool, pos: u64, map: &TempoMap) -> Vec<ClockMsg> {
        let mut out = Vec::new();
        e.step(ClockInput { now_micros: now_us, playing, position: pos, rate: 48_000, map }, &mut out);
        out
    }

    #[test]
    fn wire_bytes_match_the_midi_spec() {
        assert_eq!(ClockMsg::Clock.to_out().as_slice(), &[0xF8]);
        assert_eq!(ClockMsg::Start.to_out().as_slice(), &[0xFA]);
        assert_eq!(ClockMsg::Continue.to_out().as_slice(), &[0xFB]);
        assert_eq!(ClockMsg::Stop.to_out().as_slice(), &[0xFC]);
        // 14-bit SPP, lsb first: 200 = 0x0C8 -> lsb 0x48, msb 0x01
        assert_eq!(ClockMsg::SongPosition(200).to_out().as_slice(), &[0xF2, 0x48, 0x01]);
    }

    #[test]
    fn pulses_are_24_per_quarter_and_spp_counts_sixteenths() {
        assert_eq!(pulse_of_tick(0, 960), 0);
        assert_eq!(pulse_of_tick(960, 960), 24);
        assert_eq!(pulse_of_tick(480, 960), 12);
        assert_eq!(song_position_of_pulse(0), 0);
        assert_eq!(song_position_of_pulse(6), 1);
        assert_eq!(song_position_of_pulse(24), 4, "one beat = four sixteenths");
        assert_eq!(song_position_of_pulse(u64::MAX), 16_383, "clamped to 14 bits");
    }

    #[test]
    fn play_from_zero_emits_start_not_continue() {
        let (m, mut e) = (map120(), ClockEngine::new());
        assert_eq!(step(&mut e, 0, true, 0, &m), vec![ClockMsg::Start]);
        assert!(e.running());
    }

    #[test]
    fn play_from_mid_song_emits_song_position_then_continue() {
        let (m, mut e) = (map120(), ClockEngine::new());
        // Two beats in: 48 000 samples -> tick 1920 -> pulse 48 -> SPP 8.
        assert_eq!(
            step(&mut e, 0, true, 48_000, &m),
            vec![ClockMsg::SongPosition(8), ClockMsg::Continue]
        );
    }

    #[test]
    fn stop_emits_stop_exactly_once() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        assert_eq!(step(&mut e, 1_000, false, 0, &m), vec![ClockMsg::Stop]);
        assert_eq!(step(&mut e, 2_000, false, 0, &m), vec![], "idle emits nothing");
        assert!(!e.running());
    }

    #[test]
    fn one_beat_of_wall_time_emits_exactly_24_clocks_at_120bpm() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        let mut clocks = 0;
        // 500 ms = one beat at 120 bpm, driven in 1 ms ticks with the position
        // advancing exactly as the audio device would.
        for ms in 1..=500u64 {
            let pos = ms * 48; // 48 samples per ms
            clocks += step(&mut e, ms * 1_000, true, pos, &m).iter()
                .filter(|m| **m == ClockMsg::Clock).count();
        }
        assert_eq!(clocks, 24, "24 PPQN over one beat");
        assert_eq!(e.resyncs(), 0);
    }

    #[test]
    fn block_quantized_position_does_not_trigger_a_resync() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        // The playhead only moves once per ~10 ms audio block; wall time moves
        // every ms. This must interpolate, not resync.
        let mut clocks = 0;
        for ms in 1..=100u64 {
            let pos = (ms / 10) * 480; // one 10 ms block = 480 samples
            clocks += step(&mut e, ms * 1_000, true, pos, &m).iter()
                .filter(|m| **m == ClockMsg::Clock).count();
        }
        assert_eq!(e.resyncs(), 0, "block quantization is not a jump");
        assert!(clocks >= 4, "clocks kept flowing: {clocks}");
    }

    #[test]
    fn a_backward_jump_resyncs_with_stop_spp_continue() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 48_000, &m);
        // A loop wrap back to 0, 10 ms later.
        let out = step(&mut e, 10_000, true, 0, &m);
        assert_eq!(out, vec![ClockMsg::Stop, ClockMsg::SongPosition(0), ClockMsg::Continue]);
        assert_eq!(e.resyncs(), 1);
    }

    #[test]
    fn a_large_forward_seek_resyncs_instead_of_flooding_clocks() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        let out = step(&mut e, 10_000, true, 48_000 * 20, &m);
        assert_eq!(out[0], ClockMsg::Stop);
        assert!(matches!(out[1], ClockMsg::SongPosition(_)));
        assert_eq!(out[2], ClockMsg::Continue);
        assert!(!out.contains(&ClockMsg::Clock), "no burst of catch-up clocks");
    }

    #[test]
    fn a_starved_tick_clamps_the_burst() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        // 2 s of wall time in one tick, with the position keeping up (so it is
        // NOT a jump): 96 pulses due, clamped.
        let out = step(&mut e, 2_000_000, true, 96_000, &m);
        let clocks = out.iter().filter(|m| **m == ClockMsg::Clock).count();
        assert!(clocks <= MAX_PULSE_BURST as usize, "burst clamped, got {clocks}");
    }
}
