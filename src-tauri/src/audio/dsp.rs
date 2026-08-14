//! FUTURE EXTENSION POINT — DSP processing abstractions.
//!
//! Placeholder traits per docs/ARCHITECTURE.md §8. Nothing here is wired into
//! the render path yet; the traits exist so effects and time-stretch slot in
//! without re-architecting the engine. `process` implementations must obey
//! the §2.1 RT contract (no alloc/lock/syscall) — all allocation belongs in
//! `prepare`, which runs on the control thread.

/// In-place block of interleaved audio handed to a processor on the RT thread.
pub struct ProcessBlock<'a> {
    /// Interleaved samples, `frames * channels` long.
    pub samples: &'a mut [f32],
    pub channels: usize,
    pub sample_rate: u32,
    /// Engine-global CLAP `steady_time` base for this block (round-2 §3.5),
    /// when one exists: `Some` — the RT output callback's running sample
    /// counter, advanced once per block from `rt::SharedRt::steady` — never
    /// a per-node count, so it survives a live node's re-creation
    /// (instrument rebind, sample-rate change, a track leaving and
    /// re-entering the live set). `None` for callers with no `SharedRt` of
    /// their own (offline bounce, loopjam, preview, tests — see
    /// `mixer::render` vs `mixer::render_rt`): fix-round-1 (§3.5 adjacent
    /// hazard) — those callers' `base_pos` can move BACKWARD (a loop wrap),
    /// so it must never be used to derive a steady value; a `None` here
    /// tells the node "no engine clock, fall back to your own per-instance
    /// monotonic counter" (see `plugins::clap_host::ClapNode::process`),
    /// exactly the pre-round-2 behavior for those paths.
    pub steady: Option<u64>,
}

impl<'a> ProcessBlock<'a> {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }
}

/// RT-safe processor: `process` is called on the audio thread and must obey
/// the §2.1 contract. Allocation happens in `prepare` on the control thread.
pub trait AudioProcessor: Send {
    fn prepare(&mut self, sample_rate: u32, max_block: usize);
    /// In-place, RT-safe.
    fn process(&mut self, io: &mut ProcessBlock<'_>);
    fn reset(&mut self);
}

/// A live, in-graph instrument node (phase 3, ARCHITECTURE §15): renders
/// note events into audio ON THE RT THREAD, block by block. This is the seam
/// that PolySynth, SamplerNode and plugin nodes (CLAP/LV2) all sit behind.
///
/// Contract:
/// * `queue_event` feeds one block-relative note edge (velocity 0 = off) for
///   the NEXT `process` call; it must be RT-safe (fixed-size buffer, `false`
///   on overflow, never panic). Events arrive sorted by offset.
/// * `process` (from `AudioProcessor`) ADDS into the zeroed stereo scratch
///   it is handed and consumes the queued events, under the §2.1 contract.
/// * `all_notes_off` releases every sounding voice (transport seek/stop,
///   loop wrap). Release, not hard kill — no clicks.
/// * `set_block_context` (default no-op) delivers the ABSOLUTE timeline
///   position of the run about to be `process`ed, plus whether that run is
///   discontinuous with the previous one (seek, stop→play, loop wrap).
///   Automation-aware nodes (`plugins::automation::GainAutomatedNode`) use
///   it to keep their ramp cursors position-true across seeks and loops;
///   plain instruments ignore it. RT-safe by contract (store two words).
pub trait LiveInstrument: AudioProcessor {
    fn queue_event(&mut self, ev: crate::midi::synth::BlockNoteEvent) -> bool;
    fn all_notes_off(&mut self);
    fn set_block_context(&mut self, _base_pos: u64, _discontinuity: bool) {}
}

/// FUTURE: time-stretch / pitch-shift (Rubber Band-style) behind the RT
/// contract, with an offline path for cache pre-render. Planned uses:
/// varispeed monitoring, tempo-conform of imported clips, pitch-corrected
/// vocal comping fed by Whisper pitch data.
pub trait TimeStretcher: AudioProcessor {
    /// `time_ratio` > 1.0 = slower/longer; `pitch_ratio` > 1.0 = higher.
    fn set_ratio(&mut self, time_ratio: f64, pitch_ratio: f64);
    fn time_ratio(&self) -> f64;
    fn pitch_ratio(&self) -> f64;
    /// Processing latency introduced by the stretcher, in samples.
    fn latency_samples(&self) -> usize;
}

/// FUTURE: insert-effect slot on a track (EQ, compressor, reverb, ...).
/// Concrete effects implement this; the mixer will host a per-track chain.
pub trait Effect: AudioProcessor {
    /// Stable identifier, e.g. "aura.eq3".
    fn effect_id(&self) -> &str;
    /// Human-readable name for the UI.
    fn display_name(&self) -> &str;
    fn set_bypassed(&mut self, bypassed: bool);
    fn bypassed(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Placeholder implementations
// ---------------------------------------------------------------------------

/// Identity time-stretcher: stores ratios, passes audio through untouched.
/// Stands in until a real WSOLA/phase-vocoder implementation lands.
pub struct PassthroughStretcher {
    time_ratio: f64,
    pitch_ratio: f64,
    prepared_rate: u32,
}

impl Default for PassthroughStretcher {
    fn default() -> Self {
        Self { time_ratio: 1.0, pitch_ratio: 1.0, prepared_rate: 0 }
    }
}

impl AudioProcessor for PassthroughStretcher {
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        self.prepared_rate = sample_rate;
    }
    fn process(&mut self, _io: &mut ProcessBlock<'_>) {
        // Passthrough: leave samples untouched.
    }
    fn reset(&mut self) {}
}

impl TimeStretcher for PassthroughStretcher {
    fn set_ratio(&mut self, time_ratio: f64, pitch_ratio: f64) {
        self.time_ratio = time_ratio.max(f64::EPSILON);
        self.pitch_ratio = pitch_ratio.max(f64::EPSILON);
    }
    fn time_ratio(&self) -> f64 {
        self.time_ratio
    }
    fn pitch_ratio(&self) -> f64 {
        self.pitch_ratio
    }
    fn latency_samples(&self) -> usize {
        0
    }
}

/// No-op effect stub demonstrating the `Effect` contract.
pub struct NullEffect {
    bypassed: bool,
}

impl Default for NullEffect {
    fn default() -> Self {
        Self { bypassed: false }
    }
}

impl AudioProcessor for NullEffect {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}
    fn process(&mut self, _io: &mut ProcessBlock<'_>) {}
    fn reset(&mut self) {}
}

impl Effect for NullEffect {
    fn effect_id(&self) -> &str {
        "aura.null"
    }
    fn display_name(&self) -> &str {
        "Null"
    }
    fn set_bypassed(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
    }
    fn bypassed(&self) -> bool {
        self.bypassed
    }
}

// ---------------------------------------------------------------------------
// Offline resampling (control-plane; used when a clip's file rate differs
// from the engine rate). Linear interpolation is prototype quality — a
// windowed-sinc resampler is a future upgrade behind the same signature.
// ---------------------------------------------------------------------------

/// Resample interleaved audio from `from_rate` to `to_rate` (linear).
/// Returns the input unchanged when rates match.
pub fn linear_resample(input: &[f32], channels: usize, from_rate: u32, to_rate: u32) -> Vec<f32> {
    let channels = channels.max(1);
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let in_frames = input.len() / channels;
    if in_frames == 0 {
        return Vec::new();
    }
    let out_frames =
        ((in_frames as u64 * to_rate as u64) / from_rate as u64).max(1) as usize;
    let step = from_rate as f64 / to_rate as f64;
    let mut out = Vec::with_capacity(out_frames * channels);
    for i in 0..out_frames {
        let src = i as f64 * step;
        let i0 = (src as usize).min(in_frames - 1);
        let i1 = (i0 + 1).min(in_frames - 1);
        let t = (src - i0 as f64) as f32;
        for c in 0..channels {
            let a = input[i0 * channels + c];
            let b = input[i1 * channels + c];
            out.push(a + (b - a) * t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_leaves_audio_untouched_and_stores_ratio() {
        let mut st = PassthroughStretcher::default();
        st.prepare(48_000, 512);
        st.set_ratio(1.5, 0.5);
        assert_eq!(st.time_ratio(), 1.5);
        assert_eq!(st.pitch_ratio(), 0.5);
        assert_eq!(st.latency_samples(), 0);

        let mut buf = vec![0.1f32, -0.2, 0.3, -0.4];
        let orig = buf.clone();
        let mut blk = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: 48_000, steady: None };
        assert_eq!(blk.frames(), 2);
        st.process(&mut blk);
        assert_eq!(buf, orig);
    }

    #[test]
    fn null_effect_contract() {
        let mut e = NullEffect::default();
        assert_eq!(e.effect_id(), "aura.null");
        assert!(!e.bypassed());
        e.set_bypassed(true);
        assert!(e.bypassed());
    }

    #[test]
    fn resample_identity_when_rates_match() {
        let s = vec![0.1f32, 0.2, 0.3];
        assert_eq!(linear_resample(&s, 1, 48_000, 48_000), s);
    }

    #[test]
    fn resample_doubles_and_halves_length() {
        let s: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let up = linear_resample(&s, 1, 24_000, 48_000);
        assert_eq!(up.len(), 200);
        // linear interp of a ramp stays on the ramp
        assert!((up[3] - 1.5).abs() < 1e-4);
        let down = linear_resample(&s, 1, 48_000, 24_000);
        assert_eq!(down.len(), 50);
        assert!((down[10] - 20.0).abs() < 1e-4);
    }

    #[test]
    fn resample_keeps_channels_interleaved() {
        // stereo: L ramps up, R ramps down
        let mut s = Vec::new();
        for i in 0..50 {
            s.push(i as f32);
            s.push(-(i as f32));
        }
        let up = linear_resample(&s, 2, 22_050, 44_100);
        assert_eq!(up.len() % 2, 0);
        for f in up.chunks_exact(2) {
            assert!((f[0] + f[1]).abs() < 1e-3, "L == -R preserved");
        }
    }
}
