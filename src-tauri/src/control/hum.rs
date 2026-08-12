//! Hum-to-song: a recorded voice/hum clip becomes a MIDI melody clip
//! (wave 1A; sidecar `/sidecars/hum_to_midi.py`, NDJSON protocol §6).
//!
//! Flow (mirrors `import.rs`'s post-job pattern):
//!
//! 1. [`ControlPlane::hum_to_song`] resolves the input WAV (an existing
//!    recorded audio clip by id, or an absolute file path), reads the
//!    project's (ppq, bpm), and submits the `hum_to_midi.py` sidecar through
//!    the existing [`JobManager`] as `JobKind::HumToMidi` (wire name
//!    `"humToMidi"`, see `sidecars/types.rs`).
//! 2. The submit-time sink is wrapped: a `done` result with
//!    `kind: "humToMidi"` is parsed ([`parse_hum_result`]) and landed as a
//!    MIDI clip on the target (or an auto-created) midi track
//!    ([`apply_hum_notes`]), persisted into the open project, followed by an
//!    engine `Rebuild` — "hum it → hear it". The outcome is reported as a
//!    follow-up `log` event on the same channel (the job already succeeded).
//! 3. Optional second stage (`accompany: true`): the melody notes are fed to
//!    the existing `amtInfill` job (open-kind path) as context over the
//!    melody's tick span; the proposed notes land on a second auto-created
//!    midi track as a "Hum Accompaniment" clip.
//!
//! The parse/apply core is tauri-free (Store/ParamTable/MidiStore-level) and
//! unit-tested with a scripted fake sidecar per the established pattern.

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audio::engine::ControlMsg;
use crate::audio::rt::ParamTable;
use crate::audio::types::{Store, TrackState};
use crate::midi::{MidiClip, MidiNote, MidiStore};
use crate::sidecars::jobs::{self, EventSink, JobSpec};
use crate::sidecars::{JobKind, SidecarEvent};

use super::ops;
use super::ControlPlane;

/// SIGTERM → SIGKILL grace on cancel (same value as the other sidecar jobs).
const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Argument of `hum_to_song` (Tauri command and the control-plane method).
/// Exactly one of `clip_id` / `path` selects the input recording.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumToSongRequest {
    /// Id of an existing recorded/imported AUDIO clip in the open project.
    pub clip_id: Option<String>,
    /// Absolute path to a WAV recording (alternative to `clip_id`).
    pub path: Option<String>,
    /// Target MIDI track for the melody clip; None = auto-create one
    /// (instrument binding is left to the caller/UI).
    pub track_id: Option<String>,
    /// Timeline placement of the melody clip in ticks (default 0).
    pub at_ticks: Option<u64>,
    /// Optional light grid snap for note onsets: 4 = quarters, 8 = eighths,
    /// 16 = sixteenths... None/0 = no snapping.
    pub quantize_grid: Option<u32>,
    /// Tempo override for seconds→ticks; default: the project tempo.
    pub bpm: Option<f64>,
    /// Chain an `amtInfill` job over the melody span and land its proposal
    /// as an accompaniment clip on a second auto-created midi track.
    pub accompany: Option<bool>,
}

/// Everything the post-job apply step needs (captured at submit time).
#[derive(Debug, Clone)]
pub(crate) struct HumApplyConfig {
    pub track_id: Option<String>,
    pub at_ticks: u64,
    pub accompany: bool,
    pub clip_name: String,
    pub ppq: u32,
    pub bpm: f64,
}

// ---------------------------------------------------------------------------
// Result parsing (tauri-free)
// ---------------------------------------------------------------------------

#[inline]
fn rescale(t: u32, from_ppq: u32, to_ppq: u32) -> u32 {
    ((t as u64 * to_ppq as u64 + from_ppq as u64 / 2) / from_ppq as u64) as u32
}

/// Validate + decode a `humToMidi` result (the `result` of the job's `done`
/// event): `{"kind":"humToMidi","ppq":...,"notes":[MidiNote...],
/// "lengthTicks":...}`. Returns `(notes, clip_length_ticks)` rescaled to
/// `target_ppq`; unknown per-note fields (e.g. `pitchHz`) are ignored (D-06).
pub(crate) fn parse_hum_result(
    result: &Value,
    target_ppq: u32,
) -> Result<(Vec<MidiNote>, u64), String> {
    if result.get("kind").and_then(Value::as_str) != Some("humToMidi") {
        return Err(format!(
            "unexpected result kind {:?} (want \"humToMidi\")",
            result.get("kind")
        ));
    }
    let ppq = result
        .get("ppq")
        .and_then(Value::as_u64)
        .map(|p| p as u32)
        .filter(|p| *p > 0)
        .unwrap_or(target_ppq);
    let notes_v = result
        .get("notes")
        .ok_or_else(|| "humToMidi result has no notes".to_string())?;
    let mut notes: Vec<MidiNote> =
        serde_json::from_value(notes_v.clone()).map_err(|e| format!("notes: {e}"))?;
    if notes.is_empty() {
        return Err("humToMidi result contains no notes".into());
    }
    for n in &notes {
        n.validate()?;
    }
    let mut length = result
        .get("lengthTicks")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if ppq != target_ppq {
        for n in notes.iter_mut() {
            n.tick = rescale(n.tick, ppq, target_ppq);
            n.length_ticks = rescale(n.length_ticks, ppq, target_ppq).max(1);
        }
        length = (length * target_ppq as u64 + ppq as u64 / 2) / ppq as u64;
    }
    notes.sort_by_key(|n| (n.tick, n.key));
    let end = notes
        .iter()
        .map(|n| n.tick as u64 + n.length_ticks as u64)
        .max()
        .unwrap_or(0);
    Ok((notes, length.max(end).max(1)))
}

// ---------------------------------------------------------------------------
// Apply core (tauri-free: Store + ParamTable + MidiStore)
// ---------------------------------------------------------------------------

/// Land validated notes as a new MIDI clip: resolve the target track (must be
/// `kind: "midi"`) or auto-create one named after the clip, then register the
/// clip in the midi store. STRUCTURAL — the caller persists and sends
/// `ControlMsg::Rebuild` afterwards. Lock order: store first, then midi,
/// never both at once (same convention as `seed_demo_project`).
pub(crate) fn apply_hum_notes(
    store: &Mutex<Store>,
    params: &ParamTable,
    midi: &Mutex<MidiStore>,
    mut notes: Vec<MidiNote>,
    length_ticks: u64,
    track_id: Option<String>,
    at_ticks: u64,
    name: &str,
) -> Result<(MidiClip, Option<TrackState>), String> {
    if notes.is_empty() {
        return Err("no notes to place".into());
    }
    for n in &notes {
        n.validate()?;
    }
    let (target, created_track) = {
        let mut s = store.lock();
        match track_id {
            Some(id) => {
                let track = s
                    .tracks
                    .iter()
                    .find(|t| t.id == id)
                    .ok_or_else(|| format!("unknown track: {id}"))?;
                if track.kind != "midi" {
                    return Err(format!(
                        "track {id} is kind \"{}\" (hummed melodies need a midi track)",
                        track.kind
                    ));
                }
                (id, None)
            }
            None => {
                let track =
                    ops::add_track(&mut s, params, Some(name.to_string()), Some("midi".into()))?;
                (track.id.clone(), Some(track))
            }
        }
    };
    notes.sort_by_key(|n| (n.tick, n.key));
    let end = notes
        .iter()
        .map(|n| n.tick as u64 + n.length_ticks as u64)
        .max()
        .unwrap_or(0);
    let clip = MidiClip {
        id: uuid::Uuid::new_v4().to_string(),
        track_id: target,
        name: name.to_string(),
        timeline_start_ticks: at_ticks,
        length_ticks: length_ticks.max(end).max(1),
        notes,
    };
    midi.lock().clips.push(clip.clone());
    Ok((clip, created_track))
}

// ---------------------------------------------------------------------------
// ControlPlane front door
// ---------------------------------------------------------------------------

fn simulate_mode() -> bool {
    matches!(
        std::env::var("AURA_SIDECAR_SIMULATE").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Interpreter for the hum sidecar: prefer the repo's `.venv-sidecars`
/// environment next to the scripts (librosa/numpy live there), else any
/// `python3` on PATH — the worker degrades to its pure-python pitch tracker.
fn resolve_hum_python(script: &std::path::Path) -> Result<PathBuf, String> {
    if let Some(repo) = script.parent().and_then(|d| d.parent()) {
        let venv = repo.join(".venv-sidecars").join("bin").join("python");
        if venv.is_file() {
            return Ok(venv);
        }
    }
    jobs::resolve_python()
}

/// Build the sidecar JobSpec for a hum analysis over `params`.
fn hum_job_spec(params: &Value) -> Result<JobSpec, String> {
    let script = jobs::resolve_script("hum_to_midi.py")?;
    let python = resolve_hum_python(&script)?;
    let mut args = vec![
        script.to_string_lossy().into_owned(),
        "--params-json".into(),
        params.to_string(),
    ];
    if simulate_mode() {
        args.push("--simulate".into());
    }
    Ok(JobSpec {
        kind: JobKind::HumToMidi,
        program: python,
        args,
        grace: CANCEL_GRACE,
        env: Vec::new(),
    })
}

impl ControlPlane {
    /// Resolve the hum input to an absolute WAV path: an audio clip of the
    /// open project (by id) or a caller-supplied absolute path.
    fn resolve_hum_input(&self, req: &HumToSongRequest) -> Result<PathBuf, String> {
        match (&req.clip_id, &req.path) {
            (Some(_), Some(_)) => {
                Err("pass either clipId or path, not both".into())
            }
            (None, None) => Err("pass clipId (a recorded clip) or path (a WAV file)".into()),
            (Some(id), None) => {
                let s = self.store.lock();
                let clip = s
                    .clips
                    .iter()
                    .find(|c| &c.id == id)
                    .ok_or_else(|| format!("unknown audio clip: {id}"))?;
                let abs = s
                    .abs_path(&clip.source_path)
                    .ok_or("no project open — the clip has no on-disk home")?;
                if !abs.is_file() {
                    return Err(format!("clip audio missing on disk: {}", abs.display()));
                }
                Ok(abs)
            }
            (None, Some(p)) => {
                let path = PathBuf::from(p);
                if !path.is_absolute() {
                    return Err(format!("path must be absolute: {p}"));
                }
                if !path.is_file() {
                    return Err(format!("input file not found: {p}"));
                }
                Ok(path)
            }
        }
    }

    /// Submit a hum-to-song job: analyze the recording, then land the melody
    /// as a MIDI clip (and optionally chain an `amtInfill` accompaniment).
    /// Returns the job id immediately; progress/done/error stream over
    /// `sink`, the clip application is announced by a follow-up `log` event.
    /// Front door for BOTH the Tauri command and future MCP tooling.
    pub fn hum_to_song(
        self: &Arc<Self>,
        req: HumToSongRequest,
        sink: EventSink,
    ) -> Result<String, String> {
        let input = self.resolve_hum_input(&req)?;
        // Sync the midi store with the open project (same store->midi
        // ordering as the midi commands) so ppq/tempo are fresh.
        let (dir, project_bpm) = {
            let s = self.store.lock();
            (s.project_dir.clone(), s.transport.tempo_bpm)
        };
        crate::midi::notify_project_opened(dir, project_bpm);
        let ppq = self.midi.lock().ppq;
        let bpm = req.bpm.unwrap_or(project_bpm);
        if !(bpm > 0.0) {
            return Err(format!("bpm out of range: {bpm}"));
        }
        if let Some(g) = req.quantize_grid {
            if g > 0 && (ppq * 4) % g != 0 {
                return Err(format!("quantizeGrid {g} does not divide a bar at ppq {ppq}"));
            }
        }
        let params = json!({
            "inputPath": input,
            "ppq": ppq,
            "bpm": bpm,
            "quantizeGrid": req.quantize_grid.unwrap_or(0),
        });
        let spec = hum_job_spec(&params)?;
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Hummed".into());
        let cfg = HumApplyConfig {
            track_id: req.track_id.clone(),
            at_ticks: req.at_ticks.unwrap_or(0),
            accompany: req.accompany.unwrap_or(false),
            clip_name: format!("{stem} melody"),
            ppq,
            bpm,
        };
        let sink = wrap_sink_with_hum_apply(self, cfg, sink);
        Ok(self.jobs.submit(spec, sink))
    }

    /// Apply-side helper: land notes as a clip, persist the midi store into
    /// the open project (when there is one), and rebuild the graph.
    pub(crate) fn apply_hum_clip(
        &self,
        notes: Vec<MidiNote>,
        length_ticks: u64,
        track_id: Option<String>,
        at_ticks: u64,
        name: &str,
    ) -> Result<(MidiClip, Option<TrackState>), String> {
        let out = apply_hum_notes(
            &self.store,
            &self.params,
            &self.midi,
            notes,
            length_ticks,
            track_id,
            at_ticks,
            name,
        )?;
        let dir = self.store.lock().project_dir.clone();
        if let Some(d) = &dir {
            let m = self.midi.lock();
            if let Err(e) = crate::midi::persist::save_into_project(d, &m) {
                log::warn!("hum-to-song: persisting midi failed: {e}");
            }
        }
        self.engine.send(ControlMsg::Rebuild);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Post-job sink wrappers
// ---------------------------------------------------------------------------

/// Wrap a job event sink so a `done` result with `kind: "humToMidi"` lands
/// as a MIDI clip (and optionally chains the accompaniment job). The apply
/// runs on its own thread (store/midi locks + disk persistence must not
/// stall the job supervisor); its outcome is a follow-up `log` event.
pub(crate) fn wrap_sink_with_hum_apply(
    control: &Arc<ControlPlane>,
    cfg: HumApplyConfig,
    inner: EventSink,
) -> EventSink {
    let control = Arc::clone(control);
    let cfg = Arc::new(cfg);
    Arc::new(move |ev: SidecarEvent| {
        if let SidecarEvent::Done { job_id, result } = &ev {
            if result.get("kind").and_then(Value::as_str) == Some("humToMidi") {
                let control = Arc::clone(&control);
                let cfg = Arc::clone(&cfg);
                let inner2 = Arc::clone(&inner);
                let job_id = job_id.clone();
                let result = result.clone();
                inner(ev.clone()); // deliver `done` first; the apply is extra
                std::thread::spawn(move || {
                    let line = match apply_melody_and_maybe_accompany(
                        &control, &cfg, &result, &inner2,
                    ) {
                        Ok(line) => line,
                        Err(e) => format!("hum-to-song apply failed: {e}"),
                    };
                    inner2(SidecarEvent::Log { job_id, line });
                });
                return;
            }
        }
        inner(ev)
    })
}

/// The `done`-side work: parse → apply melody clip → optionally submit the
/// accompaniment job. Returns the human-readable log line.
fn apply_melody_and_maybe_accompany(
    control: &Arc<ControlPlane>,
    cfg: &HumApplyConfig,
    result: &Value,
    sink: &EventSink,
) -> Result<String, String> {
    let (notes, length_ticks) = parse_hum_result(result, cfg.ppq)?;
    let (clip, created) = control.apply_hum_clip(
        notes.clone(),
        length_ticks,
        cfg.track_id.clone(),
        cfg.at_ticks,
        &cfg.clip_name,
    )?;
    let mut line = format!(
        "hum-to-song: melody clip {} ({} notes) on track {}{}",
        clip.id,
        clip.notes.len(),
        clip.track_id,
        created
            .as_ref()
            .map(|t| format!(" (auto-created \"{}\")", t.name))
            .unwrap_or_default(),
    );
    if cfg.accompany {
        match submit_accompaniment(control, cfg, &notes, length_ticks, sink) {
            Ok(id) => line.push_str(&format!("; accompaniment job {id} submitted")),
            Err(e) => line.push_str(&format!("; accompaniment submit failed: {e}")),
        }
    }
    Ok(line)
}

/// Second stage: `amtInfill` over the melody's tick span with the melody as
/// context. The proposal lands as a "Hum Accompaniment" clip on its own
/// auto-created midi track (see [`wrap_sink_with_accompany_apply`]).
fn submit_accompaniment(
    control: &Arc<ControlPlane>,
    cfg: &HumApplyConfig,
    melody: &[MidiNote],
    length_ticks: u64,
    sink: &EventSink,
) -> Result<String, String> {
    let region_end = u32::try_from(length_ticks).unwrap_or(u32::MAX).max(1);
    let params = json!({
        "ppq": cfg.ppq,
        "bpm": cfg.bpm,
        "contextNotes": melody,
        "regionStartTicks": 0,
        "regionEndTicks": region_end,
    });
    let sink = wrap_sink_with_accompany_apply(control, cfg, length_ticks, Arc::clone(sink));
    crate::sidecars::run_generic_job(&control.jobs, "amtInfill", &params, sink)
}

/// Sink wrapper for the chained accompaniment job: a `done` result with
/// `kind: "amtInfill"` is decoded via the existing `midi::amt::parse_result`
/// and landed as its own clip/track.
pub(crate) fn wrap_sink_with_accompany_apply(
    control: &Arc<ControlPlane>,
    cfg: &HumApplyConfig,
    length_ticks: u64,
    inner: EventSink,
) -> EventSink {
    let control = Arc::clone(control);
    let (ppq, at_ticks) = (cfg.ppq, cfg.at_ticks);
    Arc::new(move |ev: SidecarEvent| {
        if let SidecarEvent::Done { job_id, result } = &ev {
            if result.get("kind").and_then(Value::as_str) == Some("amtInfill") {
                let control = Arc::clone(&control);
                let inner2 = Arc::clone(&inner);
                let job_id = job_id.clone();
                let result = result.clone();
                inner(ev.clone());
                std::thread::spawn(move || {
                    let line = match crate::midi::amt::parse_result(&result, ppq)
                        .map_err(|e| e)
                        .and_then(|notes| {
                            control.apply_hum_clip(
                                notes,
                                length_ticks,
                                None,
                                at_ticks,
                                "Hum Accompaniment",
                            )
                        }) {
                        Ok((clip, _)) => format!(
                            "hum-to-song: accompaniment clip {} ({} notes) on track {}",
                            clip.id,
                            clip.notes.len(),
                            clip.track_id
                        ),
                        Err(e) => format!("hum-to-song accompaniment apply failed: {e}"),
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
// Command (registration in lib.rs is deferred to the integration agent):
//   control::hum::hum_to_song
// ---------------------------------------------------------------------------

/// Submit a hum-to-song job (see [`ControlPlane::hum_to_song`]). Returns the
/// job id; events stream over `on_event` plus the `sidecar://*` app events
/// (same fan-out as the other sidecar commands).
#[tauri::command]
pub async fn hum_to_song(
    app: tauri::AppHandle,
    request: HumToSongRequest,
    on_event: tauri::ipc::Channel<SidecarEvent>,
    control: tauri::State<'_, Arc<ControlPlane>>,
) -> Result<String, String> {
    use tauri::Emitter;
    let sink: EventSink = Arc::new(move |ev: SidecarEvent| {
        let _ = on_event.send(ev.clone());
        let event_name = match &ev {
            SidecarEvent::Progress { .. } => "sidecar://progress",
            SidecarEvent::Done { .. } => "sidecar://done",
            SidecarEvent::Error { .. } => "sidecar://error",
            SidecarEvent::Log { .. } => return,
        };
        if let Err(e) = app.emit(event_name, &ev) {
            log::warn!("hum-to-song: failed to emit {event_name}: {e}");
        }
    });
    control.hum_to_song(request, sink)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::project;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn note(tick: u32, len: u32, key: u8, vel: u8) -> MidiNote {
        MidiNote { tick, length_ticks: len, key, velocity: vel, channel: 0 }
    }

    // ---- parse_hum_result -------------------------------------------------

    #[test]
    fn parse_hum_result_validates_rescales_and_sorts() {
        let r = json!({
            "kind": "humToMidi",
            "ppq": 480,
            "lengthTicks": 1920,
            "notes": [
                {"tick": 240, "lengthTicks": 120, "key": 72, "velocity": 90,
                 "pitchHz": 523.25},
                {"tick": 0, "lengthTicks": 240, "key": 69, "velocity": 100},
            ],
        });
        let (notes, length) = parse_hum_result(&r, 960).unwrap();
        assert_eq!(notes, vec![note(0, 480, 69, 100), note(480, 240, 72, 90)]);
        assert_eq!(length, 3840, "lengthTicks rescaled 480 -> 960 ppq");

        // Wrong kind / no notes / empty notes / invalid note.
        assert!(parse_hum_result(&json!({"kind": "other"}), 960).is_err());
        assert!(parse_hum_result(&json!({"kind": "humToMidi"}), 960).is_err());
        assert!(
            parse_hum_result(&json!({"kind": "humToMidi", "notes": []}), 960).is_err()
        );
        let bad = json!({
            "kind": "humToMidi",
            "notes": [{"tick": 0, "lengthTicks": 10, "key": 200, "velocity": 90}],
        });
        assert!(parse_hum_result(&bad, 960).is_err(), "range-validated");
    }

    #[test]
    fn parse_hum_result_length_covers_notes_even_when_absent() {
        let r = json!({
            "kind": "humToMidi",
            "ppq": 960,
            "notes": [{"tick": 100, "lengthTicks": 50, "key": 60, "velocity": 90}],
        });
        let (_, length) = parse_hum_result(&r, 960).unwrap();
        assert_eq!(length, 150, "falls back to the notes' span");
    }

    // ---- apply core -------------------------------------------------------

    fn plain_store(n_midi: usize, n_audio: usize) -> (Mutex<Store>, ParamTable) {
        let mut store = Store::default();
        let params = ParamTable::default();
        for i in 0..n_midi {
            ops::add_track(&mut store, &params, Some(format!("M{i}")), Some("midi".into()))
                .unwrap();
        }
        for i in 0..n_audio {
            ops::add_track(&mut store, &params, Some(format!("A{i}")), Some("audio".into()))
                .unwrap();
        }
        (Mutex::new(store), params)
    }

    #[test]
    fn apply_auto_creates_midi_track_and_places_clip() {
        let (store, params) = plain_store(0, 0);
        let midi = Mutex::new(MidiStore::default());
        let (clip, created) = apply_hum_notes(
            &store,
            &params,
            &midi,
            vec![note(480, 240, 72, 90), note(0, 480, 69, 100)],
            3840,
            None,
            960,
            "take melody",
        )
        .unwrap();
        let t = created.expect("track auto-created");
        assert_eq!(t.kind, "midi");
        assert_eq!(t.name, "take melody");
        assert_eq!(clip.track_id, t.id);
        assert_eq!(clip.timeline_start_ticks, 960);
        assert_eq!(clip.length_ticks, 3840);
        assert_eq!(
            clip.notes,
            vec![note(0, 480, 69, 100), note(480, 240, 72, 90)],
            "sorted by (tick, key)"
        );
        let m = midi.lock();
        assert_eq!(m.clips.len(), 1);
        let s = store.lock();
        assert_eq!(s.tracks.len(), 1);
        assert!(s.slots.contains_key(&t.id), "RT slot allocated");
    }

    #[test]
    fn apply_targets_existing_midi_track_and_rejects_bad_targets() {
        let (store, params) = plain_store(1, 1);
        let midi_id = store.lock().tracks[0].id.clone();
        let audio_id = store.lock().tracks[1].id.clone();
        let midi = Mutex::new(MidiStore::default());

        let (clip, created) = apply_hum_notes(
            &store, &params, &midi,
            vec![note(0, 100, 60, 90)], 0, Some(midi_id.clone()), 0, "m",
        )
        .unwrap();
        assert!(created.is_none());
        assert_eq!(clip.track_id, midi_id);
        assert_eq!(clip.length_ticks, 100, "length covers the notes");

        // Audio track target / unknown track / empty notes: structured errors,
        // no mutation.
        let e = apply_hum_notes(
            &store, &params, &midi,
            vec![note(0, 100, 60, 90)], 0, Some(audio_id), 0, "m",
        )
        .unwrap_err();
        assert!(e.contains("midi track"), "{e}");
        let e = apply_hum_notes(
            &store, &params, &midi,
            vec![note(0, 100, 60, 90)], 0, Some("ghost".into()), 0, "m",
        )
        .unwrap_err();
        assert!(e.contains("unknown track"), "{e}");
        assert!(apply_hum_notes(&store, &params, &midi, vec![], 0, None, 0, "m").is_err());
        assert_eq!(midi.lock().clips.len(), 1, "no clip from failed attempts");
        assert_eq!(store.lock().tracks.len(), 2, "no track from failed attempts");
    }

    // ---- fake-sidecar end-to-end (job manager + wrapped sink) -------------

    struct NullEvents;
    impl crate::audio::engine::EventSink for NullEvents {
        fn emit(&self, _e: &str, _p: serde_json::Value) {}
    }

    /// Headless ControlPlane over a temp project (engine works without an
    /// audio device; same pattern as control/mod.rs's loop test).
    fn test_control_plane(name: &str) -> (Arc<ControlPlane>, std::path::PathBuf) {
        let parent = std::env::temp_dir().join(format!(
            "aura-hum-test-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let (_p, dir) = project::create(&parent, "Hum", 48_000, 120.0).unwrap();
        let shared = Arc::new(crate::audio::rt::SharedRt::default());
        let params = Arc::new(ParamTable::default());
        let store = Arc::new(Mutex::new(Store::default()));
        store.lock().project_dir = Some(dir);
        store.lock().project_name = Some("Hum".into());
        let engine = crate::audio::engine::start(
            shared.clone(),
            params.clone(),
            store.clone(),
            Box::new(NullEvents),
        );
        let cp = Arc::new(ControlPlane::new(
            store,
            shared,
            params,
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Arc::new(Mutex::new(MidiStore::default())),
            Box::new(|_, _| {}),
        ));
        (cp, parent)
    }

    fn collecting_sink() -> (EventSink, Arc<Mutex<Vec<SidecarEvent>>>) {
        let events: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let e = Arc::clone(&events);
        (Arc::new(move |ev| e.lock().push(ev)), events)
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    /// A scripted fake sidecar (established fake-worker pattern) drives the
    /// REAL post-job path: done → parse → clip on an auto-created midi track
    /// → persisted into the project → follow-up log line.
    #[tokio::test]
    async fn fake_sidecar_done_lands_midi_clip_and_persists() {
        let (cp, parent) = test_control_plane("fake");
        let script = write_script(
            &parent,
            "fake_hum.sh",
            r#"#!/bin/sh
echo '{"type":"progress","progress":0.5,"stage":"trackingPitch"}'
echo '{"type":"done","result":{"kind":"humToMidi","ppq":960,"bpm":120,"lengthTicks":3840,"notes":[{"tick":0,"lengthTicks":480,"key":69,"velocity":100,"pitchHz":440.0},{"tick":480,"lengthTicks":480,"key":72,"velocity":90}],"simulated":false}}'
"#,
        );
        let spec = JobSpec {
            kind: JobKind::HumToMidi,
            program: std::path::PathBuf::from("/bin/sh"),
            args: vec![script.to_string_lossy().into_owned()],
            grace: Duration::from_millis(500),
            env: Vec::new(),
        };
        let cfg = HumApplyConfig {
            track_id: None,
            at_ticks: 1920,
            accompany: false,
            clip_name: "take melody".into(),
            ppq: 960,
            bpm: 120.0,
        };
        let (inner, events) = collecting_sink();
        let sink = wrap_sink_with_hum_apply(&cp, cfg, inner);
        let id = cp.jobs.submit(spec, sink);

        // Job goes terminal…
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let st = cp.jobs.status(&id).unwrap();
            if crate::sidecars::types::JobState::is_terminal(&st.state) {
                assert_eq!(st.state, "done", "err: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "job never finished: {st:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // …then the async apply lands the clip (poll).
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !cp.midi.lock().clips.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "melody clip never applied");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (clip, track) = {
            let m = cp.midi.lock();
            let clip = m.clips[0].clone();
            let s = cp.store.lock();
            let track = s.tracks.iter().find(|t| t.id == clip.track_id).cloned();
            (clip, track)
        };
        assert_eq!(clip.name, "take melody");
        assert_eq!(clip.timeline_start_ticks, 1920);
        assert_eq!(clip.length_ticks, 3840);
        assert_eq!(
            clip.notes,
            vec![note(0, 480, 69, 100), note(480, 480, 72, 90)],
            "pitchHz diagnostic field ignored, notes decoded"
        );
        let track = track.expect("auto-created track registered");
        assert_eq!(track.kind, "midi");
        assert_eq!(track.name, "take melody");

        // Persistence: the midi state is on disk in the temp project.
        let dir = cp.store.lock().project_dir.clone().unwrap();
        let v2 = crate::midi::persist::load_from_project(&dir)
            .unwrap()
            .expect("project persisted as v2");
        assert_eq!(v2.clips.len(), 1);
        assert_eq!(v2.clips[0].notes.len(), 2);

        // Event order: done first, then the follow-up apply log line.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let evs = events.lock();
                let done_at = evs
                    .iter()
                    .position(|e| matches!(e, SidecarEvent::Done { .. }));
                let log_at = evs.iter().position(|e| matches!(
                    e, SidecarEvent::Log { line, .. } if line.contains("hum-to-song: melody clip")));
                if let (Some(d), Some(l)) = (done_at, log_at) {
                    assert!(d < l, "done before the apply log");
                    break;
                }
            }
            assert!(Instant::now() < deadline, "apply log line never arrived");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cp.engine.send(crate::audio::engine::ControlMsg::Shutdown);
        let _ = std::fs::remove_dir_all(parent);
    }

    /// The accompaniment sink wrapper applies an `amtInfill` proposal as its
    /// own clip on an auto-created track (chaining unit, no python needed).
    #[tokio::test]
    async fn accompany_sink_applies_amt_result_as_second_clip() {
        let (cp, parent) = test_control_plane("accomp");
        let cfg = HumApplyConfig {
            track_id: None,
            at_ticks: 0,
            accompany: true,
            clip_name: "m".into(),
            ppq: 960,
            bpm: 120.0,
        };
        let (inner, events) = collecting_sink();
        let sink = wrap_sink_with_accompany_apply(&cp, &cfg, 3840, inner);
        sink(SidecarEvent::Done {
            job_id: "amt-1".into(),
            result: json!({
                "kind": "amtInfill",
                "ppq": 960,
                "notes": [
                    {"tick": 0, "lengthTicks": 480, "key": 57, "velocity": 96},
                    {"tick": 960, "lengthTicks": 480, "key": 60, "velocity": 96},
                ],
            }),
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !cp.midi.lock().clips.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "accompaniment clip never applied");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let clip = cp.midi.lock().clips[0].clone();
        assert_eq!(clip.name, "Hum Accompaniment");
        assert_eq!(clip.length_ticks, 3840);
        assert_eq!(clip.notes.len(), 2);
        let s = cp.store.lock();
        let t = s.tracks.iter().find(|t| t.id == clip.track_id).unwrap();
        assert_eq!(t.kind, "midi");
        drop(s);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if events.lock().iter().any(|e| matches!(
                e, SidecarEvent::Log { line, .. }
                    if line.contains("accompaniment clip"))) {
                break;
            }
            assert!(Instant::now() < deadline, "accompaniment log never arrived");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cp.engine.send(crate::audio::engine::ControlMsg::Shutdown);
        let _ = std::fs::remove_dir_all(parent);
    }

    /// Input resolution + request validation surface.
    #[tokio::test]
    async fn hum_to_song_request_validation_is_structured() {
        let (cp, parent) = test_control_plane("validate");
        let (sink, _) = collecting_sink();

        let e = cp.hum_to_song(HumToSongRequest::default(), Arc::clone(&sink)).unwrap_err();
        assert!(e.contains("clipId") && e.contains("path"), "{e}");

        let e = cp
            .hum_to_song(
                HumToSongRequest {
                    clip_id: Some("c".into()),
                    path: Some("/x.wav".into()),
                    ..Default::default()
                },
                Arc::clone(&sink),
            )
            .unwrap_err();
        assert!(e.contains("not both"), "{e}");

        let e = cp
            .hum_to_song(
                HumToSongRequest { path: Some("relative.wav".into()), ..Default::default() },
                Arc::clone(&sink),
            )
            .unwrap_err();
        assert!(e.contains("absolute"), "{e}");

        let e = cp
            .hum_to_song(
                HumToSongRequest { clip_id: Some("ghost".into()), ..Default::default() },
                Arc::clone(&sink),
            )
            .unwrap_err();
        assert!(e.contains("unknown audio clip"), "{e}");

        // A real input file + a bad quantize grid: rejected before submission.
        let wav = parent.join("in.wav");
        write_test_hum_wav(&wav, &[(Some(440.0), 0.2)]);
        let e = cp
            .hum_to_song(
                HumToSongRequest {
                    path: Some(wav.to_string_lossy().into_owned()),
                    quantize_grid: Some(7),
                    ..Default::default()
                },
                Arc::clone(&sink),
            )
            .unwrap_err();
        assert!(e.contains("quantizeGrid"), "{e}");
        assert!(cp.jobs.list().is_empty(), "nothing was submitted");
        cp.engine.send(crate::audio::engine::ControlMsg::Shutdown);
        let _ = std::fs::remove_dir_all(parent);
    }

    // ---- real python worker (gated) ---------------------------------------

    /// Deterministic hummed-like WAV: 16 kHz mono i16, sine with 5 Hz ±30
    /// cent vibrato + low noise + attack/release, silence for `None` spans.
    fn write_test_hum_wav(path: &Path, segments: &[(Option<f64>, f64)]) {
        let sr = 16_000u32;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: sr,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        let mut rng: u64 = 0x2545_F491_4F6C_DD1D; // deterministic xorshift
        let mut noise = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            ((rng >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 0.06
        };
        let mut phase = 0.0f64;
        for &(freq, dur) in segments {
            let n = (dur * sr as f64) as usize;
            for i in 0..n {
                let s = match freq {
                    None => noise(),
                    Some(f) => {
                        let vib = 2f64.powf(
                            30.0 / 1200.0
                                * (2.0 * std::f64::consts::PI * 5.0 * i as f64 / sr as f64).sin(),
                        );
                        phase += 2.0 * std::f64::consts::PI * f * vib / sr as f64;
                        let env = (i as f64 / (0.02 * sr as f64))
                            .min((n - i) as f64 / (0.03 * sr as f64))
                            .min(1.0);
                        0.5 * env * phase.sin() + noise()
                    }
                };
                w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    /// Python for the gated tests: the sidecars venv when present (librosa/
    /// numpy), else PATH python3 (pure-python fallback), else skip.
    fn gated_python() -> Option<std::path::PathBuf> {
        let venv = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../.venv-sidecars/bin/python");
        if venv.is_file() {
            return Some(venv);
        }
        jobs::resolve_python().ok()
    }

    fn hum_script() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../sidecars/hum_to_midi.py")
    }

    fn run_worker(python: &Path, args: &[&str]) -> (bool, String, String) {
        let out = std::process::Command::new(python)
            .arg(hum_script())
            .args(args)
            .output()
            .expect("spawn hum worker");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn done_result(stdout: &str) -> Value {
        for line in stdout.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                match v["type"].as_str() {
                    Some("done") => return v["result"].clone(),
                    Some("error") => panic!("worker error: {v}"),
                    _ => {}
                }
            }
        }
        panic!("no done event in worker output:\n{stdout}");
    }

    /// REAL end-to-end: synthesized hum WAV → real pitch analysis → notes →
    /// the store-level apply. Asserts pitch accuracy (A4 vibrato → key 69,
    /// median f0 within 25 cents). Gated on a python being available.
    #[test]
    fn real_worker_end_to_end_synthesized_hum_to_clip() {
        let Some(python) = gated_python() else {
            eprintln!("skipping: no python available");
            return;
        };
        let dir = std::env::temp_dir().join(format!("aura-hum-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("hum.wav");
        write_test_hum_wav(
            &wav,
            &[
                (None, 0.3),
                (Some(440.0), 1.0),   // A4 with vibrato -> key 69
                (None, 0.15),
                (Some(523.25), 0.5),  // C5 -> key 72
                (None, 0.2),
            ],
        );
        let params = json!({
            "inputPath": wav,
            "ppq": 960,
            "bpm": 120.0,
        })
        .to_string();
        let (ok, stdout, stderr) = run_worker(&python, &["--params-json", &params]);
        assert!(ok, "worker failed\nstdout:\n{stdout}\nstderr:\n{stderr}");
        let result = done_result(&stdout);
        assert_eq!(result["kind"], "humToMidi");
        assert_eq!(result["simulated"], false);

        let (notes, length) = parse_hum_result(&result, 960).unwrap();
        let keys: Vec<u8> = notes.iter().map(|n| n.key).collect();
        assert_eq!(keys, vec![69, 72], "A4 + C5 detected: {notes:?}");
        // Pitch accuracy: the reported median f0 of the A4 note.
        let hz = result["notes"][0]["pitchHz"].as_f64().expect("pitchHz diagnostic");
        let cents = 1200.0 * (hz / 440.0).log2();
        assert!(cents.abs() < 25.0, "A4 within 25 cents (got {cents:.1}: {hz} Hz)");
        // Onset/duration land in the right ballpark (0.3 s @120 bpm/960 ppq
        // = 576 ticks; 1.0 s = 1920 ticks).
        assert!((notes[0].tick as i64 - 576).unsigned_abs() < 200, "{:?}", notes[0]);
        assert!((notes[0].length_ticks as i64 - 1920).unsigned_abs() < 300, "{:?}", notes[0]);
        assert_eq!(length % (4 * 960), 0, "clip length is whole bars");

        // Store-level apply of the real result.
        let (store, tparams) = plain_store(0, 0);
        let midi = Mutex::new(MidiStore::default());
        let (clip, created) =
            apply_hum_notes(&store, &tparams, &midi, notes, length, None, 0, "hum melody")
                .unwrap();
        assert_eq!(clip.notes.len(), 2);
        assert_eq!(created.unwrap().kind, "midi");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The worker's own test suite (pitch accuracy on synthetic hums, WAV
    /// parsing, quantize, simulate determinism) — run under cargo so a
    /// regression in the python side fails the Rust suite too.
    #[test]
    fn worker_self_test_passes() {
        let Some(python) = gated_python() else {
            eprintln!("skipping: no python available");
            return;
        };
        let (ok, stdout, stderr) = run_worker(&python, &["--self-test"]);
        assert!(ok, "self-test failed\nstdout:\n{stdout}\nstderr:\n{stderr}");
        assert!(stderr.contains("0 failure(s)"), "{stderr}");
    }

    /// Simulate mode is deterministic and yields a valid, applicable melody.
    #[test]
    fn simulate_worker_is_deterministic_and_valid() {
        let Some(python) = gated_python() else {
            eprintln!("skipping: no python available");
            return;
        };
        let params = json!({"ppq": 960, "bpm": 120.0}).to_string();
        let (ok1, out1, _) = run_worker(&python, &["--params-json", &params, "--simulate"]);
        let (ok2, out2, _) = run_worker(&python, &["--params-json", &params, "--simulate"]);
        assert!(ok1 && ok2);
        let (r1, r2) = (done_result(&out1), done_result(&out2));
        assert_eq!(r1, r2, "deterministic");
        assert_eq!(r1["simulated"], true);
        let (notes, length) = parse_hum_result(&r1, 960).unwrap();
        assert!(notes.len() >= 4, "a plausible melody: {notes:?}");
        assert!(length >= notes.last().map(|n| n.tick as u64).unwrap_or(0));
    }
}
