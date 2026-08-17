//! Song export ("bounce") — wave-1D.
//!
//! Pipeline: snapshot the project under the store/midi locks → build a
//! private offline graph ([`crate::audio::offline`], the SAME graph path the
//! transport plays) → block render faster than realtime → WAV via hound
//! (16/24-bit int or 32-bit float) → optional format conversion / resample
//! through an EXTERNAL `ffmpeg` subprocess (mp3/flac/ogg/m4a; never a crate
//! dependency). Temp WAVs are guard-deleted on every path.
//!
//! Job tracking mirrors the sidecar registry shape (`SidecarJobStatus`
//! fields) in a module-local registry — the sidecar `JobManager` supervises
//! external processes only, and this export runs in-process on its own
//! thread. Progress flows through the established app-event pattern:
//! `export://progress` (throttled ≤ 10 Hz), `export://done`,
//! `export://error`, emitted via the control plane's injected emitter so
//! Tauri UI and MCP front doors observe the same events.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::audio::offline;
use crate::audio::types::Store;
use crate::midi::MidiStore;

use super::ControlPlane;

/// Default release tail appended after the last clip/note end of a FULL
/// bounce, seconds (covers instrument releases; the PolySynth's is 80 ms,
/// sampler envelopes may run longer). Loop-region bounces default to 0 so
/// the file length equals the region exactly.
pub const DEFAULT_TAIL_SECS: f64 = 1.0;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Argument of `export_song` (Tauri command + control-plane method).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    /// Absolute destination file path.
    pub path: String,
    /// "wav" | "mp3" | "flac" | "ogg" | "m4a". Default: inferred from the
    /// path's extension, falling back to "wav".
    pub format: Option<String>,
    /// "full" (default) or "loop" (the transport loop region).
    pub region: Option<String>,
    /// WAV bit depth: 16 | 24 | 32 (32 = IEEE float). Default 24.
    pub bit_depth: Option<u16>,
    /// Optional output sample rate; differing from the engine rate requires
    /// ffmpeg (resample happens there). Default: engine rate.
    pub sample_rate: Option<u32>,
    /// Master gain applied to the summed stereo bus, dB. Default 0.
    pub master_gain_db: Option<f64>,
    /// Release tail after the region end, seconds. Defaults:
    /// [`DEFAULT_TAIL_SECS`] for "full", 0.0 for "loop".
    pub tail_seconds: Option<f64>,
}

/// Export job status — same field shape as `SidecarJobStatus` so UI/MCP job
/// views can render both kinds uniformly.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJobStatus {
    pub job_id: String,
    /// Always "exportSong".
    pub kind: String,
    /// "queued" | "running" | "done" | "error"
    pub state: String,
    /// [0.0, 1.0]
    pub progress: f64,
    pub stage: String,
    /// Present when state == "done": `{path, format, sampleRate, bitDepth,
    /// frames, durationSeconds, sizeBytes}`.
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// Payload of `export_capabilities`: lets the UI grey out formats that need
/// a missing ffmpeg.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCapabilities {
    /// Formats exportable right now ("wav" always; the rest with ffmpeg).
    pub formats: Vec<String>,
    pub ffmpeg: bool,
    pub ffmpeg_path: Option<String>,
    /// Present when ffmpeg is missing: how to get the extra formats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

// ---------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Wav,
    Mp3,
    Flac,
    Ogg,
    M4a,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "wav" => Some(Self::Wav),
            "mp3" => Some(Self::Mp3),
            "flac" => Some(Self::Flac),
            "ogg" => Some(Self::Ogg),
            "m4a" | "aac" | "mp4" => Some(Self::M4a),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Flac => "flac",
            Self::Ogg => "ogg",
            Self::M4a => "m4a",
        }
    }

    /// Per-format ffmpeg encoder settings (sensible defaults).
    fn ffmpeg_codec_args(self) -> &'static [&'static str] {
        match self {
            Self::Wav => &[], // pcm codec chosen from bit depth by the caller
            Self::Mp3 => &["-codec:a", "libmp3lame", "-q:a", "2"],
            Self::Flac => &["-codec:a", "flac"],
            Self::Ogg => &["-codec:a", "libvorbis", "-q:a", "6"],
            Self::M4a => &["-codec:a", "aac", "-b:a", "192k"],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Full,
    Loop,
}

/// Validated export plan (pure — unit-testable without a control plane).
#[derive(Debug)]
struct ExportPlan {
    dst: PathBuf,
    format: ExportFormat,
    bit_depth: u16,
    sample_rate: Option<u32>,
    master_gain: f32,
    region: Region,
    tail_seconds: f64,
}

fn plan_export(req: &ExportRequest, have_ffmpeg: bool) -> Result<ExportPlan, String> {
    let dst = PathBuf::from(&req.path);
    if !dst.is_absolute() {
        return Err(format!("path must be absolute: {}", req.path));
    }
    let format = match &req.format {
        Some(f) => ExportFormat::parse(f)
            .ok_or_else(|| format!("unsupported export format: {f} (wav|mp3|flac|ogg|m4a)"))?,
        None => dst
            .extension()
            .and_then(|e| e.to_str())
            .and_then(ExportFormat::parse)
            .unwrap_or(ExportFormat::Wav),
    };
    if format != ExportFormat::Wav && !have_ffmpeg {
        return Err(missing_ffmpeg_error(format.as_str()));
    }
    let bit_depth = req.bit_depth.unwrap_or(24);
    if !matches!(bit_depth, 16 | 24 | 32) {
        return Err(format!("unsupported bit depth: {bit_depth} (16|24|32)"));
    }
    let region = match req.region.as_deref().unwrap_or("full") {
        "full" => Region::Full,
        "loop" => Region::Loop,
        other => return Err(format!("unknown region: {other} (full|loop)")),
    };
    let tail_seconds = req
        .tail_seconds
        .unwrap_or(match region {
            Region::Full => DEFAULT_TAIL_SECS,
            Region::Loop => 0.0,
        })
        .clamp(0.0, 60.0);
    Ok(ExportPlan {
        dst,
        format,
        bit_depth,
        sample_rate: req.sample_rate,
        master_gain: crate::audio::mixer::db_to_linear(
            req.master_gain_db.unwrap_or(0.0).clamp(-160.0, 24.0),
        ),
        region,
        tail_seconds,
    })
}

// ---------------------------------------------------------------------------
// ffmpeg detection (once) + conversion
// ---------------------------------------------------------------------------

/// Find an executable `ffmpeg` in a PATH-shaped variable (injected for the
/// missing-ffmpeg tests; production passes the process PATH).
fn find_ffmpeg_in(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path = path_var?;
    for dir in std::env::split_paths(path) {
        let cand = dir.join("ffmpeg");
        if is_executable(&cand) {
            return Some(cand);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// ffmpeg on PATH, probed once per process.
pub fn ffmpeg_path() -> Option<&'static PathBuf> {
    static FFMPEG: OnceLock<Option<PathBuf>> = OnceLock::new();
    FFMPEG
        .get_or_init(|| find_ffmpeg_in(std::env::var_os("PATH").as_deref()))
        .as_ref()
}

fn missing_ffmpeg_error(what: &str) -> String {
    format!(
        "exporting {what} requires ffmpeg, which was not found on PATH — \
         install it (Debian/Ubuntu: `apt install ffmpeg`) or export WAV instead"
    )
}

/// Capabilities snapshot for the UI/MCP (grey out formats without ffmpeg).
pub fn capabilities() -> ExportCapabilities {
    let ff = ffmpeg_path();
    let mut formats = vec!["wav".to_string()];
    if ff.is_some() {
        for f in ["mp3", "flac", "ogg", "m4a"] {
            formats.push(f.to_string());
        }
    }
    ExportCapabilities {
        formats,
        ffmpeg: ff.is_some(),
        ffmpeg_path: ff.map(|p| p.display().to_string()),
        hint: if ff.is_some() {
            None
        } else {
            Some("install ffmpeg (e.g. `apt install ffmpeg`) to export mp3/flac/ogg/m4a".into())
        },
    }
}

/// Deletes the wrapped temp file on drop — conversion temp WAVs never
/// outlive the export, whatever path it takes.
struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Convert `src` (WAV) to `dst` with ffmpeg. `progress` receives [0, 1]
/// parsed from ffmpeg's `-progress pipe:1` key/value stream (`out_time_us`),
/// falling back to stage-based jumps when unparseable. `ffmpeg: None` yields
/// the structured missing-ffmpeg error.
fn convert_with_ffmpeg(
    ffmpeg: Option<&Path>,
    src: &Path,
    dst: &Path,
    format: ExportFormat,
    bit_depth: u16,
    sample_rate: Option<u32>,
    duration_secs: f64,
    progress: &mut dyn FnMut(f64),
) -> Result<(), String> {
    let ffmpeg = ffmpeg.ok_or_else(|| missing_ffmpeg_error(format.as_str()))?;
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "error"])
        .args(["-progress", "pipe:1"])
        .arg("-y")
        .arg("-i")
        .arg(src);
    if format == ExportFormat::Wav {
        let codec = match bit_depth {
            16 => "pcm_s16le",
            24 => "pcm_s24le",
            _ => "pcm_f32le",
        };
        cmd.args(["-codec:a", codec]);
    } else {
        cmd.args(format.ffmpeg_codec_args());
    }
    if let Some(rate) = sample_rate {
        cmd.args(["-ar", &rate.to_string()]);
    }
    cmd.arg(dst)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch ffmpeg ({}): {e}", ffmpeg.display()))?;

    // stderr drained on a helper thread (avoids pipe deadlock on long logs).
    let stderr_buf = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = s.read_to_string(&mut buf);
            buf
        })
    });
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            // `-progress` emits `out_time_us=<micros>` (out_time_ms is the
            // same value in ffmpeg's historical micros quirk — either works).
            if let Some(v) = line
                .strip_prefix("out_time_us=")
                .or_else(|| line.strip_prefix("out_time_ms="))
            {
                if let Ok(us) = v.trim().parse::<i64>() {
                    if duration_secs > 0.0 && us >= 0 {
                        progress((us as f64 / 1e6 / duration_secs).clamp(0.0, 1.0));
                    }
                }
            }
        }
    }
    let status = child
        .wait()
        .map_err(|e| format!("ffmpeg did not exit cleanly: {e}"))?;
    let stderr = stderr_buf
        .and_then(|t| t.join().ok())
        .unwrap_or_default();
    if !status.success() {
        let tail: String = stderr.lines().rev().take(8).collect::<Vec<_>>().iter().rev()
            .cloned().collect::<Vec<_>>().join("\n");
        return Err(format!(
            "ffmpeg failed ({status}) while encoding {}{}",
            format.as_str(),
            if tail.is_empty() { String::new() } else { format!(":\n{tail}") }
        ));
    }
    progress(1.0);
    Ok(())
}

// ---------------------------------------------------------------------------
// WAV writing (hound)
// ---------------------------------------------------------------------------

/// Write interleaved stereo f32 as WAV: 16/24-bit int (clamped, rounded) or
/// 32-bit IEEE float.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32, bit_depth: u16) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: bit_depth,
        sample_format: if bit_depth == 32 {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let mut w = hound::WavWriter::create(path, spec)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match bit_depth {
        16 => {
            for s in samples {
                w.write_sample((s.clamp(-1.0, 1.0) * 32_767.0).round() as i16)
                    .map_err(|e| e.to_string())?;
            }
        }
        24 => {
            for s in samples {
                w.write_sample((s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32)
                    .map_err(|e| e.to_string())?;
            }
        }
        32 => {
            for s in samples {
                w.write_sample(*s).map_err(|e| e.to_string())?;
            }
        }
        other => return Err(format!("unsupported bit depth: {other} (16|24|32)")),
    }
    w.finalize().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Job registry (module-local; same status shape as sidecar jobs)
// ---------------------------------------------------------------------------

fn jobs() -> &'static Mutex<HashMap<String, ExportJobStatus>> {
    static JOBS: OnceLock<Mutex<HashMap<String, ExportJobStatus>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Status of one export job (None = unknown id).
pub fn job_status(job_id: &str) -> Option<ExportJobStatus> {
    jobs().lock().get(job_id).cloned()
}

/// All known export jobs (the registry is small: exports are rare and the
/// process-lifetime entry count is bounded by actual user exports).
pub fn list_jobs() -> Vec<ExportJobStatus> {
    jobs().lock().values().cloned().collect()
}

/// Registry writer + throttled `export://progress` emitter (≤ 10 Hz, always
/// letting stage changes and completion through — the sidecar convention).
struct Progress<'a> {
    cp: &'a ControlPlane,
    job_id: String,
    last_emit: Option<Instant>,
    last_stage: String,
}

impl<'a> Progress<'a> {
    fn new(cp: &'a ControlPlane, job_id: &str) -> Self {
        Self { cp, job_id: job_id.to_string(), last_emit: None, last_stage: String::new() }
    }

    fn update(&mut self, progress: f64, stage: &str) {
        if let Some(e) = jobs().lock().get_mut(&self.job_id) {
            if e.state == "queued" {
                e.state = "running".into();
            }
            e.progress = progress;
            e.stage = stage.to_string();
        }
        let now = Instant::now();
        let due = stage != self.last_stage
            || progress >= 1.0
            || self.last_emit.map_or(true, |t| now.duration_since(t) >= Duration::from_millis(100));
        if due {
            self.last_emit = Some(now);
            self.last_stage = stage.to_string();
            (self.cp.emit)(
                "export://progress",
                json!({ "jobId": self.job_id, "progress": progress, "stage": stage }),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Control-plane front door
// ---------------------------------------------------------------------------

/// Everything the export thread needs, snapshotted under the locks so the
/// render never blocks (or races) the live session.
struct ExportSnapshot {
    store: Store,
    midi: MidiStore,
    /// Plugin document rows (Task 9) — `offline::build_graph` needs these to
    /// resolve `plugin:<id>` track bindings the same way live playback does
    /// ("what you hear is what you export").
    plugins: crate::control::session::PluginDoc,
    /// Automation lanes (Track D close-out) — without them a bounce ignored
    /// the gain curves playback obeys.
    automation: crate::control::session::AutomationDoc,
    /// Track F modulation graph — pan (and future) ramps. Empty for v3
    /// projects; `build_graph` still compiles the lane table in that case.
    modulation: crate::modulation::ModulationDoc,
    rate: u32,
    loop_region: (u64, u64),
}

impl ControlPlane {
    /// Formats currently exportable (UI greys out the rest).
    pub fn export_capabilities(&self) -> ExportCapabilities {
        capabilities()
    }

    /// Status of an export job started by [`ControlPlane::export_song`].
    pub fn export_job_status(&self, job_id: &str) -> Result<ExportJobStatus, String> {
        job_status(job_id).ok_or_else(|| format!("unknown export job: {job_id}"))
    }

    /// Bounce the song (or the loop region) to `request.path`. Returns the
    /// job id immediately; the render runs on its own thread and reports via
    /// the export job registry + `export://progress|done|error` events.
    pub fn export_song(self: &Arc<Self>, request: ExportRequest) -> Result<String, String> {
        let plan = plan_export(&request, ffmpeg_path().is_some())?;
        if plan.format == ExportFormat::Wav {
            if let Some(rate) = plan.sample_rate {
                let engine = self.shared.sample_rate.load(Relaxed);
                if rate != engine && ffmpeg_path().is_none() {
                    return Err(missing_ffmpeg_error(&format!(
                        "WAV resampled to {rate} Hz (engine runs at {engine} Hz)"
                    )));
                }
            }
        }

        // Snapshot the project: a private Store + a cloned MidiStore. One
        // guard covers both — store and midi live behind the same lock.
        // Round-2 §2.4: `offline::build_graph` derives slots itself
        // (`types::derive_slots`) from `store.tracks`, so there is nothing
        // to pre-seed here — every store track gets a slot, always.
        let snapshot = {
            let session = self.session.lock();
            let s = &session.store;
            let mut store = Store::default();
            store.transport = s.transport.clone();
            store.project_dir = s.project_dir.clone();
            store.tracks = s.tracks.clone();
            store.clips = s.clips.clone();
            let m = &session.midi;
            ExportSnapshot {
                store,
                midi: MidiStore {
                    ppq: m.ppq,
                    tempo_events: m.tempo_events.clone(),
                    meter_events: m.meter_events.clone(),
                    clips: m.clips.clone(),
                    // The bounce reads notes, not chords: the harmony document
                    // makes no sound of its own, so an offline render has no
                    // use for it. Cloned anyway rather than defaulted, so a
                    // future harmony-aware render step finds it here.
                    harmony: m.harmony.clone(),
                    launch_maps: m.launch_maps.clone(),
                    loaded_dir: None,
                    dirty: false,
                },
                plugins: session.plugins.clone(),
                automation: crate::control::session::AutomationDoc {
                    lanes: session.automation.lanes.clone(),
                },
                modulation: session.modulation.clone(),
                rate: self.shared.sample_rate.load(Relaxed),
                loop_region: (
                    self.shared.loop_start.load(Relaxed),
                    self.shared.loop_end.load(Relaxed),
                ),
            }
        };
        if plan.region == Region::Loop && snapshot.loop_region.1 <= snapshot.loop_region.0 {
            return Err("no loop region set — set one with transport_set_loop first".into());
        }
        let has_content = !snapshot.store.clips.is_empty()
            || snapshot.midi.clips.iter().any(|c| !c.notes.is_empty());
        if !has_content {
            return Err("nothing to export — the project has no audio clips or midi notes".into());
        }

        let job_id = uuid::Uuid::new_v4().to_string();
        jobs().lock().insert(
            job_id.clone(),
            ExportJobStatus {
                job_id: job_id.clone(),
                kind: "exportSong".into(),
                state: "queued".into(),
                progress: 0.0,
                stage: "queued".into(),
                result: None,
                error: None,
            },
        );
        (self.emit)(
            "export://progress",
            json!({ "jobId": job_id, "progress": 0.0, "stage": "queued" }),
        );
        let cp = Arc::clone(self);
        let id = job_id.clone();
        std::thread::Builder::new()
            .name("aura-export".into())
            .spawn(move || run_export(cp, id, plan, snapshot))
            .map_err(|e| format!("failed to start export thread: {e}"))?;
        Ok(job_id)
    }
}

fn run_export(cp: Arc<ControlPlane>, job_id: String, plan: ExportPlan, snap: ExportSnapshot) {
    match do_export(&cp, &job_id, &plan, &snap) {
        Ok(result) => {
            if let Some(e) = jobs().lock().get_mut(&job_id) {
                e.state = "done".into();
                e.progress = 1.0;
                e.stage = "done".into();
                e.result = Some(result.clone());
            }
            let mut payload = result;
            payload["jobId"] = json!(job_id);
            (cp.emit)("export://done", payload);
        }
        Err(message) => {
            if let Some(e) = jobs().lock().get_mut(&job_id) {
                e.state = "error".into();
                e.stage = "error".into();
                e.error = Some(message.clone());
            }
            (cp.emit)(
                "export://error",
                json!({ "jobId": job_id, "message": message }),
            );
        }
    }
}

fn do_export(
    cp: &ControlPlane,
    job_id: &str,
    plan: &ExportPlan,
    snap: &ExportSnapshot,
) -> Result<serde_json::Value, String> {
    let mut prog = Progress::new(cp, job_id);
    prog.update(0.02, "building graph");

    // Sampler instruments come from the app-wide bank (None headless): the
    // guard is held only while the graph is built, never during the render.
    let mut og = {
        let bank = crate::audio::sampler::registered_bank().map(|b| b.lock());
        offline::build_graph(&snap.store, &snap.midi, &snap.plugins, &snap.automation, &snap.modulation, bank.as_deref(), snap.rate)
    };

    let tail = (plan.tail_seconds * snap.rate as f64).round() as u64;
    let (start, frames) = match plan.region {
        Region::Full => (0u64, og.end_samples + tail),
        Region::Loop => {
            let (s, e) = snap.loop_region;
            (s, (e - s) + tail)
        }
    };
    if frames == 0 {
        return Err("nothing to export — the rendered region is empty".into());
    }

    let needs_ffmpeg = plan.format != ExportFormat::Wav
        || plan.sample_rate.is_some_and(|r| r != snap.rate);
    // Progress budget: render 0.02→0.75, wav write →0.85, encode →1.0
    // (wav-only: render →0.9, write →1.0).
    let render_top = if needs_ffmpeg { 0.75 } else { 0.90 };
    let samples = offline::render(
        &mut og.graph,
        start,
        frames,
        snap.rate,
        plan.master_gain,
        &mut |done, total| {
            let f = done as f64 / total.max(1) as f64;
            prog.update(0.02 + f * (render_top - 0.02), "rendering");
        },
    );
    let duration_secs = frames as f64 / snap.rate as f64;

    if let Some(parent) = plan.dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if !needs_ffmpeg {
        prog.update(render_top, "writing wav");
        write_wav(&plan.dst, &samples, snap.rate, plan.bit_depth)?;
    } else {
        prog.update(render_top, "writing wav");
        let tmp_dir = plan
            .dst
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(std::env::temp_dir);
        let tmp = TempGuard(tmp_dir.join(format!(".aura-export-{job_id}.wav")));
        // Lossy targets encode from a float master; WAV resample keeps the
        // requested depth end-to-end.
        let tmp_depth = if plan.format == ExportFormat::Wav { plan.bit_depth } else { 32 };
        write_wav(&tmp.0, &samples, snap.rate, tmp_depth)?;
        prog.update(0.85, "encoding");
        let out_rate = plan.sample_rate.filter(|&r| r != snap.rate);
        convert_with_ffmpeg(
            ffmpeg_path().map(|p| p.as_path()),
            &tmp.0,
            &plan.dst,
            plan.format,
            plan.bit_depth,
            out_rate,
            duration_secs,
            &mut |f| prog.update(0.85 + f * 0.15, "encoding"),
        )?;
        // `tmp` guard drops here → temp WAV removed (also on every `?` above).
    }

    let size_bytes = std::fs::metadata(&plan.dst).map(|m| m.len()).unwrap_or(0);
    prog.update(1.0, "done");
    Ok(json!({
        "path": plan.dst.display().to_string(),
        "format": plan.format.as_str(),
        "region": if plan.region == Region::Loop { "loop" } else { "full" },
        "sampleRate": plan.sample_rate.unwrap_or(snap.rate),
        "bitDepth": plan.bit_depth,
        "frames": frames,
        "durationSeconds": duration_secs,
        "sizeBytes": size_bytes,
    }))
}

// ---------------------------------------------------------------------------
// Commands (need registration in the frozen lib.rs — requested via report)
// ---------------------------------------------------------------------------

/// Start a song bounce; returns the export job id. Track progress via
/// `export_job_status` polling or the `export://progress|done|error` events.
#[tauri::command]
pub fn export_song(
    request: ExportRequest,
    control: tauri::State<'_, Arc<ControlPlane>>,
) -> Result<String, String> {
    control.export_song(request)
}

/// Status of one export job.
#[tauri::command]
pub fn export_job_status(job_id: String) -> Result<ExportJobStatus, String> {
    job_status(&job_id).ok_or_else(|| format!("unknown export job: {job_id}"))
}

/// What can be exported right now (ffmpeg-gated formats included only when
/// ffmpeg is on PATH).
#[tauri::command]
pub fn export_capabilities() -> ExportCapabilities {
    capabilities()
}

// ---------------------------------------------------------------------------
// Tests (tauri-free; engine spins up headless like the control tests)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::engine::load_wav;
    use crate::audio::rt::testutil::empty_tables;
    use crate::audio::rt::SharedRt;
    use crate::midi::MidiStore;
    use crate::sidecars::jobs::JobManager;
    use serde_json::Value;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-export-test-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // ---- WAV bit depths ---------------------------------------------------

    #[test]
    fn wav_bit_depths_are_valid_and_roundtrip() {
        let dir = tmp_dir("wav-depths");
        // 1 kHz-ish ramp, stereo interleaved, amplitude 0.5.
        let samples: Vec<f32> = (0..2_000)
            .map(|i| ((i / 2) as f32 * 0.13).sin() * 0.5)
            .collect();
        for (depth, fmt, bits) in [
            (16u16, hound::SampleFormat::Int, 16u16),
            (24, hound::SampleFormat::Int, 24),
            (32, hound::SampleFormat::Float, 32),
        ] {
            let p = dir.join(format!("d{depth}.wav"));
            write_wav(&p, &samples, 48_000, depth).unwrap();
            let spec = hound::WavReader::open(&p).unwrap().spec();
            assert_eq!((spec.channels, spec.sample_rate), (2, 48_000));
            assert_eq!((spec.sample_format, spec.bits_per_sample), (fmt, bits));
            // Decodes back to the same audio within the depth's precision.
            let (ch, rate, decoded) = load_wav(&p).unwrap();
            assert_eq!((ch, rate), (2, 48_000));
            assert_eq!(decoded.len(), samples.len());
            let tol = match depth {
                16 => 1.0 / 32_768.0,
                24 => 1.0 / 8_388_608.0,
                _ => 0.0,
            };
            for (a, b) in samples.iter().zip(decoded.iter()) {
                assert!((a - b).abs() <= tol + 1e-9, "depth {depth}: {a} vs {b}");
            }
        }
        assert!(write_wav(&dir.join("bad.wav"), &samples, 48_000, 20).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- plan validation --------------------------------------------------

    #[test]
    fn plan_infers_format_and_validates() {
        let req = |path: &str, format: Option<&str>| ExportRequest {
            path: path.into(),
            format: format.map(str::to_string),
            region: None,
            bit_depth: None,
            sample_rate: None,
            master_gain_db: None,
            tail_seconds: None,
        };
        let p = plan_export(&req("/tmp/x/song.mp3", None), true).unwrap();
        assert_eq!(p.format, ExportFormat::Mp3);
        assert_eq!(p.bit_depth, 24, "default bit depth");
        assert_eq!(p.tail_seconds, DEFAULT_TAIL_SECS);
        let p = plan_export(&req("/tmp/x/song.unknownext", None), false).unwrap();
        assert_eq!(p.format, ExportFormat::Wav, "fallback format");
        assert!(plan_export(&req("relative.wav", None), true)
            .unwrap_err()
            .contains("absolute"));
        assert!(plan_export(&req("/tmp/x.wav", Some("xm")), true)
            .unwrap_err()
            .contains("unsupported export format"));
        // Loop region defaults to no tail.
        let mut r = req("/tmp/x.wav", None);
        r.region = Some("loop".into());
        assert_eq!(plan_export(&r, true).unwrap().tail_seconds, 0.0);
        r.region = Some("half".into());
        assert!(plan_export(&r, true).is_err());
        let mut r = req("/tmp/x.wav", None);
        r.bit_depth = Some(20);
        assert!(plan_export(&r, true).unwrap_err().contains("bit depth"));
    }

    // ---- missing ffmpeg ---------------------------------------------------

    #[test]
    fn missing_ffmpeg_paths_yield_structured_errors() {
        // PATH-stripped detection: nothing to find.
        assert!(find_ffmpeg_in(None).is_none());
        assert!(find_ffmpeg_in(Some(OsStr::new(""))).is_none());
        let empty = tmp_dir("no-ffmpeg-here");
        assert!(find_ffmpeg_in(Some(empty.as_os_str())).is_none());

        // Plan-time gate: mp3 without ffmpeg tells the user how to fix it.
        let req = ExportRequest {
            path: "/tmp/x/song.mp3".into(),
            format: None,
            region: None,
            bit_depth: None,
            sample_rate: None,
            master_gain_db: None,
            tail_seconds: None,
        };
        let e = plan_export(&req, false).unwrap_err();
        assert!(e.contains("apt install ffmpeg"), "{e}");

        // Convert-time gate too (belt and braces).
        let e = convert_with_ffmpeg(
            None,
            Path::new("/tmp/in.wav"),
            Path::new("/tmp/out.mp3"),
            ExportFormat::Mp3,
            24,
            None,
            1.0,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(e.contains("apt install ffmpeg"), "{e}");
        let _ = std::fs::remove_dir_all(empty);
    }

    // ---- end-to-end through the control plane ----------------------------

    struct NullEvents;
    impl crate::audio::engine::EventSink for NullEvents {
        fn emit(&self, _e: &str, _p: Value) {}
    }

    fn control_plane_with_demo() -> (Arc<ControlPlane>, Arc<SharedRt>, Arc<Mutex<Vec<(String, Value)>>>) {
        let shared = Arc::new(SharedRt::default());
        let tables = empty_tables();
        let session = Arc::new(Mutex::new(crate::control::Session::new(
            Store::default(),
            MidiStore::default(),
        )));
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
            crate::control::testutil::test_committer(&session, &shared, &tables),
        );
        // Round-trip barrier: once the engine thread answers, its startup
        // `open_output` has finished, so the engine rate is final (a mid-test
        // rate flip would make length/determinism assertions racy).
        let _ = engine.request::<()>(|reply| crate::audio::engine::ControlMsg::SelectOutput {
            device_id: None,
            reply,
        });
        let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let cp = Arc::new(ControlPlane::new(
            session.clone(),
            shared.clone(),
            tables,
            engine,
            Arc::new(JobManager::default()),
            Box::new(move |e, p| ev.lock().push((e.to_string(), p))),
            std::sync::Arc::new(crate::control::HistoryLog::new()),
        ));
        // Demo-song content, placed directly (no engine round-trip needed).
        let (keys, bass) = {
            let mut session = session.lock();
            let k = super::super::ops::add_track(&mut session.store, Some("Keys".into()), Some("midi".into())).unwrap();
            let b = super::super::ops::add_track(&mut session.store, Some("Bass".into()), Some("midi".into())).unwrap();
            (k, b)
        };
        {
            let mut session = session.lock();
            let ppq = session.midi.ppq;
            let (arp, groove) = crate::control::demo_seed_clips(keys.id.as_str(), bass.id.as_str(), ppq);
            session.midi.clips.push(arp);
            session.midi.clips.push(groove);
        }
        (cp, shared, events)
    }

    fn wait_done(job_id: &str, timeout: Duration) -> ExportJobStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let st = job_status(job_id).expect("job registered");
            if st.state == "done" || st.state == "error" {
                return st;
            }
            assert!(Instant::now() < deadline, "export did not finish: {st:?}");
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn req_to(path: &Path) -> ExportRequest {
        ExportRequest {
            path: path.to_string_lossy().into_owned(),
            format: None,
            region: None,
            bit_depth: None,
            sample_rate: None,
            master_gain_db: None,
            tail_seconds: None,
        }
    }

    /// Full-song WAV bounce through the real control plane: correct length
    /// (song end + tail at the engine rate), audible, deterministic
    /// (two exports are sample-identical), events fired, and the loop-region
    /// bounce is exactly the region length.
    #[test]
    fn export_song_wav_full_and_loop_region() {
        let (cp, shared, events) = control_plane_with_demo();
        let rate = shared.sample_rate.load(Relaxed) as u64;
        let dir = tmp_dir("e2e-wav");

        // Nothing-to-export guard is NOT hit (we seeded content); but a loop
        // export without a region is refused up front.
        let mut r = req_to(&dir.join("loop.wav"));
        r.region = Some("loop".into());
        assert!(cp.export_song(r).unwrap_err().contains("no loop region"));

        // Full bounce, default 24-bit.
        let dst = dir.join("full.wav");
        let job = cp.export_song(req_to(&dst)).unwrap();
        let st = wait_done(&job, Duration::from_secs(120));
        assert_eq!(st.state, "done", "{:?}", st.error);
        let result = st.result.expect("result");
        assert_eq!(result["format"], "wav");
        assert_eq!(result["bitDepth"], 24);
        assert_eq!(result["sampleRate"], rate);
        let frames = result["frames"].as_u64().unwrap();
        // Demo song @48k ends at 383400 (last note-off, tick 15336 at 25
        // samples/tick) plus the default 1 s tail. At another engine rate
        // (real device) the tick->sample conversion rounds per event, so
        // assert exactly at 48k and proportionally otherwise.
        if rate == 48_000 {
            assert_eq!(frames, 383_400 + rate, "song end + 1 s tail");
        } else {
            let approx = 15_336.0 * rate as f64 * 60.0 / (120.0 * 960.0) + rate as f64;
            assert!((frames as f64 - approx).abs() < 4.0, "{frames} vs {approx}");
        }
        let (ch, file_rate, samples) = load_wav(&dst).unwrap();
        assert_eq!((ch, file_rate as u64), (2, rate));
        assert_eq!(samples.len() as u64, frames * 2, "file length exact");
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "bounce is audible (peak {peak})");
        assert!(result["sizeBytes"].as_u64().unwrap() > 0);
        assert!(
            (result["durationSeconds"].as_f64().unwrap()
                - frames as f64 / rate as f64)
                .abs()
                < 1e-9
        );

        // Determinism: a second export is bit-exact.
        let dst2 = dir.join("full-2.wav");
        let job2 = cp.export_song(req_to(&dst2)).unwrap();
        assert_eq!(wait_done(&job2, Duration::from_secs(120)).state, "done");
        let (_, _, samples2) = load_wav(&dst2).unwrap();
        assert_eq!(samples, samples2, "offline bounce is deterministic");

        // Loop region: exactly the region length (default tail 0).
        let (ls, le) = (rate / 2, rate / 2 + rate / 4);
        cp.transport(crate::control::TransportAction::SetLoop {
            enabled: true,
            start_samples: ls,
            end_samples: le,
        })
        .unwrap();
        let dst3 = dir.join("loop.wav");
        let mut r = req_to(&dst3);
        r.region = Some("loop".into());
        r.bit_depth = Some(16);
        let job3 = cp.export_song(r).unwrap();
        let st3 = wait_done(&job3, Duration::from_secs(120));
        assert_eq!(st3.state, "done", "{:?}", st3.error);
        assert_eq!(st3.result.as_ref().unwrap()["frames"].as_u64().unwrap(), le - ls);
        let reader = hound::WavReader::open(&dst3).unwrap();
        assert_eq!(reader.spec().bits_per_sample, 16);
        assert_eq!(reader.duration() as u64, le - ls, "loop-region length exact");

        // Event surface: progress + done fired through the emitter.
        let evs = events.lock();
        assert!(evs.iter().any(|(e, _)| e == "export://progress"));
        let dones: Vec<_> = evs.iter().filter(|(e, _)| e == "export://done").collect();
        assert_eq!(dones.len(), 3);
        assert!(dones.iter().any(|(_, p)| p["jobId"] == json!(job)));
        drop(evs);
        cp.export_job_status(&job).unwrap();
        assert!(cp.export_job_status("ghost").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Track D close-out: the bounce obeys track-gain automation — the
    /// snapshot must CARRY the session's lanes, not just be able to compile
    /// them. A flat 0.5 lane on every track must halve the whole file,
    /// sample for sample; before this, an export ignored automation entirely
    /// and came out identical to the unautomated bounce.
    #[test]
    fn export_song_applies_track_gain_automation() {
        use crate::plugins::automation::{AutomationLane, AutomationPoint, TRACK_PARAM_GAIN};
        let (cp, _shared, _events) = control_plane_with_demo();
        let dir = tmp_dir("e2e-automation");

        let plain_path = dir.join("plain.wav");
        let job = cp.export_song(req_to(&plain_path)).unwrap();
        assert_eq!(wait_done(&job, Duration::from_secs(120)).state, "done");
        let (_, _, plain) = load_wav(&plain_path).unwrap();
        let peak = plain.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "baseline bounce is audible (peak {peak})");

        {
            let mut session = cp.session.lock();
            let ids: Vec<String> =
                session.store.tracks.iter().map(|t| t.id.to_string()).collect();
            session.automation.lanes = ids
                .iter()
                .enumerate()
                .map(|(i, id)| AutomationLane {
                    id: format!("lane-{i}"),
                    target_node: format!("track:{id}"),
                    param_id: TRACK_PARAM_GAIN,
                    points: vec![
                        AutomationPoint { tick: 0, value: 0.5 },
                        AutomationPoint { tick: 1_000_000, value: 0.5 },
                    ],
                })
                .collect();
        }

        let auto_path = dir.join("automated.wav");
        let job = cp.export_song(req_to(&auto_path)).unwrap();
        assert_eq!(wait_done(&job, Duration::from_secs(120)).state, "done");
        let (_, _, automated) = load_wav(&auto_path).unwrap();
        assert_eq!(automated.len(), plain.len(), "same region, same length");
        for (i, (p, a)) in plain.iter().zip(automated.iter()).enumerate() {
            assert!(
                (a - p * 0.5).abs() < 1e-5,
                "sample {i}: automated bounce must be half the plain one — {a} vs {p}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// ffmpeg-gated: mp3/flac/ogg conversion produces a real file whose
    /// ffprobe duration matches the bounce, and the temp WAV is cleaned up.
    #[test]
    fn export_song_ffmpeg_formats_convert_and_clean_up() {
        if ffmpeg_path().is_none() {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let (cp, shared, _events) = control_plane_with_demo();
        let rate = shared.sample_rate.load(Relaxed) as f64;
        let dir = tmp_dir("e2e-ffmpeg");
        for ext in ["mp3", "flac", "ogg"] {
            let dst = dir.join(format!("song.{ext}"));
            let job = cp.export_song(req_to(&dst)).unwrap();
            let st = wait_done(&job, Duration::from_secs(180));
            assert_eq!(st.state, "done", "{ext}: {:?}", st.error);
            assert!(dst.is_file(), "{ext} written");
            let result = st.result.unwrap();
            let expect_secs = result["durationSeconds"].as_f64().unwrap();
            // Song end is ~7.99 s independent of the engine rate; + 1 s tail.
            assert!((expect_secs - 8.99).abs() < 0.05, "{expect_secs}");
            let _ = rate;
            // ffprobe duration matches (mp3 may pad by an encoder delay).
            if let Ok(out) = Command::new("ffprobe")
                .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
                .arg(&dst)
                .output()
            {
                if out.status.success() {
                    let got: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
                    assert!(
                        (got - expect_secs).abs() < 0.2,
                        "{ext}: ffprobe {got}s vs expected {expect_secs}s"
                    );
                }
            }
        }
        // No temp WAVs left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".aura-export-"))
            .collect();
        assert!(leftovers.is_empty(), "temp files not cleaned: {leftovers:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
