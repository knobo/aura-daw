# Pitch Coach Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Live SingStar-style pitch feedback in AURA — sing against a MIDI reference melody, see how well you hit each note while you sing, and get a per-note report afterwards.

**Architecture:** An `InputHub` in the Rust audio engine decouples microphone-stream lifetime from recording lifetime, so the mic can be open for listening, rehearsing, or recording. The input callback decimates 48 kHz → 8 kHz and pushes to a ring; a dedicated non-RT pitch thread runs YIN and emits 100 Hz `PitchFrame`s to the control plane, which batches them to the frontend over a Tauri `Channel` at 60 Hz. Scoring against the MIDI reference runs on demand in the control plane. The Svelte panel is a single canvas with an rAF loop that renders pushed state and computes nothing.

**Tech Stack:** Rust (Tauri v2, cpal, rtrb, hound), Svelte 5 runes, TypeScript strict, Tailwind v4, vitest, cargo test.

**Spec:** `docs/superpowers/specs/2026-08-16-pitch-coach-design.md`

## Global Constraints

- **RT discipline (CONTRIBUTING rule 1).** No allocation, lock, syscall, logging, panic, or unbounded loop inside the cpal input callback. Reviewers hold the line on `src-tauri/src/audio/`.
- **Frozen files (CONTRIBUTING rule 4).** Never edit `src-tauri/src/lib.rs`, `Cargo.toml`, `tauri.conf.json`, `capabilities/default.json`, `build.rs`, `package.json`, `vite.config.ts`, `svelte.config.js`, `tsconfig.json`, `index.html`. Task 6 needs commands registered in `lib.rs` — that is a **request to the owner**, not an edit.
- **Additive IPC only (CONTRIBUTING rule 2).** New commands, new events, new schemas. New schemas must NOT set `additionalProperties: false` (debt item D-06). The only change to an existing event is a new **optional** `rehearseSpans` field on `recording://state`.
- **Thin renderer (ADR 0006).** No authoritative state, business logic, or time math on the frontend. The UI renders pushed state and emits gestures.
- **Time model (ADR 0002).** Audio positions are samples, musical positions are integer ticks at PPQ 960. The RT thread never does tempo math. The only tick↔sample bijection is `midi::tempo::TempoMap`.
- **Canvas pointer math.** Every canvas pointer handler must use `canvasPos()` from `src/lib/utils/canvas-pos.ts`. Raw `clientX - rect.left` is a known bug class here (PRs #11, #33).
- **Detector constants are copied verbatim** from `sidecars/hum_to_midi.py` so the live trainer and hum-to-MIDI never disagree: `TARGET_SR = 8000`, `FRAME_HOP_S = 0.010`, `FMIN = 65.0`, `FMAX = 1000.0`, `YIN_THRESHOLD = 0.20`, `RMS_GATE_REL = 0.05`, window `w = 2 * tau_max`.
- **Commits:** Conventional Commits with a scope, e.g. `feat(pitch): ...`. End every commit message with `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
- **Branch:** `feat/pitch-coach` in worktree `/home/knobo/prog/dav/.claude/worktrees/pitch-coach`. Baseline at plan time: 792 Rust tests pass, 440 frontend tests pass.
- **Pre-PR gate:** `cd src-tauri && cargo test`, `npm test`, `npx svelte-check`, `npm run build`.

---

## File Structure

**Phase 1 — backend**

| File | Responsibility |
|---|---|
| `src-tauri/src/audio/yin.rs` (create) | Pure YIN detector. No I/O, no threads, no engine types. Fully unit-testable. |
| `src-tauri/src/audio/decimate.rs` (create) | Fixed-ratio anti-aliased decimation to 8 kHz. Preallocated, no allocation in `process`. |
| `src-tauri/src/audio/pitch.rs` (create) | `PitchFrame`, the pitch thread, gating, median filter, jump limiter, timestamping. |
| `src-tauri/src/audio/engine.rs` (modify) | `InputHub`: stream lifetime split from take lifetime; `InputCb` always feeds the pitch tap; rehearse-hold silences the record sinks. |
| `src-tauri/src/audio/mod.rs` (modify) | The new `#[tauri::command]` functions. |
| `docs/ipc-schemas/pitch-frame.schema.json` (create) | Wire schema for a frame batch. |
| `docs/ipc-schemas/pitch-state.schema.json` (create) | Wire schema for `pitch://state`. |

**Phase 2 — panel**

| File | Responsibility |
|---|---|
| `src/lib/types/ipc.ts` (modify) | `PitchFrame`, `PitchFrameBatch`, `PitchState`, `PitchScoreReport`, `NoteScore` types. |
| `src/lib/tauri.ts` (modify) | Backend method declarations + implementations. |
| `src/lib/state/pitch.svelte.ts` (create) | Non-reactive frame bus; reactive mode flags only. |
| `src/lib/pitch/lane.ts` (create) | Pure drawing helpers: auto-fit range, cents→colour, trail segmentation. Node-testable, no DOM. |
| `src/lib/components/pitch/PitchCoach.svelte` (create) | The lane canvas + rAF loop + tuner mode. |
| `src/lib/components/pitch/RehearseButton.svelte` (create) | Press-and-hold button. |
| `src/lib/prefs/schema.ts` (modify) | Five new preferences. |
| `src/App.svelte` (modify) | Bottom-panel tab strip; the `H` key handler. |

**Phase 3 — scoring**

| File | Responsibility |
|---|---|
| `src-tauri/src/midi/schedule.rs` (modify) | Extract the repeat-expansion rule into one shared helper. |
| `src-tauri/src/control/pitch_coach.rs` (create) | Reference-melody derivation, per-frame comparison, `NoteScore`, `PitchScoreReport`. |
| `src-tauri/src/audio/pitch_store.rs` (create) | `cache/pitch/<clipId>.bin` read/write. |
| `src-tauri/src/audio/recorder.rs` (modify) | Fold the pitch track alongside the WAV. |
| `docs/ipc-schemas/pitch-score-report.schema.json` (create) | Wire schema. |
| `src/lib/components/pitch/PitchReport.svelte` (create) | Per-note strip. |

---

# Phase 1 — Backend

## Task 1: YIN detector

**Files:**
- Create: `src-tauri/src/audio/yin.rs`
- Modify: `src-tauri/src/audio/mod.rs` (add `pub mod yin;`)
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/audio/yin.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  ```rust
  pub const TARGET_SR: u32 = 8_000;
  pub const FRAME_HOP_S: f32 = 0.010;
  pub const FMIN: f32 = 65.0;
  pub const FMAX: f32 = 1000.0;
  pub const YIN_THRESHOLD: f32 = 0.20;

  pub struct Yin { /* preallocated buffers */ }
  impl Yin {
      pub fn new(sr: u32) -> Self;
      /// Integration window in samples (`2 * tau_max`) — the number of
      /// terms summed per lag.
      pub fn window(&self) -> usize;
      /// Samples `detect` must be given: `window() + tau_max`. The extra
      /// `tau_max` is lookahead, so EVERY lag sums the same `window()`
      /// terms. Letting the term count shrink with `tau` biases the
      /// difference function downward at large lags — an octave-down error
      /// on exactly the low male voice this is for.
      pub fn frame_len(&self) -> usize;
      /// `frame.len()` must be at least `self.frame_len()`.
      /// Returns `(f0_hz, aperiodicity)`; `f0_hz` is `None` when unvoiced.
      pub fn detect(&mut self, frame: &[f32]) -> (Option<f32>, f32);
  }
  pub fn hz_to_midi(hz: f32) -> f32;
  pub fn midi_to_hz(midi: f32) -> f32;
  pub fn cents_between(hz: f32, ref_hz: f32) -> f32;
  ```

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/audio/yin.rs` with only the constants, empty struct, and `unimplemented!()` bodies, plus:

```rust
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
        let n = y.frame_len();
        y.detect(&signal[..n]).0
    }

    #[test]
    fn detects_pitch_within_25_cents_across_the_vocal_range() {
        // 66 Hz, not 65: exactly FMIN is unreachable by construction, which
        // `the_effective_floor_sits_just_above_fmin` documents below.
        for &hz in &[66.0f32, 98.0, 147.0, 220.0, 440.0, 880.0] {
            let s = sine(hz, 4096);
            let got = detect_hz(&s).unwrap_or_else(|| panic!("{hz} Hz read as unvoiced"));
            let err = cents_between(got, hz).abs();
            assert!(err < 25.0, "{hz} Hz detected as {got} Hz ({err:.1} cents off)");
        }
    }

    #[test]
    fn silence_and_noise_are_unvoiced() {
        let mut y = Yin::new(TARGET_SR);
        let w = y.frame_len();
        // Guard against a stub: this same detector must still find a real
        // tone, so "unvoiced" below cannot be an always-None shortcut.
        assert!(y.detect(&sine(220.0, 4096)[..w]).0.is_some(), "harness sanity");
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
        assert!(f0.is_none() || ap > YIN_THRESHOLD, "white noise must not read as voiced");
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
        assert!(err < 50.0, "expected ~{f0} Hz, got {got} Hz ({err:.1} cents off)");
    }

    /// The searchable floor is `sr / tau_max`, not `FMIN` itself. `tau_max
    /// = (sr / FMIN) as usize` truncates 123.07 to 123, so the longest lag
    /// the search can reach is 8000/123 = 65.04 Hz. A tone at exactly FMIN
    /// interpolates a hair below that and the range gate rejects it.
    ///
    /// `sidecars/hum_to_midi.py` behaves identically — verified by running
    /// its own `_frame_pitch_py` on the same tone. Parity with the sidecar
    /// is the binding requirement (spec §3.2), so this is documented
    /// rather than "fixed": widening `tau_max` here would make the live
    /// detector and hum-to-MIDI disagree at the bottom of the range, which
    /// is the one thing copying the constants exists to prevent.
    ///
    /// Musically this costs nothing — 65 Hz is C2, below any sung note
    /// this feature targets.
    #[test]
    fn the_effective_floor_sits_just_above_fmin() {
        let tau_max = (TARGET_SR as f32 / FMIN) as usize;
        let floor = TARGET_SR as f32 / tau_max as f32;
        assert!(floor > FMIN, "the floor {floor} must sit just above FMIN {FMIN}");
        assert!(detect_hz(&sine(FMIN, 4096)).is_none(), "exactly FMIN is unreachable");
        assert!(detect_hz(&sine(floor + 0.5, 4096)).is_some(), "just above the floor is reachable");
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
        assert_eq!(y.window(), 2 * tau_max, "window must be 2 * tau_max, as hum_to_midi.py");
        assert_eq!(
            y.frame_len(),
            2 * tau_max + tau_max,
            "frame_len must be w + tau_max, as pitch_track's frame_len — the \
             lookahead that keeps every lag summing the same number of terms"
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
```

Add `pub mod yin;` to `src-tauri/src/audio/mod.rs`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test audio::yin`
Expected: FAIL — `unimplemented!()` panics in every test.

- [ ] **Step 3: Implement the detector**

Port `_frame_pitch_py` from `sidecars/hum_to_midi.py:200-235`. The algorithm, in order:

1. `tau_min = max(2, sr / FMAX)`, `tau_max = sr / FMIN`, `w = 2 * tau_max`.
2. Difference function `d[tau] = sum over j in 0..w of (x[j] - x[j+tau])^2` for `tau` in `1..=tau_max`, with `d[0] = 1.0`. The caller passes `frame_len() = w + tau_max` samples, so `j + tau` reaches at most `w - 1 + tau_max`, the last valid index — **every lag sums exactly `w` terms**. Do not shorten the sum to `0..w-tau`: a term count that shrinks with `tau` makes `d[tau]` fall off at large lags, which CMNDF's normalization then reads as a better period, producing octave-down errors on low voices. This matches `_frame_pitch_py` and `pitch_track`'s `frame_len = w + tau_max` exactly.
3. Cumulative mean normalized difference: `cmnd[0] = 1.0`; running sum `s += d[tau]`; `cmnd[tau] = d[tau] * tau / s` (guard `s == 0` → `cmnd[tau] = 1.0`).
4. Absolute threshold: walk `tau` from `tau_min` to `tau_max`, and on the first `cmnd[tau] < YIN_THRESHOLD`, descend to the local minimum (`while cmnd[tau+1] < cmnd[tau] { tau += 1 }`) and take it. If no `tau` crosses the threshold, take the global minimum over `tau_min..=tau_max` and report its `cmnd` as the aperiodicity — the caller's gate rejects it.
5. Parabolic interpolation around the chosen `tau` using `cmnd[tau-1]`, `cmnd[tau]`, `cmnd[tau+1]` (skip when `tau` is at either edge).
6. `f0 = sr / tau_interp`. Return `None` when `f0 < FMIN`, `f0 > FMAX`, or aperiodicity `> YIN_THRESHOLD`.

All scratch buffers (`d`, `cmnd`) live in `Yin` and are allocated in `new`, never in `detect`.

```rust
pub fn hz_to_midi(hz: f32) -> f32 { 69.0 + 12.0 * (hz / 440.0).log2() }
pub fn midi_to_hz(midi: f32) -> f32 { 440.0 * 2f32.powf((midi - 69.0) / 12.0) }
pub fn cents_between(hz: f32, ref_hz: f32) -> f32 { 1200.0 * (hz / ref_hz).log2() }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test audio::yin`
Expected: PASS, 6 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/yin.rs src-tauri/src/audio/mod.rs
git commit -m "feat(pitch): YIN detector ported from the hum sidecar"
```

---

## Task 2: Decimation to 8 kHz

**Files:**
- Create: `src-tauri/src/audio/decimate.rs`
- Modify: `src-tauri/src/audio/mod.rs` (add `pub mod decimate;`)
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `audio::yin::TARGET_SR`.
- Produces:
  ```rust
  pub struct Decimator { /* preallocated */ }
  impl Decimator {
      /// `in_rate` is the device rate. Any rate is accepted; the ratio need
      /// not be an integer.
      pub fn new(in_rate: u32) -> Self;
      /// Push interleaved input, appending decimated mono 8 kHz samples to
      /// `out`. Never allocates beyond `out`'s own growth.
      pub fn process(&mut self, input: &[f32], channels: usize, out: &mut Vec<f32>);
      pub fn reset(&mut self);
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::yin::TARGET_SR;

    fn sine(hz: f32, sr: u32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / sr as f32).sin()).collect()
    }

    #[test]
    fn output_rate_is_target_sr() {
        let mut d = Decimator::new(48_000);
        let mut out = Vec::new();
        d.process(&sine(220.0, 48_000, 48_000), 1, &mut out);
        let ratio = out.len() as f32 / TARGET_SR as f32;
        assert!((ratio - 1.0).abs() < 0.02, "1 s in must give ~1 s at 8 kHz, got {} samples", out.len());
    }

    #[test]
    fn preserves_pitch_through_the_detector() {
        let mut d = Decimator::new(48_000);
        let mut out = Vec::new();
        d.process(&sine(220.0, 48_000, 48_000), 1, &mut out);
        let mut y = crate::audio::yin::Yin::new(TARGET_SR);
        let n = y.frame_len();
        let f0 = y.detect(&out[out.len() / 2..out.len() / 2 + n]).0.expect("voiced");
        assert!(crate::audio::yin::cents_between(f0, 220.0).abs() < 25.0, "got {f0} Hz");
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
        assert!(rms < 0.1, "9 kHz must be attenuated, not aliased in; rms {rms}");
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
        assert!(peak <= 1.2, "channel sum must average, not add; peak {peak}");
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test audio::decimate`
Expected: FAIL.

- [ ] **Step 3: Implement**

Structure:
1. **Mono sum:** average the `channels` interleaved samples per frame (average, not add, so a stereo mic does not double amplitude).
2. **80 Hz one-pole high-pass**, to strip rumble and DC before analysis: `y[n] = a * (y[n-1] + x[n] - x[n-1])` with `a = 1 / (1 + 2*PI*80/in_rate)`. State persists across `process` calls.
3. **Anti-alias low-pass** at `0.4 * TARGET_SR` (3.2 kHz), a 63-tap windowed-sinc FIR with a Hamming window, coefficients computed once in `new`. Keep a `history: Vec<f32>` ring of `taps` samples so chunk boundaries are seamless.
4. **Resample** by accumulating a fractional phase `pos += TARGET_SR / in_rate` and emitting a linearly interpolated output sample each time the phase crosses an integer. This handles non-integer ratios (44.1 kHz) as well as 6×.

`process` must not allocate: the FIR history and phase state live in the struct; only `out` grows, and callers reuse one `Vec`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test audio::decimate`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/decimate.rs src-tauri/src/audio/mod.rs
git commit -m "feat(pitch): anti-aliased decimation to the 8 kHz analysis rate"
```

---

## Task 3: Pitch frames — gating, smoothing, timestamping

**Files:**
- Create: `src-tauri/src/audio/pitch.rs`
- Modify: `src-tauri/src/audio/mod.rs` (add `pub mod pitch;`)
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `yin::{Yin, hz_to_midi, TARGET_SR, FRAME_HOP_S, YIN_THRESHOLD}`.
- Produces:
  ```rust
  pub const RMS_GATE_REL: f32 = 0.05;
  pub const MEDIAN_TAPS: usize = 5;
  pub const MAX_JUMP_SEMITONES: f32 = 3.0;

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
  /// Pure and synchronous — the thread that drives it is Task 5's concern.
  pub struct PitchAnalyzer { /* ... */ }
  impl PitchAnalyzer {
      pub fn new() -> Self;
      /// `start_sample` is the transport sample position of `input[0]`, in
      /// DEVICE-rate samples. Appends one frame per completed hop.
      pub fn push(&mut self, input: &[f32], start_sample: u64, device_rate: u32, out: &mut Vec<PitchFrame>);
      pub fn reset(&mut self);
  }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::yin::{cents_between, TARGET_SR};

    fn sine_8k(hz: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / TARGET_SR as f32).sin()).collect()
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
        assert!(frames.len() >= 90 && frames.len() <= 100, "expected ~100 frames, got {}", frames.len());
    }

    #[test]
    fn voiced_frames_carry_pitch_and_midi() {
        let frames = frames_for(&sine_8k(220.0, TARGET_SR as usize));
        let voiced: Vec<_> = frames.iter().filter(|f| f.voiced).collect();
        assert!(!voiced.is_empty(), "a steady 220 Hz tone must produce voiced frames");
        for f in voiced {
            assert!(cents_between(f.hz, 220.0).abs() < 25.0, "{} Hz", f.hz);
            assert!((f.midi - 57.0).abs() < 0.3, "A3 is MIDI 57, got {}", f.midi);
            assert!(f.clarity > 0.0 && f.clarity <= 1.0);
        }
    }

    #[test]
    fn silence_is_unvoiced() {
        let frames = frames_for(&vec![0.0f32; TARGET_SR as usize]);
        assert!(frames.iter().all(|f| !f.voiced), "silence must never be voiced");
    }

    /// Breath and bleed sit far below the loud reference; the relative RMS
    /// gate is what stops them being drawn as notes.
    ///
    /// CORRECTED during Task 3. The original assertion was `voiced == 0`,
    /// which no correct implementation can satisfy: `push` carries a full
    /// analysis frame of history across the call boundary, so the first
    /// windows after the level drop still literally CONTAIN the loud tone
    /// (measured: frames 0-3 at rms 0.71 down to 0.40) and the 5-tap median
    /// holds voicing one frame past that (frame 4). Frames 5-99 were gated
    /// correctly. The property that matters is the quiet STEADY STATE, so
    /// the test now bounds voicing to a settling window instead.
    #[test]
    fn quiet_signal_below_the_relative_gate_is_unvoiced() {
        /// Worst case: the carried-over frame is one hop short of full, so
        /// `frame_len / hop` (5) windows can still hold the loud tail, and
        /// the median reports while 3 of the last MEDIAN_TAPS are voiced —
        /// 3 more. 10 leaves margin and still asserts over 90 frames.
        const SETTLE_FRAMES: usize = 10;

        let mut a = PitchAnalyzer::new();
        let mut out = Vec::new();
        a.push(&sine_8k(220.0, TARGET_SR as usize), 0, 48_000, &mut out);
        // Guard against an always-unvoiced stub.
        assert!(out.iter().any(|f| f.voiced), "the loud reference must itself be voiced");

        out.clear();
        let quiet: Vec<f32> = sine_8k(220.0, TARGET_SR as usize).iter().map(|s| s * 0.005).collect();
        a.push(&quiet, TARGET_SR as u64, 48_000, &mut out);

        let voiced: Vec<usize> =
            out.iter().enumerate().filter(|(_, f)| f.voiced).map(|(i, _)| i).collect();
        assert!(
            voiced.iter().all(|&i| i < SETTLE_FRAMES),
            "a signal 46 dB below the loud reference must be gated out once the \
             analysis window has moved past the loud tail; voiced frames were \
             {voiced:?} of {}",
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
        assert!(high <= 1, "a two-hop blip must not survive smoothing; {high} frames jumped");
    }

    /// Frames must land where the SOUND was, not where the analyser
    /// finished — the window's own group delay is subtracted.
    #[test]
    fn frames_are_timestamped_behind_the_analysis_window() {
        let frames = frames_for(&sine_8k(220.0, TARGET_SR as usize));
        let first = frames.first().expect("at least one frame");
        let mut a = PitchAnalyzer::new();
        let half_window_device = (a.window_samples() as u64 * 48_000 as u64) / (2 * TARGET_SR as u64);
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
            assert_eq!(w[1].sample - w[0].sample, hop_device, "hop must be exactly 10 ms of device samples");
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
        assert!(elapsed < 1.0, "10 s of audio took {elapsed:.3} s to analyse");
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
```

Also add `pub fn window_samples(&self) -> usize` to the interface (used by the timestamp test).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd src-tauri && cargo test audio::pitch`
Expected: FAIL.

- [ ] **Step 3: Implement**

`PitchAnalyzer` holds: a `Yin`, an input ring `Vec<f32>` of at least `frame_len + hop`, a write cursor, the loud-reference tracker, a `[Option<f32>; MEDIAN_TAPS]` history, the last accepted MIDI value, and the running frame index.

`push` loop, per completed hop:

1. Copy the newest `yin.frame_len()` samples into a scratch buffer and call `yin.detect`. Note `frame_len` is `window + tau_max`: the trailing `tau_max` samples are lookahead for the difference function, not signal being measured. RMS and the timestamp are therefore taken over the leading `window()` samples only, matching `pitch_track`'s `frame[:w]`.
2. **Loud reference:** track a decaying maximum of frame RMS — `loud = loud.max(rms)` then `loud *= 0.9995` each frame (the sidecar uses a 95th percentile of the whole file, which is not available online; a decaying max is the streaming equivalent and must be commented as such). Gate: `rms > (loud * RMS_GATE_REL).max(1e-4)`.
3. Voiced if the detector returned `Some`, aperiodicity `< YIN_THRESHOLD`, and the RMS gate passes.
4. **Median filter:** push the MIDI value (or `None`) into the 5-tap history; the reported value is the median of the voiced entries. Fewer than 3 voiced entries in the window → report unvoiced.
5. **Jump limiter:** if the median moved more than `MAX_JUMP_SEMITONES` from the last accepted value, mark this frame unvoiced and remember the candidate; if the next frame agrees with the candidate within 1 semitone, accept the jump (this is what lets a real leap through while killing a one-frame blip). When the median itself goes unvoiced — a gap long enough to empty the 5-tap window, i.e. a real breath — drop the last-accepted reference, so the next phrase is not rate-limited against the note before the breath.
6. **Timestamp:** `sample = start_of_this_window_in_device_samples + half_window_in_device_samples`, i.e. the centre of the analysis window, expressed in device-rate samples. Emit `hz`, `midi = hz_to_midi(hz)`, `clarity = 1.0 - aperiodicity`, `rms`, `voiced`.

An unvoiced frame is still emitted (with `voiced: false`) — the UI needs it to break the trail rather than interpolate across a breath.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test audio::pitch`
Expected: PASS, 9 tests (the step-1 block defines nine, not eight).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/pitch.rs src-tauri/src/audio/mod.rs
git commit -m "feat(pitch): frame gating, median smoothing and window-centred timestamps"
```

---

## Task 4: Cross-check the Rust detector against the Python sidecar

**Files:**
- Create: `src-tauri/tests/pitch_sidecar_parity.rs`
- Test: that file

**Interfaces:**
- Consumes: `aura_lib::audio::{yin, decimate, pitch}` — the lib target is
  named `aura_lib`, not `aura` (Cargo.toml `[lib] name`); integration tests
  must use that path.
- Produces: nothing consumed by later tasks. This is a guard, not a component.

**Corrections from executing it (see commit `c7e9555`):**

- Compare the sidecar's **median-smoothed** track, not its raw one. The
  Rust analyser smooths before it reports, so comparing against raw
  `pitch_track` output measures the smoother, not the detector.
- The live median is **causal**; the sidecar's `median_filter_track` is
  **centred**. The live track therefore lags by exactly the half-kernel
  (`MEDIAN_TAPS / 2` = 2 frames = 20 ms). Align by that shift. Measured
  worst-case disagreement by shift: `[29.3, 15.4, 3.3, 13.7, 26.6]` cents —
  a clean minimum at 2.
- With the lag removed the tolerance is **10 cents, not 30**. 30 would have
  passed at *any* alignment and so tested nothing; 10 fails at every
  alignment but the right one.
- A second test sweeps the alignment and asserts the minimum sits at the
  half-kernel, so adding another smoothing stage to the live path fails
  loudly instead of hiding inside the tolerance.

**Why this exists:** the spec's whole argument for copying the sidecar's constants is that the live trainer and hum-to-MIDI must never disagree about what the singer sang. That claim needs a test, or it is just a comment.

- [ ] **Step 1: Write the failing test**

```rust
//! Parity guard: the Rust live detector and `sidecars/hum_to_midi.py` must
//! agree on the same audio. They are not bit-identical by design — the
//! sidecar resamples linearly while the Rust path uses an anti-aliased FIR
//! — so agreement is asserted within 30 cents on frames both call voiced.
//!
//! Skipped (not failed) when no Python interpreter with numpy is on PATH,
//! so the suite stays green in environments without the sidecar venv.

use std::process::Command;

fn python() -> Option<String> {
    for cand in ["python3", "python"] {
        let ok = Command::new(cand)
            .args(["-c", "import numpy"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(cand.to_string());
        }
    }
    None
}

#[test]
fn rust_and_sidecar_agree_within_thirty_cents() {
    let Some(py) = python() else {
        eprintln!("skipping: no python3 with numpy on PATH");
        return;
    };
    // 1. Write a 3 s WAV: a slow glide 130 Hz -> 260 Hz with vibrato.
    // 2. Run `sidecars/hum_to_midi.py --pitch-track <wav>` and parse its
    //    NDJSON pitch track.
    // 3. Run the Rust decimator + PitchAnalyzer over the same WAV.
    // 4. For every 10 ms frame both call voiced, assert the cents
    //    difference is under 30.
    // 5. Assert at least 60% of frames are voiced in both, so a detector
    //    that silently reports nothing cannot pass.
    todo!("implement per the steps above")
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd src-tauri && cargo test --test pitch_sidecar_parity`
Expected: FAIL on the `todo!`.

- [ ] **Step 3: Implement**

Check whether `sidecars/hum_to_midi.py` already exposes a raw pitch-track mode. It does not (checked), so add a `--pitch-track` flag that prints one NDJSON object per frame (`{"t": <seconds>, "hz": <float|null>, "midi": <float|null>, "rms": <float>}`) by calling the existing `pitch_track(samples, sr)` and returning early — an additive flag that changes no existing behaviour, with the existing `--self-test` still passing. `midi` is the value after `median_filter_track`, which is the stage the Rust side must be compared against; `hz` is raw, and is what tells you whether a disagreement came from detection or from smoothing.

Write the WAV with `hound` (already a dependency) into `std::env::temp_dir()`, and delete it at the end of the test.

- [ ] **Step 4: Run to verify it passes**

Run: `cd src-tauri && cargo test --test pitch_sidecar_parity -- --nocapture`
Expected: PASS. If Python is unavailable it prints the skip line and passes; note in the commit message which of the two happened on your machine.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/tests/pitch_sidecar_parity.rs sidecars/hum_to_midi.py
git commit -m "test(pitch): parity guard between the Rust detector and the hum sidecar"
```

---

## Task 5: InputHub — decouple the mic from the take

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` (`InputBundle` at ~`:257`, `InputCb` at ~`:646`, `open_capture_group` at ~`:1788`, `start_recording` at ~`:1888`)
- Test: inline `#[cfg(test)] mod tests` in `src-tauri/src/audio/engine.rs`

**Interfaces:**
- Consumes: `pitch::{PitchAnalyzer, PitchFrame}`, `decimate::Decimator`.
- Produces:
  ```rust
  /// What the input stream is currently for. The hub opens when this is
  /// non-empty and closes when it empties.
  #[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
  pub struct InputWants { pub listening: bool, pub recording: bool }
  impl InputWants { pub fn any(&self) -> bool; }

  impl EngineControl {
      /// Idempotent. Opens, rebuilds, or closes the input stream so it
      /// matches `wants` plus the current record targets.
      fn sync_input_hub(&mut self) -> Result<(), String>;
      pub fn set_listening(&mut self, on: bool) -> Result<(), String>;
      pub fn set_rehearse_hold(&mut self, on: bool);
  }
  ```

**This is the risky task.** It sits in the most sensitive file in the repo. Read `CONTRIBUTING.md` rule 1 before starting, and keep `InputCb::capture` free of allocation, locks, and logging.

- [ ] **Step 1: Write the failing tests**

Add to `engine.rs`'s test module. These use the existing engine test harness — find how the current recording tests construct an `EngineControl` and follow that exactly; do not invent a second harness.

```rust
#[test]
fn listening_opens_and_closes_the_input_stream() {
    // set_listening(true) -> hub open; set_listening(false) -> hub closed.
    // Assert on the hub's presence (`self.input_hub.is_some()`), not on
    // cpal, so this runs without an audio device.
}

#[test]
fn recording_keeps_the_hub_open_when_listening_stops() {
    // listen on -> record start -> listen off -> hub must stay open,
    // because the take still wants it.
}

#[test]
fn stopping_the_take_while_listening_keeps_the_hub_open() {
    // The mirror of the above: a take ending must not silence the tuner.
}

#[test]
fn rehearse_hold_writes_silence_for_exactly_the_held_span() {
    // Drive InputCb::capture with a constant 1.0 signal, toggling
    // rehearse_hold partway. Assert the consumer side reads:
    //   - non-zero before the hold,
    //   - exactly zero for the held sample count,
    //   - non-zero after,
    // and that the TOTAL sample count is unchanged — the take must stay
    // sample-aligned, which is the whole reason this writes silence
    // rather than skipping.
}

#[test]
fn rehearse_hold_spans_are_reported() {
    // After a take with one hold, the collected spans are exactly one
    // {start_sample, end_sample} pair matching the held region.
}

#[test]
fn pitch_frames_are_produced_while_listening_without_recording() {
    // With the hub in listening mode and no record targets, feeding
    // InputCb::capture a 220 Hz tone must yield voiced PitchFrames on the
    // frame ring, and must NOT create any DiskWriter.
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test audio::engine`
Expected: FAIL — the new methods do not exist.

- [ ] **Step 3: Implement**

1. **Split `open_capture_group`.** Extract the device-resolution half (host lookup, `default_input_config`, the F32 check, `in_ch`/`rec_ch`/`rate`) into `fn resolve_input_device(&self, device_id: Option<&str>) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String>`. The writer-spawning half stays where it is. This extraction alone should keep all existing tests green — run them before going further.

2. **Add the hub.** A new field on `EngineControl`:
   ```rust
   struct InputHub {
       _stream: cpal::Stream,
       meter_rx: rtrb::Consumer<RawMeterBlock>,
       pitch_rx: rtrb::Consumer<PitchFrame>,
       device_key: String,
       wants: InputWants,
   }
   ```
   `InputBundle` is replaced by this; keep the `meter_rx` drain exactly as it is today.

3. **`InputCb` gains** `decimator: Decimator`, `analyzer: PitchAnalyzer`, `pitch_tx: rtrb::Producer<PitchFrame>`, `dec_buf: Vec<f32>`, `frame_buf: Vec<PitchFrame>`, and `rehearse: Arc<AtomicBool>`. In `capture`, after the existing meter block and **before** the producer fan-out:
   ```rust
   self.dec_buf.clear();
   self.decimator.process(data, self.in_ch, &mut self.dec_buf);
   self.frame_buf.clear();
   self.analyzer.push(&self.dec_buf, pos, self.rate, &mut self.frame_buf);
   for f in self.frame_buf.iter() { let _ = self.pitch_tx.push(*f); }
   ```
   `dec_buf` and `frame_buf` are `Vec`s allocated once at hub-open with `with_capacity` generous enough for the largest plausible buffer (reserve for 8192 input frames). `clear()` does not free, so steady-state `capture` never allocates. Add a comment saying exactly that, because a reader will otherwise flag the `Vec`s as an RT violation.

4. **Rehearse-hold.** In the producer fan-out, when `self.rehearse.load(Relaxed)` is true, write zeros instead of `data[...]` for the same frame count. The silence-debt logic (`owed`) is untouched — this writes a full-size chunk of zeros, so nothing is owed and the take stays aligned. Record the held span on the control side by sampling the flag's transitions against `shared.position`.

5. **`sync_input_hub`.** Computes the wanted device key (record targets' device, else `self.sel_input`, else default) and the wanted `InputWants`. If the hub is absent and something is wanted, open it. If present and nothing is wanted, drop it. If present and the device key changed, drop and reopen. **Reject a device change while recording** with `Err("cannot change input device during a take")`.

6. **`start_recording`** calls `sync_input_hub` instead of `open_capture_group` for the stream, then spawns writers and attaches their producers. Because the hub rebuilds on change, a listening hub is dropped and reopened with record sinks — accept the few-millisecond blip, and comment the reason (spec §3.1).

7. **Preserve every existing invariant.** `split_record_targets`, the count-in early return and `arm_pending_after_countin`, the silence-debt policy, the multi-device grouping, and the failure cleanup that drops already-opened groups. The existing recording tests are the regression net; if any goes red, the refactor is wrong, not the test.

- [ ] **Step 4: Run the full backend suite**

Run: `cd src-tauri && cargo test`
Expected: PASS — the 792 pre-existing tests plus the 6 new ones. Paste the `test result:` line; do not claim success without it.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio/engine.rs
git commit -m "feat(audio): input hub so the mic can open without a take"
```

---

## Task 6: Commands, events and schemas

**Files:**
- Modify: `src-tauri/src/audio/mod.rs`
- Create: `docs/ipc-schemas/pitch-frame.schema.json`, `docs/ipc-schemas/pitch-state.schema.json`
- Test: inline tests in `src-tauri/src/audio/mod.rs` plus a schema-shape test alongside the repo's existing schema tests

**Interfaces:**
- Consumes: `EngineControl::{set_listening, set_rehearse_hold}`, `PitchFrame`.
- Produces:
  ```rust
  #[tauri::command] pub fn pitch_listen_start(state: State<'_, AudioState>) -> Result<(), String>;
  #[tauri::command] pub fn pitch_listen_stop(state: State<'_, AudioState>) -> Result<(), String>;
  #[tauri::command] pub fn pitch_subscribe(channel: Channel<PitchFrameBatch>, state: State<'_, AudioState>) -> Result<(), String>;
  #[tauri::command] pub fn set_rehearse_hold(enabled: bool, state: State<'_, AudioState>) -> Result<(), String>;
  /// Stores the chosen reference MIDI track and re-emits `pitch://state`.
  /// The panel's track picker calls this; without it the picker has
  /// nowhere to put its choice.
  #[tauri::command] pub fn pitch_set_reference(track_id: Option<String>, state: State<'_, AudioState>) -> Result<(), String>;

  #[derive(Clone, serde::Serialize)]
  #[serde(rename_all = "camelCase")]
  pub struct PitchFrameBatch { pub frames: Vec<PitchFrame>, pub device_rate: u32, pub listening: bool, pub rehearse_hold: bool }
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn pitch_frame_batch_serializes_camel_case() {
    let b = PitchFrameBatch {
        frames: vec![PitchFrame { sample: 480, hz: 220.0, midi: 57.0, clarity: 0.9, rms: 0.2, voiced: true }],
        device_rate: 48_000,
        listening: true,
        rehearse_hold: false,
    };
    let v = serde_json::to_value(&b).unwrap();
    assert!(v.get("deviceRate").is_some(), "wire field must be deviceRate");
    assert!(v.get("rehearseHold").is_some());
    let f = &v["frames"][0];
    for key in ["sample", "hz", "midi", "clarity", "rms", "voiced"] {
        assert!(f.get(key).is_some(), "frame missing {key}");
    }
}

#[test]
fn new_pitch_schemas_stay_open_for_extension() {
    // D-06: new schemas must not close themselves to additive evolution.
    for name in ["pitch-frame", "pitch-state"] {
        let path = format!("../docs/ipc-schemas/{name}.schema.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["$schema"], "http://json-schema.org/draft-07/schema#");
        assert!(
            v.get("additionalProperties") != Some(&serde_json::Value::Bool(false)),
            "{name} must not set additionalProperties: false (D-06)"
        );
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test audio::mod` (or the module path the tests land in)
Expected: FAIL.

- [ ] **Step 3: Implement**

Follow `subscribe_meters` (`src-tauri/src/audio/mod.rs:373`) exactly for the channel plumbing — a `ControlMsg::Subscribe`-style sink, batched on the existing 60 Hz control-thread tick that already drains the meter ring. Drain the pitch ring on that same tick and push one `PitchFrameBatch` per tick; do not add a second timer.

Emit `pitch://state` with `{ listening, rehearseHold, referenceTrackId, deviceRate }` whenever any of those change.

Write the two schemas as draft-07, `"type": "object"`, documented `properties`, a `required` list covering only fields that are always present, and **no** `additionalProperties: false`.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS.

- [ ] **Step 5: Request registration in `lib.rs`**

`src-tauri/src/lib.rs` is FROZEN. **Stop here and ask the owner** to add these five commands to the `invoke_handler` list next to `audio::subscribe_meters` (`lib.rs:166`):

```
audio::pitch_listen_start,
audio::pitch_listen_stop,
audio::pitch_subscribe,
audio::set_rehearse_hold,
audio::pitch_set_reference,
```

Do not edit the file yourself, and do not proceed to Phase 2 until it is registered — the panel cannot be tested against unregistered commands.

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/audio/mod.rs docs/ipc-schemas/pitch-frame.schema.json docs/ipc-schemas/pitch-state.schema.json
git commit -m "feat(pitch): listen, subscribe and rehearse-hold commands"
```

---

## Phase 1 checkpoint

**Stop and hand back to the owner (spec R3).** Before any UI exists, demonstrate numerically that the backend works: with the app running (`PATH="$PWD/.venv-sidecars/bin:$PATH" npm run tauri dev`), start listening and log a few seconds of frames while singing or playing a known pitch. Report the detected Hz, the cents error against the expected note, and the voiced fraction. The owner validates by driving the app.

---

# Phase 2 — Panel

## Task 7: Types and backend bindings

**Files:**
- Modify: `src/lib/types/ipc.ts`, `src/lib/tauri.ts`
- Test: `src/lib/types/pitch.test.ts` (create)

**Interfaces:**
- Produces:
  ```ts
  export interface PitchFrame { sample: number; hz: number; midi: number; clarity: number; rms: number; voiced: boolean }
  export interface PitchFrameBatch { frames: PitchFrame[]; deviceRate: number; listening: boolean; rehearseHold: boolean }
  export interface PitchState { listening: boolean; rehearseHold: boolean; referenceTrackId: string | null; deviceRate: number }
  ```
  On `backend`:
  ```ts
  pitchListenStart(): Promise<void>;
  pitchListenStop(): Promise<void>;
  subscribePitch(onBatch: (b: PitchFrameBatch) => void): Promise<Unsubscribe>;
  setRehearseHold(enabled: boolean): Promise<void>;
  ```

- [ ] **Step 1: Write the failing test**

```ts
import { describe, it, expect } from "vitest";
import type { PitchFrame, PitchFrameBatch } from "../types/ipc";

describe("pitch wire types", () => {
  it("accepts a batch shaped exactly as the Rust serializer emits it", () => {
    const frame: PitchFrame = { sample: 480, hz: 220, midi: 57, clarity: 0.9, rms: 0.2, voiced: true };
    const batch: PitchFrameBatch = { frames: [frame], deviceRate: 48000, listening: true, rehearseHold: false };
    expect(batch.frames[0].midi).toBe(57);
    expect(batch.deviceRate).toBe(48000);
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npx vitest run src/lib/types/pitch.test.ts`
Expected: FAIL — the types do not exist.

- [ ] **Step 3: Implement**

Add the interfaces to `src/lib/types/ipc.ts`. Add the four methods to the `backend` interface (near `subscribeMeters`, `src/lib/tauri.ts:105`) and implement them in the concrete backend, copying the `subscribeMeters` channel pattern at `src/lib/tauri.ts:425`. Add matching no-op/mock implementations wherever `src/lib/demo.ts` provides the browser-only mock engine, so `npm run dev` still boots.

- [ ] **Step 4: Run to verify it passes**

Run: `npx vitest run src/lib/types/pitch.test.ts && npx svelte-check`
Expected: PASS, no new type errors.

- [ ] **Step 5: Commit**

```bash
git add src/lib/types/ipc.ts src/lib/tauri.ts src/lib/demo.ts src/lib/types/pitch.test.ts
git commit -m "feat(pitch): wire types and backend bindings"
```

---

## Task 8: Frame bus

**Files:**
- Create: `src/lib/state/pitch.svelte.ts`
- Test: `src/lib/state/pitch.svelte.test.ts`

**Interfaces:**
- Consumes: `backend.subscribePitch`, `PitchFrame`.
- Produces:
  ```ts
  /** Ring of the most recent frames. NOT reactive — read from rAF. */
  export function recentFrames(): readonly PitchFrame[];
  export function latestVoiced(): PitchFrame | null;
  export function framesBetween(startSample: number, endSample: number): PitchFrame[];
  export async function startPitchStream(): Promise<void>;
  export function stopPitchStream(): void;
  export const pitchMode: { listening: boolean; rehearseHold: boolean; referenceTrackId: string | null };
  ```

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, it, expect, beforeEach } from "vitest";
import { ingestBatch, recentFrames, latestVoiced, framesBetween, resetPitchBus, pitchMode } from "./pitch.svelte";
import type { PitchFrame } from "../types/ipc";

const f = (sample: number, midi: number, voiced = true): PitchFrame =>
  ({ sample, midi, hz: 440 * 2 ** ((midi - 69) / 12), clarity: 0.9, rms: 0.2, voiced });

describe("pitch frame bus", () => {
  beforeEach(() => resetPitchBus());

  it("keeps frames in arrival order", () => {
    ingestBatch({ frames: [f(0, 57), f(480, 57)], deviceRate: 48000, listening: true, rehearseHold: false });
    expect(recentFrames().map((x) => x.sample)).toEqual([0, 480]);
  });

  it("bounds the ring so a long session cannot grow without limit", () => {
    for (let i = 0; i < 5000; i++) {
      ingestBatch({ frames: [f(i * 480, 57)], deviceRate: 48000, listening: true, rehearseHold: false });
    }
    expect(recentFrames().length).toBeLessThanOrEqual(3000);
    // The newest frame must survive; the ring drops the oldest.
    expect(recentFrames()[recentFrames().length - 1].sample).toBe(4999 * 480);
  });

  it("latestVoiced skips unvoiced frames", () => {
    ingestBatch({ frames: [f(0, 57), f(480, 0, false)], deviceRate: 48000, listening: true, rehearseHold: false });
    expect(latestVoiced()?.sample).toBe(0);
  });

  it("framesBetween returns the half-open sample window", () => {
    ingestBatch({ frames: [f(0, 57), f(480, 58), f(960, 59)], deviceRate: 48000, listening: true, rehearseHold: false });
    expect(framesBetween(480, 960).map((x) => x.sample)).toEqual([480]);
  });

  it("mirrors mode flags from the batch", () => {
    ingestBatch({ frames: [], deviceRate: 48000, listening: true, rehearseHold: true });
    expect(pitchMode.rehearseHold).toBe(true);
  });
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/lib/state/pitch.svelte.test.ts`
Expected: FAIL.

- [ ] **Step 3: Implement**

Copy the shape of `src/lib/state/meters.svelte.ts` — module-level mutable state, no `$state` for the frames, an imperative read API. `ingestBatch` and `resetPitchBus` are exported so the bus is testable without a backend. The ring is a fixed-size array of 3000 frames (30 seconds at 100 Hz) with a write cursor. Only `pitchMode` uses `$state`, since mode changes are rare and the UI should re-render on them.

Write the file's doc comment to say why it is not reactive, matching the meters module.

- [ ] **Step 4: Run to verify they pass**

Run: `npx vitest run src/lib/state/pitch.svelte.test.ts`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src/lib/state/pitch.svelte.ts src/lib/state/pitch.svelte.test.ts
git commit -m "feat(pitch): non-reactive frame bus"
```

---

## Task 9: Lane drawing helpers

**Files:**
- Create: `src/lib/pitch/lane.ts`
- Test: `src/lib/pitch/lane.test.ts`

**Interfaces:**
- Produces:
  ```ts
  export interface TargetNote { noteId: number; startSample: number; endSample: number; key: number }
  export interface LaneRange { lowKey: number; highKey: number }
  export function autoFitRange(notes: TargetNote[], fallbackKey?: number): LaneRange;
  export function yOfKey(key: number, range: LaneRange, height: number): number;
  /** Signed cents from `midi` to the nearest occurrence of `key` in ANY octave. */
  export function centsToTarget(midi: number, key: number): number;
  export type PitchBand = "in" | "near" | "far";
  export function bandFor(cents: number, toleranceCents: number): PitchBand;
  /** Split frames into runs of consecutive voiced frames, so the trail breaks on silence. */
  export function trailSegments(frames: readonly PitchFrame[]): PitchFrame[][];
  ```

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, it, expect } from "vitest";
import { autoFitRange, yOfKey, centsToTarget, bandFor, trailSegments } from "./lane";
import type { PitchFrame } from "../types/ipc";

const note = (key: number) => ({ noteId: key, startSample: 0, endSample: 480, key });
const f = (sample: number, voiced: boolean): PitchFrame =>
  ({ sample, midi: 57, hz: 220, clarity: 0.9, rms: 0.2, voiced });

describe("autoFitRange", () => {
  it("pads two semitones either side of the phrase", () => {
    expect(autoFitRange([note(60), note(64)])).toEqual({ lowKey: 58, highKey: 66 });
  });

  it("falls back to a usable range when there are no notes", () => {
    const r = autoFitRange([], 60);
    expect(r.highKey - r.lowKey).toBeGreaterThanOrEqual(12);
    expect(r.lowKey).toBeLessThanOrEqual(60);
    expect(r.highKey).toBeGreaterThanOrEqual(60);
  });

  it("keeps a minimum span so a single note does not fill the lane", () => {
    const r = autoFitRange([note(60)]);
    expect(r.highKey - r.lowKey).toBeGreaterThanOrEqual(12);
  });
});

describe("yOfKey", () => {
  it("puts the high key at the top and the low key at the bottom", () => {
    const r = { lowKey: 55, highKey: 67 };
    expect(yOfKey(67, r, 120)).toBeLessThan(yOfKey(55, r, 120));
    expect(yOfKey(67, r, 120)).toBeCloseTo(0, 0);
    expect(yOfKey(55, r, 120)).toBeCloseTo(120, 0);
  });
});

describe("centsToTarget", () => {
  it("is zero on the note", () => {
    expect(centsToTarget(60, 60)).toBeCloseTo(0);
  });

  it("is signed", () => {
    expect(centsToTarget(60.25, 60)).toBeCloseTo(25);
    expect(centsToTarget(59.75, 60)).toBeCloseTo(-25);
  });

  it("folds octaves, so an octave slip reads as on-pitch", () => {
    expect(centsToTarget(72, 60)).toBeCloseTo(0);
    expect(centsToTarget(48, 60)).toBeCloseTo(0);
  });

  it("never exceeds half a semitone after folding", () => {
    for (let m = 40; m <= 90; m += 0.5) {
      expect(Math.abs(centsToTarget(m, 60))).toBeLessThanOrEqual(600.01);
    }
  });
});

describe("bandFor", () => {
  it("bands by the active tolerance", () => {
    expect(bandFor(10, 50)).toBe("in");
    expect(bandFor(-49, 50)).toBe("in");
    expect(bandFor(80, 50)).toBe("near");
    expect(bandFor(400, 50)).toBe("far");
  });
});

describe("trailSegments", () => {
  it("breaks the trail on unvoiced frames rather than interpolating", () => {
    const segs = trailSegments([f(0, true), f(1, true), f(2, false), f(3, true)]);
    expect(segs.map((s) => s.length)).toEqual([2, 1]);
  });

  it("returns nothing for an all-unvoiced run", () => {
    expect(trailSegments([f(0, false), f(1, false)])).toEqual([]);
  });
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/lib/pitch/lane.test.ts`
Expected: FAIL.

- [ ] **Step 3: Implement**

Pure functions, no DOM, no imports from Svelte or the backend. `centsToTarget` folds by adding or subtracting 12 semitones until the difference is within ±6, then multiplies by 100. `autoFitRange` takes min/max of `key`, pads by 2, and widens symmetrically to a minimum span of 12.

- [ ] **Step 4: Run to verify they pass**

Run: `npx vitest run src/lib/pitch/lane.test.ts`
Expected: PASS, 11 tests.

- [ ] **Step 5: Commit**

```bash
git add src/lib/pitch/lane.ts src/lib/pitch/lane.test.ts
git commit -m "feat(pitch): lane geometry, octave folding and trail segmentation"
```

---

## Task 10: Preferences

**Files:**
- Modify: `src/lib/prefs/schema.ts`
- Test: `src/lib/prefs/schema.test.ts`

**Interfaces:**
- Produces: `prefs.values.pitchTolerance`, `.pitchLatencyOffsetMs`, `.pitchLaneFollow`, `.pitchTrailSnap`, `.pitchRehearseKey`.

- [ ] **Step 1: Write the failing test**

Add to `src/lib/prefs/schema.test.ts`:

```ts
it("declares the pitch coach preferences with usable defaults", () => {
  expect(PREF_SCHEMA.pitchTolerance.default).toBe("standard");
  expect(PREF_SCHEMA.pitchLatencyOffsetMs.default).toBe(0);
  expect(PREF_SCHEMA.pitchLaneFollow.default).toBe("fixed");
  expect(PREF_SCHEMA.pitchTrailSnap.default).toBe(false);
  expect(PREF_SCHEMA.pitchRehearseKey.default).toBe("h");
});

it("maps every tolerance option to a cents value", () => {
  const ids = (PREF_SCHEMA.pitchTolerance as { options: { value: string }[] }).options.map((o) => o.value);
  expect(ids).toEqual(["forgiving", "standard", "strict", "pro"]);
  for (const id of ids) expect(TOLERANCE_CENTS[id]).toBeGreaterThan(0);
  expect(TOLERANCE_CENTS.standard).toBe(50);
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `npx vitest run src/lib/prefs/schema.test.ts`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add the fields to `interface PrefValues` and the entries to `PREF_SCHEMA`, following the existing `countInBars` and `metronomeGain` entries exactly. Also export:

```ts
export const TOLERANCE_CENTS: Record<string, number> = {
  forgiving: 100, standard: 50, strict: 33, pro: 20,
};
```

Labels and blurbs, written plainly:
- `pitchTolerance` / "Pitch tolerance" / "How close you have to be for a note to count as hit. ±50 cents is the standard research criterion for the right pitch class." — category `editing`, options `FORGIVING ±100` / `STANDARD ±50` / `STRICT ±33` / `PRO ±20`.
- `pitchLatencyOffsetMs` / "Pitch latency offset" / "Shifts detected pitch against the timeline, to compensate for your interface's round-trip delay." — number, −50..50, step 1, unit `ms`, category `editing`.
- `pitchLaneFollow` / "Pitch lane scrolling" / "FIXED pins the playhead and scrolls the lane toward you while you sing. FREE scrolls like the rest of the timeline." — enum, category `interface`.
- `pitchTrailSnap` / "Snap pitch trail" / "Game mode: snaps your line onto the target note when you hit it. Off shows your true pitch, which is more useful for practice." — boolean, category `interface`.
- `pitchRehearseKey` / "Rehearse hold key" / "Hold this key while recording to keep the transport rolling without writing anything to the take." — enum `H` / `J` / `OFF`, category `editing`.

- [ ] **Step 4: Run to verify it passes**

Run: `npx vitest run src/lib/prefs/schema.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/lib/prefs/schema.ts src/lib/prefs/schema.test.ts
git commit -m "feat(prefs): pitch coach preferences"
```

---

## Task 11: The Pitch Coach panel

**Files:**
- Create: `src/lib/components/pitch/PitchCoach.svelte`, `src/lib/components/pitch/RehearseButton.svelte`
- Modify: `src/App.svelte` (`<PianoRoll />` at `:257`, the `onKeydown` handler)
- Test: `src/lib/components/pitch/panel.test.ts`

**Interfaces:**
- Consumes: `recentFrames()`, `framesBetween()`, `pitchMode`, `autoFitRange`, `yOfKey`, `centsToTarget`, `bandFor`, `trailSegments`, `TOLERANCE_CENTS`, `midi.clipsOf()`, `midi.ticksToSamples()`, `view.xOf()`, `transport.positionAt()`, `canvasPos()`.
- Produces: a `bottomPanel: "roll" | "pitch"` field on `ui` (`src/lib/state/ui.svelte.ts`), and in `src/lib/components/pitch/panel-logic.ts`:
  ```ts
  export function laneScrollFor(mode: "fixed" | "free", positionSamples: number, spp: number, widthPx: number, currentStart?: number): number;
  export function rehearseKeyMatches(pref: string, key: string): boolean;
  ```

**Chord resolution stays in Rust.** An earlier draft of this plan had the
panel pick the top note of a chord itself. That is business logic and it
belongs to `reference_melody` (Task 13), not the renderer — ADR 0006 and
owner ruling R4. In phase 2 the lane simply draws **every** note of the
reference track as a target bar, which is rendering existing state exactly
as the piano roll already does. From phase 3 the lane dims the notes the
backend flagged `ambiguous`, using the report as its source.

**Before writing the component**, invoke the `frontend-design` skill. The lane is the feature's face; a default-looking canvas undersells it.

- [ ] **Step 1: Write the failing tests**

These test the extractable logic, not the rendered pixels — the drawing itself is validated by the owner driving the app.

```ts
import { describe, it, expect } from "vitest";
import { laneScrollFor, rehearseKeyMatches } from "./panel-logic";

describe("laneScrollFor", () => {
  it("pins the playhead at 35% in fixed mode", () => {
    expect(laneScrollFor("fixed", 96000, 1000, 400)).toBe(96000 - 0.35 * 400 * 1000);
  });

  it("leaves the viewport alone in free mode", () => {
    expect(laneScrollFor("free", 96000, 1000, 400, 12345)).toBe(12345);
  });
});

describe("rehearseKeyMatches", () => {
  it("matches the configured key and ignores others", () => {
    expect(rehearseKeyMatches("h", "h")).toBe(true);
    expect(rehearseKeyMatches("h", "j")).toBe(false);
  });

  it("never matches when the preference is off", () => {
    expect(rehearseKeyMatches("none", "h")).toBe(false);
  });

  it("is case-insensitive, so caps lock does not disarm it", () => {
    expect(rehearseKeyMatches("h", "H")).toBe(true);
  });
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/lib/components/pitch/panel.test.ts`
Expected: FAIL — `panel-logic.ts` does not exist.

- [ ] **Step 3: Implement**

Create `src/lib/components/pitch/panel-logic.ts` with the two pure functions above, then the components.

`PitchCoach.svelte`:
- One `<canvas>`, sized by `devicePixelRatio`, redrawn in its own rAF loop started in `onMount` and cancelled in `onDestroy`. **Never** read `recentFrames()` inside a reactive block.
- Read the playhead with `transport.positionAt(performance.now())`, exactly as `Timeline.svelte:168-190` does.
- Target notes come from `midi.clipsOf(pitchMode.referenceTrackId)`, converted with `midi.ticksToSamples()`. No tick maths in this file beyond those calls.
- Draw order: background, target bars (rounded, muted), the hit-fill inside each bar, rehearse-hold hatching, the trail segments, the cents badge, the off-screen arrow.
- **`pitchTrailSnap` (game mode).** When true, a trail point whose `bandFor(...)` is `"in"` is drawn at the target bar's y instead of its own, so the line reads as clean. When false (the default) the trail always shows true pitch. Never mix the two within one frame — snapped is legible, unsnapped is truthful, and a half-snapped line is neither.
- A reference-track picker (a `<select>` of MIDI tracks) and a listen toggle in the panel header; both call the backend and let `pitch://state` push the truth back. Do not optimistically set local state.
- **Tuner mode:** when `!transport.playing && pitchMode.listening`, draw the large note name, the cents needle, and the last 3 seconds as a sparkline instead of the lane. Same canvas, same rAF loop.
- Every pointer handler uses `canvasPos()`.
- If the panel is open and not listening, show one plain line about speaker bleed: singing along to speakers means the backing track gets detected too — use headphones.

`RehearseButton.svelte`: a button calling `backend.setRehearseHold(true)` on `pointerdown` and `(false)` on `pointerup`/`pointercancel`/`pointerleave`. Releasing on leave matters — a pointer dragged off the button must not leave the take silently discarding audio.

`App.svelte`: add `ui.bottomPanel` and a two-item tab strip above the bottom panel region, rendering `<PianoRoll />` or `<PitchCoach />`. Both share `ui.rollHeight` and the existing `ROLL_RESIZE` handle. Default `"roll"`. In `onKeydown`, on `keydown` with `rehearseKeyMatches(prefs.values.pitchRehearseKey, e.key)` and `!e.repeat` call `setRehearseHold(true)`; on `keyup` call `(false)`. Guard both against firing while focus is in an input or textarea, following whatever guard the existing shortcuts use.

- [ ] **Step 4: Run the checks**

Run: `npx vitest run && npx svelte-check && npm run build`
Expected: PASS, no new type errors, build green.

- [ ] **Step 5: Commit**

```bash
git add src/lib/components/pitch src/App.svelte src/lib/state/ui.svelte.ts
git commit -m "feat(pitch): the Pitch Coach lane, tuner mode and rehearse hold"
```

---

# Phase 3 — Scoring

## Task 12: Shared repeat-expansion helper

**Files:**
- Modify: `src-tauri/src/midi/schedule.rs`
- Test: inline `#[cfg(test)] mod tests` in that file

**Interfaces:**
- Produces:
  ```rust
  /// One reference note in absolute samples, carrying the identity
  /// `clip_events` drops.
  #[derive(Clone, Copy, Debug, PartialEq)]
  pub struct AbsNote {
      pub start: u64,
      pub end: u64,
      pub key: u8,
      pub clip_id_hash: u64,
      pub note_id: u32,
      pub repeat_index: u32,
  }
  pub fn clip_notes(clip: &MidiClip, map: &TempoMap) -> Vec<AbsNote>;
  ```

**Why:** `clip_events` returns `AbsNoteEvent { sample, key, velocity }` — correct timing, no identity. The report needs stable identity to address a row and seek to it. Rather than duplicating the repeat-expansion and clipping rules, both functions call one helper, and a test pins them together.

- [ ] **Step 1: Write the failing tests**

```rust
fn fixture_clip(notes: Vec<MidiNote>, content_ticks: u64, placement_ticks: u32) -> MidiClip {
    MidiClip {
        id: "fixture".into(),
        track_id: "t1".into(),
        name: "Fixture".into(),
        timeline_start_ticks: 0,
        length_ticks: placement_ticks,
        content_length_ticks: Some(content_ticks),
        notes,
    }
}

fn fixture_note(note_id: u32, tick: u32, length_ticks: u32, key: u8) -> MidiNote {
    MidiNote { tick, length_ticks, key, velocity: 100, channel: 0, note_id }
}

#[test]
fn clip_notes_and_clip_events_agree_on_timing() {
    // Content is 2 bars (PPQ 960 * 4 beats * 2), placement 4 bars, so the
    // content repeats once. The last note's off lands past the content end
    // and must be clamped identically by both functions.
    let bar = 960 * 4;
    let clip = fixture_clip(
        vec![
            fixture_note(1, 0, 480, 60),
            fixture_note(2, bar, 480, 62),
            fixture_note(3, 2 * bar - 240, 960, 64), // runs past the content end
        ],
        (2 * bar) as u64,
        4 * bar,
    );
    let map = TempoMap::default();
    let events = clip_events(&clip, &map);
    let notes = clip_notes(&clip, &map);

    let ons: Vec<u64> = events.iter().filter(|e| e.velocity > 0).map(|e| e.sample).collect();
    assert_eq!(notes.iter().map(|n| n.start).collect::<Vec<_>>(), ons,
        "the two expansions must not drift");

    let offs: Vec<u64> = events.iter().filter(|e| e.velocity == 0).map(|e| e.sample).collect();
    let mut ends: Vec<u64> = notes.iter().map(|n| n.end).collect();
    ends.sort_unstable();
    let mut offs_sorted = offs.clone();
    offs_sorted.sort_unstable();
    assert_eq!(ends, offs_sorted, "note ends must match note-offs");
}

#[test]
fn clip_notes_carries_identity_across_repeats() {
    let bar = 960 * 4;
    let clip = fixture_clip(vec![fixture_note(7, 0, 480, 60)], bar as u64, 3 * bar);
    let notes = clip_notes(&clip, &TempoMap::default());
    assert_eq!(notes.len(), 3);
    assert!(notes.iter().all(|n| n.note_id == notes[0].note_id), "same source note");
    assert_eq!(notes.iter().map(|n| n.repeat_index).collect::<Vec<_>>(), vec![0, 1, 2]);
}
```

Check the field names against `src-tauri/src/midi/types.rs` before running — if the existing `schedule.rs` tests already have a clip constructor, use theirs instead of these helpers rather than adding a second one.

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test midi::schedule`
Expected: FAIL.

- [ ] **Step 3: Implement**

Extract the loop body of `clip_events` (repeat expansion, onset clipping past the placement end, off clamping) into a private helper that yields `(start_tick, end_tick, key, velocity, note_id, repeat_index)`. `clip_events` maps it to `AbsNoteEvent` and keeps its existing sort (`(sample, velocity)`, offs before ons) and its existing signature. `clip_notes` maps it to `AbsNote`, sorted by `(start, key)`. `clip_id_hash` is a stable hash of the clip id string, so `AbsNote` stays `Copy`.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test midi::`
Expected: PASS — the new tests plus every existing schedule and playback test.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/midi/schedule.rs
git commit -m "refactor(midi): one expansion rule behind clip_events and clip_notes"
```

---

## Task 13: Scoring

**Files:**
- Create: `src-tauri/src/control/pitch_coach.rs`
- Modify: `src-tauri/src/control/mod.rs` (add `pub mod pitch_coach;`)
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `midi::schedule::{AbsNote, clip_notes}`, `audio::pitch::PitchFrame`, `audio::yin::hz_to_midi`.
- Produces:
  ```rust
  pub const ONSET_GRACE_MS: f32 = 70.0;
  pub const VIBRATO_MEAN_MS: f32 = 120.0;
  pub const SUSTAIN_SKIP_MS: f32 = 80.0;
  pub const HIT_ENTER_FRAMES: usize = 2;
  pub const HIT_LEAVE_FRAMES: usize = 3;

  #[derive(Clone, Debug, serde::Serialize)] #[serde(rename_all = "camelCase")]
  pub struct NoteScore {
      pub note_id: u32, pub start_sample: u64, pub end_sample: u64, pub key: u8,
      pub hit_fraction: f32, pub coverage: f32,
      pub mean_cents: f32, pub median_cents: f32, pub onset_offset_ms: f32,
      pub stability_cents: f32, pub vibrato_rate_hz: f32, pub vibrato_extent_cents: f32,
      pub ambiguous: bool,
  }

  #[derive(Clone, Debug, serde::Serialize)] #[serde(rename_all = "camelCase")]
  pub struct PitchScoreReport {
      pub notes: Vec<NoteScore>,
      pub in_tolerance_pct: f32, pub mean_abs_cents: f32, pub median_signed_cents: f32,
      pub mean_onset_offset_ms: f32, pub rating: String,
      pub tolerance_cents: f32, pub reference_track_id: String,
  }

  /// Resolve chords to the top note; overlapping notes come back flagged.
  pub fn reference_melody(notes: Vec<AbsNote>) -> Vec<(AbsNote, bool)>;
  pub fn score(reference: &[(AbsNote, bool)], frames: &[PitchFrame], rate: u32, tolerance_cents: f32) -> PitchScoreReport;
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::pitch::PitchFrame;

    const RATE: u32 = 48_000;
    const HOP: u64 = 480;

    fn note(id: u32, key: u8, start_ms: u64, len_ms: u64) -> AbsNote {
        AbsNote {
            start: start_ms * RATE as u64 / 1000,
            end: (start_ms + len_ms) * RATE as u64 / 1000,
            key, clip_id_hash: 0, note_id: id, repeat_index: 0,
        }
    }

    /// Frames covering [from_ms, to_ms) at `midi`, one per 10 ms hop.
    fn sung(from_ms: u64, to_ms: u64, midi: f32) -> Vec<PitchFrame> {
        let a = from_ms * RATE as u64 / 1000;
        let b = to_ms * RATE as u64 / 1000;
        (a..b).step_by(HOP as usize)
            .map(|s| PitchFrame {
                sample: s, midi,
                hz: crate::audio::yin::midi_to_hz(midi),
                clarity: 0.9, rms: 0.3, voiced: true,
            })
            .collect()
    }

    #[test]
    fn a_perfect_run_scores_full_marks() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let rep = score(&r, &sung(0, 1000, 60.0), RATE, 50.0);
        assert!(rep.in_tolerance_pct > 99.0, "got {}", rep.in_tolerance_pct);
        assert!(rep.notes[0].mean_cents.abs() < 1.0);
        assert!(rep.notes[0].coverage > 0.95);
    }

    /// The diagnostic a game cannot give: not "you missed" but "you are
    /// consistently 30 cents flat".
    #[test]
    fn a_consistently_flat_run_reports_a_signed_mean() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let rep = score(&r, &sung(0, 1000, 60.0 - 0.30), RATE, 50.0);
        assert!((rep.notes[0].mean_cents + 30.0).abs() < 3.0, "got {}", rep.notes[0].mean_cents);
        assert!(rep.notes[0].hit_fraction > 0.9, "30 cents flat is still inside a 50 cent window");
        assert!(rep.median_signed_cents < -20.0, "the session summary must keep the sign");
    }

    #[test]
    fn a_semitone_flat_run_is_not_a_hit() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let rep = score(&r, &sung(0, 1000, 59.0), RATE, 50.0);
        assert!(rep.notes[0].hit_fraction < 0.1, "got {}", rep.notes[0].hit_fraction);
    }

    /// Vibrato is singing, not error. Scoring against a 120 ms moving mean
    /// is what stops a trained voice being marked down.
    #[test]
    fn vibrato_within_tolerance_still_counts_as_a_hit() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let frames: Vec<PitchFrame> = sung(0, 1000, 60.0).into_iter().enumerate()
            .map(|(i, mut f)| {
                let t = i as f32 * 0.010;
                // 5.5 Hz, 90 cents peak-to-peak
                f.midi = 60.0 + 0.45 * (2.0 * std::f32::consts::PI * 5.5 * t).sin();
                f.hz = crate::audio::yin::midi_to_hz(f.midi);
                f
            })
            .collect();
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(rep.notes[0].hit_fraction > 0.9, "vibrato scored as error: {}", rep.notes[0].hit_fraction);
        assert!(rep.notes[0].vibrato_rate_hz > 4.0 && rep.notes[0].vibrato_rate_hz < 7.0,
            "vibrato rate {} should land near 5.5 Hz", rep.notes[0].vibrato_rate_hz);
        assert!(rep.notes[0].vibrato_extent_cents > 50.0);
    }

    /// A consonant is not a mistake: the first 70 ms of a note is free.
    #[test]
    fn an_unvoiced_onset_does_not_penalise_the_note() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let mut frames = sung(0, 1000, 60.0);
        for f in frames.iter_mut().take(6) { f.voiced = false; }
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(rep.notes[0].hit_fraction > 0.95, "60 ms of consonant must be free: {}", rep.notes[0].hit_fraction);
    }

    #[test]
    fn a_late_entry_is_reported_as_positive_onset_offset() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let rep = score(&r, &sung(120, 1000, 60.0), RATE, 50.0);
        assert!((rep.notes[0].onset_offset_ms - 120.0).abs() < 25.0, "got {}", rep.notes[0].onset_offset_ms);
    }

    #[test]
    fn an_early_entry_is_reported_as_negative_onset_offset() {
        let r = reference_melody(vec![note(1, 60, 500, 1000)]);
        let rep = score(&r, &sung(400, 1500, 60.0), RATE, 50.0);
        assert!(rep.notes[0].onset_offset_ms < -50.0, "got {}", rep.notes[0].onset_offset_ms);
    }

    /// Silence must read as "you did not sing", not as "you sang badly" —
    /// the two need different advice.
    #[test]
    fn silence_reports_low_coverage_not_a_low_hit_fraction() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let mut frames = sung(0, 1000, 60.0);
        for f in frames.iter_mut() { f.voiced = false; }
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(rep.notes[0].coverage < 0.05, "coverage {}", rep.notes[0].coverage);
    }

    #[test]
    fn an_octave_slip_is_forgiven_by_folding() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let rep = score(&r, &sung(0, 1000, 72.0), RATE, 50.0);
        assert!(rep.notes[0].hit_fraction > 0.9, "octave folding must absorb this");
    }

    #[test]
    fn a_chord_resolves_to_the_top_note_and_is_flagged() {
        let r = reference_melody(vec![note(1, 60, 0, 1000), note(2, 64, 0, 1000)]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0.key, 64, "lead lines take the top note, never the root");
        assert!(r[0].1, "an overlap must be flagged ambiguous");
    }

    /// Chord guesswork must not be allowed to claim the singer was flat.
    #[test]
    fn ambiguous_notes_are_excluded_from_the_session_statistics() {
        let r = reference_melody(vec![
            note(1, 60, 0, 500), note(2, 64, 0, 500),   // overlap -> ambiguous
            note(3, 62, 500, 500),                       // clean
        ]);
        let mut frames = sung(0, 500, 64.0 - 0.9);       // badly off on the ambiguous one
        frames.extend(sung(500, 1000, 62.0));            // perfect on the clean one
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(rep.mean_abs_cents < 20.0,
            "the ambiguous note must not drag the summary: {}", rep.mean_abs_cents);
        assert_eq!(rep.notes.len(), 2, "but it is still reported per note");
    }

    #[test]
    fn tolerance_tier_changes_what_counts_as_a_hit() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)]);
        let frames = sung(0, 1000, 60.0 + 0.40); // 40 cents sharp
        assert!(score(&r, &frames, RATE, 50.0).notes[0].hit_fraction > 0.9, "inside standard");
        assert!(score(&r, &frames, RATE, 20.0).notes[0].hit_fraction < 0.1, "outside pro");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test control::pitch_coach`
Expected: FAIL.

- [ ] **Step 3: Implement**

`reference_melody`: sort by `(start, key)`; sweep for time overlaps; within each overlapping cluster keep the highest `key` and mark it `true`; non-overlapping notes are `false`.

`score`, per note:
1. Take frames whose `sample` falls in `[start, end)`.
2. `coverage` = voiced frames / total frames in the window.
3. Build the 120 ms moving mean of `midi` over the voiced frames (12 frames at 10 ms).
4. `cents` per frame = signed octave-folded distance from the smoothed value to `key` (same rule as `centsToTarget` in Task 9 — keep the two definitions textually adjacent in the comments so a future edit changes both).
5. Frames inside the first `ONSET_GRACE_MS` are excluded from `hit_fraction` entirely.
6. `hit_fraction` = frames within tolerance / voiced non-grace frames, with the `HIT_ENTER_FRAMES` / `HIT_LEAVE_FRAMES` hysteresis applied to the hit state before counting.
7. `mean_cents`, `median_cents` over voiced non-grace frames.
8. `onset_offset_ms` = first voiced frame's sample minus `start`, in ms. When there is no voiced frame, report `0.0` and let `coverage` carry the story.
9. `stability_cents` = standard deviation of cents over the sustain, skipping `SUSTAIN_SKIP_MS`.
10. Vibrato: on the sustain's cents series, count zero-crossings of the deviation from its own mean to get the rate, and take half the peak-to-peak spread as the extent. A cheap estimator is correct here — this is feedback, not measurement.

Session aggregates weight by note duration and skip `ambiguous` notes. `rating` maps `in_tolerance_pct` to five words: under 40 "Finding it", under 60 "Getting there", under 80 "Solid", under 93 "Sharp", else "Locked in".

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test control::pitch_coach`
Expected: PASS, 12 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/control/pitch_coach.rs src-tauri/src/control/mod.rs
git commit -m "feat(pitch): per-note scoring with vibrato, onset and stability metrics"
```

---

## Task 14: Pitch track on disk

**Files:**
- Create: `src-tauri/src/audio/pitch_store.rs`
- Modify: `src-tauri/src/audio/recorder.rs`, `src-tauri/src/audio/mod.rs`
- Create: `docs/ipc-schemas/pitch-score-report.schema.json`
- Test: inline `#[cfg(test)] mod tests` in `pitch_store.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const PITCH_MAGIC: &[u8; 4] = b"APTF";
  pub const PITCH_VERSION: u16 = 1;
  pub fn write_track(path: &Path, rate: u32, hop: u32, frames: &[PitchFrame]) -> Result<(), String>;
  pub fn read_track(path: &Path) -> Result<(u32, u32, Vec<PitchFrame>), String>;
  ```
  Commands: `pitch_score`, `pitch_track`, `pitch_analyze_clip`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn round_trips_a_track() {
    let dir = std::env::temp_dir().join("aura-pitch-store-test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("round-trip.bin");
    let frames = vec![
        PitchFrame { sample: 0, hz: 220.0, midi: 57.0, clarity: 0.9, rms: 0.2, voiced: true },
        PitchFrame { sample: 480, hz: 0.0, midi: 0.0, clarity: 0.0, rms: 0.0, voiced: false },
    ];
    write_track(&path, 48_000, 480, &frames).unwrap();
    let (rate, hop, back) = read_track(&path).unwrap();
    assert_eq!((rate, hop), (48_000, 480));
    assert_eq!(back.len(), 2);
    assert!((back[0].hz - 220.0).abs() < 1e-3);
    assert_eq!(back[0].voiced, true);
    assert_eq!(back[1].voiced, false);
    // Sample positions are derived from hop, not stored per frame.
    assert_eq!(back[1].sample, 480);
    std::fs::remove_file(&path).ok();
}

#[test]
fn rejects_a_foreign_or_truncated_file() {
    let dir = std::env::temp_dir().join("aura-pitch-store-test");
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.bin");
    std::fs::write(&bad, b"NOPE").unwrap();
    assert!(read_track(&bad).is_err(), "a foreign magic must be rejected, not misread");
    std::fs::remove_file(&bad).ok();
}

#[test]
fn a_missing_file_is_an_error_not_a_panic() {
    assert!(read_track(std::path::Path::new("/nonexistent/aura/pitch.bin")).is_err());
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test audio::pitch_store`
Expected: FAIL.

- [ ] **Step 3: Implement**

Layout: magic `APTF`, `u16` version, `u32` rate, `u32` hop, `u32` frame count, then that many `(f32 hz, f32 clarity, f32 rms)` triples. `voiced` is not stored — it is `hz > 0.0` on read, which is why an unvoiced frame writes `hz = 0.0`. `sample` is `index * hop`. Document both derivations in the file header comment.

In `recorder.rs`, the writer thread already folds a `PyramidBuilder` as it writes; fold pitch the same way, using `Decimator` + `PitchAnalyzer` over the samples it is writing, and emit `Store::cache_dir_for(...)`'s sibling `cache/pitch/<clipId>.bin` on finish.

Add four commands to `audio/mod.rs`. `pitch_score` reads the clip's track, derives the reference with `clip_notes` + `reference_melody`, and calls `score`. `pitch_track` returns the stored curve decimated to at most `max_points`. `pitch_analyze_clip` decodes the clip's WAV and writes the track for clips that have none.

Write `pitch-score-report.schema.json` mirroring `PitchScoreReport`, draft-07, no `additionalProperties: false`.

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test`
Expected: PASS.

- [ ] **Step 5: Request the remaining registrations**

Ask the owner to add to `lib.rs`:
```
audio::pitch_score,
audio::pitch_track,
audio::pitch_analyze_clip,
```

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/audio/pitch_store.rs src-tauri/src/audio/recorder.rs src-tauri/src/audio/mod.rs docs/ipc-schemas/pitch-score-report.schema.json
git commit -m "feat(pitch): persist a pitch track per take and score it on demand"
```

---

## Task 15: The report

**Files:**
- Create: `src/lib/components/pitch/PitchReport.svelte`, `src/lib/pitch/report.ts`
- Modify: `src/lib/components/pitch/PitchCoach.svelte`, `src/lib/types/ipc.ts`, `src/lib/tauri.ts`
- Test: `src/lib/pitch/report.test.ts`

**Interfaces:**
- Consumes: `backend.pitchScore(clipId, referenceTrackId, toleranceCents)`.
- Produces:
  ```ts
  export function centsBarGeometry(cents: number, toleranceCents: number, widthPx: number): { x: number; w: number; band: PitchBand };
  export function sortNotes(notes: NoteScore[], by: "time" | "worst"): NoteScore[];
  ```

- [ ] **Step 1: Write the failing tests**

```ts
import { describe, it, expect } from "vitest";
import { centsBarGeometry, sortNotes } from "./report";

describe("centsBarGeometry", () => {
  it("draws from the centre, rightwards when sharp", () => {
    const g = centsBarGeometry(50, 50, 200);
    expect(g.x).toBeGreaterThanOrEqual(100);
    expect(g.w).toBeGreaterThan(0);
  });

  it("draws leftwards when flat", () => {
    const g = centsBarGeometry(-50, 50, 200);
    expect(g.x).toBeLessThan(100);
    expect(g.x + g.w).toBeCloseTo(100, 0);
  });

  it("clamps a wild reading inside the bar", () => {
    const g = centsBarGeometry(5000, 50, 200);
    expect(g.x + g.w).toBeLessThanOrEqual(200);
    expect(g.band).toBe("far");
  });

  it("is a zero-width mark exactly on pitch", () => {
    expect(centsBarGeometry(0, 50, 200).w).toBeLessThanOrEqual(2);
  });
});

describe("sortNotes", () => {
  const n = (id: number, start: number, hit: number) =>
    ({ noteId: id, startSample: start, endSample: start + 480, key: 60,
       hitFraction: hit, coverage: 1, meanCents: 0, medianCents: 0, onsetOffsetMs: 0,
       stabilityCents: 0, vibratoRateHz: 0, vibratoExtentCents: 0, ambiguous: false });

  it("orders by time by default", () => {
    expect(sortNotes([n(2, 960, 1), n(1, 0, 0.2)], "time").map((x) => x.noteId)).toEqual([1, 2]);
  });

  it("puts the worst notes first when asked, so practice starts there", () => {
    expect(sortNotes([n(2, 960, 1), n(1, 0, 0.2)], "worst").map((x) => x.noteId)).toEqual([1, 2]);
  });
});
```

- [ ] **Step 2: Run to verify they fail**

Run: `npx vitest run src/lib/pitch/report.test.ts`
Expected: FAIL.

- [ ] **Step 3: Implement**

`report.ts` holds the two pure functions. `PitchReport.svelte` renders the header (`in_tolerance_pct` as the headline number, the rating word, mean absolute cents, mean onset offset) and one row per note: note name, a cents bar via `centsBarGeometry`, the onset offset in ms, and the stability figure. Ambiguous rows get a marker and a title explaining they came from a chord. Clicking a row seeks the transport to `startSample`. A time/worst sort toggle.

Mount it under the lane in `PitchCoach.svelte`, fetched after a take finishes and whenever the reference track or tolerance changes.

- [ ] **Step 4: Run the full gate**

Run: `cd src-tauri && cargo test && cd .. && npm test && npx svelte-check && npm run build`
Expected: all PASS. Paste each result line.

- [ ] **Step 5: Commit**

```bash
git add src/lib/components/pitch/PitchReport.svelte src/lib/pitch/report.ts src/lib/pitch/report.test.ts src/lib/components/pitch/PitchCoach.svelte src/lib/types/ipc.ts src/lib/tauri.ts
git commit -m "feat(pitch): per-note take report"
```

---

## Task 16: Documentation and PR

**Files:**
- Modify: `docs/ARCHITECTURE.md`, `README.md` (if it lists features)
- Create: `docs/pitch-coach.md`

- [ ] **Step 1: Write the user-facing doc**

`docs/pitch-coach.md`: what the three modes are, how to pick a reference track, what the tolerance tiers mean, what each report column means, the latency-calibration preference, and the headphones caveat stated plainly.

- [ ] **Step 2: Update ARCHITECTURE.md**

Add the input hub and the pitch thread to the audio section, and `cache/pitch/` to the project-folder layout in §7. Keep the existing tone: as-built, binding.

- [ ] **Step 3: Run the full gate one more time**

```bash
cd src-tauri && cargo test && cd .. && npm test && npx svelte-check && npm run build
```

- [ ] **Step 4: Commit and open the PR**

```bash
git add docs/
git commit -m "docs(pitch): pitch coach usage and architecture notes"
git push -u origin feat/pitch-coach
gh pr create --title "feat(pitch): Pitch Coach — live vocal pitch feedback against a MIDI reference" --body "..."
```

The PR body should explain the input-hub change first (it is the part a reviewer must scrutinise), then the detector, then the UI. Link the spec.

---

## Notes for the executor

- **Task 5 is the one to slow down on.** Everything else is new files with no existing callers. Task 5 rewrites the input path of a 4800-line file that owns the record take. If the existing recording tests go red, the refactor is wrong — do not adjust the tests.
- **Tasks 6 and 14 both block on the owner** registering commands in the frozen `lib.rs`. Stop and ask; do not edit it and do not work around it.
- **Never claim a step passed without pasting the command output.** The `verification-before-completion` skill applies to every "Expected: PASS" line in this plan.
- Tasks 1, 2, 3, 9, 10 are independent of each other and of everything before them. They can be dispatched in parallel. Task 4 needs 1–3. Task 5 needs 2 and 3. Tasks 7–8 need 6. Task 11 needs 7, 8, 9, 10. Task 13 needs 12. Tasks 14–15 need 13.
