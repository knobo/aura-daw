//! Parity guard: the Rust live detector and `sidecars/hum_to_midi.py` must
//! agree on the same audio.
//!
//! The spec's whole argument for copying the sidecar's constants verbatim
//! into `audio::yin` is that the live trainer and hum-to-MIDI must never
//! disagree about what the singer sang. Without this test that is a comment,
//! not a property.
//!
//! The two are deliberately not bit-identical, in two known ways:
//!
//! 1. The sidecar resamples 48 kHz -> 8 kHz by linear interpolation with no
//!    anti-aliasing; the Rust path high-passes at 80 Hz and runs a 63-tap
//!    FIR first. It has to — it sees live mic input, not a clean file.
//! 2. The sidecar's median filter is CENTRED (`i-2 ..= i+2`). The live one
//!    cannot look ahead, so it is causal, and therefore lags by exactly the
//!    half-kernel: [`LAG_FRAMES`]. That lag gets its own assertion below
//!    rather than being absorbed into the tolerance.
//!
//! With the lag accounted for they agree to a few cents, which is why
//! [`TOLERANCE_CENTS`] is tight enough to catch a constant that drifted.
//!
//! Skipped (not failed) when no Python interpreter with numpy is on PATH, so
//! the suite stays green in environments without the sidecar venv.

use std::path::PathBuf;
use std::process::Command;

use aura_lib::audio::decimate::Decimator;
use aura_lib::audio::pitch::{PitchAnalyzer, PitchFrame, MEDIAN_TAPS};

const DEVICE_SR: u32 = 48_000;
const SECONDS: f32 = 3.0;

/// The live median is causal where the sidecar's is centred, so our frame
/// `i` corresponds to their frame `i - MEDIAN_TAPS/2`. Measured, not
/// assumed: `alignment_lag_is_exactly_the_median_half_kernel` fails if the
/// true best alignment ever moves.
const LAG_FRAMES: usize = MEDIAN_TAPS / 2;

/// Once the lag is removed the two pipelines differ only by their
/// resamplers. Measured worst case on this signal is ~3 cents; 10 leaves
/// headroom without letting a real divergence through (a semitone is 100
/// cents, an octave error 1200).
const TOLERANCE_CENTS: f32 = 10.0;

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

fn sidecar() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sidecars/hum_to_midi.py")
}

/// A slow glide 130 Hz -> 260 Hz (an octave through a baritone's range) with
/// 5 Hz vibrato and two harmonics. The vibrato is the point: on a steady
/// tone any two detectors agree, so a moving target is what actually
/// exercises timing and smoothing. The harmonics mean an octave error would
/// show up as a 1200-cent disagreement rather than hiding.
fn glide_with_vibrato() -> Vec<f32> {
    let n = (DEVICE_SR as f32 * SECONDS) as usize;
    let mut phase = 0.0f32;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / DEVICE_SR as f32;
        // Exponential glide, so the sweep is linear in pitch, not in Hz.
        let base = 130.0 * 2f32.powf(t / SECONDS);
        // +/- 30 cents of vibrato at 5 Hz.
        let hz = base * 2f32.powf(0.30 / 12.0 * (2.0 * std::f32::consts::PI * 5.0 * t).sin());
        phase += 2.0 * std::f32::consts::PI * hz / DEVICE_SR as f32;
        out.push(0.5 * (phase.sin() + 0.4 * (2.0 * phase).sin() + 0.2 * (3.0 * phase).sin()));
    }
    out
}

/// The sidecar's median-smoothed MIDI value per frame — the stage `analyze`
/// segments into notes, and so the stage worth comparing against.
fn sidecar_midi(py: &str, wav: &std::path::Path) -> Vec<Option<f64>> {
    let out = Command::new(py)
        .arg(sidecar())
        .arg("--pitch-track")
        .arg(wav)
        .output()
        .expect("running the sidecar");
    assert!(
        out.status.success(),
        "sidecar --pitch-track failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l)
                .unwrap_or_else(|e| panic!("sidecar emitted non-JSON line {l:?}: {e}"));
            assert!(
                v.get("type").is_none(),
                "--pitch-track must emit frames, not job-protocol lines: {l}"
            );
            v["midi"].as_f64() // null -> None
        })
        .collect()
}

/// Both detectors over the same 3 s of audio: `(sidecar midi per frame, our
/// frames)`. `None` when there is no Python with numpy to compare against.
fn both_tracks() -> Option<(Vec<Option<f64>>, Vec<PitchFrame>)> {
    let py = python()?;
    let samples = glide_with_vibrato();

    // Unique per process: other worktrees on this box run this suite too.
    let wav = std::env::temp_dir().join(format!("aura-pitch-parity-{}.wav", std::process::id()));
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: DEVICE_SR,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&wav, spec).expect("create wav");
        for s in &samples {
            w.write_sample(*s).expect("write sample");
        }
        w.finalize().expect("finalize wav");
    }
    let theirs = sidecar_midi(&py, &wav);
    let _ = std::fs::remove_file(&wav);

    // The Rust side runs the real decimator and the real analyser, exactly
    // as the engine will drive them.
    let mut eight_k = Vec::new();
    Decimator::new(DEVICE_SR).process(&samples, 1, &mut eight_k);
    let mut ours = Vec::new();
    PitchAnalyzer::new().push(&eight_k, 0, DEVICE_SR, &mut ours);

    Some((theirs, ours))
}

/// Worst absolute cents difference at a given frame alignment, and how many
/// frames were voiced in both to produce it.
fn worst_at_shift(theirs: &[Option<f64>], ours: &[PitchFrame], shift: usize) -> (f32, usize) {
    let n = theirs.len().min(ours.len());
    let mut worst = 0.0f32;
    let mut compared = 0usize;
    for i in shift..n {
        if let (Some(m), true) = (theirs[i - shift], ours[i].voiced) {
            worst = worst.max(((ours[i].midi - m as f32) * 100.0).abs());
            compared += 1;
        }
    }
    (worst, compared)
}

#[test]
fn rust_and_sidecar_agree_within_ten_cents() {
    let Some((theirs, ours)) = both_tracks() else {
        eprintln!("skipping: no python3 with numpy on PATH");
        return;
    };

    assert!(
        (theirs.len() as i64 - ours.len() as i64).abs() <= 2,
        "both must frame the same audio the same way: sidecar {} frames, rust {}",
        theirs.len(),
        ours.len()
    );

    let n = theirs.len().min(ours.len());
    let (worst, compared) = worst_at_shift(&theirs, &ours, LAG_FRAMES);

    // A detector that silently reported nothing would make the comparison
    // vacuous, so require both to have actually found the tone.
    let their_voiced = theirs.iter().filter(|m| m.is_some()).count();
    let our_voiced = ours.iter().filter(|f| f.voiced).count();
    assert!(
        their_voiced * 10 >= n * 6,
        "sidecar found only {their_voiced}/{n} voiced frames"
    );
    assert!(
        our_voiced * 10 >= n * 6,
        "rust found only {our_voiced}/{n} voiced frames"
    );
    assert!(
        compared * 10 >= n * 6,
        "only {compared}/{n} frames were voiced in BOTH — they disagree about \
         WHERE the note is, never mind its pitch"
    );

    eprintln!("parity: {compared}/{n} frames voiced in both, worst {worst:.1} cents");
    assert!(
        worst < TOLERANCE_CENTS,
        "worst disagreement {worst:.1} cents over {compared} frames exceeds \
         {TOLERANCE_CENTS}: the live detector and the hum sidecar have drifted apart"
    );
}

/// The live path's lag behind the sidecar must be exactly the median
/// half-kernel — no more.
///
/// This is what keeps the tolerance above honest. Without it, someone adding
/// another smoothing stage to the live path would push the real lag to 3 or
/// 4 frames, and the tolerance would quietly absorb the resulting error as
/// "resampler noise" instead of failing. Sweeping the alignment and
/// requiring [`LAG_FRAMES`] to be the strict minimum catches that on the
/// first run.
#[test]
fn alignment_lag_is_exactly_the_median_half_kernel() {
    let Some((theirs, ours)) = both_tracks() else {
        eprintln!("skipping: no python3 with numpy on PATH");
        return;
    };

    let residuals: Vec<f32> = (0..=LAG_FRAMES + 2)
        .map(|s| worst_at_shift(&theirs, &ours, s).0)
        .collect();
    eprintln!("alignment sweep (worst cents by shift): {residuals:?}");

    let best = residuals
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.partial_cmp(b.1).expect("residuals are finite"))
        .map(|(i, _)| i)
        .expect("non-empty sweep");
    assert_eq!(
        best, LAG_FRAMES,
        "the live detector should lag the sidecar by exactly the median \
         half-kernel ({LAG_FRAMES} frames); best alignment was {best}. \
         Worst cents by shift: {residuals:?}"
    );
}
