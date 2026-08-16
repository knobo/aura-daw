//! 8 kHz mono samples -> 100 Hz stream of gated, smoothed [`PitchFrame`]s.
//!
//! Pure and synchronous: this module decides *what was sung*, never *when to
//! run*. The thread that drives it is [`super::engine`]'s concern.

use super::yin::{hz_to_midi, midi_to_hz, Yin, FRAME_HOP_S, TARGET_SR, YIN_THRESHOLD};

pub const RMS_GATE_REL: f32 = 0.05;
pub const MEDIAN_TAPS: usize = 5;
pub const MAX_JUMP_SEMITONES: f32 = 3.0;

/// Absolute floor under the relative gate, so a silent room (where the
/// decaying loud reference has fallen to ~0) does not gate *in* its own
/// noise floor.
const RMS_GATE_FLOOR: f32 = 1e-4;

/// Per-frame decay of the loud reference: `0.9995^100` ≈ 0.95, so the
/// reference falls ~5 % per second of quiet. Slow enough to survive a
/// phrase-length rest, fast enough to follow a singer who backs off.
const LOUD_DECAY: f32 = 0.9995;

/// Voiced entries needed among the [`MEDIAN_TAPS`] before a median is
/// reported at all. Below this the frame is unvoiced: a lone voiced frame
/// inside a breath is noise, not a note.
const MEDIAN_MIN_VOICED: usize = 3;

/// How close the next frame must land to a held-back leap for that leap to
/// be accepted as real rather than a detector blip.
const JUMP_CONFIRM_SEMITONES: f32 = 1.0;

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitchFrame {
    pub sample: u64,
    pub hz: f32,
    pub midi: f32,
    pub clarity: f32,
    pub rms: f32,
    pub voiced: bool,
}

/// Turns a stream of 8 kHz mono samples into 100 Hz `PitchFrame`s.
/// Pure and synchronous — the thread that drives it is the engine's concern.
///
/// Allocation-free after [`PitchAnalyzer::new`]: `buf` is sized once to a
/// full analysis frame and slid in place, so only the caller's `out` grows.
pub struct PitchAnalyzer {
    yin: Yin,
    /// `yin.frame_len()`, cached — `window + tau_max` lookahead.
    frame_len: usize,
    /// `yin.window()`, the leading samples that are actually *measured*.
    window: usize,
    /// 10 ms at [`TARGET_SR`].
    hop: usize,

    /// The newest (at most `frame_len`) input samples, oldest first.
    buf: Vec<f32>,
    /// Total 8 kHz samples ever fed, so a window's absolute position is
    /// `consumed - frame_len`.
    consumed: u64,
    /// Device/8 kHz position pair anchoring the current `push`, so frame
    /// timestamps follow the caller's authoritative transport position
    /// rather than an independently drifting count.
    base_device: i64,
    base_consumed: i64,

    /// Decaying maximum of frame RMS. `sidecars/hum_to_midi.py` gates on the
    /// 95th percentile of the *whole file*, which a live stream cannot know;
    /// a decaying max is the streaming equivalent of that reference level.
    loud: f32,

    /// Ring of the last [`MEDIAN_TAPS`] raw MIDI readings (`None` = unvoiced).
    hist: [Option<f32>; MEDIAN_TAPS],
    hist_pos: usize,
    /// Last MIDI value that survived the jump limiter.
    last_accepted: Option<f32>,
    /// A leap held back for one frame, awaiting confirmation.
    pending: Option<f32>,
}

impl PitchAnalyzer {
    pub fn new() -> Self {
        let yin = Yin::new(TARGET_SR);
        let frame_len = yin.frame_len();
        let window = yin.window();
        Self {
            yin,
            frame_len,
            window,
            hop: (TARGET_SR as f32 * FRAME_HOP_S) as usize,
            buf: Vec::with_capacity(frame_len),
            consumed: 0,
            base_device: 0,
            base_consumed: 0,
            loud: 0.0,
            hist: [None; MEDIAN_TAPS],
            hist_pos: 0,
            last_accepted: None,
            pending: None,
        }
    }

    /// Samples of real signal each frame measures (the detector's trailing
    /// lookahead is excluded).
    pub fn window_samples(&self) -> usize {
        self.window
    }

    /// `start_sample` is the transport sample position of `input[0]`, in
    /// DEVICE-rate samples. Appends one frame per completed hop.
    pub fn push(
        &mut self,
        input: &[f32],
        start_sample: u64,
        device_rate: u32,
        out: &mut Vec<PitchFrame>,
    ) {
        self.base_device = start_sample as i64;
        self.base_consumed = self.consumed as i64;

        let mut i = 0;
        while i < input.len() {
            // Fill at most up to a whole frame, so `buf` never outgrows the
            // capacity reserved in `new` however long `input` is.
            let take = (self.frame_len - self.buf.len()).min(input.len() - i);
            self.buf.extend_from_slice(&input[i..i + take]);
            self.consumed += take as u64;
            i += take;

            if self.buf.len() == self.frame_len {
                self.analyze(device_rate, out);
                // Slide by one hop; the retained tail is the next frame's head.
                self.buf.copy_within(self.hop.., 0);
                self.buf.truncate(self.frame_len - self.hop);
            }
        }
    }

    /// One completed frame: detect, gate, smooth, timestamp, emit.
    fn analyze(&mut self, device_rate: u32, out: &mut Vec<PitchFrame>) {
        let (f0, aperiodicity) = self.yin.detect(&self.buf[..self.frame_len]);

        // RMS over the measured window only, not the lookahead — matching
        // `pitch_track`'s `frame[:w]`.
        let w = self.window;
        let rms = (self.buf[..w].iter().map(|s| s * s).sum::<f32>() / w as f32).sqrt();

        self.loud = self.loud.max(rms) * LOUD_DECAY;
        let loud_enough = rms > (self.loud * RMS_GATE_REL).max(RMS_GATE_FLOOR);

        let raw = match f0 {
            Some(hz) if aperiodicity < YIN_THRESHOLD && loud_enough => Some(hz_to_midi(hz)),
            _ => None,
        };

        // -- median of the voiced entries in the 5-tap history --
        self.hist[self.hist_pos] = raw;
        self.hist_pos = (self.hist_pos + 1) % MEDIAN_TAPS;
        let mut vals = [0.0f32; MEDIAN_TAPS];
        let mut n = 0;
        for v in self.hist.iter().flatten() {
            vals[n] = *v;
            n += 1;
        }
        let smoothed = if n >= MEDIAN_MIN_VOICED {
            let s = &mut vals[..n];
            s.sort_by(|a, b| a.partial_cmp(b).expect("MIDI values are finite"));
            Some(s[n / 2])
        } else {
            None
        };

        // -- jump limiter --
        // A leap bigger than MAX_JUMP_SEMITONES is either a real interval or
        // an octave error. Hold it back one frame: a real leap is still there
        // next frame and gets through with 10 ms of lag; a blip is not.
        let accepted = match (smoothed, self.last_accepted) {
            (Some(m), Some(last)) if (m - last).abs() > MAX_JUMP_SEMITONES => match self.pending {
                Some(cand) if (m - cand).abs() <= JUMP_CONFIRM_SEMITONES => {
                    self.pending = None;
                    self.last_accepted = Some(m);
                    Some(m)
                }
                _ => {
                    self.pending = Some(m);
                    None
                }
            },
            (Some(m), _) => {
                self.pending = None;
                self.last_accepted = Some(m);
                Some(m)
            }
            // A gap long enough to empty the median window ends the phrase:
            // drop the reference, so the next entry is not limited against a
            // note from before the breath.
            (None, _) => {
                self.pending = None;
                self.last_accepted = None;
                None
            }
        };

        // -- timestamp: the CENTRE of the analysis window, in device samples --
        let sr = TARGET_SR as i64;
        let rate = device_rate as i64;
        let window_start = self.consumed as i64 - self.frame_len as i64;
        let start_device = self.base_device + (window_start - self.base_consumed) * rate / sr;
        let half_window_device = self.window as i64 * rate / (2 * sr);
        let sample = (start_device + half_window_device).max(0) as u64;

        let (voiced, midi, hz) = match accepted {
            Some(m) => (true, m, midi_to_hz(m)),
            None => (false, 0.0, 0.0),
        };
        out.push(PitchFrame {
            sample,
            hz,
            midi,
            clarity: (1.0 - aperiodicity).clamp(0.0, 1.0),
            rms,
            voiced,
        });
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.consumed = 0;
        self.base_device = 0;
        self.base_consumed = 0;
        self.loud = 0.0;
        self.hist = [None; MEDIAN_TAPS];
        self.hist_pos = 0;
        self.last_accepted = None;
        self.pending = None;
    }
}

impl Default for PitchAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::yin::{cents_between, TARGET_SR};

    fn sine_8k(hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / TARGET_SR as f32).sin())
            .collect()
    }

    fn frames_for(sig: &[f32]) -> Vec<PitchFrame> {
        let mut a = PitchAnalyzer::new();
        let mut out = Vec::new();
        a.push(sig, 0, 48_000, &mut out);
        out
    }

    #[test]
    fn emits_one_frame_per_ten_milliseconds() {
        let frames = frames_for(&sine_8k(220.0, TARGET_SR as usize)); // 1 second
        assert!(
            frames.len() >= 90 && frames.len() <= 100,
            "expected ~100 frames, got {}",
            frames.len()
        );
    }

    #[test]
    fn voiced_frames_carry_pitch_and_midi() {
        let frames = frames_for(&sine_8k(220.0, TARGET_SR as usize));
        let voiced: Vec<_> = frames.iter().filter(|f| f.voiced).collect();
        assert!(
            !voiced.is_empty(),
            "a steady 220 Hz tone must produce voiced frames"
        );
        for f in voiced {
            assert!(cents_between(f.hz, 220.0).abs() < 25.0, "{} Hz", f.hz);
            assert!((f.midi - 57.0).abs() < 0.3, "A3 is MIDI 57, got {}", f.midi);
            assert!(f.clarity > 0.0 && f.clarity <= 1.0);
        }
    }

    #[test]
    fn silence_is_unvoiced() {
        let frames = frames_for(&vec![0.0f32; TARGET_SR as usize]);
        assert!(
            frames.iter().all(|f| !f.voiced),
            "silence must never be voiced"
        );
    }

    /// Breath and bleed sit far below the loud reference; the relative RMS
    /// gate is what stops them being drawn as notes.
    ///
    /// The assertion is "voiced only while settling", not "never voiced":
    /// `push` carries a full analysis frame of history across the call
    /// boundary, so the first windows after the level drop still literally
    /// CONTAIN the loud tone and are rightly voiced, and the 5-tap median
    /// holds voicing a few frames past that. Both are bounded and local;
    /// what must never happen is the quiet steady state reading as notes.
    #[test]
    fn quiet_signal_below_the_relative_gate_is_unvoiced() {
        /// Worst case: the carried-over frame is one hop short of full, so
        /// `frame_len / hop` (5) windows can still hold the loud tail, and
        /// the median filter reports while 3 of the last
        /// [`MEDIAN_TAPS`] are voiced — 3 more. 10 leaves margin, and still
        /// leaves 90 of the 100 frames under assertion.
        const SETTLE_FRAMES: usize = 10;

        let mut a = PitchAnalyzer::new();
        let mut out = Vec::new();
        a.push(&sine_8k(220.0, TARGET_SR as usize), 0, 48_000, &mut out);
        // Guard against an always-unvoiced stub: the reference itself must
        // have registered, or "gated out" below would be vacuous.
        assert!(
            out.iter().any(|f| f.voiced),
            "the loud reference must itself be voiced"
        );

        out.clear();
        let quiet: Vec<f32> = sine_8k(220.0, TARGET_SR as usize)
            .iter()
            .map(|s| s * 0.005)
            .collect();
        a.push(&quiet, TARGET_SR as u64, 48_000, &mut out);

        let voiced: Vec<usize> = out
            .iter()
            .enumerate()
            .filter(|(_, f)| f.voiced)
            .map(|(i, _)| i)
            .collect();
        assert!(
            voiced.iter().all(|&i| i < SETTLE_FRAMES),
            "a signal 46 dB below the loud reference must be gated out once \
             the analysis window has moved past the loud tail; voiced frames \
             were {voiced:?} of {}",
            out.len()
        );
    }

    /// A single-frame octave blip is exactly what the median filter and the
    /// jump limiter exist for.
    #[test]
    fn a_single_frame_octave_blip_is_suppressed() {
        let mut sig = sine_8k(220.0, TARGET_SR as usize);
        let hop = (TARGET_SR as f32 * FRAME_HOP_S) as usize;
        let bad = sine_8k(440.0, hop * 2);
        let at = hop * 40;
        sig[at..at + bad.len()].copy_from_slice(&bad);

        let frames = frames_for(&sig);
        let high = frames.iter().filter(|f| f.voiced && f.midi > 62.0).count();
        assert!(
            high <= 1,
            "a two-hop blip must not survive smoothing; {high} frames jumped"
        );
    }

    /// Frames must land where the SOUND was, not where the analyser
    /// finished — the window's own group delay is subtracted.
    #[test]
    fn frames_are_timestamped_behind_the_analysis_window() {
        let frames = frames_for(&sine_8k(220.0, TARGET_SR as usize));
        let first = frames.first().expect("at least one frame");
        let a = PitchAnalyzer::new();
        let half_window_device = (a.window_samples() as u64 * 48_000_u64) / (2 * TARGET_SR as u64);
        assert!(
            first.sample <= half_window_device + 1,
            "first frame at {} should be at or before the half-window mark {half_window_device}",
            first.sample
        );
    }

    #[test]
    fn sample_positions_advance_by_one_hop_in_device_samples() {
        let frames = frames_for(&sine_8k(220.0, TARGET_SR as usize));
        let hop_device = (48_000f32 * FRAME_HOP_S) as u64; // 480
        for w in frames.windows(2) {
            assert_eq!(
                w[1].sample - w[0].sample,
                hop_device,
                "hop must be exactly 10 ms of device samples"
            );
        }
    }

    /// The spec's cost claim ("far below 1% of a core") is load-bearing —
    /// it is why YIN runs on every buffer instead of on a duty cycle. A
    /// generous ceiling here fails loudly if someone makes the detector
    /// quadratically slower.
    #[test]
    fn analysis_is_far_faster_than_real_time() {
        let sig = sine_8k(220.0, TARGET_SR as usize * 10); // 10 seconds
        let mut a = PitchAnalyzer::new();
        let mut out = Vec::new();
        let t0 = std::time::Instant::now();
        a.push(&sig, 0, 48_000, &mut out);
        let elapsed = t0.elapsed().as_secs_f32();
        assert!(
            elapsed < 1.0,
            "10 s of audio took {elapsed:.3} s to analyse"
        );
    }

    #[test]
    fn is_continuous_across_push_boundaries() {
        let sig = sine_8k(220.0, TARGET_SR as usize);
        let whole = frames_for(&sig);
        let mut chunked = Vec::new();
        let mut a = PitchAnalyzer::new();
        let mut pos = 0u64;
        for c in sig.chunks(37) {
            a.push(c, pos, 48_000, &mut chunked);
            pos += (c.len() as u64 * 48_000) / TARGET_SR as u64;
        }
        assert!((whole.len() as i64 - chunked.len() as i64).abs() <= 2);
    }
}
