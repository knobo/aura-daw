//! YIN pitch detector (pure, allocation-free per call).
//!
//! Ported from `sidecars/hum_to_midi.py`'s `_frame_pitch_py` /
//! `_cmnd_pick` (lines ~60-65, 200-235) so the live detector and the
//! hum-to-MIDI sidecar agree on what the user sang — a later task adds a
//! parity test between them. Constants and algorithm order are copied
//! verbatim; do not "improve" them here.

pub const TARGET_SR: u32 = 8_000;
pub const FRAME_HOP_S: f32 = 0.010;
pub const FMIN: f32 = 65.0;
pub const FMAX: f32 = 1000.0;
pub const YIN_THRESHOLD: f32 = 0.20;

/// Cumulative-mean normalized difference (CMND) pitch detector. Buffers are
/// preallocated in [`Yin::new`] so [`Yin::detect`] never allocates — it will
/// later run adjacent to a real-time audio path.
pub struct Yin {
    sr: u32,
    tau_min: usize,
    tau_max: usize,
    /// Raw difference function, indexed `0..=tau_max`.
    d: Vec<f32>,
    /// Cumulative mean normalized difference, indexed `0..=tau_max`.
    cmnd: Vec<f32>,
}

impl Yin {
    pub fn new(sr: u32) -> Self {
        let tau_min = (sr as f32 / FMAX).max(2.0) as usize;
        let tau_max = (sr as f32 / FMIN) as usize;
        Self {
            sr,
            tau_min,
            tau_max,
            d: vec![0.0; tau_max + 1],
            cmnd: vec![0.0; tau_max + 1],
        }
    }

    /// Analysis window length in samples (`2 * tau_max`).
    pub fn window(&self) -> usize {
        2 * self.tau_max
    }

    /// `frame.len()` must equal `self.window()`.
    /// Returns `(f0_hz, aperiodicity)`; `f0_hz` is `None` when unvoiced.
    pub fn detect(&mut self, frame: &[f32]) -> (Option<f32>, f32) {
        debug_assert_eq!(frame.len(), self.window());
        let w = self.window();
        let tau_max = self.tau_max;
        let tau_min = self.tau_min;

        // Difference function. The caller passes exactly `w` samples (not
        // `w + tau_max` like the sidecar's streaming frames), so `j + tau`
        // is bounded by summing over `0..w - tau` instead of the full `0..w`.
        self.d[0] = 1.0;
        for tau in 1..=tau_max {
            let mut acc = 0.0f32;
            for j in 0..(w - tau) {
                let diff = frame[j] - frame[j + tau];
                acc += diff * diff;
            }
            self.d[tau] = acc;
        }

        // Cumulative mean normalized difference.
        self.cmnd[0] = 1.0;
        let mut running = 0.0f32;
        for tau in 1..=tau_max {
            running += self.d[tau];
            self.cmnd[tau] = if running > 0.0 {
                self.d[tau] * tau as f32 / running
            } else {
                1.0
            };
        }

        // Absolute threshold: first sub-threshold dip, descended to its
        // local minimum. Falls back to the global minimum (reported as the
        // aperiodicity so the caller's gate can reject it) when nothing
        // crosses the threshold.
        let mut tau_est = None;
        let mut tau = tau_min;
        while tau < tau_max {
            if self.cmnd[tau] < YIN_THRESHOLD {
                while tau + 1 <= tau_max - 1 && self.cmnd[tau + 1] < self.cmnd[tau] {
                    tau += 1;
                }
                tau_est = Some(tau);
                break;
            }
            tau += 1;
        }
        let tau_est = tau_est.unwrap_or_else(|| {
            (tau_min..=tau_max)
                .min_by(|&a, &b| self.cmnd[a].partial_cmp(&self.cmnd[b]).unwrap())
                .unwrap()
        });
        let aperiodicity = self.cmnd[tau_est];

        // Parabolic interpolation around the chosen minimum for a
        // fractional lag.
        let mut t = tau_est as f32;
        if tau_est >= 1 && tau_est < tau_max {
            let a = self.cmnd[tau_est - 1];
            let b = self.cmnd[tau_est];
            let c = self.cmnd[tau_est + 1];
            let denom = a - 2.0 * b + c;
            if denom.abs() > 1e-12 {
                t += 0.5 * (a - c) / denom;
            }
        }

        if t <= 0.0 {
            return (None, aperiodicity);
        }
        let f0 = self.sr as f32 / t;
        let voiced = f0 >= FMIN && f0 <= FMAX && aperiodicity <= YIN_THRESHOLD;
        (voiced.then_some(f0), aperiodicity)
    }
}

pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

pub fn midi_to_hz(midi: f32) -> f32 {
    440.0 * 2f32.powf((midi - 69.0) / 12.0)
}

pub fn cents_between(hz: f32, ref_hz: f32) -> f32 {
    1200.0 * (hz / ref_hz).log2()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate `n` samples of a sine at `hz`, sample rate `TARGET_SR`.
    fn sine(hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / TARGET_SR as f32).sin())
            .collect()
    }

    fn detect_hz(signal: &[f32]) -> Option<f32> {
        let mut y = Yin::new(TARGET_SR);
        let w = y.window();
        y.detect(&signal[..w]).0
    }

    #[test]
    fn detects_pitch_within_25_cents_across_the_vocal_range() {
        for &hz in &[65.0f32, 98.0, 147.0, 220.0, 440.0, 880.0] {
            let s = sine(hz, 4096);
            let got = detect_hz(&s).unwrap_or_else(|| panic!("{hz} Hz read as unvoiced"));
            let err = cents_between(got, hz).abs();
            assert!(
                err < 25.0,
                "{hz} Hz detected as {got} Hz ({err:.1} cents off)"
            );
        }
    }

    #[test]
    fn silence_and_noise_are_unvoiced() {
        let mut y = Yin::new(TARGET_SR);
        let w = y.window();
        assert_eq!(y.detect(&vec![0.0; w]).0, None, "silence must be unvoiced");

        // Deterministic pseudo-noise: no rand dependency, no Math.random.
        let mut state = 0x2545F491_4F6CDD1Du64;
        let noise: Vec<f32> = (0..w)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state as u32 as f32 / u32::MAX as f32) * 2.0 - 1.0
            })
            .collect();
        let (f0, ap) = y.detect(&noise);
        assert!(
            f0.is_none() || ap > YIN_THRESHOLD,
            "white noise must not read as voiced"
        );
    }

    /// The failure mode that kills FFT-peak and HPS methods on low male
    /// voice: a sung vowel whose second harmonic is louder than its
    /// fundamental. A time-domain periodicity method must still return f0,
    /// not 2*f0.
    #[test]
    fn missing_fundamental_does_not_cause_an_octave_error() {
        let f0 = 110.0f32;
        let n = 4096;
        let s: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / TARGET_SR as f32;
                let w = 2.0 * std::f32::consts::PI * t;
                0.1 * (w * f0).sin() + 1.0 * (w * 2.0 * f0).sin() + 0.8 * (w * 3.0 * f0).sin()
            })
            .collect();
        let got = detect_hz(&s).expect("harmonic tone must be voiced");
        let err = cents_between(got, f0).abs();
        assert!(
            err < 50.0,
            "expected ~{f0} Hz, got {got} Hz ({err:.1} cents off)"
        );
    }

    #[test]
    fn out_of_range_pitches_are_rejected() {
        assert_eq!(detect_hz(&sine(40.0, 4096)), None, "below FMIN");
        assert_eq!(detect_hz(&sine(2000.0, 4096)), None, "above FMAX");
    }

    #[test]
    fn window_is_two_periods_of_fmin() {
        let y = Yin::new(TARGET_SR);
        let tau_max = (TARGET_SR as f32 / FMIN) as usize;
        assert_eq!(
            y.window(),
            2 * tau_max,
            "window must be 2 * tau_max, as hum_to_midi.py"
        );
    }

    #[test]
    fn midi_conversion_round_trips() {
        assert!((hz_to_midi(440.0) - 69.0).abs() < 1e-4);
        assert!((midi_to_hz(69.0) - 440.0).abs() < 1e-3);
        assert!((cents_between(440.0, 440.0)).abs() < 1e-4);
        assert!((cents_between(880.0, 440.0) - 1200.0).abs() < 1e-2);
    }
}
