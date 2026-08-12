//! Audio import into the open project — phase 2, zone B (delegated by the
//! architect; see PHASE2-PLAN §3.B).
//!
//! Two things live here:
//!
//! * [`ControlPlane::import_audio_clip`]'s real body: validate a WAV file,
//!   copy it into `<project>/audio/<clipId>.wav`, probe channels/rate/length
//!   (hound via [`engine::load_wav`]), build the waveform pyramid, register
//!   the [`Clip`] (sample-anchored placement) and ask the engine to rebuild.
//!   A target track is auto-created when `track_id` is `None`.
//! * The post-job import convenience: submitting a generation job with the
//!   additive `importToTrackId` param (plus optional `importAtSamples`) makes
//!   the `done` event's `outputPath` land on the timeline through the SAME
//!   control-plane method — the "generate → hear it" loop, no UI round-trip.
//!
//! The core is a tauri-free function over `Store`/`ParamTable` so it unit
//! tests without an engine or webview (job-manager style).

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::audio::engine::{load_wav, ControlMsg};
use crate::audio::project;
use crate::audio::rt::ParamTable;
use crate::audio::types::{Clip, Store, TrackState};
use crate::audio::waveform::{pyramid_exists, Pyramid};
use crate::sidecars::jobs::EventSink;
use crate::sidecars::SidecarEvent;

use super::ops;
use super::{ControlPlane, ImportClipRequest};

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
    store: &Mutex<Store>,
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
    match ext.as_str() {
        "wav" => {}
        "flac" | "mp3" | "ogg" | "aiff" | "aif" => {
            return Err(format!(
                "unsupported import format `.{ext}` — v1 imports WAV only \
                 (FLAC/MP3 decode is planned; convert to WAV for now)"
            ));
        }
        other => return Err(format!("unsupported import format `.{other}` (expected .wav)")),
    }

    // Decode BEFORE touching the store: probes channels/rate/length and
    // rejects corrupt files without mutating anything.
    let (channels, sample_rate, samples) = load_wav(src)?;
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
        let mut s = store.lock();
        let dir = s
            .project_dir
            .clone()
            .ok_or("no project open — create or open a project before importing audio")?;
        match &req.track_id {
            Some(id) => {
                let track = s
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
                (dir, id.clone(), None)
            }
            None => {
                let track = ops::add_track(&mut s, params, Some(stem.clone()), Some("audio".into()))?;
                (dir, track.id.clone(), Some(track))
            }
        }
    };

    // Copy into the project layout and build the waveform pyramid cache.
    let rel = format!("audio/{clip_id}.wav");
    let dst = project_dir.join(&rel);
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(src, &dst).map_err(|e| format!("copy into project failed: {e}"))?;
    let cache_dir = Store::cache_dir_for(&project_dir, &clip_id);
    if !pyramid_exists(&cache_dir) {
        Pyramid::from_interleaved(&samples, channels as usize)
            .write_dir(&cache_dir)
            .map_err(|e| format!("waveform cache build failed: {e}"))?;
    }

    let clip = Clip {
        id: clip_id,
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
    store.lock().clips.push(clip.clone());
    Ok(ImportOutcome { clip, created_track })
}

impl ControlPlane {
    /// Real body of the frozen `import_audio_clip` surface (control/mod.rs
    /// delegates here). Registers the clip, rebuilds the graph, auto-saves
    /// project.json and re-emits `project://changed`.
    pub(crate) fn import_audio_clip_impl(&self, req: ImportClipRequest) -> Result<Clip, String> {
        let outcome = do_import(&self.store, &self.params, &req)?;
        self.engine.send(ControlMsg::Rebuild);

        // Persist the import right away (same convention as record-stop) and
        // tell every front door the project changed.
        let snapshot = {
            use std::sync::atomic::Ordering::Relaxed;
            let s = self.store.lock();
            let dir = s.project_dir.clone();
            project::from_store(
                &s,
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
    fn project_store(name: &str, n: usize) -> (Mutex<Store>, ParamTable, PathBuf) {
        let parent = tmp_parent(name);
        let (_p, dir) = project::create(&parent, "Imp", 48_000, 120.0).unwrap();
        let mut store = Store::default();
        store.project_dir = Some(dir);
        store.project_name = Some("Imp".into());
        let params = ParamTable::default();
        for i in 0..n {
            ops::add_track(&mut store, &params, Some(format!("T{i}")), None).unwrap();
        }
        (Mutex::new(store), params, parent)
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
        let (store, params, parent) = project_store("roundtrip", 1);
        let src = parent.join("source take.wav");
        write_wav(&src, 2, 44_100, 1000);
        let track_id = store.lock().tracks[0].id.clone();

        let out = do_import(
            &store,
            &params,
            &ImportClipRequest {
                track_id: Some(track_id.clone()),
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

        let s = store.lock();
        assert_eq!(s.clips.len(), 1);
        let dir = s.project_dir.clone().unwrap();
        let copied = dir.join(&clip.source_path);
        assert!(copied.is_file(), "copied into project audio/");
        // Copied file decodes identically.
        let (ch, rate, samples) = load_wav(&copied).unwrap();
        assert_eq!((ch, rate), (2, 44_100));
        assert_eq!(samples.len(), 2000);
        // Waveform pyramid cache exists.
        assert!(pyramid_exists(&Store::cache_dir_for(&dir, &clip.id)));
        drop(s);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn import_auto_creates_audio_track_when_none_given() {
        let (store, params, parent) = project_store("autotrack", 0);
        let src = parent.join("melody.wav");
        write_wav(&src, 1, 48_000, 480);

        let out = do_import(&store, &params, &req(&src)).unwrap();
        let t = out.created_track.expect("track auto-created");
        assert_eq!(t.kind, "audio");
        assert_eq!(t.name, "melody");
        assert_eq!(out.clip.track_id, t.id);
        assert_eq!(out.clip.timeline_start_samples, 0, "default placement");
        let s = store.lock();
        assert_eq!(s.tracks.len(), 1);
        assert!(s.slots.contains_key(&t.id), "RT slot allocated");
        drop(s);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn import_rejections_are_structured_and_mutation_free() {
        let (store, params, parent) = project_store("reject", 1);
        let wav = parent.join("ok.wav");
        write_wav(&wav, 1, 48_000, 100);

        // Relative path.
        let e = do_import(&store, &params, &ImportClipRequest {
            path: "relative.wav".into(), track_id: None, at_samples: None,
        }).unwrap_err();
        assert!(e.contains("absolute"), "{e}");

        // Missing file.
        let e = do_import(&store, &params, &req(&parent.join("nope.wav"))).unwrap_err();
        assert!(e.contains("not found"), "{e}");

        // FLAC: clean "future work" message.
        let flac = parent.join("song.flac");
        std::fs::write(&flac, b"fLaC....").unwrap();
        let e = do_import(&store, &params, &req(&flac)).unwrap_err();
        assert!(e.contains("WAV only"), "{e}");

        // Corrupt WAV.
        let bad = parent.join("bad.wav");
        std::fs::write(&bad, b"RIFFnope").unwrap();
        assert!(do_import(&store, &params, &req(&bad)).is_err());

        // Unknown track id.
        let e = do_import(&store, &params, &ImportClipRequest {
            track_id: Some("ghost".into()), ..req(&wav)
        }).unwrap_err();
        assert!(e.contains("unknown track"), "{e}");

        // Midi track target.
        let midi_id = {
            let mut s = store.lock();
            ops::add_track(&mut s, &params, None, Some("midi".into())).unwrap().id
        };
        let e = do_import(&store, &params, &ImportClipRequest {
            track_id: Some(midi_id), ..req(&wav)
        }).unwrap_err();
        assert!(e.contains("midi"), "{e}");

        // Nothing was registered by any failed attempt.
        let s = store.lock();
        assert!(s.clips.is_empty());
        assert_eq!(s.tracks.len(), 2);
        drop(s);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn import_without_project_is_an_error() {
        let store = Mutex::new(Store::default());
        let params = ParamTable::default();
        let parent = tmp_parent("noproj");
        let wav = parent.join("a.wav");
        write_wav(&wav, 1, 48_000, 10);
        let e = do_import(&store, &params, &req(&wav)).unwrap_err();
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
}
