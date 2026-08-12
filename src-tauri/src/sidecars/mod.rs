//! AURA sidecar module — OWNED BY AGENT 3.
//!
//! Frozen surface (registered in the FROZEN `lib.rs`): command names
//! `sidecar_split_stems`, `sidecar_transcribe`, `sidecar_job_status`,
//! `sidecar_cancel_job`, `sidecar_list_jobs`; the `SidecarState` type
//! (constructed via `Default`) and the `init(&AppHandle)` hook.
//!
//! Layout:
//!   * `types.rs` — wire types (Tauri-free, mirror docs/ipc-schemas/sidecar-job.schema.json)
//!   * `jobs.rs`  — job manager: registry, tokio process supervision, NDJSON
//!                  protocol, cancellation (SIGTERM → SIGKILL), concurrency
//!                  limits. Tauri-free and unit-tested with fake sidecars.
//!   * this file  — `#[tauri::command]` glue: request validation, script/
//!                  interpreter resolution, and fan-out of job events to the
//!                  submit-time `Channel<SidecarEvent>` plus the
//!                  `sidecar://progress|done|error` app events.
//!
//! Wrapper scripts live in the top-level `/sidecars/` directory
//! (`demucs_split.py`, `whisper_transcribe.py`); both understand
//! `--simulate`, and setting `AURA_SIDECAR_SIMULATE=1` makes the commands
//! pass that flag so the whole pipeline is demoable without the ML stacks.

pub mod jobs;
pub mod types;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter};

use jobs::{JobManager, JobSpec};
pub use types::{
    JobKind, SidecarEvent, SidecarJobStatus, StemSplitRequest, TranscribeRequest,
};

const VALID_STEMS: [&str; 4] = ["vocals", "drums", "bass", "other"];
/// SIGTERM → SIGKILL grace on cancel.
const CANCEL_GRACE: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Job registry + supervisor handle. lib.rs relies only on the type name and
/// `Default` construction.
#[derive(Default)]
pub struct SidecarState {
    manager: Arc<JobManager>,
}

impl SidecarState {
    /// Shared job manager handle for the control plane (ARCHITECTURE §11).
    pub fn manager(&self) -> Arc<JobManager> {
        Arc::clone(&self.manager)
    }
}

/// Module init hook, called once from Tauri's `setup` (lib.rs, frozen).
pub fn init(_app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    match jobs::resolve_script("demucs_split.py") {
        Ok(p) => log::info!("sidecars: wrapper scripts at {}", p.parent().unwrap_or(Path::new("?")).display()),
        Err(e) => log::warn!("sidecars: {e}"),
    }
    if simulate_mode() {
        log::info!("sidecars: AURA_SIDECAR_SIMULATE is set — jobs run in simulate mode");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Event fan-out
// ---------------------------------------------------------------------------

/// Every job event goes to the per-job Channel; progress/done/error are also
/// re-emitted as `sidecar://*` app events (log lines are channel-only).
/// Progress is already throttled to <= 10 Hz by the job manager.
fn make_sink(app: AppHandle, channel: Channel<SidecarEvent>) -> jobs::EventSink {
    Arc::new(move |ev: SidecarEvent| {
        let _ = channel.send(ev.clone());
        let event_name = match &ev {
            SidecarEvent::Progress { .. } => "sidecar://progress",
            SidecarEvent::Done { .. } => "sidecar://done",
            SidecarEvent::Error { .. } => "sidecar://error",
            SidecarEvent::Log { .. } => return,
        };
        if let Err(e) = app.emit(event_name, &ev) {
            log::warn!("sidecars: failed to emit {event_name}: {e}");
        }
    })
}

fn simulate_mode() -> bool {
    matches!(
        std::env::var("AURA_SIDECAR_SIMULATE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn require_input_file(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(format!("inputPath must be absolute: {path}"));
    }
    if !p.is_file() {
        return Err(format!("input file not found: {path}"));
    }
    Ok(())
}

/// Default stem output dir: `<input dir>/stems/<input file stem>/`.
fn default_stem_dir(input: &Path) -> String {
    let parent = input.parent().unwrap_or(Path::new("."));
    let name = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    parent.join("stems").join(name).to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Submit a Demucs stem-separation job. Returns the job id immediately;
/// progress/result stream over `on_event` and the "sidecar://*" app events.
#[tauri::command]
pub async fn sidecar_split_stems(
    app: AppHandle,
    request: StemSplitRequest,
    on_event: Channel<SidecarEvent>,
    state: tauri::State<'_, SidecarState>,
) -> Result<String, String> {
    require_input_file(&request.input_path)?;
    if let Some(stems) = &request.stems {
        for s in stems {
            if !VALID_STEMS.contains(&s.as_str()) {
                return Err(format!(
                    "invalid stem `{s}` (expected one of: {})",
                    VALID_STEMS.join(", ")
                ));
            }
        }
    }
    let python = jobs::resolve_python()?;
    let script = jobs::resolve_script("demucs_split.py")?;

    let input = Path::new(&request.input_path);
    let out_dir = request
        .output_dir
        .clone()
        .unwrap_or_else(|| default_stem_dir(input));
    let mut args = vec![
        script.to_string_lossy().into_owned(),
        "--input".into(),
        request.input_path.clone(),
        "--out-dir".into(),
        out_dir,
        "--model".into(),
        request.model.clone().unwrap_or_else(|| "htdemucs".into()),
    ];
    if let Some(stems) = &request.stems {
        if !stems.is_empty() {
            args.push("--stems".into());
            args.push(stems.join(","));
        }
    }
    if simulate_mode() {
        args.push("--simulate".into());
    }

    let spec = JobSpec {
        kind: JobKind::StemSplit,
        program: python,
        args,
        grace: CANCEL_GRACE,
        env: Vec::new(),
    };
    Ok(state.manager.submit(spec, make_sink(app, on_event)))
}

/// Submit a Whisper.cpp transcription (and optional pitch) job.
#[tauri::command]
pub async fn sidecar_transcribe(
    app: AppHandle,
    request: TranscribeRequest,
    on_event: Channel<SidecarEvent>,
    state: tauri::State<'_, SidecarState>,
) -> Result<String, String> {
    require_input_file(&request.input_path)?;
    let python = jobs::resolve_python()?;
    let script = jobs::resolve_script("whisper_transcribe.py")?;

    let mut args = vec![
        script.to_string_lossy().into_owned(),
        "--input".into(),
        request.input_path.clone(),
        "--model".into(),
        request
            .model
            .clone()
            .unwrap_or_else(|| "ggml-base.en.bin".into()),
    ];
    if let Some(lang) = &request.language {
        args.push("--language".into());
        args.push(lang.clone());
    }
    if request.with_pitch.unwrap_or(false) {
        args.push("--with-pitch".into());
    }
    if simulate_mode() {
        args.push("--simulate".into());
    }

    let spec = JobSpec {
        kind: JobKind::Transcribe,
        program: python,
        args,
        grace: CANCEL_GRACE,
        env: Vec::new(),
    };
    Ok(state.manager.submit(spec, make_sink(app, on_event)))
}

/// Open-kind job submission (phase 2, debt D-07 direction): `kind` is a wire
/// string, `params` an opaque per-kind JSON blob passed to the worker as
/// `--params-json`. Tauri-free so the MCP `run_sidecar_job` tool shares it.
///
/// Kind -> worker script (all in `/sidecars/`, all NDJSON, all support
/// `--simulate`):
///   aceStepGenerate | aceStepRepaint | aceStepAudio2Audio -> ace_step_worker.py
///   elevenLabsMusic  -> elevenlabs_music_worker.py (cloud HTTP inside worker)
///   amtInfill        -> amt_infill_worker.py
///   stableAudioSfz   -> stable_audio_sfz_worker.py
/// v1 kinds keep their dedicated commands and are rejected here.
pub fn run_generic_job(
    manager: &Arc<jobs::JobManager>,
    kind: &str,
    params: &serde_json::Value,
    sink: jobs::EventSink,
) -> Result<String, String> {
    let job_kind = JobKind::from_wire(kind).ok_or_else(|| {
        format!(
            "unknown job kind `{kind}` (expected one of: aceStepGenerate, aceStepRepaint, \
             aceStepAudio2Audio, elevenLabsMusic, amtInfill, stableAudioSfz)"
        )
    })?;
    let (script, task): (&str, Option<&str>) = match job_kind {
        JobKind::StemSplit | JobKind::Transcribe | JobKind::HumToMidi => {
            return Err(format!(
                "kind `{kind}` uses its dedicated command (sidecar_split_stems / \
                 sidecar_transcribe / hum_to_song)"
            ));
        }
        JobKind::AceStepGenerate => ("ace_step_worker.py", Some("generate")),
        JobKind::AceStepRepaint => ("ace_step_worker.py", Some("repaint")),
        JobKind::AceStepAudio2Audio => ("ace_step_worker.py", Some("audio2audio")),
        JobKind::ElevenLabsMusic => ("elevenlabs_music_worker.py", None),
        JobKind::AmtInfill => ("amt_infill_worker.py", None),
        JobKind::StableAudioSfz => ("stable_audio_sfz_worker.py", None),
    };
    if !params.is_object() {
        return Err("params must be a JSON object".into());
    }
    let python = jobs::resolve_python()?;
    let script = jobs::resolve_script(script)?;
    let mut args = vec![
        script.to_string_lossy().into_owned(),
        "--params-json".into(),
        params.to_string(),
    ];
    if let Some(task) = task {
        args.push("--task".into());
        args.push(task.into());
    }
    if simulate_mode() {
        args.push("--simulate".into());
    }
    let spec = JobSpec {
        kind: job_kind,
        program: python,
        args,
        grace: CANCEL_GRACE,
        env: Vec::new(),
    };
    Ok(manager.submit(spec, sink))
}

/// Submit a phase-2 job by open kind string (see [`run_generic_job`]).
/// Goes through the control plane so the additive `importToTrackId` /
/// `importAtSamples` params can land a `done` result's `outputPath` on the
/// timeline via `import_audio_clip` (the "generate → hear it" loop).
#[tauri::command]
pub async fn sidecar_run_job(
    app: AppHandle,
    kind: String,
    params: serde_json::Value,
    on_event: Channel<SidecarEvent>,
    control: tauri::State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<String, String> {
    control.run_sidecar_job_with_import(&kind, params, make_sink(app, on_event))
}

#[tauri::command]
pub fn sidecar_job_status(
    job_id: String,
    state: tauri::State<'_, SidecarState>,
) -> Result<SidecarJobStatus, String> {
    state.manager.status(&job_id)
}

#[tauri::command]
pub fn sidecar_cancel_job(
    job_id: String,
    state: tauri::State<'_, SidecarState>,
) -> Result<(), String> {
    state.manager.cancel(&job_id)
}

#[tauri::command]
pub fn sidecar_list_jobs(
    state: tauri::State<'_, SidecarState>,
) -> Result<Vec<SidecarJobStatus>, String> {
    Ok(state.manager.list())
}

// ---------------------------------------------------------------------------
// Tests — tauri-free: drive the REAL python workers (simulate mode / mock
// HTTP server) through the job manager. Skipped politely if python3 is
// missing. `--params-json` forwarding is proven end-to-end: the workers echo
// the params-supplied outputPath back in their results.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jobs::JobManager;
    use parking_lot::Mutex;
    use std::io::{Read as _, Write as _};
    use std::path::PathBuf;
    use std::time::Instant;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("aura-genworker-test-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn collecting_sink() -> (jobs::EventSink, Arc<Mutex<Vec<SidecarEvent>>>) {
        let events: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let e = Arc::clone(&events);
        (Arc::new(move |ev| e.lock().push(ev)), events)
    }

    /// JobSpec for one of the repo's real python workers; None = no python3
    /// on this machine (test skips).
    fn worker_spec(
        kind: JobKind,
        script: &str,
        extra: &[&str],
        env: Vec<(String, Option<String>)>,
    ) -> Option<jobs::JobSpec> {
        let python = jobs::resolve_python().ok()?;
        let script = jobs::resolve_script(script).expect("worker script in /sidecars");
        let mut args = vec![script.to_string_lossy().into_owned()];
        args.extend(extra.iter().map(|s| s.to_string()));
        Some(jobs::JobSpec {
            kind,
            program: python,
            args,
            grace: Duration::from_secs(2),
            env,
        })
    }

    async fn run_to_terminal(
        spec: jobs::JobSpec,
    ) -> (SidecarJobStatus, Arc<Mutex<Vec<SidecarEvent>>>) {
        let mgr = Arc::new(JobManager::new(2, Duration::ZERO));
        let (sink, events) = collecting_sink();
        let id = mgr.submit(spec, sink);
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let st = mgr.status(&id).unwrap();
            if types::JobState::is_terminal(&st.state) {
                return (st, events);
            }
            assert!(Instant::now() < deadline, "worker did not finish: {st:?}");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn run_generic_job_rejects_unknown_and_v1_kinds_and_bad_params() {
        let mgr = Arc::new(JobManager::new(1, Duration::ZERO));
        let (sink, _) = collecting_sink();
        let params = serde_json::json!({"prompt": "x", "outputPath": "/tmp/x.wav"});

        let err = run_generic_job(&mgr, "mysteryModel", &params, Arc::clone(&sink)).unwrap_err();
        assert!(err.contains("unknown job kind"), "{err}");

        let err = run_generic_job(&mgr, "stemSplit", &params, Arc::clone(&sink)).unwrap_err();
        assert!(err.contains("dedicated command"), "{err}");

        let err = run_generic_job(
            &mgr,
            "aceStepGenerate",
            &serde_json::json!([1, 2]),
            Arc::clone(&sink),
        )
        .unwrap_err();
        assert!(err.contains("JSON object"), "{err}");
        assert!(mgr.list().is_empty(), "nothing was submitted");
    }

    #[tokio::test]
    async fn ace_step_generate_simulate_end_to_end() {
        let dir = tmp_dir("ace-gen");
        let out = dir.join("gen.wav");
        let params = serde_json::json!({
            "prompt": "warm synthwave, driving bass",
            "durationSeconds": 1.5,
            "seed": 7,
            "outputPath": out,
        })
        .to_string();
        let Some(spec) = worker_spec(
            JobKind::AceStepGenerate,
            "ace_step_worker.py",
            &["--task", "generate", "--params-json", &params, "--simulate"],
            Vec::new(),
        ) else {
            eprintln!("skipping: python3 not found");
            return;
        };
        let (st, events) = run_to_terminal(spec).await;
        assert_eq!(st.state, "done", "err: {:?}", st.error);
        let result = st.result.unwrap();
        assert_eq!(result["kind"], "aceStepGenerate");
        assert_eq!(result["simulated"], true);
        // --params-json forwarding: worker echoes the params outputPath back.
        assert_eq!(result["outputPath"], out.to_string_lossy().as_ref());
        // The placeholder is a real, hound-decodable WAV of the right length.
        let mut r = hound::WavReader::open(&out).unwrap();
        let spec_w = r.spec();
        assert_eq!(spec_w.channels, 2);
        assert_eq!(spec_w.sample_rate, 48_000);
        let secs = r.duration() as f64 / spec_w.sample_rate as f64;
        assert!((secs - 1.5).abs() < 0.05, "duration {secs}");
        assert!(r.samples::<i16>().any(|s| s.unwrap() != 0), "not silent");
        // Staged progress arrived.
        let evs = events.lock();
        assert!(evs.iter().any(|e| matches!(
            e, SidecarEvent::Progress { stage, .. } if stage == "diffusing")));
        drop(evs);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn ace_step_repaint_simulate_preserves_input_length() {
        let dir = tmp_dir("ace-rep");
        let input = dir.join("in.wav");
        let wav_spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&input, wav_spec).unwrap();
        for i in 0..48_000 {
            let v = ((i as f32 * 0.05).sin() * 8000.0) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();

        let out = dir.join("rep.wav");
        let params = serde_json::json!({
            "inputPath": input,
            "startSeconds": 0.25,
            "endSeconds": 0.75,
            "outputPath": out,
        })
        .to_string();
        let Some(spec) = worker_spec(
            JobKind::AceStepRepaint,
            "ace_step_worker.py",
            &["--task", "repaint", "--params-json", &params, "--simulate"],
            Vec::new(),
        ) else {
            eprintln!("skipping: python3 not found");
            return;
        };
        let (st, _) = run_to_terminal(spec).await;
        assert_eq!(st.state, "done", "err: {:?}", st.error);
        assert_eq!(st.result.as_ref().unwrap()["kind"], "aceStepRepaint");
        let r = hound::WavReader::open(&out).unwrap();
        assert_eq!(r.duration(), 48_000, "repaint keeps the input length");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn ace_step_missing_param_is_clean_ndjson_error() {
        let params = serde_json::json!({"outputPath": "/tmp/never.wav"}).to_string();
        let Some(spec) = worker_spec(
            JobKind::AceStepGenerate,
            "ace_step_worker.py",
            &["--task", "generate", "--params-json", &params, "--simulate"],
            Vec::new(),
        ) else {
            eprintln!("skipping: python3 not found");
            return;
        };
        let (st, events) = run_to_terminal(spec).await;
        assert_eq!(st.state, "error");
        assert!(st.error.as_deref().unwrap().contains("prompt"), "{:?}", st.error);
        assert!(matches!(events.lock().last(), Some(SidecarEvent::Error { .. })));
    }

    #[tokio::test]
    async fn elevenlabs_simulate_end_to_end_writes_wav_without_network() {
        let dir = tmp_dir("el-sim");
        let out = dir.join("music.wav");
        let params = serde_json::json!({
            "prompt": "lofi hip hop, vinyl crackle",
            "durationSeconds": 1.0,
            "outputPath": out,
        })
        .to_string();
        // Point the base URL at a closed port: simulate must never dial it.
        let Some(spec) = worker_spec(
            JobKind::ElevenLabsMusic,
            "elevenlabs_music_worker.py",
            &["--params-json", &params, "--simulate"],
            vec![
                ("ELEVENLABS_API_KEY".into(), None),
                ("ELEVENLABS_BASE_URL".into(), Some("http://127.0.0.1:1".into())),
            ],
        ) else {
            eprintln!("skipping: python3 not found");
            return;
        };
        let (st, _) = run_to_terminal(spec).await;
        assert_eq!(st.state, "done", "err: {:?}", st.error);
        let result = st.result.unwrap();
        assert_eq!(result["kind"], "elevenLabsMusic");
        assert_eq!(result["simulated"], true);
        let r = hound::WavReader::open(&out).unwrap();
        assert_eq!(r.spec().sample_rate, 44_100);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn elevenlabs_missing_key_is_clean_error() {
        let params = serde_json::json!({
            "prompt": "x",
            "outputPath": "/tmp/never-elevenlabs.wav",
        })
        .to_string();
        let Some(spec) = worker_spec(
            JobKind::ElevenLabsMusic,
            "elevenlabs_music_worker.py",
            &["--params-json", &params],
            vec![
                ("ELEVENLABS_API_KEY".into(), None), // real path, no key
                ("ELEVENLABS_BASE_URL".into(), Some("http://127.0.0.1:1".into())),
            ],
        ) else {
            eprintln!("skipping: python3 not found");
            return;
        };
        let (st, _) = run_to_terminal(spec).await;
        assert_eq!(st.state, "error");
        assert!(
            st.error.as_deref().unwrap().contains("ELEVENLABS_API_KEY"),
            "{:?}",
            st.error
        );
    }

    /// Minimal one-shot HTTP server: reads one POST, answers `status` with
    /// `body`, records the request head+body for assertions.
    fn mock_http_server(
        status: &'static str,
        content_type: &'static str,
        body: Vec<u8>,
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            // Read headers.
            let header_end = loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break req.len();
                }
                req.extend_from_slice(&buf[..n]);
                if let Some(p) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    break p + 4;
                }
            };
            let head = String::from_utf8_lossy(&req[..header_end]).to_string();
            let content_length: usize = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())?
                })
                .unwrap_or(0);
            while req.len() < header_end + content_length {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
            }
            let _ = tx.send(String::from_utf8_lossy(&req).to_string());
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        });
        (port, rx)
    }

    #[tokio::test]
    async fn elevenlabs_real_path_against_local_mock_server() {
        // 0.5 s of mono 16-bit PCM at 44.1 kHz — what the API returns for
        // output_format=pcm_44100.
        let pcm = vec![0u8; 44_100];
        let (port, req_rx) = mock_http_server("200 OK", "audio/pcm", pcm);

        let dir = tmp_dir("el-mock");
        let out = dir.join("cloud.wav");
        let params = serde_json::json!({
            "prompt": "cinematic strings",
            "durationSeconds": 12.0,
            "outputPath": out,
        })
        .to_string();
        let Some(spec) = worker_spec(
            JobKind::ElevenLabsMusic,
            "elevenlabs_music_worker.py",
            &["--params-json", &params],
            vec![
                ("ELEVENLABS_API_KEY".into(), Some("rust-test-key".into())),
                (
                    "ELEVENLABS_BASE_URL".into(),
                    Some(format!("http://127.0.0.1:{port}")),
                ),
            ],
        ) else {
            eprintln!("skipping: python3 not found");
            return;
        };
        let (st, events) = run_to_terminal(spec).await;
        assert_eq!(st.state, "done", "err: {:?}", st.error);
        let result = st.result.unwrap();
        assert_eq!(result["kind"], "elevenLabsMusic");
        assert_eq!(result["simulated"], false);

        // Wrapped into a valid WAV of the PCM's length.
        let r = hound::WavReader::open(&out).unwrap();
        assert_eq!(r.spec().sample_rate, 44_100);
        assert_eq!(r.duration(), 22_050, "0.5 s of pcm_44100");

        // The worker sent the key as a header + prompt/length in the body...
        let request = req_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(request.contains("POST /v1/music"), "{request}");
        assert!(request.contains("output_format=pcm_44100"));
        assert!(request.to_lowercase().contains("xi-api-key: rust-test-key"));
        assert!(request.contains("cinematic strings"));
        assert!(request.contains("12000"), "durationSeconds -> music_length_ms");

        // ...and the key never leaked into any job event.
        for ev in events.lock().iter() {
            let s = serde_json::to_string(ev).unwrap();
            assert!(!s.contains("rust-test-key"), "API key leaked: {s}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn elevenlabs_quota_error_is_structured() {
        let body =
            br#"{"detail":{"status":"quota_exceeded","message":"out of credits"}}"#.to_vec();
        let (port, _req_rx) = mock_http_server("429 Too Many Requests", "application/json", body);
        let params = serde_json::json!({
            "prompt": "x",
            "outputPath": "/tmp/never-quota.wav",
        })
        .to_string();
        let Some(spec) = worker_spec(
            JobKind::ElevenLabsMusic,
            "elevenlabs_music_worker.py",
            &["--params-json", &params],
            vec![
                ("ELEVENLABS_API_KEY".into(), Some("rust-test-key".into())),
                (
                    "ELEVENLABS_BASE_URL".into(),
                    Some(format!("http://127.0.0.1:{port}")),
                ),
            ],
        ) else {
            eprintln!("skipping: python3 not found");
            return;
        };
        let (st, _) = run_to_terminal(spec).await;
        assert_eq!(st.state, "error");
        let msg = st.error.unwrap();
        assert!(msg.contains("quota"), "{msg}");
        assert!(msg.contains("out of credits"), "{msg}");
        assert!(!msg.contains("rust-test-key"), "key leaked into error");
    }
}
