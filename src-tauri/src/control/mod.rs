//! AURA control plane — the shared seam between IPC surfaces and the engine.
//! OWNED BY AGENT 1 (architecture); the `import.rs` stub is delegated to the
//! music-generation agent (phase 2, zone B). See ARCHITECTURE §11.
//!
//! WHY THIS EXISTS: the MCP server (src/mcp/) must drive AURA WITHOUT going
//! through the Tauri IPC layer (no webview, no `invoke`). [`ControlPlane`] is
//! the one front door to project/engine state: Tauri commands are thin
//! wrappers over it, and MCP tool handlers call the SAME methods. Anything
//! only reachable from a `#[tauri::command]` body is a bug.
//!
//! Layout:
//! * [`ops`]    — pure shared operations (Store/ParamTable-level, testable).
//! * this file  — the [`ControlPlane`] facade (holds the engine handles),
//!                plus its own `#[tauri::command]` wrappers
//!                (`get_project_state`, `set_track_mix`, `import_audio_clip`).

pub mod import;
pub mod ops;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::audio::engine::{ControlMsg, EngineHandle, MeterSink};
use crate::audio::rt::{ParamTable, SharedRt};
use crate::audio::types::{Clip, MeterFrame, Project, Store, TrackState, TransportState};
use crate::audio::project;
use crate::midi::MidiStore;
use crate::sidecars::jobs::{EventSink, JobManager};

pub use ops::TrackMixChange;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Full state snapshot — the payload of `get_project_state` and the MCP
/// `get_project_state` tool (one shape, one source).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSnapshot {
    pub project_name: Option<String>,
    /// Absolute path of the open .aura dir (informational).
    pub project_dir: Option<String>,
    pub transport: TransportState,
    pub tracks: Vec<TrackState>,
    pub clips: Vec<Clip>,
    pub midi_clips: Vec<crate::midi::MidiClip>,
    pub ppq: u32,
    pub tempo_events: Vec<crate::midi::TempoEvent>,
}

/// Argument of `import_audio_clip` (and the MCP tool of the same name).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportClipRequest {
    /// Absolute path to a wav/flac file to copy into `<project>/audio/`.
    pub path: String,
    /// Target track; None = create a new audio track named after the file.
    pub track_id: Option<String>,
    /// Timeline placement in samples; None = 0.
    pub at_samples: Option<u64>,
}

/// Transport action for `transport_control` (MCP tool + future command).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "action")]
pub enum TransportAction {
    Play,
    Stop,
    Seek { position_samples: u64 },
    /// Set the loop region (samples) and its enabled flag. `end <= start`
    /// with `enabled: true` is rejected; the region may be stored disabled.
    SetLoop { enabled: bool, start_samples: u64, end_samples: u64 },
}

// ---------------------------------------------------------------------------
// ControlPlane
// ---------------------------------------------------------------------------

/// Emits app-level events (`transport://state`, `project://changed`, ...);
/// lib.rs installs an `AppHandle`-backed closure.
pub type EventEmitter = Box<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// The shared control-plane facade. Managed by Tauri (constructed in setup,
/// after `audio::init`), and handed as an `Arc` to the MCP server.
pub struct ControlPlane {
    store: Arc<Mutex<Store>>,
    shared: Arc<SharedRt>,
    params: Arc<ParamTable>,
    engine: EngineHandle,
    jobs: Arc<JobManager>,
    midi: Arc<Mutex<MidiStore>>,
    latest_meters: Arc<Mutex<Option<MeterFrame>>>,
    emit: EventEmitter,
}

/// MeterSink that keeps only the latest frame so MCP's `read_meters` tool can
/// "hear" current levels without a streaming subscription. Never unsubscribes.
struct LatestMeterCache(Arc<Mutex<Option<MeterFrame>>>);

impl MeterSink for LatestMeterCache {
    fn send_frame(&self, frame: &MeterFrame) -> bool {
        *self.0.lock() = Some(frame.clone());
        true
    }
}

impl ControlPlane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<Mutex<Store>>,
        shared: Arc<SharedRt>,
        params: Arc<ParamTable>,
        engine: EngineHandle,
        jobs: Arc<JobManager>,
        midi: Arc<Mutex<MidiStore>>,
        emit: EventEmitter,
    ) -> Self {
        let latest_meters = Arc::new(Mutex::new(None));
        engine.send(ControlMsg::Subscribe(Box::new(LatestMeterCache(
            latest_meters.clone(),
        ))));
        Self { store, shared, params, engine, jobs, midi, latest_meters, emit }
    }

    fn emit_transport(&self, snap: &TransportState) {
        if let Ok(v) = serde_json::to_value(snap) {
            (self.emit)("transport://state", v);
        }
    }

    // ---- read side ------------------------------------------------------

    pub fn project_state(&self) -> ProjectSnapshot {
        let store = self.store.lock();
        let midi = self.midi.lock();
        ProjectSnapshot {
            project_name: store.project_name.clone(),
            project_dir: store.project_dir.as_ref().map(|p| p.display().to_string()),
            transport: ops::transport_snapshot(&store, &self.shared),
            tracks: store.tracks.clone(),
            clips: store.clips.clone(),
            midi_clips: midi.clips.clone(),
            ppq: midi.ppq,
            tempo_events: midi.tempo_events.clone(),
        }
    }

    pub fn transport_state(&self) -> TransportState {
        ops::transport_snapshot(&self.store.lock(), &self.shared)
    }

    /// Latest 60 Hz meter frame (None until the engine has pumped one).
    /// This is the MCP agent's "ears": peak/RMS per track + master.
    pub fn read_meters(&self) -> Option<MeterFrame> {
        self.latest_meters.lock().clone()
    }

    // ---- transport / recording -----------------------------------------

    pub fn transport(&self, action: TransportAction) -> Result<TransportState, String> {
        match action {
            TransportAction::Play => {
                {
                    let mut s = self.store.lock();
                    if s.transport.state != "recording" {
                        s.transport.state = "playing".into();
                    }
                }
                self.shared.playing.store(true, Relaxed);
            }
            TransportAction::Stop => {
                // Stopping while recording finalizes the take (DAW convention).
                if self.shared.recording.load(Relaxed) {
                    self.engine
                        .request::<Vec<Clip>>(|reply| ControlMsg::StopRecording { reply })?;
                }
                self.shared.playing.store(false, Relaxed);
                self.store.lock().transport.state = "stopped".into();
            }
            TransportAction::Seek { position_samples } => {
                self.shared.position.store(position_samples, Relaxed);
            }
            TransportAction::SetLoop { enabled, start_samples, end_samples } => {
                if enabled && end_samples <= start_samples {
                    return Err(format!(
                        "loop region is empty (start {start_samples} >= end {end_samples})"
                    ));
                }
                // RT atomics (the output callback reads these per buffer)...
                self.shared.loop_start.store(start_samples, Relaxed);
                self.shared.loop_end.store(end_samples, Relaxed);
                self.shared.loop_enabled.store(enabled, Relaxed);
                // ...and the store mirror, so project save persists the
                // region (project::from_store serializes store.transport).
                let mut s = self.store.lock();
                s.transport.loop_enabled = enabled;
                s.transport.loop_start_samples = start_samples;
                s.transport.loop_end_samples = end_samples;
            }
        }
        let snap = self.transport_state();
        self.emit_transport(&snap);
        Ok(snap)
    }

    pub fn start_recording(
        &self,
        track_ids: Option<Vec<String>>,
    ) -> Result<TransportState, String> {
        self.engine
            .request::<Vec<String>>(|reply| ControlMsg::StartRecording { track_ids, reply })?;
        let snap = self.transport_state();
        self.emit_transport(&snap);
        Ok(snap)
    }

    pub fn stop_recording(&self) -> Result<Vec<Clip>, String> {
        let clips = self
            .engine
            .request::<Vec<Clip>>(|reply| ControlMsg::StopRecording { reply })?;
        let snap = self.transport_state();
        self.emit_transport(&snap);
        Ok(clips)
    }

    // ---- structure ------------------------------------------------------

    pub fn add_track(
        &self,
        name: Option<String>,
        kind: Option<String>,
    ) -> Result<TrackState, String> {
        let track = {
            let mut s = self.store.lock();
            ops::add_track(&mut s, &self.params, name, kind)?
        };
        self.engine.send(ControlMsg::Rebuild);
        Ok(track)
    }

    /// Batched mix changes: param-table writes only, no graph rebuild (§10.2).
    pub fn set_track_mix(
        &self,
        changes: Vec<TrackMixChange>,
    ) -> Result<Vec<TrackState>, String> {
        let mut s = self.store.lock();
        ops::apply_track_mix(&mut s, &self.params, &changes)
    }

    pub fn create_project(&self, parent_dir: &str, name: &str) -> Result<Project, String> {
        let rate = self.shared.sample_rate.load(Relaxed);
        let tempo = self.store.lock().transport.tempo_bpm;
        let (mut project, dir) =
            project::create(std::path::Path::new(parent_dir), name, rate, tempo)?;
        {
            let mut s = self.store.lock();
            s.clips.clear();
            s.project_dir = Some(dir.clone());
            s.project_name = Some(name.to_string());
            s.created_at = project.created_at.clone();
            project.tracks = s.tracks.clone();
        }
        if !project.tracks.is_empty() {
            project::save(&dir, &project)?;
        }
        self.engine.send(ControlMsg::Rebuild);
        (self.emit)(
            "project://changed",
            serde_json::to_value(&project).unwrap_or_default(),
        );
        Ok(project)
    }

    /// Copy the file into `<project>/audio/`, probe channels/rate/length,
    /// register the clip, build the waveform pyramid, `Rebuild`. Frozen
    /// signature; body lives in [`import`] (zone B).
    pub fn import_audio_clip(&self, req: ImportClipRequest) -> Result<Clip, String> {
        self.import_audio_clip_impl(req)
    }

    /// Seed a content-less session with a small demo song (two midi tracks —
    /// arp + bass — rendered through the built-in PolySynth), so the very
    /// first press of PLAY makes sound. This is the control-plane end of the
    /// UI's "load demo song" affordance; it refuses to run when the session
    /// already has audible content, and needs no open project (the clips live
    /// in memory exactly like any unsaved edit).
    pub fn seed_demo_project(&self) -> Result<ProjectSnapshot, String> {
        {
            let s = self.store.lock();
            let m = self.midi.lock();
            if !s.clips.is_empty() || m.clips.iter().any(|c| !c.notes.is_empty()) {
                return Err("project already has content".to_string());
            }
        }
        // Sync the midi store with the open project first (same store->midi
        // ordering as the midi commands), so we never clobber on-disk state.
        let (dir, bpm) = {
            let s = self.store.lock();
            (s.project_dir.clone(), s.transport.tempo_bpm)
        };
        crate::midi::notify_project_opened(dir.clone(), bpm);

        let (keys, bass) = {
            let mut s = self.store.lock();
            let keys = ops::add_track(
                &mut s,
                &self.params,
                Some("Demo Keys".into()),
                Some("midi".into()),
            )?;
            let bass = ops::add_track(
                &mut s,
                &self.params,
                Some("Demo Bass".into()),
                Some("midi".into()),
            )?;
            (keys, bass)
        };
        {
            let mut m = self.midi.lock();
            let ppq = m.ppq;
            let (arp, groove) = demo_seed_clips(&keys.id, &bass.id, ppq);
            m.clips.push(arp);
            m.clips.push(groove);
            if let Some(d) = &dir {
                if let Err(e) = crate::midi::persist::save_into_project(d, &m) {
                    log::warn!("seed demo: persisting midi failed: {e}");
                }
            }
        }
        self.engine.send(ControlMsg::Rebuild);
        // No app event: the caller applies the returned snapshot (the frozen
        // event-name surface stays untouched).
        Ok(self.project_state())
    }

    // ---- sidecar jobs ---------------------------------------------------

    /// Open-kind job submission (D-07 direction); see sidecars::run_generic_job.
    ///
    /// EVERY front door (Tauri command AND MCP tool) gets the post-job
    /// conveniences here (architect merge, phase-2 integration):
    /// * zone B's import directive: params carrying `importToTrackId`
    ///   (+ optional `importAtSamples`) land a successful job's `outputPath`
    ///   on the timeline via `import_audio_clip` (see `import.rs`);
    /// * a finished `stableAudioSfz` job auto-loads its `sfzPath` into the
    ///   sampler bank so the generated instrument is immediately
    ///   previewable (`sampler_preview_note`) and bindable
    ///   (`set_track_instrument`).
    pub fn run_sidecar_job(
        self: &Arc<Self>,
        kind: &str,
        params: serde_json::Value,
        sink: EventSink,
    ) -> Result<String, String> {
        let sink = import::wrap_sink_with_import(self, &params, sink);
        let sink = wrap_sink_with_instrument_register(&self.shared, kind, sink);
        crate::sidecars::run_generic_job(&self.jobs, kind, &params, sink)
    }

    pub fn job_status(
        &self,
        job_id: &str,
    ) -> Result<crate::sidecars::SidecarJobStatus, String> {
        self.jobs.status(job_id)
    }

    pub fn list_jobs(&self) -> Vec<crate::sidecars::SidecarJobStatus> {
        self.jobs.list()
    }
}

/// The demo-song content for [`ControlPlane::seed_demo_project`]: a 4-bar
/// A-minor arpeggio (Am–F–C–G, 16ths) and an 8th-note bass groove. Pure —
/// unit-testable without an engine.
pub fn demo_seed_clips(
    keys_track_id: &str,
    bass_track_id: &str,
    ppq: u32,
) -> (crate::midi::MidiClip, crate::midi::MidiClip) {
    use crate::midi::{MidiClip, MidiNote};
    let bar = 4 * ppq;
    let chords: [[u8; 4]; 4] = [
        [57, 60, 64, 69], // Am
        [53, 57, 60, 65], // F
        [48, 52, 55, 60], // C
        [55, 59, 62, 67], // G
    ];
    let step = ppq / 4; // 16ths
    let mut arp = Vec::new();
    for b in 0..4u32 {
        let chord = chords[(b % 4) as usize];
        for s in 0..16u32 {
            let idx = if s % 8 < 4 { (s % 4) as usize } else { 3 - (s % 4) as usize };
            let octave = if s % 8 < 4 { 0 } else { 12 };
            arp.push(MidiNote {
                tick: b * bar + s * step,
                length_ticks: step * 9 / 10,
                key: chord[idx] + octave,
                velocity: if s % 4 == 0 { 110 } else { 84 },
                channel: 0,
            });
        }
    }
    let roots: [u8; 4] = [33, 29, 36, 31]; // A1 F1 C2 G1
    let eighth = ppq / 2;
    let mut groove = Vec::new();
    for b in 0..4u32 {
        for s in 0..8u32 {
            if s == 3 || s == 6 {
                continue; // breathing room
            }
            groove.push(MidiNote {
                tick: b * bar + s * eighth,
                length_ticks: eighth * 8 / 10,
                key: roots[(b % 4) as usize] + if s % 4 == 2 { 12 } else { 0 },
                velocity: if s == 0 { 118 } else { 92 },
                channel: 0,
            });
        }
    }
    let clip = |track_id: &str, name: &str, notes: Vec<MidiNote>| MidiClip {
        id: uuid::Uuid::new_v4().to_string(),
        track_id: track_id.to_string(),
        name: name.to_string(),
        timeline_start_ticks: 0,
        length_ticks: 4 * bar as u64,
        notes,
    };
    (
        clip(keys_track_id, "demo arp", arp),
        clip(bass_track_id, "demo bass", groove),
    )
}

/// Wrap a job event sink so a finished `stableAudioSfz` job's `sfzPath` is
/// loaded + compiled into the app-wide sampler bank (zone D item c, via the
/// same post-job mechanism as zone B's import directive). The load runs on
/// its own thread (sample decode must not stall the job supervisor); the
/// outcome is reported as a follow-up `log` event — the job itself already
/// succeeded.
fn wrap_sink_with_instrument_register(
    shared: &Arc<SharedRt>,
    kind: &str,
    inner: EventSink,
) -> EventSink {
    if kind != "stableAudioSfz" {
        return inner;
    }
    let shared = Arc::clone(shared);
    Arc::new(move |ev: crate::sidecars::SidecarEvent| {
        if let crate::sidecars::SidecarEvent::Done { job_id, result } = &ev {
            if let Some(sfz) = result.get("sfzPath").and_then(|p| p.as_str()) {
                let name = result.get("name").and_then(|n| n.as_str()).map(str::to_string);
                let rate = shared.sample_rate.load(Relaxed);
                let path = std::path::PathBuf::from(sfz);
                let job_id = job_id.clone();
                let inner2 = Arc::clone(&inner);
                inner(ev.clone()); // deliver `done` first; registration is extra
                std::thread::spawn(move || {
                    let line = match crate::audio::sampler_engine::load_into_registered_bank(
                        &path, name, rate,
                    ) {
                        Ok(info) => format!(
                            "auto-registered instrument \"{}\" ({}, {} regions, keys {}..={})",
                            info.name, info.id, info.region_count, info.key_low, info.key_high
                        ),
                        Err(e) => format!("instrument auto-register failed: {e}"),
                    };
                    log::info!("sampler: {line}");
                    inner2(crate::sidecars::SidecarEvent::Log { job_id, line });
                });
                return;
            }
        }
        inner(ev)
    })
}

// ---------------------------------------------------------------------------
// Commands (names frozen, registered in lib.rs)
// ---------------------------------------------------------------------------

/// Full project/engine snapshot in one invoke (cold-start / MCP parity).
#[tauri::command]
pub fn get_project_state(control: State<'_, Arc<ControlPlane>>) -> Result<ProjectSnapshot, String> {
    Ok(control.project_state())
}

/// Batched mixer mutation: `{ changes: [{trackId, gainDb?, pan?, muted?,
/// soloed?, armed?}, ...] }` applied atomically. New callers use this; the
/// frozen `set_track_*` commands remain as single-change wrappers.
#[tauri::command]
pub fn set_track_mix(
    changes: Vec<TrackMixChange>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Vec<TrackState>, String> {
    control.set_track_mix(changes)
}

/// Import an audio file as a clip (STUB until zone B lands).
#[tauri::command]
pub fn import_audio_clip(
    request: ImportClipRequest,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Clip, String> {
    control.import_audio_clip(request)
}

/// Seed an empty session with the demo song (see
/// [`ControlPlane::seed_demo_project`]). Returns the refreshed snapshot.
#[tauri::command]
pub fn seed_demo_project(
    control: State<'_, Arc<ControlPlane>>,
) -> Result<ProjectSnapshot, String> {
    control.seed_demo_project()
}

// ---------------------------------------------------------------------------
// Tests (tauri-free)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecars::SidecarEvent;
    use std::time::{Duration, Instant};

    /// Loop region through the REAL transport path (phase-3 architect round):
    /// `TransportAction::SetLoop` validates the region, mirrors it into the
    /// store (so `project::from_store` persists it), reports it in the
    /// snapshot, and the playing transport WRAPS inside the region instead
    /// of running past its end. Headless-safe (works with or without an
    /// audio device — both advance paths honor `SharedRt::loop_spec`).
    #[test]
    fn set_loop_validates_persists_and_wraps_playback() {
        struct NullEvents;
        impl crate::audio::engine::EventSink for NullEvents {
            fn emit(&self, _e: &str, _p: serde_json::Value) {}
        }
        let shared = Arc::new(SharedRt::default());
        let params = Arc::new(ParamTable::default());
        let store = Arc::new(Mutex::new(Store::default()));
        let engine = crate::audio::engine::start(
            shared.clone(),
            params.clone(),
            store.clone(),
            Box::new(NullEvents),
        );
        let cp = ControlPlane::new(
            store.clone(),
            shared.clone(),
            params,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
            Arc::new(Mutex::new(MidiStore::default())),
            Box::new(|_, _| {}),
        );
        std::thread::sleep(Duration::from_millis(100)); // let the stream open (or fail)
        let rate = shared.sample_rate.load(Relaxed) as u64;

        // Empty regions are rejected when enabling.
        assert!(cp
            .transport(TransportAction::SetLoop {
                enabled: true,
                start_samples: 100,
                end_samples: 100
            })
            .is_err());

        // A ~120 ms region at the engine rate: several wraps in 600 ms.
        let (start, end) = (rate / 2, rate / 2 + rate / 8);
        let snap = cp
            .transport(TransportAction::SetLoop {
                enabled: true,
                start_samples: start,
                end_samples: end,
            })
            .unwrap();
        assert!(snap.loop_enabled);
        assert_eq!((snap.loop_start_samples, snap.loop_end_samples), (start, end));

        // The store mirror makes the region persistent: from_store (the
        // project.json serializer) carries it.
        {
            let mut s = store.lock();
            s.project_dir = Some(std::env::temp_dir().join("aura-loop-test.aura"));
            s.project_name = Some("LoopTest".into());
        }
        let p = crate::audio::project::from_store(&store.lock(), 0, rate as u32).unwrap();
        let t = p.transport.expect("transport block");
        assert!(t.loop_enabled);
        assert_eq!((t.loop_start_samples, t.loop_end_samples), (start, end));

        // Play from inside the region: the playhead must WRAP, never escape.
        cp.transport(TransportAction::Seek { position_samples: start + 100 }).unwrap();
        cp.transport(TransportAction::Play).unwrap();
        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            let pos = shared.position.load(Relaxed);
            assert!(
                pos >= start && pos < end + rate / 10,
                "playhead escaped the loop region: {pos} not in [{start}, {end})"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        cp.transport(TransportAction::Stop).unwrap();
        let pos = shared.position.load(Relaxed);
        assert!(
            pos >= start && pos < end,
            "stopped inside the region after wrapping ({pos} in [{start}, {end}))"
        );
        // Without the loop, 600 ms of playback would have moved ~0.6*rate
        // past the seek point — far beyond the 0.125*rate region span.
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

    /// Regression (user report "press play → no sound"): the seeded demo
    /// song must render NON-ZERO audio through the midi graph path, headless
    /// (no device, no tauri). A fresh session that seeds + plays is audible.
    #[test]
    fn seeded_demo_clips_render_nonzero_audio() {
        use crate::audio::types::{Store, TrackState};
        let mut store = Store::default();
        for (id, name) in [("keys", "Demo Keys"), ("bass", "Demo Bass")] {
            store.alloc_slot(id);
            store.tracks.push(TrackState {
                id: id.into(),
                name: name.into(),
                kind: "midi".into(),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                armed: false,
                color: "#7c9cff".into(),
                instrument_id: None,
            });
        }
        let (arp, groove) = demo_seed_clips("keys", "bass", 960);
        assert!(!arp.notes.is_empty() && !groove.notes.is_empty());
        for n in arp.notes.iter().chain(groove.notes.iter()) {
            n.validate().unwrap();
        }
        let midi = crate::midi::MidiStore {
            ppq: 960,
            tempo_events: vec![crate::midi::TempoEvent { tick: 0, bpm: 120.0 }],
            clips: vec![arp, groove],
            loaded_dir: None,
        };
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        crate::midi::playback::append_from(&midi, &store, 48_000, None, &mut nodes, &mut out);
        assert_eq!(out.len(), 2, "both seeded tracks reach the RT graph");
        // Live-node model (phase 3): render the graph headlessly through the
        // real RT path and assert the seeded music is audible.
        let mut g = crate::audio::rt::RtGraph::new(out);
        let params = crate::audio::rt::ParamTable::default();
        let mut buf = vec![0.0f32; 48_000 * 2];
        let mut pos = 0u64;
        for chunk in buf.chunks_mut(512 * 2) {
            crate::audio::mixer::render(
                &mut g,
                &params,
                pos,
                &crate::audio::transport::LoopSpec::OFF,
                chunk,
                2,
                48_000,
                false,
            );
            pos += (chunk.len() / 2) as u64;
        }
        let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "seeded tracks render audibly live (peak {peak})");
    }

    /// Item-4 integration: a finished `stableAudioSfz` job whose result
    /// carries an `sfzPath` lands the instrument in the registered sampler
    /// bank and reports a follow-up log line; other kinds pass through
    /// untouched.
    #[test]
    fn stable_audio_sfz_done_auto_registers_instrument_in_bank() {
        // Minimal on-disk instrument (hand-written; no python needed).
        let dir = std::env::temp_dir().join(format!(
            "aura-autoreg-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(dir.join("s.wav"), spec).unwrap();
        for i in 0..4800 {
            let v = ((i as f32 * 0.05).sin() * 12_000.0) as i16;
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        let sfz = dir.join("gen.sfz");
        std::fs::write(&sfz, "<region> sample=s.wav pitch_keycenter=60\n").unwrap();

        // Registered app-wide bank (first registration wins process-wide;
        // engine tests never register one, so this is isolated enough).
        let bank = Arc::new(Mutex::new(crate::audio::sampler::SamplerBank::default()));
        crate::audio::sampler::register_bank(bank.clone());
        let bank = crate::audio::sampler::registered_bank().unwrap().clone();
        let baseline = bank.lock().list().len();

        let shared = Arc::new(SharedRt::default());
        let events: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev2 = Arc::clone(&events);
        let inner: EventSink = Arc::new(move |ev| ev2.lock().push(ev));

        // Non-matching kind: the sink is returned unchanged (same Arc).
        let passthrough =
            wrap_sink_with_instrument_register(&shared, "aceStepGenerate", Arc::clone(&inner));
        assert!(Arc::ptr_eq(&passthrough, &inner), "non-sfz kinds are untouched");

        let sink = wrap_sink_with_instrument_register(&shared, "stableAudioSfz", inner);
        sink(SidecarEvent::Done {
            job_id: "job-1".into(),
            result: serde_json::json!({
                "kind": "stableAudioSfz",
                "sfzPath": sfz.to_string_lossy(),
                "name": "AutoReg Test",
            }),
        });

        // The load runs on its own thread; poll for the registration.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if bank.lock().list().len() > baseline {
                break;
            }
            assert!(Instant::now() < deadline, "instrument was not registered in time");
            std::thread::sleep(Duration::from_millis(10));
        }
        let listed = bank.lock().list();
        let info = listed.iter().find(|i| i.name == "AutoReg Test").expect("named instrument");
        assert_eq!(info.region_count, 1);

        // Done was delivered first, then the follow-up log line.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let evs = events.lock();
            if evs.len() >= 2 {
                assert!(matches!(evs[0], SidecarEvent::Done { .. }));
                match &evs[1] {
                    SidecarEvent::Log { line, .. } => {
                        assert!(line.contains("auto-registered instrument"), "{line}")
                    }
                    other => panic!("expected log follow-up, got {other:?}"),
                }
                break;
            }
            drop(evs);
            assert!(Instant::now() < deadline, "log follow-up never arrived");
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
