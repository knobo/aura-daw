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
pub mod op;
pub mod ops;
pub mod loopjam;
pub mod session;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::audio::engine::{ControlMsg, EngineHandle, MeterSink};
use crate::audio::rt::{SharedGraphTables, SharedRt, FLAG_MUTE, FLAG_SOLO};
use crate::audio::types::{Clip, MeterFrame, Project, TrackState, TransportState};
use crate::audio::project;
use crate::sidecars::jobs::{EventSink, JobManager};

pub use ops::TrackMixChange;
pub use session::{Committed, EngineEffect, PersistEffect, Session, Tx};

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
    /// Additive v3 fields (round-2 §3.6, Task 9): the shipped section-table
    /// contract available from cold start, not just after a `set_tempo_map`
    /// edit — a renderer opening a project needs tick<->sample lookups
    /// before it ever calls that command.
    pub meter_map: Vec<crate::midi::MeterEvent>,
    pub period_events: Vec<crate::midi::TempoPeriodEvent>,
    pub section_table: Vec<crate::midi::SectionRow>,
    pub section_table_rule_version: u32,
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
    session: Arc<Mutex<Session>>,
    shared: Arc<SharedRt>,
    /// Control-side view of the CURRENT graph's tables (round-2 §2.4),
    /// shared with the engine control thread — `commit` resolves
    /// `TrackId -> slot` through this AFTER the session lock is released
    /// (see `SharedGraphTables`'s doc for the lock-order rule [C1]).
    tables: SharedGraphTables,
    engine: EngineHandle,
    jobs: Arc<JobManager>,
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

/// Build a property-addressed `Op::Set` for a track. `from` is advisory
/// only (`apply_raw` re-reads store truth), so it's always `Null` here —
/// same convention the op/session unit tests use.
fn set_prop(track_id: &str, path: op::PropPath, to: serde_json::Value) -> op::Op {
    op::Op::Set {
        object: op::ObjectRef::Track(track_id.into()),
        path,
        from: serde_json::Value::Null,
        to,
    }
}

/// Mark `midi` as belonging to freshly-`project::create`d `dir` and settle
/// `midi.dirty` — `create_project`/`create_project_epoch`'s case: a fresh
/// project's on-disk state already matches this blank in-memory reset (both
/// came from `v1_migration_defaults`/nothing), so there is nothing to
/// persist, just mark clean. Finding 2: a stale `dirty = true` left over
/// from a PRIOR project's failed auto-persist (M-5) must not survive into
/// the new one. (`save_project_as_epoch` has its own, DIFFERENT case —
/// adopting a dir with REAL content to persist — handled inline there via
/// `Session::midi_snapshot` + `save_snapshot_into_project`, Task 6's
/// lock-free-I/O fix; this helper's one remaining caller never needs to
/// write, so the old `persist: bool` fork was dropped.)
fn adopt_midi_dir(midi: &mut crate::midi::MidiStore, dir: &std::path::Path) {
    midi.loaded_dir = Some(dir.to_path_buf());
    midi.dirty = false;
}

impl ControlPlane {
    pub fn new(
        session: Arc<Mutex<Session>>,
        shared: Arc<SharedRt>,
        tables: SharedGraphTables,
        engine: EngineHandle,
        jobs: Arc<JobManager>,
        emit: EventEmitter,
    ) -> Self {
        let latest_meters = Arc::new(Mutex::new(None));
        engine.send(ControlMsg::Subscribe(Box::new(LatestMeterCache(
            latest_meters.clone(),
        ))));
        Self { session, shared, tables, engine, jobs, latest_meters, emit }
    }

    fn emit_transport(&self, snap: &TransportState) {
        if let Ok(v) = serde_json::to_value(snap) {
            (self.emit)("transport://state", v);
        }
    }

    // ---- read side ------------------------------------------------------

    pub fn project_state(&self) -> ProjectSnapshot {
        let session = self.session.lock();
        let store = &session.store;
        let midi = &session.midi;
        // The store's own tempo_events always passed TempoMap::new's
        // validation when they were set (set_tempo_map, migration
        // defaults, ...) — a build failure here means that invariant broke
        // elsewhere; degrade to an empty section table rather than fail
        // the whole snapshot read (reads must never panic — the session
        // lock is held).
        let tms = crate::midi::build_tempo_map_state(midi.ppq, &midi.tempo_events, &midi.meter_events)
            .unwrap_or_else(|e| {
                log::warn!("project_state: tempo map state build failed ({e}); serving an empty section table");
                crate::midi::TempoMapState {
                    ppq: midi.ppq,
                    events: midi.tempo_events.clone(),
                    meter_map: midi.meter_events.clone(),
                    period_events: Vec::new(),
                    section_table: Vec::new(),
                    section_table_rule_version: crate::midi::section_table::RULE_VERSION,
                }
            });
        ProjectSnapshot {
            project_name: store.project_name.clone(),
            project_dir: store.project_dir.as_ref().map(|p| p.display().to_string()),
            transport: ops::transport_snapshot(store, &self.shared),
            tracks: store.tracks.clone(),
            clips: store.clips.clone(),
            midi_clips: midi.clips.clone(),
            ppq: midi.ppq,
            tempo_events: midi.tempo_events.clone(),
            meter_map: tms.meter_map,
            period_events: tms.period_events,
            section_table: tms.section_table,
            section_table_rule_version: tms.section_table_rule_version,
        }
    }

    pub fn transport_state(&self) -> TransportState {
        ops::transport_snapshot(&self.session.lock().store, &self.shared)
    }

    /// All automation lanes (Plan E Task 10). PURE session-lock read — no
    /// sync, no `loaded_dir`, no disk — `automation_get`'s entire body.
    pub fn automation_lanes(&self) -> Vec<crate::plugins::automation::AutomationLane> {
        self.session.lock().automation.lanes.clone()
    }

    /// Latest 60 Hz meter frame (None until the engine has pumped one).
    /// This is the MCP agent's "ears": peak/RMS per track + master.
    pub fn read_meters(&self) -> Option<MeterFrame> {
        self.latest_meters.lock().clone()
    }

    // ---- transport / recording -----------------------------------------

    /// Plan E Task 12 (inventory rows 28-29): the transport's four direct
    /// `session.store.transport.*` writes (state="playing", state=
    /// "stopped", the loop mirror, stop_at_end) now go through the op
    /// system as one transient `commit_with` per action — transient
    /// because a play/stop/loop-drag is mid-gesture, RT/document-visible
    /// state, not a document edit a user would expect in undo history
    /// (round-2 §4.4; Task 2's `TxMeta::transient`). `emit_project_changed:
    /// false` — `project://changed`'s payload contract is the full
    /// `Project` shape; firing it once per transport action would be a
    /// behavior change from today's `transport://state`-only contract.
    ///
    /// RT atomics (the output callback reads these per buffer) are
    /// deliberately kept HERE, in `ControlPlane::transport`, executed AFTER
    /// `commit_with` returns — never folded into `EngineEffect` as a
    /// `param_writes`-style entry. They are engine-visible state, not
    /// document state: the document mirror (project.json's transport
    /// block) is what the `Op::Set` above covers, and lock order [C1]
    /// (session lock released before anything engine/RT-visible happens)
    /// is preserved exactly as `commit`'s own doc describes for param
    /// writes.
    ///
    /// `Play`/`Stop` are special-cased below, each for its own reason —
    /// worth being precise about which property survives:
    /// * `Play`'s recording guard preserves both the VALUE (state stays
    ///   "recording") AND atomicity (the check-and-set runs under one
    ///   session lock, via `tx.store()` inside the transaction closure —
    ///   fix round 1; a `self.session.lock()` taken and dropped BEFORE the
    ///   transaction would only preserve the value in the race-free case).
    /// * `Stop` preserves ordering, not atomicity: `StopRecording`'s engine
    ///   round-trip is sent BEFORE the "stopped" Set commits (restored,
    ///   fix round 1) so a reader can't observe "stopped" while the take
    ///   is still finalizing — but the round-trip itself is a separate,
    ///   non-transactional engine call (§4.2), not part of this commit.
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
                let meta = op::TxMeta::user("transport play").transient();
                self.commit_with(
                    meta,
                    |tx| {
                        // Recording owns the state label while a take is
                        // running — Play must never downgrade "recording"
                        // back to "playing". Checked-and-set ATOMICALLY:
                        // the check reads `tx.store()` (the SAME session
                        // lock `apply` writes through, held for the whole
                        // closure), not a separate `self.session.lock()`
                        // taken and dropped before the transaction — that
                        // earlier shape was a TOCTOU (fix round 1): the
                        // engine control thread's own recording-start
                        // write could land in the gap between the read and
                        // the commit, and this Set would then stomp
                        // "recording" back to "playing". Applying nothing
                        // and returning `Ok(())` is a legal empty
                        // transient commit — it preserves the pre-Task-12
                        // VALUE semantics (state stays "recording") without
                        // reproducing the old code's non-atomic check.
                        if tx.store().transport.state == "recording" {
                            return Ok(());
                        }
                        tx.apply(op::Op::Set {
                            object: op::ObjectRef::Transport,
                            path: op::PropPath::TransportState,
                            from: serde_json::Value::Null,
                            to: serde_json::json!("playing"),
                        })
                    },
                    false,
                )?;
                self.shared.playing.store(true, Relaxed);
            }
            TransportAction::Stop => {
                // Stopping while recording finalizes the take (DAW
                // convention) — sent BEFORE the state Set commits below,
                // restoring the pre-Task-12 ordering (fix round 1): the
                // old code sent this first and only wrote "stopped"
                // afterward, so any reader mid-finalize (which can take up
                // to 15s — `writer.finish`'s timeout, audio/engine.rs) saw
                // "recording", never a premature "stopped" while the take
                // is still draining to disk. Kept OUTSIDE this transaction
                // either way (§4.2) — `StopRecording`'s own finalize write
                // to `store.transport.state` (audio/engine.rs's
                // `stop_recording`) races harmlessly with the Set below
                // (both converge on "stopped"); Task 13 revisits this once
                // that finalize becomes its own `Actor::Engine` tx.
                if self.shared.recording.load(Relaxed) {
                    self.engine
                        .request::<Vec<Clip>>(|reply| ControlMsg::StopRecording { reply })?;
                }
                let meta = op::TxMeta::user("transport stop").transient();
                self.commit_with(
                    meta,
                    |tx| {
                        tx.apply(op::Op::Set {
                            object: op::ObjectRef::Transport,
                            path: op::PropPath::TransportState,
                            from: serde_json::Value::Null,
                            to: serde_json::json!("stopped"),
                        })
                    },
                    false,
                )?;
                self.shared.playing.store(false, Relaxed);
            }
            TransportAction::Seek { position_samples } => {
                // Pure RT atomic — position is engine state, not document
                // state, so this is (still) not an op.
                self.shared.position.store(position_samples, Relaxed);
            }
            TransportAction::SetLoop { enabled, start_samples, end_samples } => {
                if enabled && end_samples <= start_samples {
                    return Err(format!(
                        "loop region is empty (start {start_samples} >= end {end_samples})"
                    ));
                }
                let meta = op::TxMeta::user("transport set loop").transient();
                self.commit_with(
                    meta,
                    |tx| {
                        tx.apply(op::Op::Set {
                            object: op::ObjectRef::Transport,
                            path: op::PropPath::LoopEnabled,
                            from: serde_json::Value::Null,
                            to: serde_json::json!(enabled),
                        })?;
                        tx.apply(op::Op::Set {
                            object: op::ObjectRef::Transport,
                            path: op::PropPath::LoopStartSamples,
                            from: serde_json::Value::Null,
                            to: serde_json::json!(start_samples),
                        })?;
                        tx.apply(op::Op::Set {
                            object: op::ObjectRef::Transport,
                            path: op::PropPath::LoopEndSamples,
                            from: serde_json::Value::Null,
                            to: serde_json::json!(end_samples),
                        })
                    },
                    false,
                )?;
                // RT atomics (the output callback reads these per buffer),
                // AFTER the document commit [C1] — see this method's doc.
                self.shared.loop_start.store(start_samples, Relaxed);
                self.shared.loop_end.store(end_samples, Relaxed);
                self.shared.loop_enabled.store(enabled, Relaxed);
            }
            TransportAction::SetStopAtEnd { enabled } => {
                let meta = op::TxMeta::user("transport set stop at end").transient();
                self.commit_with(
                    meta,
                    |tx| {
                        tx.apply(op::Op::Set {
                            object: op::ObjectRef::Transport,
                            path: op::PropPath::StopAtEnd,
                            from: serde_json::Value::Null,
                            to: serde_json::json!(enabled),
                        })
                    },
                    false,
                )?;
                // RT atomic after the document commit [C1] — see above.
                self.shared.stop_at_end.store(enabled, Relaxed);
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

    // ---- device selection -------------------------------------------------
    // Plan E Task 12 (§4.5 "moves behind the ControlPlane for attribution"):
    // the input/output device is app config, not document state — no `Op`,
    // no `commit`. Moving the Tauri commands' direct `ControlMsg` sends
    // behind these two methods is purely so the ACTOR/LABEL attribution is
    // captured (logged) at the one front door both Tauri and MCP call
    // through, instead of only at the (Tauri-only, MCP-unreachable)
    // `#[tauri::command]` body.

    /// Select the input device, logging the attribution before sending the
    /// existing `ControlMsg::SelectInput`. Restarting the input stream is
    /// refused (by the engine) while a recording is running — unchanged.
    pub fn select_input_device(&self, device_id: String, meta: op::TxMeta) -> Result<(), String> {
        log::info!(
            "select_input_device: actor={:?} label={:?} device={device_id:?}",
            meta.actor,
            meta.label
        );
        self.engine
            .request(|reply| ControlMsg::SelectInput { device_id: Some(device_id), reply })
    }

    /// Select the output device — same shape as `select_input_device`.
    pub fn select_output_device(&self, device_id: String, meta: op::TxMeta) -> Result<(), String> {
        log::info!(
            "select_output_device: actor={:?} label={:?} device={device_id:?}",
            meta.actor,
            meta.label
        );
        self.engine
            .request(|reply| ControlMsg::SelectOutput { device_id: Some(device_id), reply })
    }

    // ---- structure ------------------------------------------------------

    /// Create a track and insert it through the transaction channel
    /// (`Op::TrackAdd`, `commit`). Round-2 §2.4: there is no slot to
    /// allocate and no RT param to reset here anymore — the next rebuild
    /// derives the row's slot from display order and populates its params
    /// fresh (`ops::new_track_row`'s doc). A fresh track never has clips
    /// yet, so `Op::TrackAdd`'s `clips` payload is empty here.
    pub fn add_track(
        &self,
        name: Option<String>,
        kind: Option<String>,
        meta: op::TxMeta,
    ) -> Result<TrackState, String> {
        let (track, index) = {
            let session = self.session.lock();
            ops::new_track_row(&session.store, name, kind)?
        };
        self.commit(meta, |tx| {
            tx.apply(op::Op::TrackAdd { track: track.clone(), index, clips: vec![], clip_indices: vec![] })
        })?;
        Ok(track)
    }

    /// Remove a track through the transaction channel (`Op::TrackRemove`,
    /// `commit`). Fix (post-review): clips are DOCUMENT state, part of
    /// `Project`, not effect-layer bookkeeping — `apply_raw`'s
    /// `Op::TrackRemove` arm now collects and removes them from store truth
    /// as part of the op itself, so the computed inverse (`Op::TrackAdd`)
    /// carries them back too (an undo restores clips, not just an empty
    /// track), and there is NO store write outside `commit` for them.
    /// Round-2 §2.4: there is no slot to free either — slots are derived
    /// fresh from display order on every rebuild, so a removed row simply
    /// stops appearing in the next `GraphTables`; nothing is "freed" for a
    /// later `add_track` to alias (the O-13 defect this task fixes).
    /// `commit` sends the single `Rebuild` and recomputes `any_solo`
    /// (controller ruling 2) — the removed row may have been the only
    /// soloed track.
    pub fn remove_track(&self, id: &str, meta: op::TxMeta) -> Result<(), String> {
        let track = {
            let session = self.session.lock();
            session
                .store
                .tracks
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| format!("unknown track: {id}"))?
        };
        self.commit(meta, |tx| {
            tx.apply(op::Op::TrackRemove { track, index: 0, clips: vec![], clip_indices: vec![] })
        })?;
        Ok(())
    }

    /// Batched mix changes through the transaction channel: one `Op::Set`
    /// per present field, applied atomically (an unknown track id fails —
    /// and rolls back — the whole batch, same as the retired
    /// `ops::apply_track_mix` (deleted in Plan B, behavior preserved here)).
    /// Param-table writes only, no graph rebuild (§10.2) — `Op::Set`'s
    /// effect never sets `rebuild`.
    ///
    /// `commit` emits `project://changed` on success. The Tauri command path
    /// doesn't strictly need it (the frontend patches its store optimistically
    /// and gets the updated `TrackState`s back from the invoke), but
    /// non-webview front doors — the MCP tools — have no return channel into
    /// the UI: without this event an agent's gain/pan change is applied and
    /// audible yet invisible in the mixer.
    pub fn set_track_mix(
        &self,
        changes: Vec<TrackMixChange>,
        meta: op::TxMeta,
    ) -> Result<Vec<TrackState>, String> {
        // Validate every id BEFORE writing anything: `Session::transact`
        // already rolls back a failed batch, but a change with every field
        // `None` never calls `tx.apply`, so an unknown id in an all-None
        // change would otherwise slip past a rollback that only guards
        // ops that were actually applied. Pre-checking (as the retired
        // `ops::apply_track_mix`, deleted in Plan B, did) keeps ALL bad ids
        // failing the batch atomically, before any commit.
        {
            let session = self.session.lock();
            for c in &changes {
                if !session.store.tracks.iter().any(|t| t.id == c.track_id) {
                    return Err(format!("unknown track: {}", c.track_id));
                }
            }
        }
        self.commit(meta, |tx| {
            for c in &changes {
                if let Some(g) = c.gain_db {
                    tx.apply(set_prop(&c.track_id, op::PropPath::Gain, serde_json::json!(g)))?;
                }
                if let Some(p) = c.pan {
                    tx.apply(set_prop(&c.track_id, op::PropPath::Pan, serde_json::json!(p)))?;
                }
                if let Some(m) = c.muted {
                    tx.apply(set_prop(&c.track_id, op::PropPath::Muted, serde_json::json!(m)))?;
                }
                if let Some(s) = c.soloed {
                    tx.apply(set_prop(&c.track_id, op::PropPath::Soloed, serde_json::json!(s)))?;
                }
                if let Some(a) = c.armed {
                    tx.apply(set_prop(&c.track_id, op::PropPath::Armed, serde_json::json!(a)))?;
                }
            }
            Ok(())
        })?;
        // Re-lock (a fresh acquisition, separate from the pre-check and the
        // commit above) to read back the post-commit state. A track that
        // existed at the pre-check can vanish here if a concurrent
        // `remove_track` lands between `commit` returning and this lock —
        // `.expect("validated above")` used to panic a command thread on
        // that race. `filter_map` instead treats a vanished row as simply
        // absent from "the final post-batch state": every OTHER requested
        // track's up-to-date row is still returned, matching this method's
        // contract (`Vec<TrackState>` reflecting store state after the
        // batch), and the caller already gets `project://changed` off the
        // authoritative post-commit store regardless.
        let session = self.session.lock();
        let updated: Vec<TrackState> = changes
            .iter()
            .filter_map(|c| session.store.tracks.iter().find(|t| t.id == c.track_id).cloned())
            .collect();
        Ok(updated)
    }

    /// Move a clip on the timeline through the transaction channel
    /// (`Op::Set`, `PropPath::TimelineStartSamples`) — Plan E Task 3
    /// (round-2 inventory row 3). A thin `commit` wrapper mirroring
    /// `set_track_mix`'s shape. NOTE: the Tauri `move_clip` COMMAND is
    /// Task 4's job, not this one — only this `ControlPlane` method lands
    /// here.
    pub fn move_clip(
        &self,
        clip_id: &str,
        timeline_start_samples: u64,
        meta: op::TxMeta,
    ) -> Result<session::Committed, String> {
        self.commit(meta, |tx| {
            tx.apply(op::Op::Set {
                object: op::ObjectRef::Clip(clip_id.into()),
                path: op::PropPath::TimelineStartSamples,
                from: serde_json::Value::Null,
                to: serde_json::json!(timeline_start_samples),
            })
        })
    }

    /// Bind (or unbind, with `instrument_id: None`) a track's instrument
    /// through the transaction channel (`Op::Set`, `PropPath::InstrumentId`)
    /// — Plan E Task 3 (round-2 inventory row 2). `set_track_instrument`
    /// (audio/mod.rs) is a thin wrapper over this, keeping its own
    /// track-kind/plugin-existence validation ahead of the call.
    pub fn set_track_instrument(
        &self,
        track_id: &str,
        instrument_id: Option<String>,
        meta: op::TxMeta,
    ) -> Result<TrackState, String> {
        self.commit(meta, |tx| {
            tx.apply(op::Op::Set {
                object: op::ObjectRef::Track(track_id.into()),
                path: op::PropPath::InstrumentId,
                from: serde_json::Value::Null,
                to: serde_json::json!(instrument_id),
            })
        })?;
        // Re-lock (a fresh acquisition, separate from `commit`) to read
        // back the post-commit row — same pattern `set_track_mix` uses.
        let session = self.session.lock();
        session
            .store
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .cloned()
            .ok_or_else(|| format!("unknown track: {track_id}"))
    }

    /// Compose the full-project payload `project://changed` carries (the
    /// same shape `project::from_store` serializes, minus its requirement of
    /// an open project dir — mix/structural changes are legal in an unsaved
    /// session). `commit`'s event emission (Task 7: every A-slice command
    /// now goes live through it) is the sole caller; `create_project` builds
    /// its own `Project` from `project::create`'s return instead, since that
    /// one always has an open project dir.
    fn project_changed_payload(&self) -> Project {
        let session = self.session.lock();
        let s = &session.store;
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
    }

    /// Runs a `Session::transact` closure, then — with the session lock
    /// RELEASED — executes the folded `EngineEffect`: param writes resolved
    /// through `self.tables` (the CURRENT graph's tables — round-2 §2.4),
    /// at most one `ControlMsg::Rebuild`, and exactly one `project://changed`
    /// event. `project://changed` is a FROZEN event whose payload contract
    /// is the full `Project` shape (project.schema.json; ARCHITECTURE §3.4)
    /// — this carries EXACTLY that (via `project_changed_payload`, the same
    /// serialization `create_project` uses), with `rev`/`label`/`actor`
    /// folded in as ADDITIVE top-level fields (D-06: readers ignore fields
    /// they don't recognize).
    ///
    /// Zero engine/param-table/event calls happen while the SESSION lock is
    /// held — `Session::transact` (session.rs) only ever computes the effect
    /// DESCRIPTION; everything below this comment runs after it returns
    /// (`project_changed_payload` takes its own fresh, separate lock).
    ///
    /// A track without a slot yet (`self.tables.lock().slots` doesn't have
    /// it) is skipped — sound ONLY because `rebuild` publishes `GraphTables`
    /// INSIDE the session lock it holds while reading the store [C1]:
    /// either this commit's `Session::transact` above ran BEFORE that
    /// rebuild read the document (so the fresh tables already bake this
    /// write's value in), or it ran AFTER the rebuild published (so the
    /// write below executes against the new table). There is no window
    /// where a commit's own write can be silently lost. Same reasoning
    /// covers `any_solo`.
    pub fn commit<F>(&self, meta: op::TxMeta, f: F) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
    {
        self.commit_with(meta, f, true)
    }

    /// Same as [`Self::commit`], but the caller controls whether the frozen
    /// `project://changed` event fires (Plan E Task 12). `commit` delegates
    /// here with `emit_project_changed: true`; `ControlPlane::transport`
    /// passes `false` — `project://changed`'s payload contract is the full
    /// `Project` shape (project.schema.json), and firing it once per
    /// play/stop/loop-drag transport commit would be a behavior change
    /// from today's `transport://state`-only contract. Transport commits
    /// still bump `rev` and still run through the full effect pipeline
    /// below (param writes / rebuild / persist) exactly like any other
    /// commit — `emit_project_changed` only gates the LAST step.
    pub fn commit_with<F>(
        &self,
        meta: op::TxMeta,
        f: F,
        emit_project_changed: bool,
    ) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
    {
        let committed = Session::transact(&self.session, meta, f)?;
        // ---- session lock is released here; everything below executes
        // the effect the session merely described. ----
        {
            let tables = self.tables.lock();
            for (tid, path, value) in &committed.effect.param_writes {
                let Some(&slot) = tables.slots.get(tid) else { continue };
                match path {
                    op::PropPath::Gain => tables.params.set_gain_linear(slot, *value),
                    op::PropPath::Pan => tables.params.set_pan(slot, *value),
                    op::PropPath::Muted => tables.params.set_flag(slot, FLAG_MUTE, *value != 0.0),
                    op::PropPath::Soloed => tables.params.set_flag(slot, FLAG_SOLO, *value != 0.0),
                    // No ParamTable counterpart for Armed (the retired
                    // apply_track_mix, deleted in Plan B, didn't write one
                    // either), nor for InstrumentId/TimelineStartSamples
                    // (Plan E Task 3): `apply_raw` never pushes those two
                    // into `param_writes` in the first place (they're
                    // structural — rebuild, not a param-table write), so
                    // this arm is unreachable in practice; kept as an
                    // honest no-op rather than a `todo!()` so a future path
                    // added here without a `param_writes` producer doesn't
                    // panic a live commit.
                    // Plan E Task 5: same reasoning for the three MidiClip
                    // paths — `apply_raw` never pushes them into
                    // `param_writes` either (structural: rebuild only).
                    // Plan E Task 12: same reasoning again for the six
                    // Transport paths — the Transport `apply_raw` arm
                    // (session.rs) never pushes anything into
                    // `param_writes` (it isn't `TrackId`-keyed at all).
                    op::PropPath::Armed
                    | op::PropPath::InstrumentId
                    | op::PropPath::TimelineStartSamples
                    | op::PropPath::TimelineStartTicks
                    | op::PropPath::LengthTicks
                    | op::PropPath::ContentLengthTicks
                    | op::PropPath::TransportState
                    | op::PropPath::LoopEnabled
                    | op::PropPath::LoopStartSamples
                    | op::PropPath::LoopEndSamples
                    | op::PropPath::StopAtEnd
                    | op::PropPath::SampleRate => {}
                }
            }
            if let Some(any_solo) = committed.effect.any_solo {
                tables.params.any_solo.store(any_solo, Relaxed);
            }
        }
        if committed.effect.rebuild {
            self.engine.send(ControlMsg::Rebuild);
        }
        // Persist runs after the effect writes above and BEFORE the
        // `project://changed` emit below — the event announces durable
        // truth, so persistence must have already happened by the time it
        // fires (round-2 §4: persistence is an effect, executed here, never
        // I/O under the session lock).
        if committed.effect.persist != session::PersistEffect::default() {
            self.execute_persist(&committed.effect.persist, committed.epoch);
        }
        // Full-Project payload (the frozen contract) + rev/label/actor as
        // additive fields (D-06). `project_changed_payload` serializes to a
        // JSON object (all of `Project`'s fields are named), so inserting
        // extra keys is safe; the `unwrap_or_default` fallback (an empty
        // object) only matters if serialization itself somehow failed.
        //
        // Plan E Task 12: gated by `emit_project_changed` — transport
        // commits pass `false` and rely on `ControlPlane::transport`'s own
        // `transport://state` emit instead (see `commit_with`'s doc).
        if emit_project_changed {
            let mut payload = serde_json::to_value(self.project_changed_payload())
                .unwrap_or_else(|_| serde_json::json!({}));
            if let serde_json::Value::Object(map) = &mut payload {
                map.insert("rev".into(), serde_json::json!(committed.rev));
                map.insert("label".into(), serde_json::json!(committed.meta.label));
                map.insert(
                    "actor".into(),
                    serde_json::to_value(&committed.meta.actor).unwrap_or_default(),
                );
            }
            (self.emit)("project://changed", payload);
        }
        Ok(committed)
    }

    /// Executes a `PersistEffect` `commit` merely described. Snapshots are
    /// taken under a fresh, SHORT session lock; ALL disk I/O happens after
    /// the guard drops — round-2 §4's whole point (persistence is an
    /// effect, not I/O under the lock). No public trigger sets
    /// `effect.persist` yet (that arrives with later tasks' apply_raw arms);
    /// `pub(crate)` so tests can construct a `PersistEffect` and call this
    /// directly in the meantime.
    ///
    /// `committed_epoch`: the `Committed.epoch` the triggering commit
    /// captured under `Session::transact`'s lock (fix round 1, Task 7
    /// review finding 2). Re-checked against the CURRENT `session.epoch`
    /// under the fresh lock this fn takes below — a mismatch means an epoch
    /// function (project open/create/save-as) swapped the document AFTER
    /// this commit's `transact` returned but BEFORE this re-lock, so the
    /// snapshot this fn would take belongs to a DIFFERENT document than the
    /// one `p` describes; persisting it would be silent data loss either way
    /// (corrupting the new document, or dropping this commit's edit — see
    /// `Session::epoch`'s doc). Direct test callers of this fn (that don't
    /// go through `commit`) should pass the session's current epoch.
    pub(crate) fn execute_persist(&self, p: &session::PersistEffect, committed_epoch: u64) {
        let (dir, epoch_now, midi_snapshot, project_snapshot, automation_snapshot) = {
            let s = self.session.lock();
            (
                s.store.project_dir.clone(),
                s.epoch,
                p.midi.then(|| s.midi_snapshot()),
                p.project.then(|| {
                    project::from_store(
                        &s.store,
                        self.shared.position.load(Relaxed),
                        self.shared.sample_rate.load(Relaxed),
                    )
                }),
                // Plan E Task 10: snapshot taken under this SAME short lock
                // as the midi/project snapshots above; the actual write
                // (chunk files + `automation[]` RMW + chunk GC) happens
                // below, after the guard drops — round-2 §4: no disk I/O
                // under the session lock.
                p.automation.then(|| s.automation.lanes.clone()),
            )
        };
        if epoch_now != committed_epoch {
            log::warn!(
                "persist skipped: epoch changed between commit and persist ({committed_epoch} -> \
                 {epoch_now}) — the epoch's own save owns durability now"
            );
            return;
        }
        let Some(dir) = dir else { return }; // unsaved in-memory project
        if let Some(m) = midi_snapshot {
            if let Err(e) = crate::midi::persist::save_snapshot_into_project(&dir, &m) {
                log::warn!("midi persist failed: {e}");
                self.session.lock().midi.dirty = true; // M-5 semantics preserved
            } else {
                self.session.lock().midi.dirty = false;
            }
        }
        if let Some(pr) = project_snapshot {
            match pr {
                Ok(pr) => {
                    if let Err(e) = project::save(&dir, &pr) {
                        log::warn!("project save failed: {e}");
                    }
                }
                Err(e) => log::warn!("project snapshot build failed: {e}"),
            }
        }
        if let Some(lanes) = automation_snapshot {
            if let Err(e) = crate::plugins::automation::save_into_project(&dir, &lanes) {
                log::warn!("automation persist failed: {e}");
            }
        }
    }

    /// Test-only accessor to the shared session lock, for tests that need
    /// to assert on/mutate store state directly around a `commit`-driven
    /// call (Task 7 brief).
    #[cfg(test)]
    pub fn session(&self) -> &Arc<Mutex<Session>> {
        &self.session
    }

    /// New Project = blank slate: the previous session's tracks, clips, midi
    /// state, plugin/automation registries, and transport are all reset, and
    /// the freshly created `.aura` dir becomes the open project. (Until
    /// 2026-08 this kept the session's tracks; the UI "New Project" flow
    /// wants a true blank, and materializing an unsaved session is
    /// [`Self::save_project_as`]'s job.)
    ///
    /// Kept at this 2-arg shape (frontend's file-dialog-picked `parent_dir`
    /// + `name`, MCP tool `create_project`'s params, and this crate's own
    /// tests all call it this way) — [`Self::create_project_epoch`] is the
    /// Task 6-literal, dir-free additive sibling for a caller with no folder
    /// to hand it; both share [`Self::create_project_at`]'s body.
    pub fn create_project(&self, parent_dir: &str, name: &str) -> Result<Project, String> {
        self.create_project_at(Path::new(parent_dir), name)
    }

    /// Epoch boundary (round-2 §4.5 carve-out, "document birth"): same
    /// blank-slate reset as [`Self::create_project`], for a caller with no
    /// user-picked folder — resolves the same default location
    /// `ensure_project_epoch`/the engine's auto-project use
    /// (`project::default_project_parent`), and mints an "Untitled"/
    /// "Untitled-N" name when `name` is `None`.
    pub fn create_project_epoch(&self, name: Option<String>) -> Result<Project, String> {
        let parent = project::default_project_parent()?;
        let name = name.unwrap_or_else(|| project::unique_untitled_name(&parent));
        self.create_project_at(&parent, &name)
    }

    /// Shared body: `create_project`/`create_project_epoch`'s only
    /// difference is where `dir` comes from. Sequencing matches the epoch
    /// contract: (1) short lock — swap store fields + reset midi to blank
    /// defaults; (2) drop lock; (3) adopt plugins + automation from `dir`
    /// (host round-trips OUTSIDE the session lock); (4) Rebuild; (5) emit
    /// `project://changed`.
    fn create_project_at(&self, parent_dir: &Path, name: &str) -> Result<Project, String> {
        let rate = self.shared.sample_rate.load(Relaxed);
        let tempo = self.session.lock().store.transport.tempo_bpm;
        let (project, dir) = project::create(parent_dir, name, rate, tempo)?;
        {
            // One `session` guard for the store reset, the tables reset
            // (session-before-tables is already the documented lock order
            // [C1], so nesting `self.tables.lock()` here is sound), and the
            // midi reset — all three touch state gated by the same lock and
            // nothing in between needs it released.
            let mut session = self.session.lock();
            let s = &mut session.store;
            // Round-2 §2.4: nothing to free — slots are derived fresh from
            // display order on the next rebuild, which an empty track list
            // trivially satisfies.
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

            // Immediate reset ahead of the async Rebuild below (which will
            // publish fresh, empty tables anyway once processed) — keeps
            // `any_solo` from reading stale-true between here and then.
            self.tables.lock().params.any_solo.store(false, Relaxed);
            self.shared.playing.store(false, Relaxed);
            self.shared.position.store(0, Relaxed);
            self.shared.loop_enabled.store(false, Relaxed);

            // epoch boundary: Task 17 hooks history-clear + journal rotation here
            // Fix round 1 (Task 7 review finding 2): bump the document-swap
            // epoch counter here — see `Session::epoch`'s doc.
            session.epoch += 1;

            // Blank midi state bound to the new dir; `adopt_midi_dir` then
            // sees loaded_dir == dir and leaves it alone. Same lock as
            // `store` now (session merge) — no separate `self.midi` field.
            let d0 = crate::midi::persist::v1_migration_defaults(tempo);
            session.midi.ppq = d0.ppq;
            session.midi.tempo_events = d0.tempo_events;
            session.midi.clips = d0.clips;
            // Finding 2: a stale `dirty = true` left over from a prior
            // auto-persist failure (M-5) must not survive into this fresh
            // project — otherwise the first midi mutation here persists a
            // BLANK store over this project's real midi (the guard added to
            // `with_synced_store` for finding 1 would otherwise be fooled:
            // `loaded_dir` is set correctly above, so it WOULD persist, and
            // dirty=true is only meant to block resync-from-disk, not writes).
            adopt_midi_dir(&mut session.midi, &dir);
        }
        // ---- session lock released; host round-trips + rebuild + emit below ----
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

    /// Open an existing `.aura` project (or a direct `project.json` path) —
    /// epoch boundary (round-2 §4.5 carve-out, "document swap, history
    /// root"). Sequencing: (1) short lock — swap store fields + midi from
    /// the parsed project; (2) drop lock; (3) adopt plugins + automation
    /// from `dir` (host round-trips OUTSIDE the session lock); (4) Rebuild;
    /// (5) emit `project://changed`. Absorbs `audio::open_project_impl`'s
    /// former body; the Tauri `open_project` command is now a one-line
    /// delegate.
    pub fn open_project_epoch(&self, dir: &Path) -> Result<Project, String> {
        let (project, dir) = project::load(dir)?;
        // Validate BEFORE mutating any in-memory state (review fix carried
        // over: a project with duplicate track ids must fail cleanly, not
        // after tracks/clips were replaced).
        project::validate(&project)?;
        {
            let mut session = self.session.lock();
            session.store.tracks = project.tracks.clone();
            session.store.clips = project.clips.clone();
            session.store.project_dir = Some(dir.clone());
            session.store.project_name = Some(project.name.clone());
            session.store.created_at = project.created_at.clone();
            if let Some(t) = &project.transport {
                session.store.transport.tempo_bpm = t.tempo_bpm;
                session.store.transport.state = "stopped".into();
                // Store mirror AND RT atomics for the loop region, so the
                // next save round-trips it (from_store serializes
                // store.transport).
                session.store.transport.loop_enabled = t.loop_enabled;
                session.store.transport.loop_start_samples = t.loop_start_samples;
                session.store.transport.loop_end_samples = t.loop_end_samples;
                self.shared.playing.store(false, Relaxed);
                self.shared.position.store(t.position_samples, Relaxed);
                self.shared.loop_enabled.store(t.loop_enabled, Relaxed);
                self.shared.loop_start.store(t.loop_start_samples, Relaxed);
                self.shared.loop_end.store(t.loop_end_samples, Relaxed);
            }
            // epoch boundary: Task 17 hooks history-clear + journal rotation here
            // Fix round 1 (Task 7 review finding 2): bump the document-swap
            // epoch counter here — see `Session::epoch`'s doc.
            session.epoch += 1;
            // Eager midi adopt (Task 6: no more lazy resync on the first
            // midi command after an open) — same lock as the store swap
            // above, no separate re-acquisition.
            let bpm = session.store.transport.tempo_bpm;
            crate::midi::adopt_midi_from_dir(&mut session.midi, &dir, bpm);
        }
        // ---- session lock released; host round-trips + rebuild + emit below ----
        crate::plugins::state::adopt_open_project(&dir);
        crate::plugins::automation::adopt_open_project(&dir);
        self.engine.send(ControlMsg::Rebuild);
        (self.emit)("project://changed", serde_json::to_value(&project).unwrap_or_default());
        Ok(project)
    }

    /// First save of a session that never had a project: create the `.aura`
    /// dir and persist the CURRENT in-memory content (tracks, clips, midi)
    /// into it. Refuses when a project is already open — that is plain
    /// `save_project` territory, not a fork. Kept at this 2-arg shape (no
    /// other caller besides the frozen `save_project_as` command exists);
    /// [`Self::save_project_as_epoch`] does the actual swap + I/O once
    /// `dir` is minted, fixing the lock-then-write bug this method used to
    /// have (the midi write ran under the session lock).
    pub fn save_project_as(&self, parent_dir: &str, name: &str) -> Result<Project, String> {
        let tempo = {
            let session = self.session.lock();
            if session.store.project_dir.is_some() {
                return Err("a project is already open; use save_project".into());
            }
            session.store.transport.tempo_bpm
        };
        let rate = self.shared.sample_rate.load(Relaxed);
        let (_created, dir) = project::create(Path::new(parent_dir), name, rate, tempo)?;
        self.save_project_as_epoch(&dir)
    }

    /// Epoch boundary: materialize the CURRENT in-memory session (tracks,
    /// clips, midi) into `dir`, an ALREADY-CREATED `.aura` directory (minted
    /// by [`Self::save_project_as`] via `project::create`, which also wrote
    /// its own initial `project.json` — read back here for `created_at`).
    /// Fixes control/mod.rs:659 (round-2 inventory row 26): the midi
    /// snapshot is taken under a short lock and WRITTEN to disk only after
    /// the lock drops — no disk I/O ever runs while the session lock is
    /// held.
    pub fn save_project_as_epoch(&self, dir: &Path) -> Result<Project, String> {
        let name = dir
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        let created_at = project::load(dir).ok().and_then(|(p, _)| p.created_at);
        let rate = self.shared.sample_rate.load(Relaxed);
        let position = self.shared.position.load(Relaxed);
        let (project, midi_snapshot) = {
            let mut session = self.session.lock();
            session.store.project_dir = Some(dir.to_path_buf());
            session.store.project_name = Some(name);
            session.store.created_at = created_at;
            // epoch boundary: Task 17 hooks history-clear + journal rotation here
            // Fix round 1 (Task 7 review finding 2): bump the document-swap
            // epoch counter here — see `Session::epoch`'s doc.
            session.epoch += 1;
            // Mark the midi store as belonging to `dir` NOW (under the same
            // lock as the store swap); the snapshot taken alongside it is
            // written to disk below, AFTER the lock drops.
            session.midi.loaded_dir = Some(dir.to_path_buf());
            let project = project::from_store(&session.store, position, rate)?;
            let midi_snapshot = session.midi_snapshot();
            (project, midi_snapshot)
        };
        // ---- session lock released; all disk I/O below ----
        project::save(dir, &project)?;
        match crate::midi::persist::save_snapshot_into_project(dir, &midi_snapshot) {
            Ok(()) => self.session.lock().midi.dirty = false,
            Err(e) => {
                self.session.lock().midi.dirty = true;
                log::warn!("save_project_as_epoch: persisting midi failed: {e}");
            }
        }
        (self.emit)(
            "project://changed",
            serde_json::to_value(&project).unwrap_or_default(),
        );
        Ok(project)
    }

    /// Snapshot mark (today's `save_project`, sanctioned): persist the
    /// CURRENT in-memory document to its already-open project dir. Not a
    /// document swap (no dir change, nothing to adopt from disk — the
    /// project already open is exactly the one being written), so there is
    /// no plugin/automation adopt step here, only the write. Absorbs
    /// `audio::save_project_impl`'s former body; the Tauri `save_project`
    /// command is now a one-line delegate (its frozen `Result<(), String>`
    /// shape discards the returned `Project`).
    pub fn save_project_mark(&self) -> Result<Project, String> {
        let rate = self.shared.sample_rate.load(Relaxed);
        let position = self.shared.position.load(Relaxed);
        let (project, dir) = {
            let session = self.session.lock();
            let dir = session.store.project_dir.clone().ok_or("no project open")?;
            let project = project::from_store(&session.store, position, rate)?;
            (project, dir)
        };
        // epoch boundary: no document swap here (same project, same
        // in-memory content) — Task 17 still journals a "save" mark record.
        project::save(&dir, &project)?;
        (self.emit)(
            "project://changed",
            serde_json::to_value(&project).unwrap_or_default(),
        );
        Ok(project)
    }

    /// Engine auto-project, as a sanctioned `ControlPlane` epoch fn (round-2
    /// §4.5 carve-out, "document birth") — same behavior as the engine
    /// control thread's own `ensure_project` (engine.rs), sharing its core
    /// via `project::ensure_default_project` (the store swap, and its Task
    /// 17 epoch-boundary marker, live there — this fn does no store write
    /// of its own). NOT yet wired to the engine (Task 13 switches that call
    /// site over). No-op (just returns the already-open dir) when a project
    /// is already open.
    ///
    /// Deliberately skips steps (3) adopt and (4) Rebuild of the 5-step
    /// epoch contract the other four functions follow — this is NOT an
    /// oversight, it's why this fn exists as a separate, narrower epoch:
    /// (a) `ensure_default_project` only ever swaps when
    ///     `store.project_dir` was `None` — there is no PRIOR project's
    ///     plugin/automation state to evict, so adopting from the fresh
    ///     (empty) dir would be a no-op against an already-empty
    ///     registry/lane set;
    /// (b) no graph rebuild is needed because "document birth" here changes
    ///     no tracks/clips — the store's musical content is UNCHANGED, only
    ///     `project_dir`/`project_name`/`created_at` are set, so there is
    ///     nothing for a rebuild to pick up that the current graph doesn't
    ///     already have;
    /// (c) this fn's whole reason to exist is byte-for-byte parity with the
    ///     engine's current `ensure_project` (Task 13 routes the engine's
    ///     own call site through here) — adding adopt/Rebuild would make it
    ///     diverge from the behavior it's standing in for. A future
    ///     standalone caller (MCP tool, command) that actually needs
    ///     adopt/Rebuild semantics on project birth should call
    ///     `create_project_at` (via `create_project`/`create_project_epoch`),
    ///     not this one.
    pub fn ensure_project_epoch(&self) -> Result<PathBuf, String> {
        let rate = self.shared.sample_rate.load(Relaxed);
        match project::ensure_default_project(&self.session, rate)? {
            Some(project) => {
                (self.emit)(
                    "project://changed",
                    serde_json::to_value(&project).unwrap_or_default(),
                );
                Ok(PathBuf::from(
                    project.path.clone().expect("just-created project has a path"),
                ))
            }
            None => Ok(self
                .session
                .lock()
                .store
                .project_dir
                .clone()
                .expect("ensure_default_project returns None only when a project is already open")),
        }
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
        // Task 6: no lazy resync needed here — eager epoch adoption
        // (open/create/save-as/ensure-project) keeps the midi store synced
        // to `project_dir` at all times, so the open project's on-disk
        // state is never at risk of being clobbered by a stale in-memory
        // copy the way a lazy resync used to guard against.
        {
            let session = self.session.lock();
            if !session.store.clips.is_empty()
                || session.midi.clips.iter().any(|c| !c.notes.is_empty())
            {
                return Err("project already has content".to_string());
            }
        }

        // Zyn upgrade path: build the three patched instances BEFORE the
        // transaction so a failure leaves no half-bound state (None =
        // PolySynth).
        let zyn = try_seed_zyn_demo_instruments();

        // Task 7: one commit — 3x add_track_tx, the instrument bindings (if
        // Zyn is available), and the 3 demo clips, all through the channel.
        // `persist.project` (set only by the InstrumentId `Set`s below, same
        // as the pre-Task-7 code's zyn-gated project::save) and
        // `persist.midi` (set unconditionally by `MidiClipAdd`, same as the
        // pre-Task-7 code's unconditional `save_into_project`) replace the
        // manual saves; `commit` also emits `project://changed`, fixing this
        // command's previously missing event.
        self.commit(op::TxMeta::system("seed demo project"), |tx| {
            let pad = ops::add_track_tx(tx, Some("Demo Pad".into()), Some("midi".into()))?;
            let lead = ops::add_track_tx(tx, Some("Demo Lead".into()), Some("midi".into()))?;
            let bass = ops::add_track_tx(tx, Some("Demo Bass".into()), Some("midi".into()))?;

            if let Some(ids) = &zyn {
                for (track_id, instance_id) in
                    [(&pad.id, &ids[0]), (&lead.id, &ids[1]), (&bass.id, &ids[2])]
                {
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::Track(track_id.clone()),
                        path: op::PropPath::InstrumentId,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(format!("plugin:{instance_id}")),
                    })?;
                }
            }

            let ppq = tx.midi().ppq;
            let (pad_clip, lead_clip, bass_clip) =
                demo_seed_clips_v2(pad.id.as_str(), lead.id.as_str(), bass.id.as_str(), ppq);
            for clip in [pad_clip, lead_clip, bass_clip] {
                let index = tx.midi().clips.len();
                tx.apply(op::Op::MidiClipAdd { clip, index })?;
            }
            Ok(())
        })?;

        // Plugin instance + state blobs (not the `PersistEffect` machinery's
        // job yet — Tasks 9/10), so a save/open cycle replays the same demo
        // through the same patches (zone P4 restore path). Unchanged from
        // pre-Task-7: a no-op when there's no open project dir
        // (`persist_after_mutation` checks internally) or no Zyn instances.
        if zyn.is_some() {
            if let Some(reg) = crate::plugins::registered_registry() {
                crate::plugins::state::persist_after_mutation(&self.session, reg, true);
            }
        }
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
    use crate::ids::NoteId;
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
                note_id: NoteId(0),
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
                note_id: NoteId(0),
            });
        }
    }
    let clip = |track_id: &str, name: &str, notes: Vec<MidiNote>| {
        let mut c = MidiClip {
            id: crate::ids::ClipId::mint(),
            track_id: track_id.into(),
            name: name.to_string(),
            timeline_start_ticks: 0,
            length_ticks: 4 * bar as u64,
            notes,
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track_id),
            content_length_ticks: None,
        };
        c.ensure_note_ids().expect("demo notes never collide");
        c
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
    use crate::ids::NoteId;
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
                note_id: NoteId(0),
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
                note_id: NoteId(0),
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
                note_id: NoteId(0),
            });
        }
    }

    let clip = |track_id: &str, name: &str, notes: Vec<MidiNote>| {
        let mut c = MidiClip {
            id: crate::ids::ClipId::mint(),
            track_id: track_id.into(),
            name: name.to_string(),
            timeline_start_ticks: 0,
            length_ticks: 4 * bar as u64,
            notes,
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track_id),
            content_length_ticks: None,
        };
        c.ensure_note_ids().expect("demo notes never collide");
        c
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
    control.set_track_mix(changes, op::TxMeta::user("set track mix"))
}

/// Move a clip on the timeline — thin delegate over
/// [`ControlPlane::move_clip`] (Plan E Task 3's channel path), mirroring
/// `set_track_mix`'s State/ControlPlane access shape. The frontend drags a
/// clip locally (`project.moveClip`, a live preview only) and calls this
/// once at gesture end (`project.commitClipMove`), same split as the MIDI
/// clip's `midi.moveClip`/`midi_set_clip_bounds` pair.
#[tauri::command]
pub fn move_clip(
    clip_id: String,
    timeline_start_samples: u64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.move_clip(&clip_id, timeline_start_samples, op::TxMeta::user("move clip")).map(|_| ())
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
    use crate::audio::rt::{testutil::empty_tables, GraphTables, ParamTable};
    use crate::audio::types::{derive_slots, Store};
    use crate::midi::MidiStore;
    use crate::sidecars::SidecarEvent;
    use std::time::{Duration, Instant};

    /// Recorded app events + a ControlPlane whose emitter records into them
    /// (real engine, headless-safe — the shared harness for the event-parity
    /// tests below).
    type RecordedEvents = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    // ---- Task 6 helpers: commit() folding (for_tests engine double) ----

    use crate::control::op::{ObjectRef, Op, PropPath, TxMeta};

    use crate::audio::types::testutil::{test_clip, test_track};
    use crate::control::op::testutil::set_gain;

    /// A `ControlPlane` wired to `EngineHandle::for_tests()` (no real engine
    /// thread — just a channel that records sent `ControlMsg`s) and a
    /// Vec-capturing event emitter, seeded with one slotted track per given
    /// id. Used by `commit()`'s folding tests.
    ///
    /// [M2] There is no real engine thread here, so nothing ever calls
    /// `rebuild` to publish `GraphTables` — without seeding one by hand,
    /// `self.tables` would stay the empty gen-0 default and EVERY param
    /// write made through this harness would silently skip (`commit`
    /// resolves `TrackId -> slot` through `self.tables.slots`, which would
    /// have no entries). Publish a table matching the seeded tracks up
    /// front so the next person asserting `params.gain` post-commit gets a
    /// real failure instead of a silent no-op.
    fn test_plane_with_tracks(
        ids: &[&str],
    ) -> (ControlPlane, crossbeam_channel::Receiver<ControlMsg>, RecordedEvents) {
        let mut store = Store::default();
        for &id in ids {
            store.tracks.push(test_track(id));
        }
        let session = Arc::new(Mutex::new(Session::new(store, MidiStore::default())));
        let shared = Arc::new(SharedRt::default());
        let tables: SharedGraphTables = Arc::new(Mutex::new(GraphTables {
            generation: 1,
            params: Arc::new(ParamTable::default()),
            slots: derive_slots(&session.lock().store.tracks),
        }));
        let (engine, engine_rx) = EngineHandle::for_tests();
        let events: RecordedEvents = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let cp = ControlPlane::new(
            session,
            shared,
            tables,
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Box::new(move |e, p| sink.lock().push((e.to_string(), p))),
        );
        (cp, engine_rx, events)
    }

    /// Round-2 O-13: the alias window (Gate B, Task 8 step 2). Sequence:
    /// graph gen-N plays track X. X is removed and Y added (both through the
    /// channel); the rebuild for gen-N+1 is queued but the callback has NOT
    /// adopted it. A param write to Y (via commit) must land in gen-N+1's
    /// table only; gen-N's table — which the still-playing old graph reads —
    /// must be untouched. With Store-owned slots this was the aliasing bug;
    /// with per-graph tables it is impossible, and this test pins it at the
    /// `ControlPlane` level.
    ///
    /// In-crate (not `tests/identity_properties.rs`, controller ruling for
    /// Task 8): `EngineHandle::for_tests` is `#[cfg(test)]`-gated, reachable
    /// only when the crate compiles its own test target, not when pulled in
    /// as a library dependency of an integration-test binary under `tests/`
    /// — widening its visibility via a Cargo feature was ruled out for this
    /// task, so the test lives here instead, alongside `test_plane_with_tracks`.
    #[test]
    fn old_graph_never_sees_the_new_tracks_params() {
        use crate::audio::mixer::db_to_linear;
        use crate::ids::TrackId;

        let track_x = TrackId::from("track-x");

        let mut store = Store::default();
        store.tracks.push(test_track(track_x.as_str()));
        let session = Arc::new(Mutex::new(Session::new(store, MidiStore::default())));

        // gen-1 tables: X on slot 0, gain -6 dB (linear, as ParamTable
        // stores it — `ControlPlane::commit` resolves `Op::Set`'s Gain path
        // through `ParamTable::set_gain_linear`).
        let gen1_params = Arc::new(ParamTable::default());
        let x_gain_linear = db_to_linear(-6.0);
        gen1_params.set_gain_linear(0, x_gain_linear);
        let gen1_slots: std::collections::HashMap<TrackId, usize> =
            [(track_x.clone(), 0)].into_iter().collect();
        let tables: SharedGraphTables = Arc::new(Mutex::new(GraphTables {
            generation: 1,
            params: gen1_params.clone(),
            slots: gen1_slots,
        }));

        let shared = Arc::new(SharedRt::default());
        let (engine, _engine_rx) = EngineHandle::for_tests();
        let events: RecordedEvents = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);

        let plane = ControlPlane::new(
            session,
            shared,
            tables.clone(),
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Box::new(move |e, p| sink.lock().push((e.to_string(), p))),
        );

        // Real ops through the real channel — remove X, add Y — mirroring
        // round-2 O-13's actual sequence rather than manual store surgery.
        plane.remove_track(track_x.as_str(), TxMeta::user("remove x")).unwrap();
        let track_y_row =
            plane.add_track(Some("Y".into()), Some("audio".into()), TxMeta::user("add y")).unwrap();
        let track_y = track_y_row.id.clone();

        // Simulate the control thread's rebuild WITHOUT adopting a new graph
        // — this is the alias window itself: the rebuild for gen-2 is
        // "queued" (here: hand-published) but no callback has adopted it,
        // exactly as `engine::Control::rebuild` would publish under the
        // session lock (rule [C1]) before the RT thread ever sees the new
        // graph.
        let gen2_params = Arc::new(ParamTable::default());
        let gen2_slots: std::collections::HashMap<TrackId, usize> =
            [(track_y.clone(), 0)].into_iter().collect();
        *tables.lock() =
            GraphTables { generation: 2, params: gen2_params.clone(), slots: gen2_slots };

        // A param write to Y (through the real channel) must resolve
        // through gen-2's CURRENT table.
        let y_gain_db = -12.0;
        plane
            .commit(TxMeta::user("mix y"), |tx| {
                tx.apply(Op::Set {
                    object: ObjectRef::Track(track_y.clone()),
                    path: PropPath::Gain,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(y_gain_db),
                })
            })
            .unwrap();

        // gen-2's slot 0 changed to Y's write...
        let gen2_slot0_gain =
            f32::from_bits(gen2_params.gain[0].load(std::sync::atomic::Ordering::Relaxed));
        assert!(
            (gen2_slot0_gain - db_to_linear(y_gain_db)).abs() < 1e-4,
            "gen-2's table must carry Y's write (got {gen2_slot0_gain}, want ~{})",
            db_to_linear(y_gain_db)
        );

        // ...but the RETAINED gen-1 Arc — held by this test exactly the way
        // a still-rendering old `RtGraph` would hold it — must be untouched.
        // With Store-owned slots this was the aliasing bug (a freed-then-
        // reused slot 0 would show Y's gain under X's still-playing graph);
        // with per-graph tables there is nothing to free, so gen-1's Arc is
        // a wholly separate object and this assertion cannot fail by
        // construction.
        let gen1_slot0_gain =
            f32::from_bits(gen1_params.gain[0].load(std::sync::atomic::Ordering::Relaxed));
        assert!(
            (gen1_slot0_gain - x_gain_linear).abs() < 1e-6,
            "gen-1's retained table must be untouched by Y's write — this is the whole point of \
             per-graph tables (round-2 O-13); got {gen1_slot0_gain}, want ~{x_gain_linear}"
        );
    }

    /// The gate test (task-6 brief): a batch with two structural ops
    /// (`TrackAdd` x2) and one mix `Set` must fold to exactly one
    /// `ControlMsg::Rebuild` and emit exactly one `project://changed`, and
    /// both must land only AFTER the session lock is released (`commit`
    /// itself proves this by construction: it can't touch `self.tables` /
    /// `self.engine` / `self.emit` until `Session::transact` returns).
    #[test]
    fn three_ops_one_rebuild_one_event() {
        let (plane, engine_rx, events) = test_plane_with_tracks(&["t-1"]);
        plane
            .commit(TxMeta::user("batch"), |tx| {
                tx.apply(Op::TrackAdd { track: test_track("t-2"), index: 1, clips: vec![], clip_indices: vec![] })?;
                tx.apply(Op::TrackAdd { track: test_track("t-3"), index: 2, clips: vec![], clip_indices: vec![] })?;
                tx.apply(set_gain("t-1", 0.5))?;
                Ok(())
            })
            .unwrap();
        let rebuilds = engine_rx.try_iter().filter(|m| matches!(m, ControlMsg::Rebuild)).count();
        assert_eq!(rebuilds, 1, "two structural ops must fold to one Rebuild");
        assert_eq!(
            events.lock().iter().filter(|(n, _)| n == "project://changed").count(),
            1
        );
    }

    /// Task 7 brief, step 1: `remove_track` runs through the channel — the
    /// row disappears from the store and exactly one `Rebuild` is sent
    /// (today's asymmetry with `add_track`, which always went through
    /// `commit`, is gone).
    #[test]
    fn remove_track_goes_through_the_channel_and_rebuilds_once() {
        let (plane, engine_rx, _events) = test_plane_with_tracks(&["t-1", "t-2"]);
        plane.remove_track("t-1", TxMeta::user("remove track")).unwrap();
        assert!(plane.session().lock().store.tracks.iter().all(|t| t.id != "t-1"));
        assert_eq!(engine_rx.try_iter().filter(|m| matches!(m, ControlMsg::Rebuild)).count(), 1);
    }

    /// Plan E Task 3: `move_clip` runs through the channel — the store row
    /// updates and exactly one `Rebuild` is sent (structural, since a moved
    /// clip changes what the RT graph renders).
    #[test]
    fn move_clip_goes_through_the_channel_and_rebuilds_once() {
        let (plane, engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        {
            let mut session = plane.session().lock();
            session.store.clips.push(test_clip("c-1", "t-1"));
        }
        let committed = plane.move_clip("c-1", 48_000, TxMeta::user("move clip")).unwrap();
        assert!(committed.effect.rebuild);
        assert_eq!(plane.session().lock().store.clips[0].timeline_start_samples, 48_000);
        assert_eq!(engine_rx.try_iter().filter(|m| matches!(m, ControlMsg::Rebuild)).count(), 1);
    }

    /// Plan E Task 4 brief, step 1: `move_clip` emits `project://changed`
    /// carrying the clip's NEW position (the MCP-parity shape
    /// `set_track_mix_emits_project_changed_with_updated_tracks` already
    /// pins for track mixes) — the same webview-return-channel gap would
    /// otherwise leave an agent-driven clip move invisible in the UI. A
    /// move naming an unknown clip id must error cleanly and emit nothing.
    #[test]
    fn move_clip_emits_project_changed_with_new_position_and_errs_on_unknown_id() {
        let (cp, events, engine) = recording_control_plane();
        let track = cp
            .add_track(Some("Move Me".into()), None, TxMeta::user("add track"))
            .unwrap();
        {
            let mut session = cp.session().lock();
            session.store.clips.push(test_clip("c-1", track.id.as_str()));
        }
        events.lock().clear();

        cp.move_clip("c-1", 48_000, TxMeta::user("move clip")).unwrap();

        {
            let evs = events.lock();
            let payloads: Vec<&serde_json::Value> = evs
                .iter()
                .filter(|(name, _)| name == "project://changed")
                .map(|(_, p)| p)
                .collect();
            assert_eq!(payloads.len(), 1, "exactly one project://changed per move");
            let c = payloads[0]["clips"]
                .as_array()
                .expect("clips array")
                .iter()
                .find(|c| c["id"] == "c-1")
                .expect("moved clip in payload")
                .clone();
            assert_eq!(c["timelineStartSamples"], 48_000);
        }

        events.lock().clear();
        let result = cp.move_clip("no-such-clip", 1_000, TxMeta::user("move unknown"));
        assert!(result.is_err(), "moving an unknown clip id must error, not panic");
        assert!(
            events.lock().iter().all(|(name, _)| name != "project://changed"),
            "a failed move must not announce a change"
        );
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

    /// Plan E Task 3: `set_track_instrument` runs through the channel and
    /// returns the updated row (mirroring `set_track_mix`'s re-lock/read-back
    /// shape) with exactly one `Rebuild`.
    #[test]
    fn set_track_instrument_goes_through_the_channel_and_rebuilds_once() {
        let (plane, engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        let updated = plane
            .set_track_instrument("t-1", Some("plugin:x".into()), TxMeta::user("set instrument"))
            .unwrap();
        assert_eq!(updated.instrument_id, Some("plugin:x".to_string()));
        assert_eq!(plane.session().lock().store.tracks[0].instrument_id, Some("plugin:x".to_string()));
        assert_eq!(engine_rx.try_iter().filter(|m| matches!(m, ControlMsg::Rebuild)).count(), 1);
    }

    /// Controller ruling 2: removing the only soloed track through the
    /// channel must recompute the store-wide `any_solo` RT atomic (old
    /// `remove_track`, audio/mod.rs:378, did this too).
    #[test]
    fn remove_track_recomputes_any_solo() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1", "t-2"]);
        {
            let mut session = plane.session().lock();
            session.store.tracks[0].soloed = true;
        }
        plane.tables.lock().params.any_solo.store(true, Relaxed);
        plane.remove_track("t-1", TxMeta::user("remove soloed track")).unwrap();
        assert!(
            !plane.tables.lock().params.any_solo.load(Relaxed),
            "any_solo must go false"
        );
    }

    /// Post-review fix: clips are document state, not effect-layer
    /// bookkeeping — `Op::TrackRemove`'s effect must carry the removed
    /// track's clips so its computed inverse (`Op::TrackAdd`) can restore
    /// BOTH, not resurrect an empty track. Removes a track with two clips
    /// through the channel, confirms the clips left with the row, then
    /// replays the commit's own inverses through the SAME channel and
    /// asserts the row AND its clips come back byte-identically.
    #[test]
    fn remove_track_inverse_restores_row_and_clips_byte_identically() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1", "t-2"]);
        let (c1, c2) = (test_clip("clip-a", "t-1"), test_clip("clip-b", "t-1"));
        {
            let mut session = plane.session().lock();
            session.store.clips.push(c1.clone());
            session.store.clips.push(c2.clone());
        }
        let before = {
            let session = plane.session().lock();
            (session.store.tracks.clone(), session.store.clips.clone())
        };
        let track =
            { plane.session().lock().store.tracks.iter().find(|t| t.id == "t-1").cloned().unwrap() };

        let committed = plane
            .commit(TxMeta::user("remove"), |tx| {
                tx.apply(Op::TrackRemove { track, index: 0, clips: vec![], clip_indices: vec![] })
            })
            .unwrap();
        {
            let session = plane.session().lock();
            assert!(session.store.tracks.iter().all(|t| t.id != "t-1"));
            assert!(
                session.store.clips.iter().all(|c| c.track_id != "t-1"),
                "clips must leave the store WITH the track, not linger orphaned"
            );
        }

        plane
            .commit(TxMeta::user("undo"), |tx| {
                for op in committed.inverses.clone() {
                    tx.apply(op)?;
                }
                Ok(())
            })
            .unwrap();
        let after = {
            let session = plane.session().lock();
            (session.store.tracks.clone(), session.store.clips.clone())
        };
        assert_eq!(after, before, "row AND clips restored byte-identically");
    }

    fn recording_control_plane() -> (Arc<ControlPlane>, RecordedEvents, EngineHandle) {
        struct NullEvents;
        impl crate::audio::engine::EventSink for NullEvents {
            fn emit(&self, _e: &str, _p: serde_json::Value) {}
        }
        let shared = Arc::new(SharedRt::default());
        let tables = empty_tables();
        let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
        );
        let events: RecordedEvents = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let cp = Arc::new(ControlPlane::new(
            session,
            shared,
            tables,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
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
        let track = cp
            .add_track(Some("Agent Mix".into()), None, TxMeta::user("add track"))
            .unwrap();
        events.lock().clear();

        cp.set_track_mix(
            vec![TrackMixChange {
                gain_db: Some(-6.0),
                pan: Some(0.5),
                muted: Some(true),
                ..TrackMixChange::new(track.id.as_str())
            }],
            TxMeta::user("set track mix"),
        )
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
        assert!(cp
            .set_track_mix(vec![TrackMixChange::new("no-such-track")], TxMeta::user("bad batch"))
            .is_err());
        assert!(
            events.lock().iter().all(|(name, _)| name != "project://changed"),
            "failed batch must not announce a change"
        );
        engine.send(crate::audio::engine::ControlMsg::Shutdown);
    }

    /// Post-review fix: `set_track_mix`'s read-back used to `.expect()` a
    /// row that a concurrent `remove_track` could make vanish between the
    /// commit and the read-back lock, panicking a command thread. A truly
    /// concurrent repro of that exact window isn't cheap to make
    /// deterministic (it depends on interleaving two lock acquisitions
    /// inside one method with no test seam between them), so this test
    /// takes the cheap, honest route the fix description calls out
    /// explicitly: drive the track through a real mix change, remove it
    /// through the channel (same machinery a racing `remove_track` command
    /// would use), then confirm a mix call naming that now-gone id returns
    /// a clean `Err` — never a panic — rather than reaching the vanished
    /// row. It exercises the pre-check's guard for "id unknown by the time
    /// we look" and proves `set_track_mix` never panics on this shape of
    /// input; it does not, by itself, force execution through the
    /// `filter_map` read-back line (that line's dead code without the
    /// race), but it pins the observable contract the fix restores: no
    /// path through this method panics on a track that isn't there
    /// anymore.
    #[test]
    fn set_track_mix_on_a_removed_track_errs_cleanly_instead_of_panicking() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);

        // The track is live: an ordinary mix change succeeds.
        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-3.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set mix"),
            )
            .unwrap();

        // Removed through the channel — the same path a concurrent
        // `remove_track` command takes.
        plane.remove_track("t-1", TxMeta::user("remove track")).unwrap();
        assert!(plane.session().lock().store.tracks.iter().all(|t| t.id != "t-1"));

        // A second mix call naming the now-gone id must fail cleanly, not
        // panic.
        let result = plane.set_track_mix(
            vec![TrackMixChange { gain_db: Some(1.0), ..TrackMixChange::new("t-1") }],
            TxMeta::user("set mix on gone track"),
        );
        assert!(result.is_err(), "mix change on a removed track must error, not panic");
    }

    // ---- Plan E Task 12: transport family + commit_with -------------------

    /// `commit_with(meta, f, false)` is the primitive `ControlPlane::transport`
    /// builds on: bumps `rev`, threads the caller's `meta` through (transient
    /// included) unchanged, and — the whole point of the new parameter —
    /// never fires `project://changed`, unlike plain `commit`.
    #[test]
    fn commit_with_false_bumps_rev_and_meta_but_skips_project_changed() {
        let (plane, _engine_rx, events) = test_plane_with_tracks(&[]);
        let rev_before = plane.session().lock().rev;
        let committed = plane
            .commit_with(
                TxMeta::user("test transport set").transient(),
                |tx| {
                    tx.apply(Op::Set {
                        object: ObjectRef::Transport,
                        path: PropPath::TransportState,
                        from: serde_json::Value::Null,
                        to: serde_json::json!("playing"),
                    })
                },
                false,
            )
            .unwrap();
        assert!(committed.meta.transient, "meta.transient must survive the round trip");
        assert_eq!(committed.rev, rev_before + 1, "commit_with still bumps rev");
        assert_eq!(plane.session().lock().store.transport.state, "playing");
        assert!(
            events.lock().iter().all(|(name, _)| name != "project://changed"),
            "commit_with(..., false) must never fire project://changed"
        );
    }

    /// `commit` (unchanged public signature) still fires `project://changed`
    /// — it now merely delegates to `commit_with(meta, f, true)`.
    #[test]
    fn commit_still_fires_project_changed_via_commit_with_delegation() {
        let (plane, _engine_rx, events) = test_plane_with_tracks(&["t-1"]);
        plane.commit(TxMeta::user("set gain"), |tx| tx.apply(set_gain("t-1", -6.0))).unwrap();
        assert!(
            events.lock().iter().any(|(name, _)| name == "project://changed"),
            "commit must still announce project://changed"
        );
    }

    /// The Task 12 brief's TDD step: transport play->stop leaves `rev`
    /// bumped twice, each commit transient (asserted at the `commit_with`
    /// level above and at the session.rs level), `transport://state`
    /// emitted once per call, and NO `project://changed` fires — the fake
    /// emitter is what makes that last assertion possible.
    #[test]
    fn transport_play_then_stop_bumps_rev_twice_and_never_emits_project_changed() {
        let (plane, _engine_rx, events) = test_plane_with_tracks(&[]);
        let rev_before = plane.session().lock().rev;

        plane.transport(TransportAction::Play).unwrap();
        plane.transport(TransportAction::Stop).unwrap();

        assert_eq!(
            plane.session().lock().rev,
            rev_before + 2,
            "play + stop each commit exactly once"
        );
        let evs = events.lock();
        let transport_events =
            evs.iter().filter(|(name, _)| name == "transport://state").count();
        assert_eq!(transport_events, 2, "transport://state emitted exactly once per call");
        assert!(
            evs.iter().all(|(name, _)| name != "project://changed"),
            "transport commits must never announce project://changed"
        );
    }

    /// Play never downgrades an in-progress "recording" take back to
    /// "playing" — preserved exactly as the pre-Task-12 direct write did.
    /// No commit happens in that branch, so `rev` doesn't move either.
    #[test]
    fn transport_play_does_not_downgrade_recording_state() {
        // Sets `state = "recording"` via a direct store write (as the
        // engine control thread's own recording-start write would), THEN
        // calls Play — exercising the guard through the SAME path a racing
        // recording-start would use, not just as a pre-existing fixture
        // value. Fix round 1: the guard lives inside the commit closure
        // (`tx.store()`), checked-and-set under the one session lock —
        // there is no window between the check and the write for a
        // concurrent recording-start to land in.
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&[]);
        plane.session().lock().store.transport.state = "recording".into();
        let rev_before = plane.session().lock().rev;

        let snap = plane.transport(TransportAction::Play).unwrap();

        assert_eq!(snap.state, "recording", "play must not overwrite an active recording");
        // The guard's `Ok(())` with no `tx.apply` is a legal EMPTY
        // transient commit — `Session::transact` still bumps `rev`
        // unconditionally (it doesn't inspect whether the closure applied
        // any ops), so `rev` moves by exactly one even though the document
        // itself is untouched. That's the VALUE guarantee ("recording"
        // survives); it is a distinct claim from atomicity (checked above).
        assert_eq!(plane.session().lock().rev, rev_before + 1, "an empty transient commit still bumps rev");
    }

    /// `TransportAction::SetLoop`'s three Sets (`LoopEnabled`,
    /// `LoopStartSamples`, `LoopEndSamples`) fold into ONE commit (one `rev`
    /// bump), and the RT atomics land — this is the headless-safe half of
    /// `set_loop_validates_persists_and_wraps_playback` below, which needs a
    /// real engine only for the wrap-around playback assertion.
    #[test]
    fn transport_set_loop_is_one_commit_and_writes_rt_atomics() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let rev_before = plane.session().lock().rev;
        let snap = plane
            .transport(TransportAction::SetLoop { enabled: true, start_samples: 100, end_samples: 200 })
            .unwrap();
        assert_eq!(plane.session().lock().rev, rev_before + 1, "the three loop Sets fold into one commit");
        assert!(snap.loop_enabled);
        assert_eq!((snap.loop_start_samples, snap.loop_end_samples), (100, 200));
    }

    /// Mismatch combos err atomically at the `ControlPlane` level too:
    /// `TransportAction::SetLoop` with an empty region is rejected BEFORE
    /// any commit runs (unchanged validation, now guarding the op path).
    #[test]
    fn transport_set_loop_empty_region_rejected_without_committing() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let rev_before = plane.session().lock().rev;
        let r = plane.transport(TransportAction::SetLoop {
            enabled: true,
            start_samples: 100,
            end_samples: 100,
        });
        assert!(r.is_err());
        assert_eq!(plane.session().lock().rev, rev_before, "a rejected loop region must not commit");
    }

    /// Device selection has no `Op` (§4.5 config carve-out) — this pins the
    /// one observable contract `select_input_device`/`select_output_device`
    /// DO have: the existing `ControlMsg` still goes out, carrying the
    /// caller's device id, same as the pre-Task-12 direct Tauri command did.
    /// (The attribution log line itself isn't asserted here — `log`'s
    /// output isn't a `RecordedEvents`-style seam — but the method takes a
    /// `TxMeta` specifically so a caller's actor/label reaches that line.)
    #[test]
    fn select_input_and_output_device_send_the_existing_control_msg() {
        let (plane, engine_rx, _events) = test_plane_with_tracks(&[]);
        // No real engine thread behind `for_tests()` — reply immediately so
        // `request` (which blocks up to 30s) doesn't stall the test.
        // `ControlPlane::new` itself already sent a `Subscribe` — skip
        // anything that isn't the two device-select messages this test
        // cares about.
        let responder = std::thread::spawn(move || {
            let mut answered = 0;
            for msg in engine_rx.iter() {
                match msg {
                    ControlMsg::SelectInput { reply, .. } => {
                        reply.send(Ok(())).unwrap();
                        answered += 1;
                    }
                    ControlMsg::SelectOutput { reply, .. } => {
                        reply.send(Ok(())).unwrap();
                        answered += 1;
                    }
                    _ => {}
                }
                if answered == 2 {
                    break;
                }
            }
        });
        plane
            .select_input_device("mic-1".into(), TxMeta::user("select input device"))
            .unwrap();
        plane
            .select_output_device("speakers-1".into(), TxMeta::user("select output device"))
            .unwrap();
        responder.join().unwrap();
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
        let tables = empty_tables();
        let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(Recorder(Arc::clone(&events))),
        );
        let cp = ControlPlane::new(
            session,
            shared.clone(),
            tables,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
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
        let tables = empty_tables();
        let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
        );
        let cp = ControlPlane::new(
            session.clone(),
            shared.clone(),
            tables,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
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
            let mut session = session.lock();
            session.store.project_dir = Some(std::env::temp_dir().join("aura-loop-test.aura"));
            session.store.project_name = Some("LoopTest".into());
        }
        let p =
            crate::audio::project::from_store(&session.lock().store, 0, rate as u32).unwrap();
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
        let slots = derive_slots(&store.tracks);
        let (pad, lead, groove) = demo_seed_clips_v2("pad", "lead", "bass", 960);
        assert!(!pad.notes.is_empty() && !lead.notes.is_empty() && !groove.notes.is_empty());
        for n in pad.notes.iter().chain(lead.notes.iter()).chain(groove.notes.iter()) {
            n.validate().unwrap();
        }
        let midi = crate::midi::MidiStore {
            ppq: 960,
            tempo_events: vec![crate::midi::TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![crate::midi::MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![pad, lead, groove],
            loaded_dir: None,
            dirty: false,
        };
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        crate::midi::playback::append_from(&midi, &store, &slots, 48_000, None, &mut nodes, &mut out);
        assert_eq!(out.len(), 3, "all three seeded tracks reach the RT graph");
        // Live-node model (phase 3): render the graph headlessly through the
        // real RT path and assert the seeded music is audible.
        let mut g = crate::audio::rt::RtGraph::new(out, 1, Arc::new(ParamTable::default()));
        let mut buf = vec![0.0f32; 48_000 * 2];
        let mut pos = 0u64;
        for chunk in buf.chunks_mut(512 * 2) {
            crate::audio::mixer::render(
                &mut g,
                pos,
                &crate::audio::transport::LoopSpec::OFF,
                chunk,
                2,
                48_000,
                false,
                None,
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
        let slots = derive_slots(&store.tracks);
        let (pad, lead, groove) = demo_seed_clips_v2("pad", "lead", "bass", 960);
        let midi = crate::midi::MidiStore {
            ppq: 960,
            tempo_events: vec![crate::midi::TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![crate::midi::MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![pad, lead, groove],
            loaded_dir: None,
            dirty: false,
        };
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        crate::midi::playback::append_from(&midi, &store, &slots, 48_000, None, &mut nodes, &mut out);
        assert_eq!(out.len(), 3);
        for (track, inst) in [("pad", &ids[0]), ("lead", &ids[1]), ("bass", &ids[2])] {
            assert_eq!(
                nodes.key_of(track),
                Some(format!("plugin:{inst}@48000").as_str()),
                "track {track} resolved to its Zyn node (not the fallback)"
            );
        }
        let mut g = crate::audio::rt::RtGraph::new(out, 1, Arc::new(ParamTable::default()));
        let mut buf = vec![0.0f32; 2 * 48_000 * 2];
        let mut pos = 0u64;
        for chunk in buf.chunks_mut(512 * 2) {
            crate::audio::mixer::render(
                &mut g,
                pos,
                &crate::audio::transport::LoopSpec::OFF,
                chunk,
                2,
                48_000,
                false,
                None,
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
            source_id: crate::ids::SourceId::default(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 480,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: 480,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track_id),
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
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track_id),
            content_length_ticks: None,
        }
    }

    /// Task 2 (Plan E): a commit whose effect sets `persist.midi` writes
    /// `events/<clip>.bin` + `project.json` WITHOUT holding the session
    /// lock during I/O, and clears M-5's `midi.dirty` on success. No public
    /// trigger exists yet (later tasks' apply_raw arms set the flag), so
    /// this drives `execute_persist` directly with a constructed
    /// `PersistEffect` — sanctioned by the Task 2 brief; Task 7's wrapper
    /// tests re-cover this end-to-end through `commit`.
    #[test]
    fn persist_effect_writes_midi_after_the_lock_and_before_the_emit() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("persist-midi");
        cp.create_project(parent.to_str().unwrap(), "PersistMe").unwrap();
        let dir = cp.session().lock().store.project_dir.clone().unwrap();

        {
            let mut session = cp.session().lock();
            let mut clip = dummy_midi_clip("t-1");
            clip.notes.push(crate::midi::MidiNote {
                tick: 0,
                length_ticks: 480,
                key: 60,
                velocity: 100,
                channel: 0,
                note_id: crate::ids::NoteId(1),
            });
            session.midi.clips.push(clip);
            // Stale flag (M-5): a successful persist must clear it, not
            // merely leave it however it already was.
            session.midi.dirty = true;
        }

        let epoch = cp.session().lock().epoch;
        cp.execute_persist(&PersistEffect { midi: true, ..PersistEffect::default() }, epoch);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["content"].as_array().unwrap().len(), 1, "midi clip persisted to disk");
        let ev_ref = raw["content"][0]["eventsRef"]
            .as_str()
            .expect("events chunk ref written for a clip with notes");
        assert!(dir.join(ev_ref).exists(), "AMEV chunk file exists on disk after execute_persist");
        assert!(
            !cp.session().lock().midi.dirty,
            "successful persist clears the M-5 dirty flag"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// Fix round 1 (Task 7 review finding 2): `execute_persist` must SKIP —
    /// not write — when the session's document epoch has moved past the
    /// epoch the triggering commit captured. Simulates the exact race:
    /// commit a real midi op through the channel (capturing its genuine
    /// `Committed.epoch`), then manually bump `session.epoch` (standing in
    /// for an epoch function — project open/create/save-as — interleaving
    /// between `transact` returning and `execute_persist`'s re-lock), then
    /// drive `execute_persist` directly with the now-STALE captured epoch
    /// (same "Task 2 brief" direct-drive sanction the sibling test above
    /// uses) against NEW midi content. Evidence the skip fired: the new
    /// content never reaches disk (file content unchanged) and
    /// `project.json`'s mtime never moves (proving no write ran at all, not
    /// just a write that happened not to change the clip count).
    #[test]
    fn execute_persist_skips_when_the_session_epoch_moved_past_the_committed_one() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("persist-epoch-skip");
        cp.create_project(parent.to_str().unwrap(), "EpochSkip").unwrap();
        let dir = cp.session().lock().store.project_dir.clone().unwrap();

        // A real commit through the channel — this one's OWN internal
        // execute_persist runs normally (no race here) and persists clip 1.
        let committed = cp
            .commit(TxMeta::user("add clip"), |tx| {
                tx.apply(crate::control::op::Op::MidiClipAdd {
                    clip: dummy_midi_clip("t-1"),
                    index: 0,
                })
            })
            .unwrap();
        assert!(committed.effect.persist.midi, "MidiClipAdd sets persist.midi");

        let project_json = dir.join("project.json");
        let before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&project_json).unwrap()).unwrap();
        assert_eq!(before["content"].as_array().unwrap().len(), 1, "clip 1 landed on disk");
        let mtime_before = std::fs::metadata(&project_json).unwrap().modified().unwrap();

        // Simulate an epoch function racing in between `transact` and a
        // (would-be) `execute_persist` re-lock: the document identity moves
        // on from under `committed.epoch`.
        cp.session().lock().epoch += 1;

        // New content that a persist call WOULD write if it ran — added
        // directly (not through `commit`, which would capture the NEW
        // epoch and persist successfully; this simulates the stale
        // in-flight commit's own deferred `execute_persist` call still
        // carrying the OLD, now-mismatched epoch).
        {
            let mut clip2 = dummy_midi_clip("t-1");
            clip2.id = "mc2".into();
            cp.session().lock().midi.clips.push(clip2);
        }

        cp.execute_persist(&PersistEffect { midi: true, ..PersistEffect::default() }, committed.epoch);

        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&project_json).unwrap()).unwrap();
        assert_eq!(
            after["content"].as_array().unwrap().len(),
            1,
            "the second clip must NOT reach disk — the skip fired, not a write"
        );
        let mtime_after = std::fs::metadata(&project_json).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "project.json was never touched by the skipped persist");

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// Plan E Task 10 mirror of the midi persist test above: a commit whose
    /// effect sets `persist.automation` writes `automation[]` + chunk files
    /// WITHOUT holding the session lock during I/O.
    #[test]
    fn persist_effect_writes_automation_after_the_lock() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("persist-automation");
        cp.create_project(parent.to_str().unwrap(), "PersistAutoMe").unwrap();
        let dir = cp.session().lock().store.project_dir.clone().unwrap();

        let lane = crate::plugins::automation::AutomationLane {
            id: "a-1".into(),
            target_node: "track:t-1".into(),
            param_id: 0,
            points: vec![crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 }],
        };
        cp.session().lock().automation.lanes.push(lane.clone());

        let epoch = cp.session().lock().epoch;
        cp.execute_persist(&PersistEffect { automation: true, ..PersistEffect::default() }, epoch);

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
        let rows = raw["automation"].as_array().expect("automation[] written");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "a-1");
        let pref = rows[0]["pointsRef"].as_str().expect("chunk ref for a lane with points");
        assert!(dir.join(pref).exists(), "AMEV point chunk written to disk");

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// End-to-end through the real channel (Plan E Task 10 brief: "lane
    /// persisted to project.json only after commit returns"): the file has
    /// no automation row before `commit`, and has the full row (chunk
    /// included) the instant `commit` returns — no async gap, no separate
    /// "flush" step to remember.
    #[test]
    fn automation_set_lane_commits_and_persists_synchronously() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("automation-commit");
        cp.create_project(parent.to_str().unwrap(), "AutoCommit").unwrap();
        let dir = cp.session().lock().store.project_dir.clone().unwrap();

        let before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert!(
            before.get("automation").is_none() || before["automation"].as_array().unwrap().is_empty(),
            "no automation persisted before the commit"
        );

        let lane = crate::plugins::automation::AutomationLane {
            id: "a-1".into(),
            target_node: "track:t-1".into(),
            param_id: 3,
            points: vec![crate::plugins::automation::AutomationPoint { tick: 0, value: 0.5 }],
        };
        let committed = cp
            .commit(op::TxMeta::user("edit automation"), |tx| {
                tx.apply(op::Op::AutomationSetLane { key: "a-1".into(), lane: Some(lane) })
            })
            .unwrap();
        assert!(committed.effect.persist.automation);
        assert!(!committed.effect.rebuild, "automation edits don't rebuild yet (see session.rs's arm doc)");

        // By the time `commit` returned above, the write already happened
        // (persist runs synchronously inside `commit`, before the event
        // emit) — read it back right away, no waiting.
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
        let rows = after["automation"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["paramId"], 3);
        assert_eq!(cp.automation_lanes().len(), 1, "session.automation reflects the commit too");

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// `automation_get`'s purity (Task 10 fix round 1, reviewer finding):
    /// routed through the REAL production entry point,
    /// `ControlPlane::automation_lanes()`, not a reimplementation of its
    /// body — a future regression inside `automation_lanes` itself (e.g.
    /// someone bolts on a defensive `adopt_open_project` call) would be
    /// caught here. Asserts: no project needs to be open, no disk access is
    /// needed, and two consecutive reads return identical lanes with no
    /// mutation in between (snapshot equality).
    #[test]
    fn automation_get_is_a_pure_session_read_no_disk_no_project_dir() {
        let (cp, _events, _engine) = recording_control_plane();
        assert!(cp.session().lock().store.project_dir.is_none(), "no project ever opened");

        let lane = crate::plugins::automation::AutomationLane {
            id: "a-1".into(),
            target_node: "track:t-1".into(),
            param_id: 0,
            points: vec![crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 }],
        };
        cp.session().lock().automation.lanes.push(lane.clone());

        // The production entry point, called directly — not reimplemented.
        let read = cp.automation_lanes();
        assert_eq!(read, vec![lane], "no project dir, no disk, still reads the lane");

        // Repeat reads are side-effect-free: nothing mutates the session
        // between them (unlike the retired `with_synced`, which wrote
        // `loaded_dir` on every single call, mutating reader included) —
        // snapshot equality pins that.
        let read_again = cp.automation_lanes();
        assert_eq!(read, read_again, "two consecutive reads return identical lanes");
    }

    /// "New Project" is a blank slate: the previous session's tracks, clips,
    /// midi state, and playhead must all be gone, and the on-disk project.json
    /// must describe the empty project.
    #[test]
    fn create_project_resets_to_a_blank_slate() {
        let (cp, _events, _engine) = recording_control_plane();
        let t = cp.add_track(Some("Old".into()), None, TxMeta::user("add track")).unwrap();
        cp.session().lock().store.clips.push(dummy_audio_clip(t.id.as_str()));
        cp.session().lock().midi.clips.push(dummy_midi_clip(t.id.as_str()));
        cp.shared.position.store(1234, Relaxed);

        let parent = cp_tmp_parent("blank");
        let project = cp.create_project(parent.to_str().unwrap(), "Fresh").unwrap();

        assert!(project.tracks.is_empty(), "returned project is empty");
        {
            let session = cp.session().lock();
            let s = &session.store;
            assert!(s.tracks.is_empty(), "tracks cleared");
            assert!(s.clips.is_empty(), "clips cleared");
            assert_eq!(s.project_dir.as_deref(), Some(parent.join("Fresh.aura").as_path()));
        }
        assert!(cp.session().lock().midi.clips.is_empty(), "midi state reset");
        assert_eq!(cp.shared.position.load(Relaxed), 0, "playhead back at 0");
        let (loaded, _) = project::load(&parent.join("Fresh.aura")).unwrap();
        assert!(loaded.tracks.is_empty(), "blank project on disk");
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// Finding 2: a `dirty = true` left over from a PRIOR project's failed
    /// auto-persist must not survive `create_project`'s reset. If it did,
    /// the stale flag itself is harmless in isolation, but combined with
    /// finding 1's `loaded_dir == dir` persist guard it would NOT block
    /// anything here (loaded_dir is correctly set to the new dir) — the real
    /// danger is a caller reading `dirty` as "this project has an
    /// unpersisted edit" when it never did. Assert it comes back false.
    #[test]
    fn create_project_clears_a_stale_dirty_flag() {
        let (cp, _events, _engine) = recording_control_plane();
        cp.session().lock().midi.dirty = true; // simulate a prior failed auto-persist

        let parent = cp_tmp_parent("blank-dirty");
        cp.create_project(parent.to_str().unwrap(), "Fresh").unwrap();

        assert!(!cp.session().lock().midi.dirty, "stale dirty flag must not survive a blank-slate reset");
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// First save of a session that never had a project: `save_project_as`
    /// creates the .aura dir and persists the CURRENT content (tracks + midi),
    /// unlike create_project which resets.
    #[test]
    fn save_project_as_materializes_an_unsaved_session() {
        let (cp, events, _engine) = recording_control_plane();
        let t = cp.add_track(Some("Keys".into()), None, TxMeta::user("add track")).unwrap();
        cp.session().lock().midi.clips.push(dummy_midi_clip(t.id.as_str()));
        events.lock().clear();

        let parent = cp_tmp_parent("saveas");
        let project = cp.save_project_as(parent.to_str().unwrap(), "First").unwrap();

        assert_eq!(project.tracks.len(), 1, "session tracks kept");
        {
            let session = cp.session().lock();
            let s = &session.store;
            assert_eq!(s.tracks.len(), 1);
            assert_eq!(s.project_dir.as_deref(), Some(parent.join("First.aura").as_path()));
        }
        let json: serde_json::Value = serde_json::from_slice(
            &std::fs::read(parent.join("First.aura/project.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(json["tracks"][0]["name"], "Keys", "tracks on disk");
        assert_eq!(json["placements"][0]["id"], "mc1", "in-memory midi materialized");
        assert!(
            events.lock().iter().any(|(n, _)| n == "project://changed"),
            "project://changed emitted"
        );
        assert!(!cp.session().lock().midi.dirty, "finding 2: successful persist clears dirty");
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
