//! Minimal built-in polyphonic synth — owned by the MIDI agent (phase 2,
//! zone C).
//!
//! Purpose: generated/infilled MIDI must be AUDIBLE without any third-party
//! instrument. Contract (frozen):
//!
//! * Implements [`crate::audio::dsp::AudioProcessor`]: `prepare` computes
//!   coefficients on the control thread; `process` renders on the RT thread
//!   under the §2.1 contract (no alloc/lock/syscall — the voice pool and the
//!   per-block event buffer are fixed-size arrays inside the struct).
//! * Receives note events as pre-converted *sample-offset* events inside the
//!   current block (the control thread converts ticks -> samples via
//!   [`super::tempo::TempoMap`] during pre-roll; the RT side never sees
//!   ticks). Feed them with [`PolySynth::queue_event`] /
//!   [`PolySynth::queue_events`] before calling `process` for the block.
//! * Voice model: fixed-size preallocated voice pool ([`MAX_VOICES`]),
//!   oldest-note stealing, per-voice phase-accumulator sine oscillator +
//!   linear attack / linear release envelope. Deliberately cheap — this is a
//!   scaffold instrument; "real" instruments are the sampler
//!   (audio::sampler) behind the same event contract.
//! * If a `kind:"midi"` track has an `instrumentId`, the sampler renders it;
//!   otherwise this synth is the fallback so MIDI is always audible.

use crate::audio::dsp::{AudioProcessor, ProcessBlock};

/// Preallocated polyphony for the built-in synth.
pub const MAX_VOICES: usize = 32;

/// Fixed capacity of the per-block event buffer (events beyond this in a
/// single block are dropped — 512 note edges in one audio block is far past
/// musical content).
pub const MAX_BLOCK_EVENTS: usize = 512;

/// Envelope attack time.
const ATTACK_SECS: f32 = 0.003;
/// Envelope release time. Public so the offline pre-render can size its tail.
pub const RELEASE_SECS: f32 = 0.08;
/// Output scaling so a handful of simultaneous voices stays below full scale.
const MASTER_GAIN: f32 = 0.25;

/// One note event, already converted to a sample offset within the current
/// render block. POD — safe to move through preallocated event buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockNoteEvent {
    /// Offset in samples from the start of the current block.
    pub offset: u32,
    pub key: u8,
    /// 0 velocity = note off.
    pub velocity: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VoiceState {
    Idle,
    /// Attack then sustain while the key is held.
    Held,
    /// Linear ramp to zero, then idle.
    Released,
}

#[derive(Debug, Clone, Copy)]
struct Voice {
    state: VoiceState,
    key: u8,
    /// Velocity-derived amplitude (0..1].
    amp: f32,
    /// Phase in radians.
    phase: f32,
    phase_inc: f32,
    /// Envelope level 0..1.
    env: f32,
    /// Steal order: lower = older.
    age: u64,
}

impl Voice {
    const IDLE: Voice = Voice {
        state: VoiceState::Idle,
        key: 0,
        amp: 0.0,
        phase: 0.0,
        phase_inc: 0.0,
        env: 0.0,
        age: 0,
    };
}

/// The built-in polyphonic synth. One instance per midi track / offline
/// render; NOT shared across threads while rendering.
pub struct PolySynth {
    voices: [Voice; MAX_VOICES],
    events: [BlockNoteEvent; MAX_BLOCK_EVENTS],
    n_events: usize,
    dropped_events: u64,
    sample_rate: u32,
    attack_step: f32,
    release_step: f32,
    age_counter: u64,
}

impl PolySynth {
    pub fn new() -> Self {
        let mut s = Self {
            voices: [Voice::IDLE; MAX_VOICES],
            events: [BlockNoteEvent { offset: 0, key: 0, velocity: 0 }; MAX_BLOCK_EVENTS],
            n_events: 0,
            dropped_events: 0,
            sample_rate: 48_000,
            attack_step: 0.0,
            release_step: 0.0,
            age_counter: 0,
        };
        s.compute_steps();
        s
    }

    fn compute_steps(&mut self) {
        let rate = self.sample_rate.max(1) as f32;
        self.attack_step = 1.0 / (ATTACK_SECS * rate).max(1.0);
        self.release_step = 1.0 / (RELEASE_SECS * rate).max(1.0);
    }

    /// Number of currently sounding (non-idle) voices.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.state != VoiceState::Idle).count()
    }

    /// Events dropped because a block carried more than [`MAX_BLOCK_EVENTS`].
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    /// Queue one event for the NEXT `process` call. RT-safe (no alloc);
    /// returns false when the fixed buffer is full (event dropped).
    pub fn queue_event(&mut self, ev: BlockNoteEvent) -> bool {
        if self.n_events >= MAX_BLOCK_EVENTS {
            self.dropped_events += 1;
            return false;
        }
        self.events[self.n_events] = ev;
        self.n_events += 1;
        true
    }

    /// Queue a batch of events for the next `process` call. RT-safe.
    pub fn queue_events(&mut self, evs: &[BlockNoteEvent]) {
        for &ev in evs {
            self.queue_event(ev);
        }
    }

    fn note_on(&mut self, key: u8, velocity: u8) {
        self.age_counter += 1;
        let age = self.age_counter;
        // Reuse an idle voice, else steal the oldest.
        let idx = match self.voices.iter().position(|v| v.state == VoiceState::Idle) {
            Some(i) => i,
            None => {
                let mut oldest = 0usize;
                let mut oldest_age = u64::MAX;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.age < oldest_age {
                        oldest_age = v.age;
                        oldest = i;
                    }
                }
                oldest
            }
        };
        let freq = key_to_freq(key);
        self.voices[idx] = Voice {
            state: VoiceState::Held,
            key,
            amp: velocity as f32 / 127.0,
            phase: 0.0,
            phase_inc: std::f32::consts::TAU * freq / self.sample_rate.max(1) as f32,
            env: 0.0,
            age,
        };
    }

    fn note_off(&mut self, key: u8) {
        for v in self.voices.iter_mut() {
            if v.key == key && v.state == VoiceState::Held {
                v.state = VoiceState::Released;
            }
        }
    }

    #[inline]
    fn apply_event(&mut self, ev: BlockNoteEvent) {
        if ev.velocity == 0 {
            self.note_off(ev.key);
        } else {
            self.note_on(ev.key, ev.velocity);
        }
    }
}

impl Default for PolySynth {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioProcessor for PolySynth {
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.sample_rate = sample_rate.max(1);
        self.compute_steps();
        self.reset();
    }

    /// Renders the queued events + sounding voices ADDITIVELY into the block
    /// (mono signal duplicated to all channels). RT-safe: fixed-size state
    /// only, no alloc/lock/syscall. Consumes the queued events.
    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        let ch = io.channels.max(1);
        let frames = io.frames();
        // Events must fire in offset order; sort_unstable is in-place/alloc-free.
        self.events[..self.n_events].sort_unstable_by_key(|e| e.offset);
        let n_events = self.n_events;
        let mut ev_i = 0usize;

        for f in 0..frames {
            while ev_i < n_events && self.events[ev_i].offset as usize <= f {
                let ev = self.events[ev_i];
                self.apply_event(ev);
                ev_i += 1;
            }
            let mut s = 0.0f32;
            for v in self.voices.iter_mut() {
                match v.state {
                    VoiceState::Idle => continue,
                    VoiceState::Held => {
                        if v.env < 1.0 {
                            v.env = (v.env + self.attack_step).min(1.0);
                        }
                    }
                    VoiceState::Released => {
                        v.env -= self.release_step;
                        if v.env <= 0.0 {
                            *v = Voice::IDLE;
                            continue;
                        }
                    }
                }
                s += v.phase.sin() * v.amp * v.env;
                v.phase += v.phase_inc;
                if v.phase > std::f32::consts::TAU {
                    v.phase -= std::f32::consts::TAU;
                }
            }
            s *= MASTER_GAIN;
            let base = f * ch;
            for c in 0..ch {
                io.samples[base + c] += s;
            }
        }
        // Late events (offset beyond the block, defensive) still fire so no
        // note-off is ever lost.
        while ev_i < n_events {
            let ev = self.events[ev_i];
            self.apply_event(ev);
            ev_i += 1;
        }
        self.n_events = 0;
    }

    fn reset(&mut self) {
        self.voices = [Voice::IDLE; MAX_VOICES];
        self.n_events = 0;
        self.age_counter = 0;
    }
}

/// Live-node seam (phase 3, ARCHITECTURE §15): the PolySynth is the first
/// in-graph instrument. `queue_event` already matches; `all_notes_off`
/// releases (not kills) every held voice so seeks/loop-wraps don't click.
impl crate::audio::dsp::LiveInstrument for PolySynth {
    fn queue_event(&mut self, ev: BlockNoteEvent) -> bool {
        PolySynth::queue_event(self, ev)
    }

    fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut() {
            if v.state == VoiceState::Held {
                v.state = VoiceState::Released;
            }
        }
        self.n_events = 0;
    }
}

/// Equal temperament, A4 (key 69) = 440 Hz.
#[inline]
pub fn key_to_freq(key: u8) -> f32 {
    440.0 * ((key as f32 - 69.0) / 12.0).exp2()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(synth: &mut PolySynth, frames: usize, rate: u32) -> Vec<f32> {
        let mut buf = vec![0.0f32; frames * 2];
        let mut blk = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: rate };
        synth.process(&mut blk);
        buf
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    #[test]
    fn key_to_freq_reference_points() {
        assert!((key_to_freq(69) - 440.0).abs() < 1e-3);
        assert!((key_to_freq(57) - 220.0).abs() < 1e-3);
        assert!((key_to_freq(60) - 261.6256).abs() < 0.01);
    }

    #[test]
    fn note_on_produces_audio_and_release_decays_to_silence() {
        let mut s = PolySynth::new();
        s.prepare(48_000, 512);
        s.queue_event(BlockNoteEvent { offset: 0, key: 69, velocity: 100 });
        let audible = render(&mut s, 4_800, 48_000); // 100 ms held
        assert!(peak(&audible) > 0.05, "held note must be audible");
        assert!(peak(&audible) <= 1.0, "single voice must not clip");
        assert_eq!(s.active_voices(), 1);

        s.queue_event(BlockNoteEvent { offset: 0, key: 69, velocity: 0 });
        // Render well past the release time.
        let tail = render(&mut s, 48_000 / 4, 48_000);
        assert_eq!(s.active_voices(), 0, "voice freed after release");
        // The last chunk of the tail must be digital silence.
        assert_eq!(peak(&tail[tail.len() - 1000..]), 0.0);
    }

    #[test]
    fn event_offset_is_sample_accurate() {
        let mut s = PolySynth::new();
        s.prepare(48_000, 512);
        s.queue_event(BlockNoteEvent { offset: 100, key: 60, velocity: 127 });
        let buf = render(&mut s, 512, 48_000);
        // Strictly zero before the event offset...
        assert_eq!(peak(&buf[..100 * 2]), 0.0);
        // ...and sound after it (attack is 3 ms, so give it some frames).
        assert!(peak(&buf[300 * 2..]) > 0.0);
    }

    #[test]
    fn oldest_voice_is_stolen_beyond_max_polyphony() {
        let mut s = PolySynth::new();
        s.prepare(48_000, 64);
        for i in 0..(MAX_VOICES as u8 + 1) {
            s.queue_event(BlockNoteEvent { offset: 0, key: 30 + i, velocity: 100 });
            let _ = render(&mut s, 16, 48_000);
        }
        assert_eq!(s.active_voices(), MAX_VOICES);
        // The very first note (key 30) was the oldest -> stolen: releasing it
        // must be a no-op (no voice holds that key anymore).
        s.queue_event(BlockNoteEvent { offset: 0, key: 30, velocity: 0 });
        let _ = render(&mut s, 16, 48_000);
        assert_eq!(s.active_voices(), MAX_VOICES, "stolen key off releases nothing");
    }

    #[test]
    fn event_buffer_overflow_drops_but_never_panics() {
        let mut s = PolySynth::new();
        s.prepare(48_000, 64);
        for i in 0..(MAX_BLOCK_EVENTS + 10) {
            s.queue_event(BlockNoteEvent { offset: 0, key: (i % 128) as u8, velocity: 1 });
        }
        assert_eq!(s.dropped_events(), 10);
        let _ = render(&mut s, 32, 48_000);
    }

    #[test]
    fn unsorted_events_fire_in_offset_order() {
        let mut s = PolySynth::new();
        s.prepare(48_000, 512);
        // Off scheduled BEFORE on within the same block, queued out of order:
        // queue on@0 off@200 as (off first) — sorting must fix ordering.
        s.queue_event(BlockNoteEvent { offset: 200, key: 60, velocity: 0 });
        s.queue_event(BlockNoteEvent { offset: 0, key: 60, velocity: 100 });
        let _ = render(&mut s, 512, 48_000);
        // note released -> decays to idle shortly after
        let _ = render(&mut s, 48_000 / 4, 48_000);
        assert_eq!(s.active_voices(), 0);
    }
}
