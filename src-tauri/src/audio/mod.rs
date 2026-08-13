//! AURA audio engine module — OWNED BY AGENT 2.
//!
//! Layering (docs/ARCHITECTURE.md):
//!
//! * [`types`]     — IPC payload structs + control-plane `Store` (pure).
//! * [`rt`]        — atomics + immutable audio-graph types shared with the
//!                   RT callbacks (RCU snapshot swap, atomic params).
//! * [`transport`] — playhead/loop position math (pure).
//! * [`mixer`]     — gain/pan/solo-mute law + the RT render function (pure).
//! * [`meters`]    — POD meter blocks and 60 Hz aggregation (pure).
//! * [`waveform`]  — min/max LOD pyramids + AWTF binary tiles (pure + fs).
//! * [`recorder`]  — disk-writer thread (rtrb → hound WAV f32).
//! * [`engine`]    — control thread owning cpal streams and graph swaps.
//! * [`project`]   — `*.aura` folder persistence (atomic project.json).
//! * [`dsp`]       — FUTURE: `AudioProcessor`/`TimeStretcher`/`Effect` traits.
//!
//! This file is only Tauri glue: `#[tauri::command]` handlers (names frozen,
//! registered in the frozen `lib.rs`), the managed `AudioState`, and the
//! `init` hook that starts the engine control thread.

pub mod dsp;
pub mod engine;
pub mod meters;
pub mod mixer;
pub mod project;
pub mod recorder;
pub mod rt;
pub mod sampler;
pub mod sampler_engine;
pub mod sampler_preview;
pub mod sampler_voice;
pub mod transport;
pub mod types;
pub mod waveform;
pub mod offline;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, OnceLock};

use cpal::traits::{DeviceTrait, HostTrait};
use parking_lot::Mutex;
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::control::{self, ControlPlane, Session, TrackMixChange};
use crate::midi::MidiStore;
use engine::{ControlMsg, EngineHandle};
use rt::{GraphTables, ParamTable, SharedGraphTables, SharedRt};
use sampler::{InstrumentInfo, SamplerBank};

pub use types::{
    AudioDevice, Clip, MeterFrame, Project, Store, TrackMeter, TrackState, TransportState,
};

// ---------------------------------------------------------------------------
// Shared state (constructed with Default and `.manage()`d by lib.rs)
// ---------------------------------------------------------------------------

/// Engine-facing shared state. lib.rs relies only on the type name and
/// `Default` construction.
pub struct AudioState {
    session: Arc<Mutex<Session>>,
    shared: Arc<SharedRt>,
    /// Control-side view of the CURRENT graph's tables (round-2 §2.4),
    /// shared with the engine control thread and the `ControlPlane`.
    tables: SharedGraphTables,
    engine: OnceLock<EngineHandle>,
    /// Loaded SFZ instruments (phase 2, sampler zone).
    samplers: Arc<Mutex<SamplerBank>>,
    /// Lazily started preview/audition player (phase 2, sampler zone).
    preview: OnceLock<sampler_preview::PreviewHandle>,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            session: Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default()))),
            shared: Arc::new(SharedRt::default()),
            tables: Arc::new(Mutex::new(GraphTables {
                generation: 0,
                params: Arc::new(ParamTable::default()),
                slots: std::collections::HashMap::new(),
            })),
            engine: OnceLock::new(),
            samplers: Arc::new(Mutex::new(SamplerBank::default())),
            preview: OnceLock::new(),
        }
    }
}

impl AudioState {
    fn engine(&self) -> Result<&EngineHandle, String> {
        self.engine.get().ok_or_else(|| "audio engine not started".to_string())
    }

    /// Compose the live transport snapshot (atomics + store fields).
    fn transport_snapshot(&self) -> TransportState {
        control::ops::transport_snapshot(&self.session.lock().store, &self.shared)
    }

    // ---- control-plane wiring (ARCHITECTURE §11) ------------------------
    // lib.rs uses these to construct the shared `ControlPlane` after init;
    // they exist so the seam never needs private access to this module.

    pub(crate) fn control_parts(
        &self,
    ) -> (Arc<Mutex<Session>>, Arc<SharedRt>, SharedGraphTables) {
        (self.session.clone(), self.shared.clone(), self.tables.clone())
    }

    pub(crate) fn engine_handle(&self) -> Option<EngineHandle> {
        self.engine.get().cloned()
    }
}

/// Module init hook, called once from Tauri's `setup` (lib.rs, frozen).
/// Starts the engine control thread (which opens the output stream).
pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let state = app.state::<AudioState>();
    // Make the sampler bank reachable from the engine graph rebuild
    // (midi-track instrument routing) and the post-job auto-register hook.
    sampler::register_bank(state.samplers.clone());
    let handle = engine::start(
        state.shared.clone(),
        state.tables.clone(),
        state.session.clone(),
        Box::new(TauriEvents(app.clone())),
    );
    let _ = state.engine.set(handle);
    log::info!("audio::init — engine control thread started");
    Ok(())
}

/// App-event bridge (control thread -> `app.emit`).
struct TauriEvents(AppHandle);

impl engine::EventSink for TauriEvents {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        if let Err(e) = self.0.emit(event, payload) {
            log::warn!("emit {event}: {e}");
        }
    }
}

/// Meter bridge (control thread -> Tauri IPC channel). Returning false when
/// the JS side dropped the channel unsubscribes it.
struct ChannelMeterSink(Channel<MeterFrame>);

impl engine::MeterSink for ChannelMeterSink {
    fn send_frame(&self, frame: &MeterFrame) -> bool {
        self.0.send(frame.clone()).is_ok()
    }
}

// ---------------------------------------------------------------------------
// Transport commands — thin wrappers over the shared control plane (§11);
// the MCP `transport_control` tool drives the exact same code path.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn transport_play(
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TransportState, String> {
    control.transport(control::TransportAction::Play)
}

#[tauri::command]
pub fn transport_stop(
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TransportState, String> {
    control.transport(control::TransportAction::Stop)
}

/// `position_samples` is an absolute sample offset at the engine sample rate.
#[tauri::command]
pub fn transport_seek(
    position_samples: u64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TransportState, String> {
    control.transport(control::TransportAction::Seek { position_samples })
}

/// Set the transport loop region (phase-3 architect round, additive).
/// Samples at the engine rate; `enabled: true` with an empty region errors.
/// Persisted with the project (transport block) and honored by the RT
/// mixer's loop-aware rendering (voices release on wrap).
#[tauri::command]
pub fn transport_set_loop(
    enabled: bool,
    start_samples: u64,
    end_samples: u64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TransportState, String> {
    control.transport(control::TransportAction::SetLoop { enabled, start_samples, end_samples })
}

/// Stop the transport when the playhead reaches the end of the material.
/// Pure policy: the engine detects the boundary either way and reports it —
/// this decides whether reaching it stops playback. Ignored while a loop is
/// active or while recording. Persisted with the project.
#[tauri::command]
pub fn transport_set_stop_at_end(
    enabled: bool,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TransportState, String> {
    control.transport(control::TransportAction::SetStopAtEnd { enabled })
}

#[tauri::command]
pub fn get_transport_state(state: State<'_, AudioState>) -> Result<TransportState, String> {
    Ok(state.transport_snapshot())
}

// ---------------------------------------------------------------------------
// Device commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_input_devices() -> Result<Vec<AudioDevice>, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let devices = host.input_devices().map_err(|e| e.to_string())?;
    Ok(devices
        .filter_map(|d| {
            let name = d.name().ok()?;
            let cfg = d.default_input_config().ok()?;
            Some(AudioDevice {
                id: name.clone(),
                is_default: name == default_name,
                max_channels: cfg.channels(),
                default_sample_rate: cfg.sample_rate().0,
                name,
            })
        })
        .collect())
}

#[tauri::command]
pub fn list_output_devices() -> Result<Vec<AudioDevice>, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| d.name().ok())
        .unwrap_or_default();
    let devices = host.output_devices().map_err(|e| e.to_string())?;
    Ok(devices
        .filter_map(|d| {
            let name = d.name().ok()?;
            let cfg = d.default_output_config().ok()?;
            Some(AudioDevice {
                id: name.clone(),
                is_default: name == default_name,
                max_channels: cfg.channels(),
                default_sample_rate: cfg.sample_rate().0,
                name,
            })
        })
        .collect())
}

/// Selecting a device restarts the corresponding stream (input restarts are
/// refused while a recording is running).
#[tauri::command]
pub fn select_input_device(
    device_id: String,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    state.engine()?.request(|reply| ControlMsg::SelectInput {
        device_id: Some(device_id),
        reply,
    })
}

#[tauri::command]
pub fn select_output_device(
    device_id: String,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    state.engine()?.request(|reply| ControlMsg::SelectOutput {
        device_id: Some(device_id),
        reply,
    })
}

// ---------------------------------------------------------------------------
// Recording commands
// ---------------------------------------------------------------------------

/// Arm-and-go: starts capturing on all armed tracks (or `track_ids` if given).
/// Auto-creates an "Untitled" project when none is open.
#[tauri::command]
pub fn start_recording(
    track_ids: Option<Vec<String>>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TransportState, String> {
    control.start_recording(track_ids)
}

/// Finalizes the take: WAV files closed, waveform pyramids cached, clips
/// registered on their tracks and returned.
#[tauri::command]
pub fn stop_recording(control: State<'_, Arc<ControlPlane>>) -> Result<Vec<Clip>, String> {
    control.stop_recording()
}

// ---------------------------------------------------------------------------
// Metering / waveform commands
// ---------------------------------------------------------------------------

/// Frontend subscribes with a Tauri IPC `Channel<MeterFrame>`; the engine
/// control thread pushes batched meter frames at ~60 Hz (silent frames while
/// idle). The subscription ends automatically when the JS side drops the
/// channel (send starts failing). Peak-hold/decay is a UI concern.
#[tauri::command]
pub fn subscribe_meters(
    channel: Channel<MeterFrame>,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    state
        .engine()?
        .send(ControlMsg::Subscribe(Box::new(ChannelMeterSink(channel))));
    Ok(())
}

/// Binary waveform tile fetch (zero-JSON path). Returns a raw AWTF payload
/// via `tauri::ipc::Response` (ArrayBuffer on the JS side); layout in
/// docs/ARCHITECTURE.md §2.5. Tiles come from the per-clip LOD pyramid cached
/// under `<project>/cache/waveforms/<clipId>/` (built on record-finish and on
/// project open). Unknown clips / missing LODs yield a valid zero-bin tile.
#[tauri::command]
pub fn get_waveform_tile(
    clip_id: String,
    lod: u32,
    tile_index: u64,
    state: State<'_, AudioState>,
) -> Result<Response, String> {
    let (cache_dir, channels) = {
        let session = state.session.lock();
        let store = &session.store;
        let clip = store.clips.iter().find(|c| c.id == clip_id);
        (
            store.waveform_cache_dir(&clip_id),
            clip.map(|c| c.source_channels).unwrap_or(2),
        )
    };
    let bytes = match cache_dir {
        Some(dir) => match waveform::read_tile(&dir, lod, tile_index) {
            Ok(Some((ch, data))) => waveform::encode_tile(ch, lod, tile_index, &data),
            Ok(None) => waveform::empty_tile(channels, lod, tile_index),
            Err(e) => return Err(format!("waveform tile: {e}")),
        },
        None => waveform::empty_tile(channels, lod, tile_index),
    };
    Ok(Response::new(bytes))
}

// ---------------------------------------------------------------------------
// Track commands — thin wrappers over the shared control plane (§11). The
// frozen per-property setters are single-change wrappers over the batched
// `set_track_mix` path (SCALABILITY §5 migration step 2).
// ---------------------------------------------------------------------------

/// `kind` is additive (default "audio"; "midi" lands with phase 2).
#[tauri::command]
pub fn add_track(
    name: Option<String>,
    kind: Option<String>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    control.add_track(name, kind, control::op::TxMeta::user("add track"))
}

/// Runs through the transaction channel (`ControlPlane::remove_track`) —
/// the clip cleanup + slot free + single `Rebuild` sequencing that used to
/// live directly in this command body now lives there.
#[tauri::command]
pub fn remove_track(
    track_id: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.remove_track(&track_id, control::op::TxMeta::user("remove track"))
}

#[tauri::command]
pub fn get_tracks(state: State<'_, AudioState>) -> Result<Vec<TrackState>, String> {
    Ok(state.session.lock().store.tracks.clone())
}

/// Apply one mix change through the batched control-plane path (param-table
/// writes only — knob-rate safe, NO graph rebuild).
fn single_mix_change(
    control: &ControlPlane,
    change: TrackMixChange,
    label: &str,
) -> Result<TrackState, String> {
    let mut updated = control.set_track_mix(vec![change], control::op::TxMeta::user(label))?;
    updated.pop().ok_or_else(|| "empty mix result".to_string())
}

/// `gain_db`: fader gain in decibels; -160.0 is treated as -inf.
#[tauri::command]
pub fn set_track_gain(
    track_id: String,
    gain_db: f64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    single_mix_change(
        &control,
        TrackMixChange { gain_db: Some(gain_db), ..TrackMixChange::new(track_id) },
        "set gain",
    )
}

/// `pan`: -1.0 (L) .. 1.0 (R).
#[tauri::command]
pub fn set_track_pan(
    track_id: String,
    pan: f64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    single_mix_change(
        &control,
        TrackMixChange { pan: Some(pan), ..TrackMixChange::new(track_id) },
        "set pan",
    )
}

#[tauri::command]
pub fn set_track_mute(
    track_id: String,
    muted: bool,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    single_mix_change(
        &control,
        TrackMixChange { muted: Some(muted), ..TrackMixChange::new(track_id) },
        "set mute",
    )
}

#[tauri::command]
pub fn set_track_solo(
    track_id: String,
    soloed: bool,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    single_mix_change(
        &control,
        TrackMixChange { soloed: Some(soloed), ..TrackMixChange::new(track_id) },
        "set solo",
    )
}

#[tauri::command]
pub fn set_track_arm(
    track_id: String,
    armed: bool,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    single_mix_change(
        &control,
        TrackMixChange { armed: Some(armed), ..TrackMixChange::new(track_id) },
        "set arm",
    )
}

// ---------------------------------------------------------------------------
// Sampler commands (phase 2, sampler zone; names frozen). The parser/types
// live in [`sampler`]; engine playback integration is the owning agent's job.
// ---------------------------------------------------------------------------

/// Parse an .sfz file (AURA SFZ SUBSET v1, see sampler.rs), decode/resample
/// its samples to the engine rate (load time, control side — the RT thread
/// never touches files), and register it in the bank.
#[tauri::command]
pub fn sampler_load_instrument(
    sfz_path: String,
    name: Option<String>,
    state: State<'_, AudioState>,
) -> Result<InstrumentInfo, String> {
    let path = std::path::Path::new(&sfz_path);
    if !path.is_absolute() {
        return Err(format!("sfzPath must be absolute: {sfz_path}"));
    }
    let engine_rate = state.shared.sample_rate.load(Relaxed);
    sampler_engine::load_into_bank(&state.samplers, path, name, engine_rate)
}

#[tauri::command]
pub fn sampler_list_instruments(
    state: State<'_, AudioState>,
) -> Result<Vec<InstrumentInfo>, String> {
    Ok(state.samplers.lock().list())
}

/// Audition a note on a loaded instrument — no project or midi clip needed.
/// Plays through the dedicated preview output path (`sampler_preview`): a
/// full envelope-shaped note with auto-release. This is the MCP/UI
/// "can hear it" hook for generated instruments.
#[tauri::command]
pub fn sampler_preview_note(
    instrument_id: String,
    key: u8,
    velocity: u8,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    if key > 127 {
        return Err(format!("key out of range: {key}"));
    }
    let compiled = state
        .samplers
        .lock()
        .compiled(&instrument_id)
        .ok_or_else(|| format!("unknown instrument: {instrument_id}"))?;
    let handle = state.preview.get_or_init(sampler_preview::start);
    handle.play(compiled, key, velocity.clamp(1, 127))
}

/// Bind (or unbind, with `instrument_id: null`) an instrument to a midi
/// track. STRUCTURAL: triggers a graph rebuild so the track immediately
/// renders through its LIVE instrument node (ARCHITECTURE §15).
///
/// Accepted refs (phase 3, additive): a loaded sampler-bank id, or
/// `plugin:<instanceId>` naming a registered plugin instance
/// (`plugin_instantiate`). Plugin-backed tracks render silence while the
/// instance status is `"stub"` (until zones P1/P2 land the real hosts).
#[tauri::command]
pub fn set_track_instrument(
    track_id: String,
    instrument_id: Option<String>,
    state: State<'_, AudioState>,
) -> Result<TrackState, String> {
    if let Some(id) = &instrument_id {
        if let Some(pid) = id.strip_prefix("plugin:") {
            if !crate::plugins::instance_exists(pid) {
                return Err(format!(
                    "unknown plugin instance: {pid} (plugin_instantiate first)"
                ));
            }
        } else if state.samplers.lock().compiled(id).is_none() {
            return Err(format!("unknown instrument: {id} (load it first)"));
        }
    }
    let track = {
        let mut session = state.session.lock();
        let s = &mut session.store;
        let t = s
            .tracks
            .iter_mut()
            .find(|t| t.id == track_id)
            .ok_or_else(|| format!("unknown track: {track_id}"))?;
        if t.kind != "midi" {
            return Err(format!(
                "track {track_id} is kind \"{}\" (instruments bind to midi tracks)",
                t.kind
            ));
        }
        t.instrument_id = instrument_id;
        t.clone()
    };
    state.engine()?.send(ControlMsg::Rebuild);
    Ok(track)
}

// ---------------------------------------------------------------------------
// Project commands (project.json format: docs/ipc-schemas/project.schema.json)
// ---------------------------------------------------------------------------

// All project commands are async: sync commands run on the MAIN thread, and
// on Linux the WebKitGTK webview shares the GTK main loop — disk I/O plus
// plugin/graph restore would freeze the UI mid-paint. `spawn_blocking` keeps
// the (blocking) bodies off the async runtime's core threads as well.

/// Creates `<parent_dir>/<name>.aura/` with project.json + audio/stems/cache
/// subdirs and makes it the open project, resetting the session to a blank
/// slate (tracks, clips, midi, transport). Materializing an unsaved session
/// is `save_project_as`. Thin wrapper over the shared control plane (§11).
#[tauri::command]
pub async fn create_project(
    parent_dir: String,
    name: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Project, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cp.create_project(&parent_dir, &name))
        .await
        .map_err(|e| e.to_string())?
}

/// First save of a session with no open project: creates the `.aura` dir and
/// persists the current in-memory content into it. Fails when a project is
/// already open. Thin wrapper over the shared control plane (§11).
#[tauri::command]
pub async fn save_project_as(
    parent_dir: String,
    name: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Project, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cp.save_project_as(&parent_dir, &name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn open_project(path: String, app: AppHandle) -> Result<Project, String> {
    tauri::async_runtime::spawn_blocking(move || open_project_impl(path, app))
        .await
        .map_err(|e| e.to_string())?
}

fn open_project_impl(path: String, app: AppHandle) -> Result<Project, String> {
    let state = app.state::<AudioState>();
    let (project, dir) = project::load(std::path::Path::new(&path))?;
    // Validate BEFORE mutating any in-memory state (review fix: a project
    // with duplicate track ids must fail cleanly, not after tracks/clips
    // were replaced — the track-count cap this comment used to describe is
    // gone, Task 7: slot assignment is per-graph now).
    project::validate(&project)?;
    {
        let mut session = state.session.lock();
        let s = &mut session.store;
        // Round-2 §2.4: no slot/param seeding here anymore — adoption
        // (below) + the `Rebuild` sent after this block is enough; the
        // next rebuild derives slots from display order and populates a
        // fresh `ParamTable` from the adopted rows.
        s.tracks = project.tracks.clone();
        s.clips = project.clips.clone();
        s.project_dir = Some(dir);
        s.project_name = Some(project.name.clone());
        s.created_at = project.created_at.clone();
        if let Some(t) = &project.transport {
            s.transport.tempo_bpm = t.tempo_bpm;
            s.transport.state = "stopped".into();
            // Store mirror AND RT atomics for the loop region, so the next
            // save round-trips it (from_store serializes store.transport).
            s.transport.loop_enabled = t.loop_enabled;
            s.transport.loop_start_samples = t.loop_start_samples;
            s.transport.loop_end_samples = t.loop_end_samples;
            state.shared.playing.store(false, Relaxed);
            state.shared.position.store(t.position_samples, Relaxed);
            state.shared.loop_enabled.store(t.loop_enabled, Relaxed);
            state.shared.loop_start.store(t.loop_start_samples, Relaxed);
            state.shared.loop_end.store(t.loop_end_samples, Relaxed);
        }
    }
    // Eager midi resync (zone C's requested seam): the midi store adopts the
    // opened project's v2 fields NOW, so the first `get_project_state` after
    // an open (and the rebuild below) already see fresh midi state.
    let (dir, bpm) = {
        let session = state.session.lock();
        (session.store.project_dir.clone(), session.store.transport.tempo_bpm)
    };
    crate::midi::notify_project_opened(dir, bpm);
    // Load clip audio + (re)build waveform pyramids off the IPC path.
    state.engine()?.send(ControlMsg::Rebuild);
    let _ = app.emit("project://changed", serde_json::to_value(&project).unwrap_or_default());
    Ok(project)
}

#[tauri::command]
pub async fn save_project(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || save_project_impl(app))
        .await
        .map_err(|e| e.to_string())?
}

fn save_project_impl(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AudioState>();
    let (project, dir) = {
        let session = state.session.lock();
        let s = &session.store;
        let dir = s.project_dir.clone().ok_or("no project open")?;
        let p = project::from_store(
            s,
            state.shared.position.load(Relaxed),
            state.shared.sample_rate.load(Relaxed),
        )?;
        (p, dir)
    };
    project::save(&dir, &project)?;
    let _ = app.emit("project://changed", serde_json::to_value(&project).unwrap_or_default());
    Ok(())
}
