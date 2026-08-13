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
pub mod hum;
pub mod export;
pub mod ops;
pub mod loopjam;

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
    /// Stop the transport when the playhead reaches the end of the material.
    /// Pure policy — the engine detects the boundary either way.
    SetStopAtEnd { enabled: bool },
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
        // An explicit transport command supersedes any parking position the
        // engine still owes (`SharedRt::park`) — otherwise a stop that is
        // immediately followed by a seek would be undone a buffer later.
        if matches!(
            action,
            TransportAction::Play | TransportAction::Stop | TransportAction::Seek { .. }
        ) {
            self.shared.park.store(crate::audio::rt::NO_PARK, Relaxed);
        }
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
            TransportAction::SetStopAtEnd { enabled } => {
                self.shared.stop_at_end.store(enabled, Relaxed);
                self.store.lock().transport.stop_at_end = enabled;
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
    ///
    /// Emits `project://changed` on success. The Tauri command path doesn't
    /// need it (the frontend patches its store optimistically and gets the
    /// updated `TrackState` back from the invoke), but non-webview front
    /// doors — the MCP tools above all — have no return channel into the UI:
    /// without this event an agent's gain/pan change is applied and audible
    /// yet invisible in the mixer.
    pub fn set_track_mix(
        &self,
        changes: Vec<TrackMixChange>,
    ) -> Result<Vec<TrackState>, String> {
        let updated = {
            let mut s = self.store.lock();
            ops::apply_track_mix(&mut s, &self.params, &changes)?
        };
        self.emit_project_changed();
        Ok(updated)
    }

    /// Emit `project://changed` with the full project payload (the same
    /// shape `project::from_store` serializes, minus its requirement of an
    /// open project dir — mix changes are legal in an unsaved session).
    fn emit_project_changed(&self) {
        let payload = {
            let s = self.store.lock();
            let sample_rate = self.shared.sample_rate.load(Relaxed);
            let mut transport = s.transport.clone();
            transport.position_samples = self.shared.position.load(Relaxed);
            transport.sample_rate = sample_rate;
            Project {
                schema_version: 1,
                name: s.project_name.clone().unwrap_or_else(|| "Untitled".into()),
                path: s.project_dir.as_ref().map(|p| p.to_string_lossy().into_owned()),
                created_at: s.created_at.clone(),
                modified_at: None,
                sample_rate,
                tempo_bpm: transport.tempo_bpm,
                time_signature: Some((4, 4)),
                tracks: s.tracks.clone(),
                clips: s.clips.clone(),
                transport: Some(transport),
            }
        };
        (self.emit)(
            "project://changed",
            serde_json::to_value(&payload).unwrap_or_default(),
        );
    }

    /// New Project = blank slate: the previous session's tracks, clips, midi
    /// state, plugin/automation registries, and transport are all reset, and
    /// the freshly created `.aura` dir becomes the open project. (Until
    /// 2026-08 this kept the session's tracks; the UI "New Project" flow
    /// wants a true blank, and materializing an unsaved session is
    /// [`Self::save_project_as`]'s job.)
    pub fn create_project(&self, parent_dir: &str, name: &str) -> Result<Project, String> {
        let rate = self.shared.sample_rate.load(Relaxed);
        let tempo = self.store.lock().transport.tempo_bpm;
        let (project, dir) =
            project::create(std::path::Path::new(parent_dir), name, rate, tempo)?;
        {
            let mut s = self.store.lock();
            for t in s.tracks.clone() {
                s.free_slot(&t.id);
            }
            s.tracks.clear();
            s.clips.clear();
            s.project_dir = Some(dir.clone());
            s.project_name = Some(name.to_string());
            s.created_at = project.created_at.clone();
            s.transport.state = "stopped".into();
            s.transport.position_samples = 0;
            s.transport.loop_enabled = false;
            s.transport.loop_start_samples = 0;
            s.transport.loop_end_samples = 0;
        }
        self.params.any_solo.store(false, Relaxed);
        self.shared.playing.store(false, Relaxed);
        self.shared.position.store(0, Relaxed);
        self.shared.loop_enabled.store(false, Relaxed);
        {
            // Blank midi state bound to the new dir; `sync_midi_store` then
            // sees loaded_dir == dir and leaves it alone (self.midi is the
            // same Arc as MidiState.inner / the playback-registered store).
            let mut m = self.midi.lock();
            let d0 = crate::midi::persist::v1_migration_defaults(tempo);
            m.ppq = d0.ppq;
            m.tempo_events = d0.tempo_events;
            m.clips = d0.clips;
            m.loaded_dir = Some(dir.clone());
        }
        // App-global plugin/automation registries adopt the (empty) project.
        crate::plugins::state::adopt_open_project(&dir);
        crate::plugins::automation::adopt_open_project(&dir);
        self.engine.send(ControlMsg::Rebuild);
        (self.emit)(
            "project://changed",
            serde_json::to_value(&project).unwrap_or_default(),
        );
        Ok(project)
    }

    /// First save of a session that never had a project: create the `.aura`
    /// dir and persist the CURRENT in-memory content (tracks, clips, midi)
    /// into it. Refuses when a project is already open — that is plain
    /// `save_project` territory, not a fork.
    pub fn save_project_as(&self, parent_dir: &str, name: &str) -> Result<Project, String> {
        if self.store.lock().project_dir.is_some() {
            return Err("a project is already open; use save_project".into());
        }
        let rate = self.shared.sample_rate.load(Relaxed);
        let tempo = self.store.lock().transport.tempo_bpm;
        let (created, dir) = project::create(std::path::Path::new(parent_dir), name, rate, tempo)?;
        let project = {
            let mut s = self.store.lock();
            s.project_dir = Some(dir.clone());
            s.project_name = Some(name.to_string());
            s.created_at = created.created_at.clone();
            let position = self.shared.position.load(Relaxed);
            project::from_store(&s, position, rate)?
        };
        project::save(&dir, &project)?;
        {
            // Adopt the in-memory midi state into the new project and write
            // it now (normally that only happens on the next midi mutation).
            let mut m = self.midi.lock();
            m.loaded_dir = Some(dir.clone());
            if let Err(e) = crate::midi::persist::save_into_project(&dir, &m) {
                log::warn!("save_project_as: persisting midi failed: {e}");
            }
        }
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

    /// Seed a content-less session with a small demo song (demo v2: pad
    /// chords + plucked lead + bass over Am–F–C–G), so the very first press
    /// of PLAY makes sound. When ZynAddSubFX (LV2) and its stock banks are
    /// installed, the three tracks are bound to Zyn instances loaded with
    /// bank patches (Pads/Analog Pad 1, Plucked/Plucked 1, Bass/Analogue
    /// Bass); on machines without Zyn the tracks keep the built-in PolySynth
    /// — the demo NEVER fails for lack of plugins. This is the control-plane
    /// end of the UI's "load demo song" affordance; it refuses to run when
    /// the session already has audible content, and needs no open project
    /// (the clips live in memory exactly like any unsaved edit). With a
    /// project open, midi, track bindings AND plugin state are persisted so
    /// the demo survives save/open.
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

        // Zyn upgrade path: build the three patched instances BEFORE the
        // tracks so a failure leaves no half-bound state (None = PolySynth).
        let zyn = try_seed_zyn_demo_instruments();

        let (pad, lead, bass) = {
            let mut s = self.store.lock();
            let pad = ops::add_track(
                &mut s,
                &self.params,
                Some("Demo Pad".into()),
                Some("midi".into()),
            )?;
            let lead = ops::add_track(
                &mut s,
                &self.params,
                Some("Demo Lead".into()),
                Some("midi".into()),
            )?;
            let bass = ops::add_track(
                &mut s,
                &self.params,
                Some("Demo Bass".into()),
                Some("midi".into()),
            )?;
            if let Some(ids) = &zyn {
                for (track_id, instance_id) in
                    [(&pad.id, &ids[0]), (&lead.id, &ids[1]), (&bass.id, &ids[2])]
                {
                    if let Some(t) = s.tracks.iter_mut().find(|t| &t.id == track_id) {
                        t.instrument_id = Some(format!("plugin:{instance_id}"));
                    }
                }
            }
            (pad, lead, bass)
        };
        {
            let mut m = self.midi.lock();
            let ppq = m.ppq;
            let (pad_clip, lead_clip, bass_clip) =
                demo_seed_clips_v2(&pad.id, &lead.id, &bass.id, ppq);
            m.clips.push(pad_clip);
            m.clips.push(lead_clip);
            m.clips.push(bass_clip);
            if let Some(d) = &dir {
                if let Err(e) = crate::midi::persist::save_into_project(d, &m) {
                    log::warn!("seed demo: persisting midi failed: {e}");
                }
            }
        }
        // Persist the track bindings (project.json tracks) and the plugin
        // instances + state blobs, so a save/open cycle replays the same
        // demo through the same patches (zone P4 restore path).
        if let (Some(d), Some(_)) = (&dir, &zyn) {
            let snapshot = {
                let s = self.store.lock();
                project::from_store(
                    &s,
                    self.shared.position.load(Relaxed),
                    self.shared.sample_rate.load(Relaxed),
                )
            };
            match snapshot {
                Ok(p) => {
                    if let Err(e) = project::save(d, &p) {
                        log::warn!("seed demo: persisting tracks failed: {e}");
                    }
                }
                Err(e) => log::warn!("seed demo: project snapshot failed: {e}"),
            }
            if let Some(reg) = crate::plugins::registered_registry() {
                crate::plugins::state::persist_after_mutation(&self.store, reg, true);
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

    /// EventSink that fans job events out as the standard
    /// `sidecar://progress|done|error` app events (log lines stay off the
    /// app-event bus, same as `sidecars::make_sink`). This is the sink for
    /// front doors WITHOUT a per-job Tauri channel — the MCP `run_sidecar_job`
    /// tool passes it so agent-launched jobs light up the UI JOBS indicator
    /// exactly like UI-launched ones.
    pub fn app_event_sink(self: &Arc<Self>) -> EventSink {
        let cp = Arc::clone(self);
        Arc::new(move |ev: crate::sidecars::SidecarEvent| {
            use crate::sidecars::SidecarEvent;
            let event_name = match &ev {
                SidecarEvent::Progress { .. } => "sidecar://progress",
                SidecarEvent::Done { .. } => "sidecar://done",
                SidecarEvent::Error { .. } => "sidecar://error",
                SidecarEvent::Log { .. } => return,
            };
            match serde_json::to_value(&ev) {
                Ok(v) => (cp.emit)(event_name, v),
                Err(e) => log::warn!("app_event_sink: serialize {event_name}: {e}"),
            }
        })
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

/// Try to build the three Zyn demo instances (pad / lead / bass), each
/// loaded with a stock bank patch. Returns their instance ids, or None when
/// anything on the Zyn path is unavailable (plugin not installed, banks
/// missing, no registered plugin registry) — the caller then keeps the
/// PolySynth fallback, so a machine without plugins is never broken.
/// Partial failures roll back (no orphan instances).
fn try_seed_zyn_demo_instruments() -> Option<[String; 3]> {
    use crate::plugins::{self, patches};
    let registry = plugins::registered_registry()?;
    // Patches chosen to sit well together: soft pad chords, a plucked lead
    // that cuts through, and a round analog-style bass.
    let wanted = [
        patches::find_zyn_patch("Pads", "analog pad")?,
        patches::find_zyn_patch("Plucked", "plucked")?,
        patches::find_zyn_patch("Bass", "analogue bass")
            .or_else(|| patches::find_zyn_patch("Bass", "bass"))?,
    ];
    let uid = plugins::descriptor::lv2_uid(patches::ZYN_URI);
    {
        let mut reg = registry.lock();
        if reg.scanned.is_none() {
            // LV2-only scan: metadata via the lilv world, no plugin code —
            // safe to run inline (the CLAP subprocess scan stays on-demand).
            reg.scanned = Some(plugins::scan::scan_lv2());
        }
        if !reg.scanned.as_ref().is_some_and(|s| s.iter().any(|d| d.uid == uid)) {
            return None;
        }
    }
    let mut ids: Vec<String> = Vec::with_capacity(3);
    for patch in &wanted {
        match plugins::instantiate_and_activate(registry, &uid) {
            Ok(info) => {
                if let Err(e) =
                    patches::load_zyn_patch(&info.id, std::path::Path::new(&patch.path))
                {
                    // Zyn's default patch still sounds; keep the instance.
                    log::warn!(
                        "seed demo: loading patch {}/{} failed ({e}); using Zyn default",
                        patch.bank,
                        patch.name
                    );
                }
                ids.push(info.id);
            }
            Err(e) => {
                log::warn!("seed demo: Zyn instantiation failed ({e}); PolySynth fallback");
                for id in &ids {
                    let _ = registry.lock().remove(id);
                    if let Some(host) = plugins::lv2_host::try_global() {
                        host.unregister_instance(id);
                    }
                }
                return None;
            }
        }
    }
    log::info!(
        "seed demo: Zyn instances ready ({})",
        wanted.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
    );
    ids.try_into().ok()
}

/// The ORIGINAL two-track demo content (v1: 16th-note arp + bass groove),
/// kept byte-for-byte under the old name/signature for callers that pinned
/// expectations to it (offline render / export tests assert its exact song
/// length). The seeded demo song itself uses [`demo_seed_clips_v2`]'s
/// three-track arrangement.
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

/// The demo-song content for [`ControlPlane::seed_demo_project`] (v2): four
/// bars of Am–F–C–G as sustained pad chords, a plucked 8th-note lead
/// melody, and an 8th-note bass groove. Pure — unit-testable without an
/// engine; the same music renders through Zyn patches or PolySynth.
pub fn demo_seed_clips_v2(
    pad_track_id: &str,
    lead_track_id: &str,
    bass_track_id: &str,
    ppq: u32,
) -> (crate::midi::MidiClip, crate::midi::MidiClip, crate::midi::MidiClip) {
    use crate::midi::{MidiClip, MidiNote};
    let bar = 4 * ppq;
    let eighth = ppq / 2;

    // Pad: one voiced chord per bar, smooth voice leading, held nearly the
    // whole bar so pad patches breathe.
    let chords: [&[u8]; 4] = [
        &[57, 60, 64], // Am
        &[53, 57, 60], // F
        &[55, 60, 64], // C (2nd inversion keeps the top line close)
        &[55, 59, 62], // G
    ];
    let mut pad = Vec::new();
    for b in 0..4u32 {
        for &key in chords[(b % 4) as usize] {
            pad.push(MidiNote {
                tick: b * bar,
                length_ticks: bar * 95 / 100,
                key,
                velocity: 72,
                channel: 0,
            });
        }
    }

    // Lead: an 8th-note phrase per bar (A-minor melody over the changes) —
    // short notes so plucked patches speak naturally.
    let phrases: [[u8; 8]; 4] = [
        [69, 72, 76, 72, 69, 76, 74, 72], // over Am
        [67, 69, 72, 69, 65, 69, 72, 74], // over F
        [76, 74, 72, 67, 64, 67, 72, 74], // over C
        [71, 74, 79, 74, 71, 67, 69, 71], // over G
    ];
    let mut lead = Vec::new();
    for b in 0..4u32 {
        for (s, &key) in phrases[(b % 4) as usize].iter().enumerate() {
            lead.push(MidiNote {
                tick: b * bar + s as u32 * eighth,
                length_ticks: eighth * 8 / 10,
                key,
                velocity: if s % 2 == 0 { 102 } else { 84 },
                channel: 0,
            });
        }
    }

    // Bass: the v1 groove (roots with octave pushes, rests for air).
    let roots: [u8; 4] = [33, 29, 36, 31]; // A1 F1 C2 G1
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
        clip(pad_track_id, "demo pad", pad),
        clip(lead_track_id, "demo lead", lead),
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
///
/// Async on purpose: sync commands run on the MAIN thread, and on Linux the
/// WebKitGTK webview shares the GTK main loop — a seconds-long build (Zyn
/// instantiation) would freeze the UI so the button's busy state never
/// paints. `spawn_blocking` keeps the heavy work off the async runtime too.
#[tauri::command]
pub async fn seed_demo_project(
    control: State<'_, Arc<ControlPlane>>,
) -> Result<ProjectSnapshot, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cp.seed_demo_project())
        .await
        .map_err(|e| e.to_string())?
}

// ---------------------------------------------------------------------------
// Tests (tauri-free)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecars::SidecarEvent;
    use std::time::{Duration, Instant};

    /// Recorded app events + a ControlPlane whose emitter records into them
    /// (real engine, headless-safe — the shared harness for the event-parity
    /// tests below).
    type RecordedEvents = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    fn recording_control_plane() -> (Arc<ControlPlane>, RecordedEvents, EngineHandle) {
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
        let events: RecordedEvents = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let cp = Arc::new(ControlPlane::new(
            store,
            shared,
            params,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Arc::new(Mutex::new(MidiStore::default())),
            Box::new(move |e, p| sink.lock().push((e.to_string(), p))),
        ));
        (cp, events, engine)
    }

    /// MCP-parity regression (found filming the MCP demo): a control-plane
    /// `set_track_mix` — the path MCP tools drive, with NO webview return
    /// channel — must emit `project://changed` carrying the updated track
    /// state, or the UI mixer never reflects an agent's gain/pan change.
    /// A failing batch must emit nothing.
    #[test]
    fn set_track_mix_emits_project_changed_with_updated_tracks() {
        let (cp, events, engine) = recording_control_plane();
        let track = cp.add_track(Some("Agent Mix".into()), None).unwrap();
        events.lock().clear();

        cp.set_track_mix(vec![TrackMixChange {
            gain_db: Some(-6.0),
            pan: Some(0.5),
            muted: Some(true),
            ..TrackMixChange::new(&track.id)
        }])
        .unwrap();

        {
            let evs = events.lock();
            let payloads: Vec<&serde_json::Value> = evs
                .iter()
                .filter(|(name, _)| name == "project://changed")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(payloads.len(), 1, "exactly one project://changed per batch");
            let t = payloads[0]["tracks"]
                .as_array()
                .expect("tracks array")
                .iter()
                .find(|t| t["id"] == track.id.as_str())
                .expect("changed track in payload")
                .clone();
            assert_eq!(t["gainDb"], -6.0);
            assert_eq!(t["pan"], 0.5);
            assert_eq!(t["muted"], true);
            // Works without an open project (unsaved session).
            assert_eq!(payloads[0]["name"], "Untitled");
        }

        events.lock().clear();
        assert!(cp.set_track_mix(vec![TrackMixChange::new("no-such-track")]).is_err());
        assert!(
            events.lock().iter().all(|(name, _)| name != "project://changed"),
            "failed batch must not announce a change"
        );
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

    /// MCP-parity regression (found filming the MCP demo): jobs submitted
    /// through `ControlPlane::app_event_sink` — the sink the MCP
    /// `run_sidecar_job` tool now passes — must fan progress/done out as the
    /// standard `sidecar://*` app events so agent-launched jobs light up the
    /// UI JOBS indicator. Driven by a fake sidecar (shell script speaking
    /// the NDJSON protocol); log lines stay off the app-event bus.
    #[tokio::test]
    async fn app_event_sink_fans_fake_sidecar_job_out_as_app_events() {
        let (cp, events, engine) = recording_control_plane();
        let dir = std::env::temp_dir().join(format!(
            "aura-mcp-sink-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake_sidecar.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             echo '{\"type\":\"progress\",\"progress\":0.5,\"stage\":\"halfway\"}'\n\
             echo 'log noise stays off the app-event bus'\n\
             echo '{\"type\":\"done\",\"result\":{\"kind\":\"fake\",\"outputPath\":\"/tmp/none.wav\"}}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let spec = crate::sidecars::jobs::JobSpec {
            kind: crate::sidecars::JobKind::StemSplit,
            program: std::path::PathBuf::from("/bin/sh"),
            args: vec![script.to_string_lossy().into_owned()],
            grace: Duration::from_millis(500),
            env: Vec::new(),
        };
        let job_id = cp.jobs.submit(spec, cp.app_event_sink());

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let st = cp.job_status(&job_id).unwrap();
            if crate::sidecars::types::JobState::is_terminal(&st.state) {
                assert_eq!(st.state, "done", "err: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "fake sidecar did not finish: {st:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The sink is called synchronously by the supervisor before the job
        // flips terminal, so all events are recorded by now.
        let evs = events.lock();
        let sidecar: Vec<&(String, serde_json::Value)> =
            evs.iter().filter(|(n, _)| n.starts_with("sidecar://")).collect();
        assert!(
            sidecar.iter().any(|(n, p)| n == "sidecar://progress"
                && p["jobId"] == job_id.as_str()
                && p["stage"] == "halfway"),
            "progress reached the app-event bus: {evs:?}"
        );
        let (last_name, last_payload) = sidecar.last().expect("sidecar events emitted");
        assert_eq!(last_name, "sidecar://done");
        assert_eq!(last_payload["jobId"], job_id.as_str());
        assert_eq!(last_payload["result"]["kind"], "fake");
        assert!(
            !evs.iter().any(|(_, p)| p
                .get("line")
                .and_then(|l| l.as_str())
                .is_some_and(|l| l.contains("log noise"))),
            "log lines must not become app events"
        );
        drop(evs);
        let _ = std::fs::remove_dir_all(&dir);
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

    /// A real engine whose app events are recorded, for the auto-stop tests.
    /// Headless-safe: both advance paths (RT callback and the wall-clock
    /// headless tick) run the same boundary detection and policy.
    fn engine_recording_events() -> (
        ControlPlane,
        Arc<SharedRt>,
        RecordedEvents,
        crate::audio::engine::EngineHandle,
    ) {
        struct Recorder(RecordedEvents);
        impl crate::audio::engine::EventSink for Recorder {
            fn emit(&self, e: &str, p: serde_json::Value) {
                self.0.lock().push((e.to_string(), p));
            }
        }
        let events: RecordedEvents = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(SharedRt::default());
        let params = Arc::new(ParamTable::default());
        let store = Arc::new(Mutex::new(Store::default()));
        let engine = crate::audio::engine::start(
            shared.clone(),
            params.clone(),
            store.clone(),
            Box::new(Recorder(Arc::clone(&events))),
        );
        let cp = ControlPlane::new(
            store,
            shared.clone(),
            params,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
            Arc::new(Mutex::new(MidiStore::default())),
            Box::new(|_, _| {}),
        );
        // Barrier: once the engine answers, its startup open_output is done.
        engine
            .request(|reply| crate::audio::engine::ControlMsg::SelectInput {
                device_id: None,
                reply,
            })
            .expect("engine control thread responds");
        (cp, shared, events, engine)
    }

    /// Wait until `f` holds, or fail after `ms`.
    fn wait_until(ms: u64, what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Auto-stop through the REAL engine: the playhead crossing the end of
    /// the material stops the transport and parks EXACTLY on the boundary
    /// (never a buffer past it — see `SharedRt::park`), and the stop is
    /// announced on `transport://state` so every front door sees it.
    #[test]
    fn auto_stop_parks_exactly_at_song_end_and_announces_it() {
        let (cp, shared, events, engine) = engine_recording_events();
        let rate = shared.sample_rate.load(Relaxed) as u64;
        let end = rate / 4; // 250 ms of "material"
        shared.song_end.store(end, Relaxed);

        cp.transport(TransportAction::Seek { position_samples: end - rate / 20 }).unwrap();
        events.lock().clear();
        cp.transport(TransportAction::Play).unwrap();

        wait_until(4000, "the transport to stop itself", || {
            !shared.playing.load(Relaxed)
        });
        // Let any callback still in flight write its position, so this
        // asserts the SETTLED playhead rather than a lucky instant.
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            shared.position.load(Relaxed),
            end,
            "parks on the boundary sample, not a buffer past it"
        );

        let evs = events.lock();
        let stopped = evs
            .iter()
            .filter(|(n, _)| n == "transport://state")
            .find(|(_, p)| p["state"] == "stopped")
            .map(|(_, p)| p.clone())
            .expect("the engine announced the auto-stop");
        assert_eq!(stopped["positionSamples"], end);
        assert_eq!(stopped["songEndSamples"], end);
        assert_eq!(stopped["stopAtEnd"], true);
        drop(evs);
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

    /// The policy switch is the whole point of keeping the decision out of
    /// the audio callback: with it off, the same crossing changes nothing.
    #[test]
    fn stop_at_end_off_runs_past_the_end() {
        let (cp, shared, _events, engine) = engine_recording_events();
        let rate = shared.sample_rate.load(Relaxed) as u64;
        let end = rate / 20; // 50 ms
        shared.song_end.store(end, Relaxed);
        cp.transport(TransportAction::SetStopAtEnd { enabled: false }).unwrap();

        cp.transport(TransportAction::Seek { position_samples: 0 }).unwrap();
        cp.transport(TransportAction::Play).unwrap();
        wait_until(4000, "the playhead to run past the end", || {
            shared.position.load(Relaxed) > end * 2
        });
        assert!(shared.playing.load(Relaxed), "still rolling");
        cp.transport(TransportAction::Stop).unwrap();
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

    /// An active loop owns the playhead: it can never reach the end, and the
    /// two features must not fight over the transport.
    #[test]
    fn an_active_loop_suppresses_auto_stop() {
        let (cp, shared, _events, engine) = engine_recording_events();
        let rate = shared.sample_rate.load(Relaxed) as u64;
        // Loop entirely before the end of the material.
        let (start, lend) = (rate / 100, rate / 25);
        shared.song_end.store(rate / 4, Relaxed);
        cp.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: start,
            end_samples: lend,
        })
        .unwrap();
        cp.transport(TransportAction::Seek { position_samples: start }).unwrap();
        cp.transport(TransportAction::Play).unwrap();

        // Several loop spans' worth of wall time; the transport must survive.
        std::thread::sleep(Duration::from_millis(400));
        assert!(shared.playing.load(Relaxed), "loop playback was cut short");
        let pos = shared.position.load(Relaxed);
        assert!(pos < lend, "playhead escaped the loop region: {pos} >= {lend}");
        cp.transport(TransportAction::Stop).unwrap();
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

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
        // Round-trip barrier: once the engine thread answers, its startup
        // `open_output` has finished, so the rate below is final (a fixed
        // sleep races the stream open under load).
        engine
            .request(|reply| crate::audio::engine::ControlMsg::SelectInput {
                device_id: None,
                reply,
            })
            .expect("engine control thread responds");
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
    /// song (v2: pad + lead + bass) must render NON-ZERO audio through the
    /// midi graph path, headless (no device, no tauri, no plugins — the
    /// PolySynth fallback). A fresh session that seeds + plays is audible.
    #[test]
    fn seeded_demo_clips_render_nonzero_audio() {
        use crate::audio::types::{Store, TrackState};
        let mut store = Store::default();
        for (id, name) in [("pad", "Demo Pad"), ("lead", "Demo Lead"), ("bass", "Demo Bass")] {
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
        let (pad, lead, groove) = demo_seed_clips_v2("pad", "lead", "bass", 960);
        assert!(!pad.notes.is_empty() && !lead.notes.is_empty() && !groove.notes.is_empty());
        for n in pad.notes.iter().chain(lead.notes.iter()).chain(groove.notes.iter()) {
            n.validate().unwrap();
        }
        let midi = crate::midi::MidiStore {
            ppq: 960,
            tempo_events: vec![crate::midi::TempoEvent { tick: 0, bpm: 120.0 }],
            clips: vec![pad, lead, groove],
            loaded_dir: None,
        };
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        crate::midi::playback::append_from(&midi, &store, 48_000, None, &mut nodes, &mut out);
        assert_eq!(out.len(), 3, "all three seeded tracks reach the RT graph");
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

    /// Demo v2's Zyn upgrade path (gated on zynaddsubfx-lv2 + banks): the
    /// seeder's instance builder yields three ACTIVE patched Zyn instances,
    /// and the demo arrangement bound to them renders non-silent audio
    /// through the real graph path. Machines without Zyn skip (the
    /// PolySynth fallback is covered by the test above).
    #[test]
    fn seeded_demo_zyn_instruments_bind_and_render() {
        use crate::audio::types::{Store, TrackState};
        // The seeder resolves instances through the registered app-global
        // registry (register-once semantics shared across the test process).
        crate::plugins::register_registry(Arc::new(Mutex::new(
            crate::plugins::PluginRegistry::default(),
        )));
        let registry = crate::plugins::registered_registry().unwrap().clone();
        let Some(ids) = try_seed_zyn_demo_instruments() else {
            eprintln!("skipping: ZynAddSubFX or its banks not installed");
            return;
        };
        for id in &ids {
            let info = registry.lock().instances.get(id).cloned().expect("registered");
            assert_eq!(info.status, "active", "demo Zyn instance is active");
        }

        let mut store = Store::default();
        for (slot, id) in ["pad", "lead", "bass"].iter().enumerate() {
            store.alloc_slot(id);
            store.tracks.push(TrackState {
                id: (*id).into(),
                name: (*id).into(),
                kind: "midi".into(),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                armed: false,
                color: "#7c9cff".into(),
                instrument_id: Some(format!("plugin:{}", ids[slot])),
            });
        }
        let (pad, lead, groove) = demo_seed_clips_v2("pad", "lead", "bass", 960);
        let midi = crate::midi::MidiStore {
            ppq: 960,
            tempo_events: vec![crate::midi::TempoEvent { tick: 0, bpm: 120.0 }],
            clips: vec![pad, lead, groove],
            loaded_dir: None,
        };
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        crate::midi::playback::append_from(&midi, &store, 48_000, None, &mut nodes, &mut out);
        assert_eq!(out.len(), 3);
        for (track, inst) in [("pad", &ids[0]), ("lead", &ids[1]), ("bass", &ids[2])] {
            assert_eq!(
                nodes.key_of(track),
                Some(format!("plugin:{inst}@48000").as_str()),
                "track {track} resolved to its Zyn node (not the fallback)"
            );
        }
        let mut g = crate::audio::rt::RtGraph::new(out);
        let params = crate::audio::rt::ParamTable::default();
        let mut buf = vec![0.0f32; 2 * 48_000 * 2];
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
        assert!(peak > 0.01, "Zyn-bound demo renders audibly (peak {peak})");

        // Cleanup: drop the demo instances from the shared registry/host.
        for id in &ids {
            let _ = registry.lock().remove(id);
            if let Some(host) = crate::plugins::lv2_host::try_global() {
                host.unregister_instance(id);
            }
        }
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

    // ── project new / save-as semantics ──

    fn cp_tmp_parent(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aura-cp-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn dummy_audio_clip(track_id: &str) -> Clip {
        Clip {
            id: "c1".into(),
            track_id: track_id.into(),
            name: "take".into(),
            source_path: "audio/c1.wav".into(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 480,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: 480,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
        }
    }

    fn dummy_midi_clip(track_id: &str) -> crate::midi::MidiClip {
        crate::midi::MidiClip {
            id: "mc1".into(),
            track_id: track_id.into(),
            name: "riff".into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: Vec::new(),
        }
    }

    /// "New Project" is a blank slate: the previous session's tracks, clips,
    /// midi state, and playhead must all be gone, and the on-disk project.json
    /// must describe the empty project.
    #[test]
    fn create_project_resets_to_a_blank_slate() {
        let (cp, _events, _engine) = recording_control_plane();
        let t = cp.add_track(Some("Old".into()), None).unwrap();
        cp.store.lock().clips.push(dummy_audio_clip(&t.id));
        cp.midi.lock().clips.push(dummy_midi_clip(&t.id));
        cp.shared.position.store(1234, Relaxed);

        let parent = cp_tmp_parent("blank");
        let project = cp.create_project(parent.to_str().unwrap(), "Fresh").unwrap();

        assert!(project.tracks.is_empty(), "returned project is empty");
        {
            let s = cp.store.lock();
            assert!(s.tracks.is_empty(), "tracks cleared");
            assert!(s.clips.is_empty(), "clips cleared");
            assert_eq!(s.project_dir.as_deref(), Some(parent.join("Fresh.aura").as_path()));
        }
        assert!(cp.midi.lock().clips.is_empty(), "midi state reset");
        assert_eq!(cp.shared.position.load(Relaxed), 0, "playhead back at 0");
        let (loaded, _) = project::load(&parent.join("Fresh.aura")).unwrap();
        assert!(loaded.tracks.is_empty(), "blank project on disk");
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// First save of a session that never had a project: `save_project_as`
    /// creates the .aura dir and persists the CURRENT content (tracks + midi),
    /// unlike create_project which resets.
    #[test]
    fn save_project_as_materializes_an_unsaved_session() {
        let (cp, events, _engine) = recording_control_plane();
        let t = cp.add_track(Some("Keys".into()), None).unwrap();
        cp.midi.lock().clips.push(dummy_midi_clip(&t.id));
        events.lock().clear();

        let parent = cp_tmp_parent("saveas");
        let project = cp.save_project_as(parent.to_str().unwrap(), "First").unwrap();

        assert_eq!(project.tracks.len(), 1, "session tracks kept");
        {
            let s = cp.store.lock();
            assert_eq!(s.tracks.len(), 1);
            assert_eq!(s.project_dir.as_deref(), Some(parent.join("First.aura").as_path()));
        }
        let json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(parent.join("First.aura/project.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(json["tracks"][0]["name"], "Keys", "tracks on disk");
        assert_eq!(json["midiClips"][0]["id"], "mc1", "in-memory midi materialized");
        assert!(
            events.lock().iter().any(|(n, _)| n == "project://changed"),
            "project://changed emitted"
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// With a project already open, plain save_project is the right call —
    /// save_project_as must refuse instead of silently forking state.
    #[test]
    fn save_project_as_refuses_when_a_project_is_open() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("saveas-open");
        cp.create_project(parent.to_str().unwrap(), "Open").unwrap();

        let err = cp.save_project_as(parent.to_str().unwrap(), "Other").unwrap_err();
        assert!(err.contains("already open"), "clear message: {err}");
        assert!(!parent.join("Other.aura").exists(), "no dir left behind");
        let _ = std::fs::remove_dir_all(&parent);
    }
}
