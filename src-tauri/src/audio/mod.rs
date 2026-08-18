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

pub mod decimate;
pub mod dsp;
pub mod engine;
pub mod insert;
pub mod meters;
pub mod metronome;
pub mod midi_in;
pub mod mixer;
pub mod pdc;
pub mod pitch;
pub mod pitch_store;
pub mod pitch_thread;
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
pub mod yin;

use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, OnceLock};

use cpal::traits::{DeviceTrait, HostTrait};
use parking_lot::Mutex;
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::control::{self, ControlPlane, Session, TrackMixChange};
use crate::midi::MidiStore;
use engine::{ControlMsg, EngineHandle};
use rt::{GraphTables, SharedGraphTables, SharedRt};
use sampler::{InstrumentInfo, SamplerBank};

pub use pitch::{PitchFrame, PitchFrameBatch, PitchState};
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
    /// The ONE undo/redo history + op journal (Plan E Task 17), created
    /// here because `init` builds the engine's `Committer` before
    /// `ControlPlane` exists — both must share this exact `Arc`, or the
    /// engine's non-transient commits (recording finalize) would land in a
    /// second, invisible history. `lib.rs`'s setup passes it on via
    /// [`AudioState::history_log`].
    log: Arc<control::HistoryLog>,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            session: Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default()))),
            shared: Arc::new(SharedRt::default()),
            tables: GraphTables::empty(),
            engine: OnceLock::new(),
            samplers: Arc::new(Mutex::new(SamplerBank::default())),
            preview: OnceLock::new(),
            log: Arc::new(control::HistoryLog::new()),
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

    /// The shared history/journal handle (Plan E Task 17) — `lib.rs` hands
    /// this to `ControlPlane::new` so the control plane and the engine
    /// control thread commit into the SAME log.
    pub(crate) fn history_log(&self) -> Arc<control::HistoryLog> {
        self.log.clone()
    }
}

/// Module init hook, called once from Tauri's `setup` (lib.rs, frozen).
/// Starts the engine control thread (which opens the output stream).
pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let state = app.state::<AudioState>();
    // Make the sampler bank reachable from the engine graph rebuild
    // (midi-track instrument routing) and the post-job auto-register hook.
    sampler::register_bank(state.samplers.clone());
    midi_in::hub().attach_shared(state.shared.clone());
    // The engine's own commit core (Plan E Task 13) — the SAME
    // session/shared/tables `Arc`s `ControlPlane::new` gets a moment later
    // (lib.rs's setup), with its own `emit` closure instance: two closure
    // instances, both ultimately calling the one live `AppHandle::emit`, so
    // they're behaviorally one emitter (see `control::Committer`'s doc).
    let engine_emit: control::EventEmitter = {
        let app = app.clone();
        Box::new(move |event: &str, payload: serde_json::Value| {
            if let Err(e) = app.emit(event, payload) {
                log::warn!("emit {event}: {e}");
            }
        })
    };
    let committer = control::Committer::new(
        state.session.clone(),
        state.shared.clone(),
        state.tables.clone(),
        std::sync::Arc::new(engine_emit),
        // The SAME log `ControlPlane` gets a moment later (lib.rs setup):
        // the engine's `stop recording` finalize is NON-transient, so it
        // must be an undo step in the user's one history (Task 17).
        state.log.clone(),
    );
    let handle = engine::start(
        state.shared.clone(),
        state.tables.clone(),
        state.session.clone(),
        Box::new(TauriEvents(app.clone())),
        committer,
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
/// refused while a recording is running). Delegates to `ControlPlane` (Plan
/// E Task 12, §4.5) so the selection is attributed at the one front door
/// both this command and the MCP surface call through.
#[tauri::command]
pub fn select_input_device(
    device_id: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.select_input_device(device_id, control::op::TxMeta::user("select input device"))
}

/// Click track + count-in bars. App config, no document op.
#[tauri::command]
pub fn set_metronome(
    enabled: bool,
    gain: f32,
    count_in_bars: u8,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    state.engine()?.request(|reply| ControlMsg::SetMetronome {
        enabled,
        gain,
        count_in_bars,
        reply,
    })
}

#[tauri::command]
pub fn select_output_device(
    device_id: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.select_output_device(device_id, control::op::TxMeta::user("select output device"))
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
///
/// An `Err` reaching the frontend means the take WAS registered (the MIDI
/// clip and any audio clips that survived) but the WAV write failed — the
/// caller should surface it and carry on with its own stop bookkeeping,
/// never retry.
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

// ---------------------------------------------------------------------------
// Pitch Coach — listen / subscribe / rehearse (plan task 6)
// ---------------------------------------------------------------------------

/// Open the input hub for live pitch without starting a take (owner ruling R6).
#[tauri::command]
pub fn pitch_listen_start(state: State<'_, AudioState>) -> Result<(), String> {
    state
        .engine()?
        .request(|reply| ControlMsg::SetListening { on: true, reply })
}

/// Close the listen-only hub. A running take keeps the microphone.
#[tauri::command]
pub fn pitch_listen_stop(state: State<'_, AudioState>) -> Result<(), String> {
    state
        .engine()?
        .request(|reply| ControlMsg::SetListening { on: false, reply })
}

/// Frontend subscribes with a Tauri IPC `Channel<PitchFrameBatch>`; the
/// engine control thread pushes one batch per 60 Hz meter tick.
#[tauri::command]
pub fn pitch_subscribe(
    channel: Channel<PitchFrameBatch>,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    state
        .engine()?
        .send(ControlMsg::SubscribePitch(Box::new(ChannelPitchSink(channel))));
    Ok(())
}

/// Drop a pitch subscription. `channel_id` is the `id` of the `Channel`
/// handed to `pitch_subscribe` — a JS `Channel` exposes it as `.id`.
///
/// This exists because closing the JS end of a channel is invisible to the
/// engine: a `Channel` whose `onmessage` has been cleared still accepts
/// sends, so without this every open/close of the Pitch Coach would leave a
/// live sink behind and the engine would ship one more batch per 60 Hz tick
/// for the rest of the session.
#[tauri::command]
pub fn pitch_unsubscribe(channel_id: u32, state: State<'_, AudioState>) -> Result<(), String> {
    state.engine()?.send(ControlMsg::UnsubscribePitch(channel_id));
    Ok(())
}

/// Momentary rehearse-hold: the take writes silence for the held span.
#[tauri::command]
pub fn set_rehearse_hold(enabled: bool, state: State<'_, AudioState>) -> Result<(), String> {
    state
        .engine()?
        .request(|reply| ControlMsg::SetRehearseHold { enabled, reply })
}

/// Store the MIDI track that holds the target melody and re-emit `pitch://state`.
#[tauri::command]
pub fn pitch_set_reference(
    track_id: Option<String>,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    state
        .engine()?
        .request(|reply| ControlMsg::SetPitchReference { track_id, reply })
}

// ---------------------------------------------------------------------------
// Pitch Coach — the recorded take: pitch track and score (plan task 14)
// ---------------------------------------------------------------------------

/// One point of a stored pitch curve, at a PROJECT timeline position.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitchPoint {
    pub sample: u64,
    pub midi: f32,
    pub clarity: f32,
    pub voiced: bool,
}

/// A clip's pitch curve, thinned for drawing.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitchTrackView {
    pub clip_id: String,
    /// The rate the curve was analysed at — the take's own, which is not
    /// necessarily the project's.
    pub source_rate: u32,
    pub hop: u32,
    pub provenance: pitch_store::PitchProvenance,
    /// How many points the stored curve had before thinning.
    pub total_points: usize,
    pub points: Vec<PitchPoint>,
}

/// The clip and the project directory, or a clean error.
fn clip_for_pitch(state: &AudioState, clip_id: &str) -> Result<(std::path::PathBuf, Clip), String> {
    let session = state.session.lock();
    let store = &session.store;
    let dir = store
        .project_dir
        .clone()
        .ok_or_else(|| "the project has no folder on disk yet".to_string())?;
    let clip = store
        .clips
        .iter()
        .find(|c| c.id == clip_id)
        .cloned()
        .ok_or_else(|| format!("unknown audio clip {clip_id}"))?;
    Ok((dir, clip))
}

/// Read the clip's pitch track, analysing the audio first if there is none
/// (or if what is there cannot be read).
///
/// Analysis runs at roughly 100x real time, so a three-minute take costs a
/// couple of seconds on the command thread. That is the price of never
/// making the UI ask twice.
fn ensure_pitch_track(
    dir: &std::path::Path,
    clip: &Clip,
) -> Result<pitch_store::PitchTrack, String> {
    let path = pitch_store::track_path(dir, clip.id.as_str());
    if path.exists() {
        match pitch_store::read_track(&path) {
            Ok(t) => return Ok(t),
            // A cache that cannot be read is a cache to rebuild, not an
            // error to show someone who asked for a score.
            Err(e) => eprintln!("[pitch] re-analysing {}: {e}", clip.id),
        }
    }
    let wav = dir.join(&clip.source_path);
    let (channels, rate, samples) = crate::control::import::decode_audio(&wav)?;
    let track = pitch_store::analyze_interleaved(&samples, channels, rate);
    if let Err(e) = pitch_store::write_track(&path, &track) {
        eprintln!("[pitch] track analysed but not cached: {e}");
    }
    Ok(track)
}

/// Take-local source samples -> project timeline samples. `None` for a
/// frame the clip's trim leaves off the timeline.
fn pitch_frame_to_timeline(sample: u64, clip: &Clip, project_rate: u32, source_rate: u32) -> Option<u64> {
    // The decode cache is resampled to the engine rate, which is the domain
    // `offset_samples` and `length_samples` live in — `source_length_samples`
    // and this curve are the two things measured in SOURCE samples.
    let in_project = sample * project_rate.max(1) as u64 / source_rate.max(1) as u64;
    let rel = in_project.checked_sub(clip.offset_samples)?;
    if rel >= clip.length_samples {
        return None;
    }
    Some(clip.timeline_start_samples + rel)
}

fn pitch_frames_on_timeline(
    track: &pitch_store::PitchTrack,
    clip: &Clip,
    project_rate: u32,
) -> Vec<PitchFrame> {
    track
        .frames
        .iter()
        .filter_map(|f| {
            let sample = pitch_frame_to_timeline(f.sample, clip, project_rate, track.rate)?;
            Some(PitchFrame { sample, ..*f })
        })
        .collect()
}

/// Analyse a clip's audio and (re)write its pitch track. Returns the number
/// of frames. Idempotent, and the only way to give an older take a curve.
///
/// `async` + `spawn_blocking` for the same reason as `seed_demo_project`:
/// sync commands run on the MAIN thread, which on Linux is the GTK main loop
/// the WebKitGTK webview draws on. Analysing a three-minute take is a couple
/// of seconds, and on the main thread that is a couple of seconds in which
/// the panel's own "ANALYSING THE TAKE…" state cannot paint. The session lock
/// is taken and released BEFORE the move, so the audio thread's control plane
/// is never held across the analysis.
#[tauri::command]
pub async fn pitch_analyze_clip(
    clip_id: String,
    state: State<'_, AudioState>,
) -> Result<usize, String> {
    let (dir, clip) = clip_for_pitch(&state, &clip_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let wav = dir.join(&clip.source_path);
        let (channels, rate, samples) = crate::control::import::decode_audio(&wav)?;
        let track = pitch_store::analyze_interleaved(&samples, channels, rate);
        pitch_store::write_track(&pitch_store::track_path(&dir, &clip_id), &track)?;
        Ok(track.frames.len())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The clip's pitch curve on the timeline, thinned to at most `max_points`
/// (default 2000 — a wide screen has fewer pixels than a minute has frames).
///
/// `async` + `spawn_blocking`: a missing cache makes this an analysis (see
/// [`pitch_analyze_clip`]).
#[tauri::command]
pub async fn pitch_track(
    clip_id: String,
    max_points: Option<u32>,
    state: State<'_, AudioState>,
) -> Result<PitchTrackView, String> {
    let (dir, clip) = clip_for_pitch(&state, &clip_id)?;
    let project_rate = state.shared.sample_rate.load(Relaxed).max(1);
    tauri::async_runtime::spawn_blocking(move || {
        let track = ensure_pitch_track(&dir, &clip)?;
        let frames = pitch_frames_on_timeline(&track, &clip, project_rate);
        let cap = max_points.unwrap_or(2000).max(1) as usize;
        let stride = frames.len().div_ceil(cap).max(1);
        Ok(PitchTrackView {
            clip_id,
            source_rate: track.rate,
            hop: track.hop,
            provenance: track.provenance,
            total_points: frames.len(),
            points: frames
                .iter()
                .step_by(stride)
                .map(|f| PitchPoint {
                    sample: f.sample,
                    midi: f.midi,
                    clarity: f.clarity,
                    voiced: f.voiced,
                })
                .collect(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Score a recorded take against a MIDI track's melody.
///
/// Scores are never persisted (spec): recomputing means the report stays
/// correct after the reference melody is edited.
///
/// `async` + `spawn_blocking` for the analysis half (see
/// [`pitch_analyze_clip`]). The reference melody is derived first, under the
/// session lock, and only owned data crosses onto the blocking pool — this
/// command must never hold the session lock while it decodes a take.
#[tauri::command]
pub async fn pitch_score(
    clip_id: String,
    reference_track_id: String,
    tolerance_cents: f32,
    state: State<'_, AudioState>,
) -> Result<control::pitch_coach::PitchScoreReport, String> {
    let (dir, clip) = clip_for_pitch(&state, &clip_id)?;
    let project_rate = state.shared.sample_rate.load(Relaxed).max(1);

    let reference = {
        let session = state.session.lock();
        let midi = &session.midi;
        if !session.store.tracks.iter().any(|t| t.id == reference_track_id) {
            return Err(format!("unknown reference track {reference_track_id}"));
        }
        let map = crate::midi::TempoMap::new(midi.ppq, midi.tempo_events.clone(), project_rate)
            .map_err(|e| format!("tempo map: {e}"))?;
        let mut notes = Vec::new();
        for c in midi.clips.iter().filter(|c| c.track_id == reference_track_id) {
            notes.extend(crate::midi::schedule::clip_notes(c, &map));
        }
        control::pitch_coach::reference_melody(notes, project_rate)
    };
    if reference.is_empty() {
        return Err(format!("track {reference_track_id} has no notes to score against"));
    }

    tauri::async_runtime::spawn_blocking(move || {
        let track = ensure_pitch_track(&dir, &clip)?;
        let frames = pitch_frames_on_timeline(&track, &clip, project_rate);
        let mut report =
            control::pitch_coach::score(&reference, &frames, project_rate, tolerance_cents);
        report.reference_track_id = reference_track_id;
        Ok(report)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Pitch-batch bridge (control thread -> Tauri IPC channel).
struct ChannelPitchSink(Channel<PitchFrameBatch>);

impl engine::PitchSink for ChannelPitchSink {
    fn send_batch(&self, batch: &PitchFrameBatch) -> bool {
        self.0.send(batch.clone()).is_ok()
    }
    fn id(&self) -> u32 {
        self.0.id()
    }
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

/// Rename a track (lanes UX). Additive command; the name is trimmed and an
/// empty one is rejected by the write side, so the returned row carries the
/// name that was actually stored, not the caller's raw string.
#[tauri::command]
pub fn set_track_name(
    track_id: String,
    name: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    control.set_track_name(&track_id, name, control::op::TxMeta::user("rename track"))
}

/// Put a track in a lane group, or take it out (`group: None`). Lane groups
/// are a display grouping only — see `TrackState::group`.
#[tauri::command]
pub fn set_track_group(
    track_id: String,
    group: Option<String>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    control.set_track_group(&track_id, group, control::op::TxMeta::user("set lane group"))
}

/// Apply a whole lane arrangement — display order + group membership — in
/// one transaction, so a drag that reorders lanes AND moves one into a
/// group is a single undo step. `lanes` must name every track exactly once.
#[tauri::command]
pub fn arrange_lanes(
    lanes: Vec<control::LaneArrangement>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Vec<TrackState>, String> {
    control.arrange_lanes(lanes, control::op::TxMeta::user("arrange lanes"))
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
    let handle = state.preview.get_or_init(|| sampler_preview::start("ui"));
    handle.play(compiled, key, velocity.clamp(1, 127))
}

/// Audition a LIBRARY FILE — no project, no clip, no document. The Gate-safe
/// preview category (`sampler_preview_note`, `midi_input`'s live monitoring):
/// no op, no document field, no dirty flag. Plays at most
/// `library::AUDITION_MAX_SECS` of the file once through the shared "ui"
/// preview stream; whatever was auditioning is released first, so a click
/// cancels the previous one.
///
/// `async` + `spawn_blocking`: decoding is disk + CPU work and sync commands
/// run on the MAIN thread (see `seed_demo_project`).
#[tauri::command]
pub async fn library_audition(
    path: String,
    max_seconds: Option<f32>,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    let handle = state.preview.get_or_init(|| sampler_preview::start("ui")).clone();
    let secs = max_seconds.unwrap_or(crate::library::AUDITION_MAX_SECS);
    tauri::async_runtime::spawn_blocking(move || {
        let inst = crate::library::compile_audition(std::path::Path::new(&path), secs)?;
        // Cancel whatever is sounding before installing the next sample.
        handle.all_off()?;
        handle.play_held(std::sync::Arc::new(inst), 60, 100)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Release whatever the audition path is sounding. A no-op (never an error)
/// when nothing has ever been auditioned — the preview stream is lazy.
#[tauri::command]
pub fn library_audition_stop(state: State<'_, AudioState>) -> Result<(), String> {
    match state.preview.get() {
        Some(h) => h.all_off(),
        None => Ok(()),
    }
}

/// Bind (or unbind, with `instrument_id: null`) an instrument to a midi
/// track. STRUCTURAL: triggers a graph rebuild so the track immediately
/// renders through its LIVE instrument node (ARCHITECTURE §15).
///
/// Accepted refs (phase 3, additive): a loaded sampler-bank id, or
/// `plugin:<instanceId>` naming a registered plugin instance
/// (`plugin_instantiate`). Plugin-backed tracks render silence while the
/// instance status is `"stub"` (until zones P1/P2 land the real hosts).
///
/// Plan E Task 3: this command's own validation (plugin/sampler existence,
/// track-kind gate) still runs here, ahead of the call — but the actual
/// mutation is now `ControlPlane::set_track_instrument`, a thin wrapper
/// over the transaction channel (`Op::Set`, `PropPath::InstrumentId`). Bug
/// fix (dossier 10 §2.4): this now emits `project://changed` via `commit`,
/// which it never did before — the rebuild used to run silently.
#[tauri::command]
pub fn set_track_instrument(
    track_id: String,
    instrument_id: Option<String>,
    state: State<'_, AudioState>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TrackState, String> {
    if let Some(id) = &instrument_id {
        if let Some(pid) = id.strip_prefix("plugin:") {
            if !control.plugin_exists(pid) {
                return Err(format!(
                    "unknown plugin instance: {pid} (plugin_instantiate first)"
                ));
            }
            // Plan G1 Task 3 / R6: mirror ControlPlane gates (effect / insert).
            // ControlPlane::set_track_instrument also enforces these; keep the
            // wrapper checks so the command path fails before commit.
            let uid = {
                let session = state.session.lock();
                for t in &session.store.tracks {
                    if t.inserts.iter().any(|s| s.instance_id == pid) {
                        return Err(format!(
                            "plugin instance {pid} is already an insert; cannot bind as instrument"
                        ));
                    }
                }
                session
                    .plugins
                    .instances
                    .iter()
                    .find(|r| r.id == pid)
                    .map(|r| r.uid.clone())
            };
            if let Some(uid) = uid {
                if let Some(reg) = crate::plugins::registered_registry() {
                    let reg = reg.lock();
                    if let Some(scanned) = reg.scanned.as_ref() {
                        if let Some(desc) = scanned.iter().find(|d| d.uid == uid) {
                            if !desc.is_instrument {
                                return Err(format!(
                                    "{} is an effect; cannot bind as instrument",
                                    desc.name
                                ));
                            }
                        }
                    }
                }
            }
        } else if state.samplers.lock().compiled(id).is_none() {
            return Err(format!("unknown instrument: {id} (load it first)"));
        }
    }
    {
        let session = state.session.lock();
        let t = session
            .store
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .ok_or_else(|| format!("unknown track: {track_id}"))?;
        if t.kind != "midi" {
            return Err(format!(
                "track {track_id} is kind \"{}\" (instruments bind to midi tracks)",
                t.kind
            ));
        }
    }
    control.set_track_instrument(&track_id, instrument_id, control::op::TxMeta::user("set instrument"))
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

/// Open an existing `.aura` project (or a direct `project.json` path).
/// One-line delegate over the sanctioned epoch fn (Task 6, round-2
/// inventory row 25): [`ControlPlane::open_project_epoch`] absorbs what
/// used to be this file's own `open_project_impl` body.
#[tauri::command]
pub async fn open_project(
    path: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Project, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cp.open_project_epoch(std::path::Path::new(&path)))
        .await
        .map_err(|e| e.to_string())?
}

/// Persist the CURRENT in-memory document to its already-open project dir.
/// One-line delegate over the sanctioned snapshot-mark fn (Task 6, round-2
/// inventory row 26): [`ControlPlane::save_project_mark`] absorbs what used
/// to be this file's own `save_project_impl` body. The frozen command
/// return shape (`Result<(), String>`) discards the returned `Project`.
#[tauri::command]
pub async fn save_project(control: State<'_, Arc<ControlPlane>>) -> Result<(), String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cp.save_project_mark().map(|_| ()))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod pitch_ipc_tests {
    use super::{PitchFrame, PitchFrameBatch};

    #[test]
    fn pitch_frame_batch_serializes_camel_case() {
        let b = PitchFrameBatch {
            frames: vec![PitchFrame {
                sample: 480,
                hz: 220.0,
                midi: 57.0,
                clarity: 0.9,
                rms: 0.2,
                voiced: true,
            }],
            device_rate: 48_000,
            listening: true,
            rehearse_hold: false,
        };
        let v = serde_json::to_value(&b).unwrap();
        assert!(v.get("deviceRate").is_some(), "wire field must be deviceRate");
        assert!(v.get("rehearseHold").is_some());
        let f = &v["frames"][0];
        for key in ["sample", "hz", "midi", "clarity", "rms", "voiced"] {
            assert!(f.get(key).is_some(), "frame missing {key}");
        }
    }

    #[test]
    fn new_pitch_schemas_stay_open_for_extension() {
        // D-06: new schemas must not close themselves to additive evolution.
        for name in ["pitch-frame", "pitch-state"] {
            let path = format!("../docs/ipc-schemas/{name}.schema.json");
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["$schema"], "http://json-schema.org/draft-07/schema#");
            assert!(
                v.get("additionalProperties") != Some(&serde_json::Value::Bool(false)),
                "{name} must not set additionalProperties: false (D-06)"
            );
        }
    }
}
