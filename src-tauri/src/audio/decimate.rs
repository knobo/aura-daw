//! Interleaved device audio -> mono 8 kHz, for the live pitch detector
//! ([`super::yin`]). Sample-dropping is not enough: a device tone above the
//! 4 kHz output Nyquist would fold down into the vocal range and get
//! detected as a sung note, so this filters before it decimates.
//!
//! Runs adjacent to the real-time audio callback (a later task), so
//! [`Decimator::process`] must not allocate — all filter state lives in the
//! struct and only the caller's `out` `Vec` grows.

use super::yin::TARGET_SR;

/// Low-pass tap count. 63 taps at a 0.4 * `TARGET_SR` cutoff gives enough
/// stopband attenuation to keep content above the 4 kHz output Nyquist from
/// folding into the vocal range (see `rejects_content_above_the_output_nyquist`).
const TAPS: usize = 63;

/// Interleaved device audio -> mono `TARGET_SR` (8 kHz): average channels,
/// high-pass to strip rumble/DC, low-pass to band-limit, then resample with
/// a fractional phase accumulator so non-integer ratios (e.g. 44.1 kHz)
/// work as well as clean multiples (48 kHz).
pub struct Decimator {
    // -- 80 Hz one-pole high-pass state (persists across `process` calls) --
    hp_a: f32,
    hp_prev_x: f32,
    hp_prev_y: f32,

    // -- anti-alias low-pass FIR --
    /// Windowed-sinc coefficients, computed once in `new`.
    fir: [f32; TAPS],
    /// Ring of the last `TAPS` input samples, so chunk boundaries are
    /// seamless (no re-zeroing history between `process` calls).
    history: Vec<f32>,

    // -- fractional-phase resampler --
    /// Output samples produced per input sample (`TARGET_SR / in_rate`).
    step: f64,
    /// Fractional position within the current input sample; carries across
    /// calls so chunking doesn't shift the output phase.
    pos: f64,
    /// Previous filtered sample, the left side of the interpolation.
    prev_filtered: f32,
    /// Whether `prev_filtered` holds a real sample yet (vs. the `new()`/
    /// `reset()` default), so the very first output isn't interpolated
    /// against a phantom zero.
    have_prev: bool,
}

impl Decimator {
    /// `in_rate` is the device rate. Any rate is accepted; the ratio need
    /// not be an integer.
    pub fn new(in_rate: u32) -> Self {
        let hp_a = 1.0 / (1.0 + 2.0 * std::f32::consts::PI * 80.0 / in_rate as f32);

        // Windowed-sinc low-pass, cutoff 0.4 * TARGET_SR, Hamming window.
        let cutoff = 0.4 * TARGET_SR as f64 / in_rate as f64;
        let m = (TAPS - 1) as f64;
        let mut fir = [0.0f32; TAPS];
        for (i, coef) in fir.iter_mut().enumerate() {
            let x = i as f64 - m / 2.0;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let window = 0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / m).cos();
            *coef = (sinc * window) as f32;
        }
        let sum: f32 = fir.iter().sum();
        if sum != 0.0 {
            for c in fir.iter_mut() {
                *c /= sum;
            }
        }

        Self {
            hp_a,
            hp_prev_x: 0.0,
            hp_prev_y: 0.0,
            fir,
            history: vec![0.0; TAPS],
            step: TARGET_SR as f64 / in_rate as f64,
            pos: 0.0,
            prev_filtered: 0.0,
            have_prev: false,
        }
    }

    /// Push interleaved input, appending decimated mono 8 kHz samples to
    /// `out`. Never allocates beyond `out`'s own growth.
    pub fn process(&mut self, input: &[f32], channels: usize, out: &mut Vec<f32>) {
        let channels = channels.max(1);
        for frame in input.chunks_exact(channels) {
            let mono = frame.iter().sum::<f32>() / channels as f32;

            // 80 Hz one-pole high-pass: y[n] = a * (y[n-1] + x[n] - x[n-1]).
            let hp = self.hp_a * (self.hp_prev_y + mono - self.hp_prev_x);
            self.hp_prev_x = mono;
            self.hp_prev_y = hp;

            // Shift the FIR history and convolve.
            self.history.copy_within(1.., 0);
            *self.history.last_mut().unwrap() = hp;
            let filtered: f32 = self
                .history
                .iter()
                .zip(self.fir.iter())
                .map(|(x, c)| x * c)
                .sum();

            if !self.have_prev {
                self.prev_filtered = filtered;
                self.have_prev = true;
            }

            // Emit every output sample whose fractional position falls
            // within this input step — linear interpolation between the
            // previous and current filtered sample handles non-integer
            // ratios (44.1 kHz) as well as clean multiples (6x at 48 kHz).
            //
            // `self.pos` (after the subtraction below) is how far PAST the
            // crossing we landed, in units of `step`; dividing by `step`
            // converts that into "how far past the crossing" in units of
            // one input-sample interval, so `1 - pos/step` is the fraction
            // of the [prev, current] interval closest to `current`. Omitting
            // the `/step` (i.e. treating `pos` itself as the fraction) is
            // wrong whenever `step != 1` — it silently reproduces uneven
            // output spacing instead of a clean decimation.
            self.pos += self.step;
            while self.pos >= 1.0 {
                self.pos -= 1.0;
                let t = 1.0 - (self.pos / self.step) as f32;
                out.push(self.prev_filtered + (filtered - self.prev_filtered) * t);
            }
            self.prev_filtered = filtered;
        }
    }

    pub fn reset(&mut self) {
        self.hp_prev_x = 0.0;
        self.hp_prev_y = 0.0;
        self.history.iter_mut().for_each(|s| *s = 0.0);
        self.pos = 0.0;
        self.prev_filtered = 0.0;
        self.have_prev = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::yin::TARGET_SR;

    fn sine(hz: f32, sr: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / sr as f32).sin())
            .collect()
    }

    #[test]
    fn output_rate_is_target_sr() {
        let mut d = Decimator::new(48_000);
        let mut out = Vec::new();
        d.process(&sine(220.0, 48_000, 48_000), 1, &mut out);
        let ratio = out.len() as f32 / TARGET_SR as f32;
        assert!(
            (ratio - 1.0).abs() < 0.02,
            "1 s in must give ~1 s at 8 kHz, got {} samples",
            out.len()
        );
    }

    #[test]
    fn preserves_pitch_through_the_detector() {
        let mut d = Decimator::new(48_000);
        let mut out = Vec::new();
        d.process(&sine(220.0, 48_000, 48_000), 1, &mut out);
        let mut y = crate::audio::yin::Yin::new(TARGET_SR);
        let n = y.frame_len();
        let f0 = y
            .detect(&out[out.len() / 2..out.len() / 2 + n])
            .0
            .expect("voiced");
        assert!(
            crate::audio::yin::cents_between(f0, 220.0).abs() < 25.0,
            "got {f0} Hz"
        );
    }

    /// The reason this module exists rather than plain sample-dropping: a
    /// tone above the 4 kHz output Nyquist must be attenuated, not folded
    /// down into the vocal range where it would be detected as a note.
    #[test]
    fn rejects_content_above_the_output_nyquist() {
        let mut d = Decimator::new(48_000);
        let mut out = Vec::new();
        d.process(&sine(9_000.0, 48_000, 48_000), 1, &mut out);
        let tail = &out[out.len() / 2..];
        let rms = (tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(
            rms < 0.1,
            "9 kHz must be attenuated, not aliased in; rms {rms}"
        );
    }

    #[test]
    fn sums_channels_to_mono() {
        let mut d = Decimator::new(48_000);
        let mut out = Vec::new();
        // Stereo, both channels the same tone: mono sum must not clip to
        // twice the amplitude.
        let mono = sine(220.0, 48_000, 24_000);
        let stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, s]).collect();
        d.process(&stereo, 2, &mut out);
        let peak = out.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
        assert!(
            peak <= 1.2,
            "channel sum must average, not add; peak {peak}"
        );
    }

    #[test]
    fn is_continuous_across_chunk_boundaries() {
        let sig = sine(220.0, 48_000, 48_000);
        let mut whole = Vec::new();
        Decimator::new(48_000).process(&sig, 1, &mut whole);

        let mut chunked = Vec::new();
        let mut d = Decimator::new(48_000);
        for c in sig.chunks(128) {
            d.process(c, 1, &mut chunked);
        }
        assert!((whole.len() as i64 - chunked.len() as i64).abs() <= 1);
        let n = whole.len().min(chunked.len());
        // Skip filter warm-up; the steady state must match.
        for i in 200..n {
            assert!((whole[i] - chunked[i]).abs() < 1e-4, "discontinuity at {i}");
        }
    }
}
