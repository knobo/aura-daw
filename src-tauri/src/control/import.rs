//! Audio import into the open project — phase 2, zone B (delegated by the
//! architect; see PHASE2-PLAN §3.B). Wave 1B extends it to universal decode.
//!
//! Three things live here:
//!
//! * [`ControlPlane::import_audio_clip`]'s real body: validate an audio file,
//!   land it in `<project>/audio/<clipId>.wav`, probe channels/rate/length,
//!   build the waveform pyramid, register the [`Clip`] (sample-anchored
//!   placement) and ask the engine to rebuild. A target track is auto-created
//!   when `track_id` is `None`. WAV keeps the hound fast path (bit-exact file
//!   copy); MP3/FLAC/OGG/AAC/M4A are decoded via symphonia and re-written as
//!   an f32 WAV in the project (the rest of the pipeline — pyramid, RT cache,
//!   resample-at-load — is identical from there).
//! * The post-job import convenience: submitting a generation job with the
//!   additive `importToTrackId` param (plus optional `importAtSamples`) makes
//!   the `done` event's `outputPath` land on the timeline through the SAME
//!   control-plane method — the "generate → hear it" loop, no UI round-trip.
//! * The import→stem-split chain: [`ControlPlane::import_and_split_stems`]
//!   imports a file and immediately submits the Demucs job on the imported
//!   copy; the `done` result's `stems` map is auto-imported as four new
//!   tracks (same post-job mechanism, same control-plane import method).
//!
//! The core is a tauri-free function over `Store`/`ParamTable` so it unit
//! tests without an engine or webview (job-manager style).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::audio::engine::{load_wav, ControlMsg};
use crate::audio::project;
use crate::audio::rt::ParamTable;
use crate::audio::types::{Clip, Store, TrackState};
use crate::audio::waveform::{pyramid_exists, Pyramid};
use crate::sidecars::jobs::{self, EventSink, JobSpec};
use crate::sidecars::{JobKind, SidecarEvent};

use super::ops;
use super::session::Session;
use super::{ControlPlane, ImportClipRequest};

/// Formats decoded through symphonia (everything except the WAV fast path).
const SYMPHONIA_EXTS: [&str; 6] = ["mp3", "flac", "ogg", "aac", "m4a", "mp4"];

/// Decode any supported audio file to interleaved f32 at its NATIVE rate:
/// `(channels, sample_rate, samples)`. WAV goes through hound (fast path,
/// identical to the original import); compressed formats through symphonia.
pub(crate) fn decode_audio(path: &Path) -> Result<(u16, u32, Vec<f32>), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wav" => load_wav(path),
        e if SYMPHONIA_EXTS.contains(&e) => decode_with_symphonia(path),
        "aiff" | "aif" | "wma" | "opus" => Err(format!(
            "unsupported import format `.{ext}` — supported: wav, mp3, flac, \
             ogg, aac, m4a (convert to one of these for now)"
        )),
        other => Err(format!(
            "unsupported import format `.{other}` — supported: wav, mp3, \
             flac, ogg, aac, m4a"
        )),
    }
}

/// Symphonia decode path: probe container, pick the first decodable track,
/// decode every packet to interleaved f32. Corrupt files fail with a clean
/// message; a decode error mid-stream after audio was produced truncates
/// (matching common DAW behavior for damaged tails) rather than failing.
fn decode_with_symphonia(path: &Path) -> Result<(u16, u32, Vec<f32>), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SymErr;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("cannot read {} (corrupt or not audio): {e}", path.display()))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| format!("no decodable audio track in {}", path.display()))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("unsupported codec in {}: {e}", path.display()))?;

    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut samples: Vec<f32> = Vec::new();
    let mut sbuf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean end of stream (symphonia reports EOF as an IO error).
            Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymErr::ResetRequired) => break, // chained stream: keep first
            Err(e) => {
                if samples.is_empty() {
                    return Err(format!("decode failed for {}: {e}", path.display()));
                }
                log::warn!("import: damaged tail in {} ({e}); truncating", path.display());
                break;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                channels = spec.channels.count() as u16;
                sample_rate = spec.rate;
                let needed = decoded.capacity() as u64;
                let recreate = sbuf
                    .as_ref()
                    .map_or(true, |b| b.capacity() < needed as usize * channels as usize);
                if recreate {
                    sbuf = Some(SampleBuffer::<f32>::new(needed, spec));
                }
                let sb = sbuf.as_mut().unwrap();
                sb.copy_interleaved_ref(decoded);
                samples.extend_from_slice(sb.samples());
            }
            // Skip isolated corrupt packets (decoder recovers on the next).
            Err(SymErr::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode failed for {}: {e}", path.display())),
        }
    }
    if channels == 0 || sample_rate == 0 || samples.is_empty() {
        return Err(format!("no audio frames decoded from {}", path.display()));
    }
    Ok((channels, sample_rate, samples))
}

/// Write interleaved f32 as a 32-bit float WAV (the project-native format —
/// same as the recorder's output).
pub(crate) fn write_f32_wav(
    path: &Path,
    channels: u16,
    sample_rate: u32,
    samples: &[f32],
) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for s in samples {
        w.write_sample(*s).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

/// What `do_import` did (the caller decides how to announce it).
#[derive(Debug)]
pub(crate) struct ImportOutcome {
    pub clip: Clip,
    /// Present when no `track_id` was given and a track was auto-created.
    pub created_track: Option<TrackState>,
}

/// Tauri-free import core: validate + decode + copy + pyramid + register.
/// STRUCTURAL — the caller must send `ControlMsg::Rebuild` afterwards.
pub(crate) fn do_import(
    session: &Mutex<Session>,
    params: &ParamTable,
    req: &ImportClipRequest,
) -> Result<ImportOutcome, String> {
    let src = Path::new(&req.path);
    if !src.is_absolute() {
        return Err(format!("path must be absolute: {}", req.path));
    }
    if !src.is_file() {
        return Err(format!("audio file not found: {}", req.path));
    }
    let ext = src
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let is_wav = ext == "wav";

    // Decode BEFORE touching the store: probes channels/rate/length and
    // rejects unsupported/corrupt files without mutating anything.
    let (channels, sample_rate, samples) = decode_audio(src)?;
    if channels == 0 || samples.is_empty() {
        return Err(format!("audio file has no samples: {}", req.path));
    }
    let frames = (samples.len() / channels as usize) as u64;
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Imported".into());

    // Resolve the project + target track under the lock (auto-create when
    // no track was named); file I/O happens after the lock is dropped.
    let clip_id = uuid::Uuid::new_v4().to_string();
    let (project_dir, track_id, created_track) = {
        let mut session = session.lock();
        let dir = session
            .store
            .project_dir
            .clone()
            .ok_or("no project open — create or open a project before importing audio")?;
        match &req.track_id {
            Some(id) => {
                let track = session
                    .store
                    .tracks
                    .iter()
                    .find(|t| &t.id == id)
                    .ok_or_else(|| format!("unknown track: {id}"))?;
                if track.kind != "audio" {
                    return Err(format!(
                        "track {id} is a {} track — audio clips need an audio track",
                        track.kind
                    ));
                }
                (dir, crate::ids::TrackId::from(id.clone()), None)
            }
            None => {
                let track = ops::add_track(
                    &mut session.store,
                    params,
                    Some(stem.clone()),
                    Some("audio".into()),
                )?;
                (dir, track.id.clone(), Some(track))
            }
        }
    };

    // Land the audio in the project layout and build the waveform pyramid
    // cache. WAV sources are copied bit-exact; compressed sources are written
    // back as the decoded f32 WAV (project-native, same as recorded takes).
    let rel = format!("audio/{clip_id}.wav");
    let dst = project_dir.join(&rel);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if is_wav {
        std::fs::copy(src, &dst).map_err(|e| format!("copy into project failed: {e}"))?;
    } else {
        write_f32_wav(&dst, channels, sample_rate, &samples)
            .map_err(|e| format!("writing decoded audio into project failed: {e}"))?;
    }
    let cache_dir = Store::cache_dir_for(&project_dir, &clip_id);
    if !pyramid_exists(&cache_dir) {
        Pyramid::from_interleaved(&samples, channels as usize)
            .write_dir(&cache_dir)
            .map_err(|e| format!("waveform cache build failed: {e}"))?;
    }

    let clip = Clip {
        id: clip_id.into(),
        track_id,
        name: stem,
        source_path: rel,
        source_channels: channels,
        source_sample_rate: sample_rate,
        source_length_samples: frames,
        timeline_start_samples: req.at_samples.unwrap_or(0),
        offset_samples: 0,
        length_samples: frames,
        gain_db: 0.0,
        fade_in_samples: 0,
        fade_out_samples: 0,
    };
    session.lock().store.clips.push(clip.clone());
    Ok(ImportOutcome { clip, created_track })
}

impl ControlPlane {
    /// Real body of the frozen `import_audio_clip` surface (control/mod.rs
    /// delegates here). Registers the clip, rebuilds the graph, auto-saves
    /// project.json and re-emits `project://changed`.
    pub(crate) fn import_audio_clip_impl(&self, req: ImportClipRequest) -> Result<Clip, String> {
        let outcome = do_import(&self.session, &self.params, &req)?;
        self.engine.send(ControlMsg::Rebuild);

        // Persist the import right away (same convention as record-stop) and
        // tell every front door the project changed.
        let snapshot = {
            use std::sync::atomic::Ordering::Relaxed;
            let session = self.session.lock();
            let dir = session.store.project_dir.clone();
            project::from_store(
                &session.store,
                self.shared.position.load(Relaxed),
                self.shared.sample_rate.load(Relaxed),
            )
            .ok()
            .zip(dir)
        };
        if let Some((p, dir)) = snapshot {
            if let Err(e) = project::save(&dir, &p) {
                log::warn!("auto-save after import failed: {e}");
            }
            (self.emit)(
                "project://changed",
                serde_json::to_value(&p).unwrap_or_default(),
            );
        }
        if let Some(t) = &outcome.created_track {
            log::info!("import: auto-created audio track {} ({})", t.name, t.id);
        }
        Ok(outcome.clip)
    }

    /// Open-kind job submission WITH the post-job import convenience: when
    /// `params` carries `importToTrackId` (additive, ignored by workers), a
    /// successful job whose result has an `outputPath` is imported onto that
    /// track through [`ControlPlane::import_audio_clip`] — a control-plane
    /// call, never a Tauri command. `importToTrackId: null`/`""` auto-creates
    /// a track; `importAtSamples` places the clip (default 0).
    ///
    /// Since the architect merge, [`ControlPlane::run_sidecar_job`] itself
    /// applies the import directive (MCP parity) — this method is kept as
    /// the documented front-door alias and simply delegates.
    pub fn run_sidecar_job_with_import(
        self: &Arc<Self>,
        kind: &str,
        params: serde_json::Value,
        sink: EventSink,
    ) -> Result<String, String> {
        self.run_sidecar_job(kind, params, sink)
    }
}

/// Parse the additive import directive out of job params.
/// `None` = no directive; `Some((track_id, at_samples))` otherwise, where a
/// `null` or empty-string track id means "auto-create a track".
pub(crate) fn import_directive(
    params: &serde_json::Value,
) -> Option<(Option<String>, Option<u64>)> {
    let v = params.get("importToTrackId")?;
    let track_id = match v {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) if s.is_empty() => None,
        serde_json::Value::String(s) => Some(s.clone()),
        _ => return None, // malformed directive: ignore, do not fail the job
    };
    let at = params.get("importAtSamples").and_then(|a| a.as_u64());
    Some((track_id, at))
}

/// Wrap a job event sink so a `done` result with an `outputPath` triggers
/// `import_audio_clip` (when the params asked for it). The import runs on its
/// own thread (decode + pyramid build must not stall the job supervisor);
/// its outcome is reported as a follow-up `log` event on the same channel —
/// the job itself already succeeded.
pub(crate) fn wrap_sink_with_import(
    control: &Arc<ControlPlane>,
    params: &serde_json::Value,
    inner: EventSink,
) -> EventSink {
    let Some((track_id, at_samples)) = import_directive(params) else {
        return inner;
    };
    let control = Arc::clone(control);
    Arc::new(move |ev: SidecarEvent| {
        if let SidecarEvent::Done { job_id, result } = &ev {
            if let Some(path) = result.get("outputPath").and_then(|p| p.as_str()) {
                let req = ImportClipRequest {
                    path: path.to_string(),
                    track_id: track_id.clone(),
                    at_samples,
                };
                let control = Arc::clone(&control);
                let inner2 = Arc::clone(&inner);
                let job_id = job_id.clone();
                inner(ev.clone()); // deliver `done` first, import is extra
                std::thread::spawn(move || {
                    let line = match control.import_audio_clip_impl(req) {
                        Ok(clip) => format!(
                            "auto-import: clip {} ({}) placed on track {}",
                            clip.id, clip.name, clip.track_id
                        ),
                        Err(e) => format!("auto-import failed: {e}"),
                    };
                    inner2(SidecarEvent::Log { job_id, line });
                });
                return;
            }
        }
        inner(ev)
    })
}

// ---------------------------------------------------------------------------
// Import → auto-stem-split chain (wave 1B)
// ---------------------------------------------------------------------------

/// Stem import order (stable track layout under the source clip).
const STEM_ORDER: [&str; 4] = ["vocals", "drums", "bass", "other"];

/// Mirror of the sidecar module's simulate switch (that fn is private there).
fn simulate_mode() -> bool {
    matches!(
        std::env::var("AURA_SIDECAR_SIMULATE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Build the Demucs job spec for an imported clip: stems land in
/// `<project>/stems/<clipId>/` (ARCHITECTURE §7).
fn demucs_split_spec(input: &Path, out_dir: &Path, simulate: bool) -> Result<JobSpec, String> {
    let python = jobs::resolve_python()?;
    let script = jobs::resolve_script("demucs_split.py")?;
    let mut args = vec![
        script.to_string_lossy().into_owned(),
        "--input".into(),
        input.to_string_lossy().into_owned(),
        "--out-dir".into(),
        out_dir.to_string_lossy().into_owned(),
        "--model".into(),
        "htdemucs".into(),
    ];
    if simulate {
        args.push("--simulate".into());
    }
    Ok(JobSpec {
        kind: JobKind::StemSplit,
        program: python,
        args,
        grace: Duration::from_secs(3),
        env: Vec::new(),
    })
}

/// Wrap a job event sink so a `stemSplit` `done` result (`stems` name→path
/// map) is imported as one new audio track per stem, aligned with the source
/// clip's timeline position. Runs on its own thread (decode + pyramid per
/// stem must not stall the job supervisor); outcomes arrive as follow-up
/// `log` events — the job itself already succeeded.
pub(crate) fn wrap_sink_with_stem_import(
    control: &Arc<ControlPlane>,
    source_clip: &Clip,
    inner: EventSink,
) -> EventSink {
    let control = Arc::clone(control);
    let clip_name = source_clip.name.clone();
    let at_samples = source_clip.timeline_start_samples;
    Arc::new(move |ev: SidecarEvent| {
        if let SidecarEvent::Done { job_id, result } = &ev {
            if let Some(stems) = result.get("stems").and_then(|s| s.as_object()) {
                // Fixed order first, then any extra stems a model produced.
                let mut ordered: Vec<(String, String)> = Vec::new();
                for name in STEM_ORDER {
                    if let Some(p) = stems.get(name).and_then(|p| p.as_str()) {
                        ordered.push((name.to_string(), p.to_string()));
                    }
                }
                for (name, p) in stems {
                    if !STEM_ORDER.contains(&name.as_str()) {
                        if let Some(p) = p.as_str() {
                            ordered.push((name.clone(), p.to_string()));
                        }
                    }
                }
                let control = Arc::clone(&control);
                let inner2 = Arc::clone(&inner);
                let job_id = job_id.clone();
                let clip_name = clip_name.clone();
                inner(ev.clone()); // deliver `done` first, imports are extra
                std::thread::spawn(move || {
                    for (stem, path) in ordered {
                        let line = (|| -> Result<String, String> {
                            let track = control.add_track(
                                Some(format!("{clip_name} {stem}")),
                                Some("audio".into()),
                                crate::control::op::TxMeta::system(format!(
                                    "auto-import stem: {stem}"
                                )),
                            )?;
                            let clip = control.import_audio_clip_impl(ImportClipRequest {
                                path,
                                track_id: Some(track.id.to_string()),
                                at_samples: Some(at_samples),
                            })?;
                            Ok(format!(
                                "auto-import stem `{stem}`: clip {} on track {} ({})",
                                clip.id, track.name, track.id
                            ))
                        })()
                        .unwrap_or_else(|e| format!("auto-import stem `{stem}` failed: {e}"));
                        inner2(SidecarEvent::Log { job_id: job_id.clone(), line });
                    }
                });
                return;
            }
        }
        inner(ev)
    })
}

impl ControlPlane {
    /// Import an audio file (any supported format) and immediately submit a
    /// Demucs stem-split on the imported project copy. The job's `done`
    /// result auto-imports the four stems as new audio tracks aligned with
    /// the source clip. Returns `(imported clip, job id)`.
    pub fn import_and_split_stems(
        self: &Arc<Self>,
        req: ImportClipRequest,
        sink: EventSink,
    ) -> Result<(Clip, String), String> {
        let clip = self.import_audio_clip_impl(req)?;
        let (input, out_dir) = {
            let session = self.session.lock();
            let dir = session.store.project_dir.clone().ok_or("no project open")?;
            (dir.join(&clip.source_path), dir.join("stems").join(clip.id.as_str()))
        };
        let spec = demucs_split_spec(&input, &out_dir, simulate_mode())?;
        let job_id = self.submit_split_job(&clip, spec, sink);
        Ok((clip, job_id))
    }

    /// Spec-injectable submission seam (tests drive it with a fake sidecar).
    pub(crate) fn submit_split_job(
        self: &Arc<Self>,
        clip: &Clip,
        spec: JobSpec,
        sink: EventSink,
    ) -> String {
        let sink = wrap_sink_with_stem_import(self, clip, sink);
        self.jobs.submit(spec, sink)
    }
}

/// Wire shape of [`import_audio_clip_split_stems`]'s reply.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSplitReply {
    pub clip: Clip,
    pub job_id: String,
}

/// Import + auto-stem-split front door (wave 1B; needs registration in the
/// frozen lib.rs: `control::import::import_audio_clip_split_stems`). Job
/// events stream over `on_event` and the `sidecar://*` app events, exactly
/// like `sidecar_run_job`.
#[tauri::command]
pub async fn import_audio_clip_split_stems(
    app: tauri::AppHandle,
    request: ImportClipRequest,
    on_event: tauri::ipc::Channel<SidecarEvent>,
    control: tauri::State<'_, Arc<ControlPlane>>,
) -> Result<ImportSplitReply, String> {
    use tauri::Emitter;
    // Same fan-out as sidecars::make_sink (private there): every event to the
    // per-job channel; progress/done/error also as `sidecar://*` app events.
    let sink: EventSink = Arc::new(move |ev: SidecarEvent| {
        let _ = on_event.send(ev.clone());
        let name = match &ev {
            SidecarEvent::Progress { .. } => "sidecar://progress",
            SidecarEvent::Done { .. } => "sidecar://done",
            SidecarEvent::Error { .. } => "sidecar://error",
            SidecarEvent::Log { .. } => return,
        };
        if let Err(e) = app.emit(name, &ev) {
            log::warn!("import-split: failed to emit {name}: {e}");
        }
    });
    let (clip, job_id) = control.import_and_split_stems(request, sink)?;
    Ok(ImportSplitReply { clip, job_id })
}

// ---------------------------------------------------------------------------
// Tests (tauri-free: temp project dir + hound-written WAVs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_parent(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-import-test-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_wav(path: &Path, channels: u16, rate: u32, frames: usize) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..frames {
            for _ in 0..channels {
                w.write_sample(((i % 100) as i32 - 50) * 300).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    /// Store with an open temp project and `n` audio tracks.
    fn project_session(name: &str, n: usize) -> (Mutex<Session>, ParamTable, PathBuf) {
        let parent = tmp_parent(name);
        let (_p, dir) = project::create(&parent, "Imp", 48_000, 120.0).unwrap();
        let mut store = Store::default();
        store.project_dir = Some(dir);
        store.project_name = Some("Imp".into());
        let params = ParamTable::default();
        for i in 0..n {
            ops::add_track(&mut store, &params, Some(format!("T{i}")), None).unwrap();
        }
        (Mutex::new(Session::new(store, crate::midi::MidiStore::default())), params, parent)
    }

    fn req(path: &Path) -> ImportClipRequest {
        ImportClipRequest {
            path: path.to_string_lossy().into_owned(),
            track_id: None,
            at_samples: None,
        }
    }

    #[test]
    fn import_roundtrip_copies_probes_and_builds_pyramid() {
        let (session, params, parent) = project_session("roundtrip", 1);
        let src = parent.join("source take.wav");
        write_wav(&src, 2, 44_100, 1000);
        let track_id = session.lock().store.tracks[0].id.clone();

        let out = do_import(
            &session,
            &params,
            &ImportClipRequest {
                track_id: Some(track_id.to_string()),
                at_samples: Some(4800),
                ..req(&src)
            },
        )
        .unwrap();
        let clip = &out.clip;
        assert!(out.created_track.is_none());
        assert_eq!(clip.track_id, track_id);
        assert_eq!(clip.name, "source take");
        assert_eq!(clip.source_channels, 2);
        assert_eq!(clip.source_sample_rate, 44_100);
        assert_eq!(clip.source_length_samples, 1000);
        assert_eq!(clip.length_samples, 1000);
        assert_eq!(clip.timeline_start_samples, 4800);
        assert_eq!(clip.source_path, format!("audio/{}.wav", clip.id));

        let s = session.lock();
        assert_eq!(s.store.clips.len(), 1);
        let dir = s.store.project_dir.clone().unwrap();
        let copied = dir.join(&clip.source_path);
        assert!(copied.is_file(), "copied into project audio/");
        // Copied file decodes identically.
        let (ch, rate, samples) = load_wav(&copied).unwrap();
        assert_eq!((ch, rate), (2, 44_100));
        assert_eq!(samples.len(), 2000);
        // Waveform pyramid cache exists.
        assert!(pyramid_exists(&Store::cache_dir_for(&dir, clip.id.as_str())));
        drop(s);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn import_auto_creates_audio_track_when_none_given() {
        let (session, params, parent) = project_session("autotrack", 0);
        let src = parent.join("melody.wav");
        write_wav(&src, 1, 48_000, 480);

        let out = do_import(&session, &params, &req(&src)).unwrap();
        let t = out.created_track.expect("track auto-created");
        assert_eq!(t.kind, "audio");
        assert_eq!(t.name, "melody");
        assert_eq!(out.clip.track_id, t.id);
        assert_eq!(out.clip.timeline_start_samples, 0, "default placement");
        let s = session.lock();
        assert_eq!(s.store.tracks.len(), 1);
        assert!(s.store.slots.contains_key(&t.id), "RT slot allocated");
        drop(s);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn import_rejections_are_structured_and_mutation_free() {
        let (session, params, parent) = project_session("reject", 1);
        let wav = parent.join("ok.wav");
        write_wav(&wav, 1, 48_000, 100);

        // Relative path.
        let e = do_import(&session, &params, &ImportClipRequest {
            path: "relative.wav".into(), track_id: None, at_samples: None,
        }).unwrap_err();
        assert!(e.contains("absolute"), "{e}");

        // Missing file.
        let e = do_import(&session, &params, &req(&parent.join("nope.wav"))).unwrap_err();
        assert!(e.contains("not found"), "{e}");

        // Unsupported format: clean message listing what IS supported.
        let aiff = parent.join("song.aiff");
        std::fs::write(&aiff, b"FORM....AIFF").unwrap();
        let e = do_import(&session, &params, &req(&aiff)).unwrap_err();
        assert!(e.contains("unsupported import format"), "{e}");
        assert!(e.contains("mp3"), "{e}");

        // Corrupt WAV.
        let bad = parent.join("bad.wav");
        std::fs::write(&bad, b"RIFFnope").unwrap();
        assert!(do_import(&session, &params, &req(&bad)).is_err());

        // Corrupt compressed file (a .flac that isn't FLAC).
        let flac = parent.join("song.flac");
        std::fs::write(&flac, b"fLaC-not-really-a-flac-stream").unwrap();
        let e = do_import(&session, &params, &req(&flac)).unwrap_err();
        assert!(
            e.contains("corrupt") || e.contains("decode") || e.contains("no audio"),
            "{e}"
        );

        // Unknown track id.
        let e = do_import(&session, &params, &ImportClipRequest {
            track_id: Some("ghost".into()), ..req(&wav)
        }).unwrap_err();
        assert!(e.contains("unknown track"), "{e}");

        // Midi track target.
        let midi_id = {
            let mut s = session.lock();
            ops::add_track(&mut s.store, &params, None, Some("midi".into())).unwrap().id
        };
        let e = do_import(&session, &params, &ImportClipRequest {
            track_id: Some(midi_id.to_string()), ..req(&wav)
        }).unwrap_err();
        assert!(e.contains("midi"), "{e}");

        // Nothing was registered by any failed attempt.
        let s = session.lock();
        assert!(s.store.clips.is_empty());
        assert_eq!(s.store.tracks.len(), 2);
        drop(s);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn import_without_project_is_an_error() {
        let session = Mutex::new(Session::new(Store::default(), crate::midi::MidiStore::default()));
        let params = ParamTable::default();
        let parent = tmp_parent("noproj");
        let wav = parent.join("a.wav");
        write_wav(&wav, 1, 48_000, 10);
        let e = do_import(&session, &params, &req(&wav)).unwrap_err();
        assert!(e.contains("no project open"), "{e}");
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn import_directive_parses_the_additive_params() {
        use serde_json::json;
        assert!(import_directive(&json!({"prompt": "x"})).is_none());
        assert_eq!(
            import_directive(&json!({"importToTrackId": "t1"})),
            Some((Some("t1".into()), None))
        );
        assert_eq!(
            import_directive(&json!({"importToTrackId": null, "importAtSamples": 960})),
            Some((None, Some(960)))
        );
        assert_eq!(
            import_directive(&json!({"importToTrackId": ""})),
            Some((None, None))
        );
        // Malformed directive is ignored rather than failing the job.
        assert!(import_directive(&json!({"importToTrackId": 7})).is_none());
    }

    // -- universal decode (symphonia) ------------------------------------

    /// Write a 0.5 s 44.1 kHz stereo sine WAV (the encode source).
    fn write_sine_wav(path: &Path, frames: usize) {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..frames {
            let v = ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 44_100.0).sin()
                * 12_000.0) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
    }

    /// Encode `src` to `dst` with the ffmpeg CLI; false = ffmpeg missing
    /// (test skips politely) — panics on an actual encode failure.
    fn ffmpeg_encode(src: &Path, dst: &Path) -> bool {
        let out = match std::process::Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(src)
            .arg(dst)
            .output()
        {
            Ok(o) => o,
            Err(_) => return false,
        };
        assert!(
            out.status.success(),
            "ffmpeg encode to {} failed: {}",
            dst.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        true
    }

    #[test]
    fn import_decodes_compressed_formats_via_symphonia() {
        let frames = 22_050usize; // 0.5 s at 44.1 kHz
        let (session, params, parent) = project_session("formats", 0);
        let src_wav = parent.join("tone.wav");
        write_sine_wav(&src_wav, frames);

        for ext in ["mp3", "flac", "ogg", "m4a"] {
            let enc = parent.join(format!("tone.{ext}"));
            if !ffmpeg_encode(&src_wav, &enc) {
                eprintln!("skipping compressed-format import test: ffmpeg not found");
                return;
            }
            let out = do_import(&session, &params, &req(&enc))
                .unwrap_or_else(|e| panic!("import .{ext} failed: {e}"));
            let clip = &out.clip;
            assert_eq!(clip.source_channels, 2, ".{ext} keeps stereo");
            assert_eq!(clip.source_sample_rate, 44_100, ".{ext} keeps the rate");
            // Lossy codecs pad (encoder delay/priming): duration must be the
            // source ±0.25 s worth of samples, never wildly off.
            let frames_u = frames as i64;
            let got = clip.source_length_samples as i64;
            assert!(
                (got - frames_u).abs() < 11_025,
                ".{ext}: decoded {got} frames, source {frames_u}"
            );
            // The project copy is a real (f32) WAV: hound decodes it and the
            // frame count matches the probe.
            let s = session.lock();
            let dir = s.store.project_dir.clone().unwrap();
            let copied = dir.join(&clip.source_path);
            drop(s);
            let (ch, rate, samples) = load_wav(&copied)
                .unwrap_or_else(|e| panic!(".{ext}: project copy unreadable: {e}"));
            assert_eq!((ch, rate), (2, 44_100));
            assert_eq!(samples.len() as u64 / 2, clip.source_length_samples);
            // Not silent, and the waveform pyramid exists.
            assert!(samples.iter().any(|s| s.abs() > 0.05), ".{ext}: silent decode");
            assert!(pyramid_exists(&Store::cache_dir_for(&dir, clip.id.as_str())));
        }
        // Each import auto-created its own track.
        assert_eq!(session.lock().store.tracks.len(), 4);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn corrupt_compressed_import_leaves_store_untouched() {
        let (session, params, parent) = project_session("corrupt", 0);
        for name in ["junk.mp3", "junk.ogg", "junk.m4a"] {
            let p = parent.join(name);
            std::fs::write(&p, b"\xffgarbage garbage garbage garbage").unwrap();
            let e = do_import(&session, &params, &req(&p)).unwrap_err();
            assert!(
                e.contains("corrupt") || e.contains("decode") || e.contains("no audio"),
                "{name}: {e}"
            );
        }
        let s = session.lock();
        assert!(s.store.clips.is_empty());
        assert!(s.store.tracks.is_empty());
        drop(s);
        let _ = std::fs::remove_dir_all(parent);
    }

    // -- import → auto-stem-split chain ----------------------------------

    struct NullEvents;
    impl crate::audio::engine::EventSink for NullEvents {
        fn emit(&self, _e: &str, _p: serde_json::Value) {}
    }

    /// Real ControlPlane over a headless engine + temp project.
    fn control_plane(name: &str) -> (Arc<ControlPlane>, PathBuf) {
        let shared = Arc::new(crate::audio::rt::SharedRt::default());
        let params = Arc::new(ParamTable::default());
        let session = Arc::new(Mutex::new(Session::new(Store::default(), crate::midi::MidiStore::default())));
        let engine = crate::audio::engine::start(
            shared.clone(),
            params.clone(),
            session.clone(),
            Box::new(NullEvents),
        );
        let cp = Arc::new(ControlPlane::new(
            session,
            shared,
            params,
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Box::new(|_, _| {}),
        ));
        let parent = tmp_parent(name);
        cp.create_project(parent.to_str().unwrap(), "Split").unwrap();
        (cp, parent)
    }

    /// Same, but with an emitter that records the app events it is handed.
    fn control_plane_recording(
        name: &str,
    ) -> (Arc<ControlPlane>, PathBuf, Arc<Mutex<Vec<(String, serde_json::Value)>>>) {
        let shared = Arc::new(crate::audio::rt::SharedRt::default());
        let params = Arc::new(ParamTable::default());
        let session = Arc::new(Mutex::new(Session::new(Store::default(), crate::midi::MidiStore::default())));
        let engine = crate::audio::engine::start(
            shared.clone(),
            params.clone(),
            session.clone(),
            Box::new(NullEvents),
        );
        let events: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let cp = Arc::new(ControlPlane::new(
            session,
            shared,
            params,
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Box::new(move |e, p| sink.lock().push((e.to_string(), p))),
        ));
        let parent = tmp_parent(name);
        cp.create_project(parent.to_str().unwrap(), "Announce").unwrap();
        events.lock().clear(); // project creation is not what we're asserting
        (cp, parent, events)
    }

    /// Pins the guarantee the MCP front door leans on. The auto-import runs
    /// on its own thread AFTER `done` is delivered and reports itself as a
    /// `log` line — and logs never reach the app-event bus, so a caller with
    /// no per-job channel (the MCP `run_sidecar_job` tool) cannot learn that
    /// way when the clip landed. What saves it is that `import_audio_clip_impl`
    /// emits `project://changed` itself, with the clip already IN the payload;
    /// the frontend's `project://changed` listener is what makes an agent's
    /// generated clip appear. Whoever moves that emit breaks agent imports
    /// silently — hence this test.
    #[tokio::test]
    async fn auto_import_announces_the_landed_clip_over_project_changed() {
        let (cp, parent, events) = control_plane_recording("announce");
        let src = parent.join("generated.wav");
        write_wav(&src, 2, 44_100, 800);

        let seen: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let seen2 = Arc::clone(&seen);
        let inner: EventSink = Arc::new(move |ev| seen2.lock().push(ev));
        let sink = wrap_sink_with_import(
            &cp,
            &serde_json::json!({"importToTrackId": null, "importAtSamples": 9_600}),
            inner,
        );

        sink(SidecarEvent::Done {
            job_id: "job-1".into(),
            result: serde_json::json!({
                "kind": "aceStepGenerate",
                "outputPath": src.to_string_lossy(),
            }),
        });

        // `done` is synchronous; the import lands on its own thread and
        // finishes with the follow-up log line.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let landed = seen.lock().iter().any(
                |e| matches!(e, SidecarEvent::Log { line, .. } if line.starts_with("auto-import:")),
            );
            if landed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "auto-import never finished: {:?}",
                seen.lock()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let evs = events.lock();
        let payload = evs
            .iter()
            .rev()
            .find(|(name, _)| name == "project://changed")
            .map(|(_, p)| p)
            .expect("auto-import announced itself");
        let clips = payload["clips"].as_array().expect("clips array");
        // The import copies the source into the project (`audio/<uuid>.wav`),
        // so the clip's NAME is what carries the identity here.
        assert!(
            clips.iter().any(|c| c["name"] == "generated"
                && c["timelineStartSamples"] == 9_600
                && c["lengthSamples"] == 800),
            "announcement carries the landed clip, not a pre-import snapshot: {clips:?}"
        );
        assert!(
            payload["tracks"].as_array().is_some_and(|t| t.len() == 1),
            "importToTrackId=null auto-created exactly one track"
        );
    }

    fn write_sh(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn sh_spec(script: &Path) -> JobSpec {
        JobSpec {
            kind: JobKind::StemSplit,
            program: PathBuf::from("/bin/sh"),
            args: vec![script.to_string_lossy().into_owned()],
            grace: Duration::from_millis(500),
            env: Vec::new(),
        }
    }

    /// Store-level chain test with a FAKE sidecar: import a wav, submit the
    /// (injected) stem job, and assert the `done` result's stems land as four
    /// new aligned audio tracks through the control plane.
    #[tokio::test]
    async fn import_and_split_chain_lands_four_stem_tracks() {
        let (cp, parent) = control_plane("chain");
        let src = parent.join("song.wav");
        write_wav(&src, 2, 44_100, 2000);

        // The "model output": four small stem wavs the fake sidecar reports.
        let stems_dir = parent.join("fake-stems");
        std::fs::create_dir_all(&stems_dir).unwrap();
        let mut stems_json = serde_json::Map::new();
        for stem in ["vocals", "drums", "bass", "other"] {
            let p = stems_dir.join(format!("{stem}.wav"));
            write_wav(&p, 2, 44_100, 500);
            stems_json.insert(
                stem.to_string(),
                serde_json::Value::String(p.to_string_lossy().into_owned()),
            );
        }
        let done = serde_json::json!({
            "type": "done",
            "result": {"kind": "stemSplit", "stems": stems_json}
        });
        let script = write_sh(
            &parent,
            "fake_demucs.sh",
            &format!(
                "#!/bin/sh\necho '{{\"type\":\"progress\",\"progress\":0.5,\"stage\":\"separating\"}}'\necho '{}'\n",
                done
            ),
        );

        let clip = cp
            .import_audio_clip_impl(ImportClipRequest {
                path: src.to_string_lossy().into_owned(),
                track_id: None,
                at_samples: Some(4_800),
            })
            .unwrap();
        let events: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev2 = Arc::clone(&events);
        let sink: EventSink = Arc::new(move |ev| ev2.lock().push(ev));
        let job_id = cp.submit_split_job(&clip, sh_spec(&script), sink);

        // Wait for done + the four follow-up stem-import log lines.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            {
                let evs = events.lock();
                let done = evs.iter().any(|e| matches!(e, SidecarEvent::Done { .. }));
                let stems_logged = evs
                    .iter()
                    .filter(|e| matches!(e, SidecarEvent::Log { line, .. } if line.contains("auto-import stem")))
                    .count();
                if done && stems_logged >= 4 {
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "stem imports did not finish: {:?}",
                events.lock()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(cp.job_status(&job_id).unwrap().state, "done");

        // 1 source + 4 stem tracks, stems aligned with the source clip.
        let snap = cp.project_state();
        assert_eq!(snap.tracks.len(), 5, "{:?}", snap.tracks);
        assert_eq!(snap.clips.len(), 5);
        for stem in ["vocals", "drums", "bass", "other"] {
            let t = snap
                .tracks
                .iter()
                .find(|t| t.name == format!("song {stem}"))
                .unwrap_or_else(|| panic!("missing stem track for {stem}"));
            assert_eq!(t.kind, "audio");
            let c = snap
                .clips
                .iter()
                .find(|c| c.track_id == t.id)
                .unwrap_or_else(|| panic!("missing stem clip for {stem}"));
            assert_eq!(c.timeline_start_samples, 4_800, "stem aligned with source");
            assert_eq!(c.source_length_samples, 500);
        }
        let _ = std::fs::remove_dir_all(parent);
    }

    /// A failed split job must not import anything (error passes through).
    #[tokio::test]
    async fn failed_split_job_imports_nothing() {
        let (cp, parent) = control_plane("chain-err");
        let src = parent.join("s.wav");
        write_wav(&src, 1, 48_000, 100);
        let script = write_sh(
            &parent,
            "fail.sh",
            "#!/bin/sh\necho '{\"type\":\"error\",\"message\":\"demucs is not installed\"}'\nexit 1\n",
        );
        let clip = cp
            .import_audio_clip_impl(ImportClipRequest {
                path: src.to_string_lossy().into_owned(),
                track_id: None,
                at_samples: None,
            })
            .unwrap();
        let events: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev2 = Arc::clone(&events);
        let sink: EventSink = Arc::new(move |ev| ev2.lock().push(ev));
        let job_id = cp.submit_split_job(&clip, sh_spec(&script), sink);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let st = cp.job_status(&job_id).unwrap();
            if st.state == "error" {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{st:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let snap = cp.project_state();
        assert_eq!(snap.tracks.len(), 1, "no stem tracks on failure");
        assert_eq!(snap.clips.len(), 1);
        assert!(events
            .lock()
            .iter()
            .any(|e| matches!(e, SidecarEvent::Error { message, .. } if message.contains("demucs"))));
        let _ = std::fs::remove_dir_all(parent);
    }
}
