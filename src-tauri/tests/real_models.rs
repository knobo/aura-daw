//! Real-model pipeline proof (env-gated integration tests).
//!
//! Drives the REAL python sidecars (no `--simulate`) through the same
//! `JobManager` NDJSON pipeline the app uses, proving progress parsing,
//! event fan-out, and result import work with genuine model output.
//!
//! Skipped politely unless `AURA_REAL_MODELS` is set, so plain `cargo test`
//! stays green on machines without the model stack. To run:
//!
//! ```sh
//! AURA_REAL_MODELS=1 \
//! AURA_REAL_PYTHON=$HOME/prog/dav/.venv-sidecars/bin/python \
//!     cargo test --test real_models -- --nocapture
//! ```
//!
//! `AURA_REAL_PYTHON` points at the interpreter of the venv holding the
//! model stacks (demucs, acestep); it defaults to `python3` on PATH.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use aura_lib::sidecars::jobs::{self, JobManager, JobSpec};
use aura_lib::sidecars::types::{JobKind, JobState, SidecarEvent};

fn gated() -> bool {
    if std::env::var_os("AURA_REAL_MODELS").is_none() {
        eprintln!("skipping: AURA_REAL_MODELS not set (real-model test)");
        return false;
    }
    true
}

fn python() -> PathBuf {
    std::env::var_os("AURA_REAL_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| jobs::resolve_python().expect("python3 on PATH"))
}

fn script(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .join("sidecars")
        .join(name);
    assert!(p.is_file(), "worker script missing: {}", p.display());
    p.to_string_lossy().into_owned()
}

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aura-real-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn collecting_sink() -> (jobs::EventSink, Arc<Mutex<Vec<SidecarEvent>>>) {
    let events: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let e = Arc::clone(&events);
    (Arc::new(move |ev| e.lock().push(ev)), events)
}

async fn run_to_terminal(
    spec: JobSpec,
    deadline: Duration,
) -> (
    aura_lib::sidecars::types::SidecarJobStatus,
    Arc<Mutex<Vec<SidecarEvent>>>,
) {
    let mgr = Arc::new(JobManager::new(1, Duration::ZERO));
    let (sink, events) = collecting_sink();
    let id = mgr.submit(spec, sink);
    let deadline = Instant::now() + deadline;
    loop {
        let st = mgr.status(&id).unwrap();
        if JobState::is_terminal(&st.state) {
            return (st, events);
        }
        assert!(Instant::now() < deadline, "job did not finish: {st:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Write a short drums+bass+melody mix — enough structure for Demucs to
/// produce clearly non-silent drum/bass/other stems.
fn write_test_mix(path: &PathBuf, seconds: f32) {
    let sr = 44_100u32;
    let n = (seconds * sr as f32) as usize;
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let mut noise = 0x2545F491u32; // xorshift for snare/hat noise
    for i in 0..n {
        let t = i as f32 / sr as f32;
        // Kick on quarters (120 bpm), noise burst on offbeats.
        let beat = t % 0.5;
        let kick = (2.0 * std::f32::consts::PI * (50.0 + 40.0 * (-20.0 * beat).exp()) * t).sin()
            * (-beat * 12.0).exp();
        noise ^= noise << 13;
        noise ^= noise >> 17;
        noise ^= noise << 5;
        let off = (t + 0.25) % 0.5;
        let snare = ((noise as f32 / u32::MAX as f32) - 0.5) * (-off * 25.0).exp();
        // Bass root + simple lead arpeggio.
        let bass = (2.0 * std::f32::consts::PI * 55.0 * t).sin() * 0.4;
        let step = [440.0, 523.25, 659.25, 587.33][(t / 0.4) as usize % 4];
        let lead = (2.0 * std::f32::consts::PI * step * t).sin() * 0.25;
        let s = (kick * 0.8 + snare * 0.5 + bass + lead) * 0.45;
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        w.write_sample(v).unwrap();
        w.write_sample(v).unwrap();
    }
    w.finalize().unwrap();
}

fn read_stats(path: &PathBuf) -> (f64, f64) {
    // (durationSeconds, rms)
    let mut r = hound::WavReader::open(path).unwrap();
    let spec = r.spec();
    let mut sum = 0.0f64;
    let mut count = 0u64;
    for s in r.samples::<i16>() {
        let v = s.unwrap() as f64 / 32768.0;
        sum += v * v;
        count += 1;
    }
    let frames = count / spec.channels as u64;
    (
        frames as f64 / spec.sample_rate as f64,
        (sum / count.max(1) as f64).sqrt(),
    )
}

#[tokio::test]
async fn real_demucs_split_through_job_manager() {
    if !gated() {
        return;
    }
    let dir = tmp_dir("demucs");
    let input = dir.join("mix.wav");
    let out_dir = dir.join("stems");
    write_test_mix(&input, 8.0);

    let spec = JobSpec {
        kind: JobKind::StemSplit,
        program: python(),
        args: vec![
            script("demucs_split.py"),
            "--input".into(),
            input.to_string_lossy().into_owned(),
            "--out-dir".into(),
            out_dir.to_string_lossy().into_owned(),
        ],
        grace: Duration::from_secs(5),
        env: Vec::new(),
    };
    let (st, events) = run_to_terminal(spec, Duration::from_secs(600)).await;
    assert_eq!(st.state, "done", "err: {:?}", st.error);
    let result = st.result.expect("result");
    assert_eq!(result["kind"], "stemSplit");
    let stems = result["stems"].as_object().expect("stems map");
    assert_eq!(stems.len(), 4, "expected 4 stems, got {stems:?}");
    for name in ["drums", "bass", "other", "vocals"] {
        let p = PathBuf::from(stems[name].as_str().unwrap());
        let (secs, rms) = read_stats(&p);
        assert!((secs - 8.0).abs() < 0.1, "{name}: duration {secs}");
        // The synthetic mix has no real voice, so only require energy from
        // the stems its sources actually map to.
        if name != "vocals" {
            assert!(rms > 1e-3, "{name}: silent stem (rms {rms})");
        }
    }
    // Real NDJSON progress flowed through the manager.
    let evs = events.lock();
    assert!(
        evs.iter().any(|e| matches!(
            e, SidecarEvent::Progress { stage, .. } if stage == "separating")),
        "no separating progress events"
    );
    drop(evs);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn real_ace_step_generate_through_job_manager() {
    if !gated() {
        return;
    }
    let dir = tmp_dir("ace");
    let out = dir.join("gen.wav");
    let params = serde_json::json!({
        "prompt": "warm synthwave, driving bass, punchy drums",
        "durationSeconds": 10.0,
        "seed": 7,
        "outputPath": out,
    })
    .to_string();

    let spec = JobSpec {
        kind: JobKind::AceStepGenerate,
        program: python(),
        args: vec![
            script("ace_step_worker.py"),
            "--task".into(),
            "generate".into(),
            "--params-json".into(),
            params,
        ],
        grace: Duration::from_secs(5),
        // 12 GB card shared with a desktop: sequential offload keeps the
        // 3.5B pipeline inside the VRAM budget (see worker docstring).
        env: vec![
            ("ACESTEP_CPU_OFFLOAD".into(), Some("1".into())),
            ("ACESTEP_OVERLAPPED_DECODE".into(), Some("1".into())),
            (
                "PYTORCH_CUDA_ALLOC_CONF".into(),
                Some("expandable_segments:True".into()),
            ),
        ],
    };
    let (st, events) = run_to_terminal(spec, Duration::from_secs(600)).await;
    assert_eq!(st.state, "done", "err: {:?}", st.error);
    let result = st.result.expect("result");
    assert_eq!(result["kind"], "aceStepGenerate");
    assert_eq!(result["simulated"], false);
    assert_eq!(result["sampleRate"], 48_000);
    let secs = result["durationSeconds"].as_f64().expect("duration");
    assert!((secs - 10.0).abs() < 1.0, "duration {secs}");
    // Real model output: a float WAV with actual audio energy.
    let bytes = std::fs::metadata(&out).unwrap().len();
    assert!(bytes > 100_000, "output too small: {bytes} bytes");
    let evs = events.lock();
    assert!(
        evs.iter().any(|e| matches!(
            e, SidecarEvent::Progress { stage, .. } if stage == "diffusing")),
        "no diffusing progress events"
    );
    drop(evs);
    let _ = std::fs::remove_dir_all(dir);
}
