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
pub mod history;
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

pub use history::{EpochEvent, History, HistoryEntry, HistoryLog, HistoryMode, JournalWriter};
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
    emit: Arc<EventEmitter>,
    /// The commit core (Plan E Task 13) — `commit`/`commit_with` are thin
    /// wrappers over this. A SECOND, independent `Committer` (same
    /// `session`/`shared`/`tables` `Arc`s, its own `emit` closure instance)
    /// is built in `audio::init` and carried by the engine control thread —
    /// see `Committer`'s doc.
    committer: Committer,
    /// The (at most one) open gesture boundary (Plan E Task 14) —
    /// `gesture_begin`/`gesture_end` and `set_track_mix`'s transient-fold
    /// check all go through it. See `GestureState`'s doc.
    gesture: GestureState,
    /// The last gesture batch `close_gesture` synthesized, parked for
    /// `take_last_gesture_batch`. TEST-ONLY as of Task 17: history is now
    /// fed DIRECTLY by `close_gesture` (see its doc), so nothing in
    /// production reads this slot — it exists so a test can inspect the
    /// exact synthesized `ops`/`inverses`/`meta` without reaching into the
    /// history stacks.
    last_gesture_batch: Mutex<Option<session::Committed>>,
}

/// The commit core, shareable with the engine control thread (Plan E Task
/// 13, round-2 inventory rows 21-24). Owns everything `commit_with`/`commit`
/// need EXCEPT the `EngineHandle`: the engine executes a `rebuild` effect by
/// calling its OWN `Control::rebuild` directly (it IS the engine control
/// thread) instead of sending itself a `ControlMsg::Rebuild` — this is why
/// `commit_with_rebuild` takes `do_rebuild` as a caller-supplied closure
/// rather than hardcoding a `ControlMsg::Rebuild` send: it keeps "at most
/// one `Rebuild` per transaction" (§4.4) a claim about ALL rebuilds, not
/// just the ones a Tauri/MCP-driven `ControlPlane::commit` triggers.
///
/// `emit` is `Arc`-shared (not the plain `Box<dyn Fn>` `ControlPlane` alone
/// used before this task) so a commit built through EITHER `Committer`
/// instance — `ControlPlane`'s own, or the engine's — can call it without
/// taking ownership of the underlying closure. `ControlPlane::new` wraps its
/// caller-supplied `EventEmitter` in one `Arc` and clones it into both
/// `self.emit` and `self.committer`'s copy; `audio::init` builds a SEPARATE
/// `Arc<EventEmitter>` closure for the engine's own `Committer` — two
/// closure instances, but both ultimately call the one live
/// `AppHandle::emit`, so they're behaviorally one emitter.
///
/// DEADLOCK AUDIT — why the engine control thread calling
/// `commit_with_rebuild` (i.e. `Session::transact`, under the session lock)
/// from inside its own message loop is safe, traced against the actual
/// code rather than just asserted:
///
/// (a) `Session::transact` never sends an engine message. `session.rs` has
///     no `EngineHandle`/`ControlMsg` import at all — `Tx::apply`/
///     `apply_raw` only ever touch `Session`/`Store`/`MidiStore` fields; the
///     whole call is in-memory, no channel send, no lock other than the one
///     `Session::transact` itself takes and releases before returning.
///
/// (b) The only blocking request-reply INTO the engine
///     (`EngineHandle::request` — `ControlPlane::transport`'s Stop arm:
///     `self.engine.request::<Vec<Clip>>(|reply| ControlMsg::StopRecording
///     { reply })`, plus `start_recording`/`select_*_device`) is serviced by
///     the SAME control loop that would run a commit: `Control::run` calls
///     `Control::handle`, whose `StopRecording` arm is
///     `let _ = reply.send(self.stop_recording());` — `self.stop_recording`
///     is where the engine's own finalize commit runs, and it runs entirely
///     BEFORE `reply.send(...)`. The reply sender is never held open DURING
///     a commit; it's touched again only AFTER the site's commit (and the
///     whole handler) returns. So the CALLER's `.recv_timeout` is what
///     blocks — the engine thread itself never waits on its own reply, and
///     `Control::handle` is never re-entered mid-commit (one `Control`
///     value, one thread, one `handle` call in flight at a time, driven by
///     that thread's own `rx.recv_timeout` loop).
///
/// The rule THIS actually rests on (review round 1, Important-4 — (b)
/// shows the engine thread is safe; this is what keeps every OTHER caller
/// safe too, including future ones): no thread may hold the session lock
/// across an `EngineHandle::request` — the engine takes that SAME lock
/// inside `handle()` (every write site, and most of the read-only ones —
/// see the `// read-only:` sites in engine.rs), so a `request` issued while
/// the calling thread is still holding a session guard would deadlock the
/// instant the engine's own `handle()` tries to lock it: the engine can't
/// finish the request (so never sends the reply), and the caller can't
/// drop its guard (it's blocked inside `request`'s `recv_timeout`) — a real
/// deadlock, not just the starvation (c) describes, though bounded by
/// `request`'s own 30s timeout rather than hanging forever. All five
/// current production `request` call sites comply — none holds `self.
/// session.lock()` (or any session guard) across the call:
/// `ControlPlane::transport`'s Stop arm (control/mod.rs:1301),
/// `start_recording` (:1388), `stop_recording` (:1397),
/// `select_input_device` (:1422), `select_output_device` (:1433).
/// (Those five numbers were stale by the end of Plan E — the audit is only
/// as good as its navigability, so they are re-checked whenever this file
/// moves; the INVARIANT above, not the line numbers, is what binds.)
///
/// (c) All five engine write sites (`open_output`, `apply_end_policy`,
///     `start_recording`, `stop_recording`, `ensure_project`) commit
///     synchronously on the control thread's own turn, fire-and-forget from
///     the loop's perspective — none holds a `Reply<T>` sender across its
///     `commit_with_rebuild` call (per (b), any sender for that turn's own
///     message is sent only after the site's commit returns). Every site's
///     `do_rebuild` closure calls `self.rebuild()` directly, never
///     `self.engine.send(ControlMsg::Rebuild)` — sending to `self.engine`
///     would round-trip through the very channel this thread is reading
///     `ControlMsg`s from, and could queue behind whatever this turn is
///     already doing (starvation, not deadlock, since nothing blocks on the
///     reply — but still wrong: the whole point of "the engine IS the
///     engine" is that it never needs to ask itself for anything).
#[derive(Clone)]
pub struct Committer {
    session: Arc<Mutex<Session>>,
    shared: Arc<SharedRt>,
    tables: SharedGraphTables,
    emit: Arc<EventEmitter>,
    /// Undo/redo stacks + the on-disk journal (Plan E Task 17). SHARED by
    /// every `Committer` instance — `audio::init` creates the one
    /// `Arc<HistoryLog>` and hands it to both the engine's `Committer` and
    /// (via `AudioState::history_log`) `ControlPlane::new`. This has to be
    /// shared, not per-instance: the engine's recording finalize commits
    /// NON-transiently (`TxMeta::engine("stop recording")`,
    /// audio/engine.rs), so a recorded take is a real undo step in the
    /// user's history, and two independent `History`s would hide it.
    ///
    /// LOCK ORDER: `HistoryLog`'s own mutexes are LEAF-most — see its
    /// module doc. Everything below touches it only after the session lock
    /// is released.
    log: Arc<history::HistoryLog>,
}

impl Committer {
    pub fn new(
        session: Arc<Mutex<Session>>,
        shared: Arc<SharedRt>,
        tables: SharedGraphTables,
        emit: Arc<EventEmitter>,
        log: Arc<history::HistoryLog>,
    ) -> Self {
        Self { session, shared, tables, emit, log }
    }

    /// The shared history/journal handle (Task 17) — `ControlPlane` reads
    /// it for `undo`/`redo` and the epoch functions.
    pub fn log(&self) -> &Arc<history::HistoryLog> {
        &self.log
    }

    /// Compose the full-project payload `project://changed` carries (the
    /// same shape `project::from_store` serializes, minus its requirement of
    /// an open project dir — mix/structural changes are legal in an unsaved
    /// session). `commit_with_rebuild`'s event emission (Task 7: every
    /// A-slice command now goes live through it) is the sole caller;
    /// `ControlPlane::create_project`/`create_project_at` builds its own
    /// `Project` from `project::create`'s return instead, since that one
    /// always has an open project dir.
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
    /// `do_rebuild()` called at most once, plugin host round-trips, persist,
    /// and — gated by `emit_project_changed` — exactly one
    /// `project://changed` event. `project://changed` is a FROZEN event
    /// whose payload contract is the full `Project` shape
    /// (project.schema.json; ARCHITECTURE §3.4) — this carries EXACTLY that
    /// (via `project_changed_payload`, the same serialization
    /// `ControlPlane::create_project` uses), with `rev`/`label`/`actor`
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
    ///
    /// Task 13: `do_rebuild()` replaces what used to be a hardcoded
    /// `self.engine.send(ControlMsg::Rebuild)` here — the CALLER now
    /// decides how "one more rebuild" reaches its engine: a
    /// `ControlPlane`-driven commit sends `ControlMsg::Rebuild` over the
    /// channel; the engine control thread's OWN commits call `Control::
    /// rebuild` directly instead (see this struct's deadlock audit for why
    /// that distinction is load-bearing, not stylistic).
    pub fn commit_with_rebuild<F, R>(
        &self,
        meta: op::TxMeta,
        f: F,
        emit_project_changed: bool,
        do_rebuild: R,
    ) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
        R: FnOnce(),
    {
        self.commit_with_rebuild_mode(meta, f, emit_project_changed, do_rebuild, history::HistoryMode::Record)
    }

    /// [`Self::commit_with_rebuild`] with an explicit [`history::HistoryMode`]
    /// (Plan E Task 17). `Record` — the default every ordinary caller gets
    /// — records a new undo entry; `Replay` journals the batch but creates
    /// no entry, because the commit IS an undo or a redo and
    /// `ControlPlane::undo`/`redo` migrate the ORIGINAL entry between the
    /// stacks themselves.
    ///
    /// The mode travels down the CALL PATH, not through a thread-local:
    /// undo runs on whatever thread invoked the command, and a thread-local
    /// would mis-classify any commit a wrapper made on its behalf.
    pub(crate) fn commit_with_rebuild_mode<F, R>(
        &self,
        meta: op::TxMeta,
        f: F,
        emit_project_changed: bool,
        do_rebuild: R,
        history_mode: history::HistoryMode,
    ) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
        R: FnOnce(),
    {
        let committed = Session::transact(&self.session, meta, f)?;
        // ---- session lock is released here; everything below executes
        // the effect the session merely described. ----
        debug_assert_transient_invariant(&committed);
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
                    // Plan E Task 8: same reasoning for LengthSamples/
                    // OffsetSamples (LoopJam's in-place clip trims) —
                    // structural (rebuild), no ParamTable counterpart.
                    // Plan E Task 9: same reasoning for `Param` —
                    // `apply_raw` never pushes plugin params into
                    // `param_writes` either (they travel through
                    // `host_forward::ParamWrite` instead, resolved by
                    // instance id, not by a `GraphTables` slot).
                    op::PropPath::Armed
                    | op::PropPath::InstrumentId
                    | op::PropPath::TimelineStartSamples
                    | op::PropPath::LengthSamples
                    | op::PropPath::OffsetSamples
                    | op::PropPath::TimelineStartTicks
                    | op::PropPath::LengthTicks
                    | op::PropPath::ContentLengthTicks
                    | op::PropPath::TransportState
                    | op::PropPath::LoopEnabled
                    | op::PropPath::LoopStartSamples
                    | op::PropPath::LoopEndSamples
                    | op::PropPath::StopAtEnd
                    | op::PropPath::SampleRate
                    | op::PropPath::Param { .. } => {}
                }
            }
            if let Some(any_solo) = committed.effect.any_solo {
                tables.params.any_solo.store(any_solo, Relaxed);
            }
        }
        // Plugin host round-trips (Task 9): after the param-table writes,
        // before persist — same "session lock is released" guarantee as
        // everything else in `commit_with_rebuild` ([C1]: hosts have their
        // own locks, never called while the session lock is held).
        if !committed.effect.host_forward.is_empty() {
            self.execute_host_forward(&committed.effect.host_forward, committed.epoch);
        }
        if committed.effect.rebuild {
            do_rebuild();
        }
        // Persist runs after the effect writes above and BEFORE the
        // `project://changed` emit below — the event announces durable
        // truth, so persistence must have already happened by the time it
        // fires (round-2 §4: persistence is an effect, executed here, never
        // I/O under the session lock).
        if committed.effect.persist != session::PersistEffect::default() {
            self.execute_persist(&committed.effect.persist, committed.epoch);
        }
        // THE OP LOG (Plan E Task 17, Gate E). After the effects — a batch
        // is journaled once it has actually happened, never before — and
        // ONLY when it is not transient: scope ruling 2's "through the
        // channel, never journaled, never undoable" is enforced here, at
        // exactly one place, for transport ops, mid-gesture folds and the
        // engine's state mirrors alike.
        //
        // Before the `project://changed` emit below, for the same reason
        // persist runs before it: the event announces durable truth, and by
        // the time a listener reacts the log must already agree.
        //
        // A batch that folded to ZERO ops is dropped by `record_commit`
        // itself (fix round 1, I-1 — see its doc): `fold_ops` elides a
        // net-no-op `Set` group, so an empty `Committed` is an ordinary
        // outcome, and it must produce neither a phantom undo step nor an
        // empty journal line.
        if !committed.meta.transient {
            self.log.record_commit(
                committed.rev,
                committed.epoch,
                &committed.meta,
                &committed.ops,
                &committed.inverses,
                history_mode,
            );
        }
        // Full-Project payload (the frozen contract) + rev/label/actor as
        // additive fields (D-06). `project_changed_payload` serializes to a
        // JSON object (all of `Project`'s fields are named), so inserting
        // extra keys is safe; the `unwrap_or_default` fallback (an empty
        // object) only matters if serialization itself somehow failed.
        //
        // Plan E Task 12: gated by `emit_project_changed` — transport
        // commits (`ControlPlane::transport`, and Task 13's engine-side
        // transient sites) pass `false` and rely on their own
        // `transport://state`/`recording://state` emit instead;
        // `project://changed`'s payload contract is the full `Project`
        // shape, and firing it once per play/stop/loop-drag gesture would
        // be a behavior change from today's narrower event contract.
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

    /// Executes a `PersistEffect` a commit merely described. Snapshots are
    /// taken under a fresh, SHORT session lock; ALL disk I/O happens after
    /// the guard drops — round-2 §4's whole point (persistence is an
    /// effect, not I/O under the lock). `pub(crate)` so tests can construct
    /// a `PersistEffect` and call this directly (`ControlPlane::committer()`
    /// is a `#[cfg(test)]`-only accessor for exactly that — Task 13; there
    /// is no non-test caller left on `ControlPlane` itself, only
    /// `commit_with_rebuild` above, which is why this moved here instead of
    /// staying a `ControlPlane` method with a forwarding wrapper).
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
    /// go through `commit_with_rebuild`) should pass the session's current
    /// epoch.
    pub(crate) fn execute_persist(&self, p: &session::PersistEffect, committed_epoch: u64) {
        let (dir, epoch_now, midi_snapshot, project_snapshot, automation_snapshot, plugin_snapshot) = {
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
                // Plan E Task 9: plugin doc snapshot, taken under this SAME
                // short lock — the actual write (state blobs + `plugins[]`
                // dirty-ladder + clearing `dirty_state` for whichever ids
                // were written) happens below, after the guard drops.
                p.plugins.then(|| s.plugin_snapshot()),
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
        // `with_host_state: false` — a fresh host round-trip (state save)
        // is never needed here: `PluginAdd`/`PluginRemove`/`PluginSetState`
        // already keep `session.plugins.pending_state` current through
        // `host_forward`'s `LoadState`/`Destroy` handling (executed before
        // `execute_persist` runs, in `commit_with_rebuild`), and a bare
        // param write (`plugin_set_param`) must NOT round-trip the plugin
        // main thread per rAF batch — same reasoning the retired
        // `persist_after_mutation(..., with_host_state: false)` call site
        // used.
        if let Some(doc) = plugin_snapshot {
            match crate::plugins::state::save_snapshot_into_project(&dir, &doc, false) {
                Ok(cleared) if !cleared.is_empty() => {
                    // Clear `dirty_state` for whichever ids' pending bytes
                    // just landed on disk (Task 9 review round 1,
                    // Critical-2) — a short, separate re-lock, no disk I/O
                    // under it.
                    let mut s = self.session.lock();
                    for id in cleared {
                        s.plugins.dirty_state.remove(&id);
                    }
                }
                Ok(_) => {}
                Err(e) => log::warn!("plugins persist failed: {e}"),
            }
        }
    }

    /// Executes a `HostForward` list a commit merely described — calling
    /// the SAME host entry points commands call today
    /// (`plugins::clap_host`/`lv2_host`/`state`'s `HostStateBridge`), now
    /// sequenced after the session lock is released ([C1]: hosts have their
    /// own locks, never called while the session lock is held — this
    /// method takes ONLY brief re-locks of `self.session` to read a row's
    /// format/uid or to write back a post-host result, never spanning a
    /// host call).
    /// I-3: the `Instantiate` arm's post-host document writeback, split out
    /// of the match so it is testable without a live plugin host, and
    /// EPOCH-GUARDED (same rule as `execute_persist`: a document swapped
    /// between the commit and this re-lock is a different document, and
    /// writing this commit's host result into it is silent corruption —
    /// `params.entry(..).or_default()` would even CREATE a row for an
    /// instance the new project never had). Recorded as residual R-4 in
    /// `docs/SIDE-CHANNEL-INVENTORY.md`; see that doc for why this stays a
    /// carve-out rather than becoming an op (the M-3 transient invariant).
    pub(crate) fn apply_instantiate_writeback(
        &self,
        instance: &str,
        params: Vec<crate::plugins::ParamInfo>,
        committed_epoch: u64,
    ) {
        let mut s = self.session.lock();
        if s.epoch != committed_epoch {
            log::warn!(
                "plugins: instantiate writeback for {instance} skipped: epoch changed \
                 between commit and host round-trip ({committed_epoch} -> {})",
                s.epoch
            );
            return;
        }
        if let Some(r) = s.plugins.instances.iter_mut().find(|r| r.id == instance) {
            r.status = "active".into();
        }
        // Fill only when absent: an undo-of-remove already restored the
        // REAL param mirror into `session.plugins.params` (parked there by
        // `apply_raw`'s `PluginRemove` arm) — this must not clobber it with
        // the host's fresh-reinstantiate DEFAULTS. A genuinely fresh
        // instantiate's mirror is still the empty seed `Op::PluginAdd`
        // left, so it gets filled here exactly as before.
        let entry = s.plugins.params.entry(instance.to_string()).or_default();
        if entry.is_empty() {
            *entry = params;
        }
    }

    pub(crate) fn execute_host_forward(&self, forwards: &[session::HostForward], committed_epoch: u64) {
        use crate::plugins::{clap_host, lv2_host, state as pstate};
        use session::HostForward;
        for hf in forwards {
            match hf {
                HostForward::ParamWrite { instance, index, value } => {
                    let format = {
                        let s = self.session.lock();
                        s.plugins.instances.iter().find(|r| &r.id == instance).map(|r| r.format.clone())
                    };
                    match format.as_deref() {
                        Some("lv2") => {
                            if let Some(host) = lv2_host::try_global() {
                                host.set_params(instance, vec![(*index, *value)]);
                            }
                        }
                        Some("clap") => {
                            let change = crate::plugins::ParamChange { id: *index, value: *value as f64 };
                            if let Err(e) = clap_host::set_params(instance, vec![change]) {
                                log::warn!("plugins: clap param write for {instance}: {e}");
                            }
                        }
                        _ => {}
                    }
                }
                HostForward::Instantiate { instance } => {
                    let row = {
                        let s = self.session.lock();
                        s.plugins.instances.iter().find(|r| &r.id == instance).cloned()
                    };
                    let Some(row) = row else { continue }; // row vanished meanwhile
                    // Idempotent by construction (doc on `HostForward::
                    // Instantiate`): if the host already has this id live
                    // (the prepare-outside fresh-instantiate path), re-sync
                    // params via a plain read instead of re-instantiating —
                    // re-registering a live id would reset its voice state.
                    let hosted = match row.format.as_str() {
                        "clap" => match clap_host::has_instance(instance) {
                            Ok(true) => clap_host::get_params(instance),
                            Ok(false) => clap_host::instantiate(instance, &row.uid),
                            Err(e) => Err(e),
                        },
                        "lv2" => {
                            let host = lv2_host::global();
                            match host.has_instance(instance) {
                                Ok(true) => host.get_params(instance),
                                Ok(false) => host.register_instance(instance, &row.uid),
                                Err(e) => Err(e),
                            }
                        }
                        _ => continue, // non-hosted format: stays "stub", nothing to sync
                    };
                    match hosted {
                        Ok(params) => self.apply_instantiate_writeback(instance, params, committed_epoch),
                        Err(e) => log::warn!("plugins: instantiate forward for {instance} failed: {e}"),
                    }
                }
                HostForward::Destroy { instance } => {
                    let format = {
                        let s = self.session.lock();
                        s.plugins.instances.iter().find(|r| &r.id == instance).map(|r| r.format.clone())
                    };
                    match format.as_deref() {
                        Some("lv2") => {
                            if let Some(host) = lv2_host::try_global() {
                                host.unregister_instance(instance);
                            }
                        }
                        Some("clap") => {
                            if let Err(e) = clap_host::remove(instance) {
                                log::warn!("plugins: clap destroy for {instance}: {e}");
                            }
                        }
                        _ => {}
                    }
                }
                HostForward::LoadState { instance } => {
                    let blob = {
                        let s = self.session.lock();
                        s.plugins.pending_state.get(instance).cloned()
                    };
                    let Some(bytes) = blob else { continue };
                    let decoded = pstate::decode_state(&bytes);
                    match decoded {
                        Ok((_uid, blob)) => {
                            if let Some(bridge) = pstate::registered_state_bridge() {
                                if let Err(e) = bridge.load_state(instance, &blob) {
                                    log::warn!("plugins: state load for {instance} failed: {e}");
                                }
                            }
                        }
                        Err(e) => log::warn!("plugins: pending state blob for {instance} unreadable: {e}"),
                    }
                }
            }
        }
    }
}

/// Test-only fixture, reachable crate-wide (`crate::control::testutil::
/// test_committer`) — every `engine::start` test call site across the crate
/// (control/{export,hum,import,loopjam}.rs, mcp/server.rs, this file's own
/// test module, engine.rs's `spin_up`) needs a `Committer` now (Task 13);
/// this is the one place that fixture lives instead of duplicated inline
/// construction at every call site. Mirrors `audio::rt::testutil::
/// empty_tables`'s shape.
#[cfg(test)]
pub(crate) mod testutil {
    use super::*;

    /// A `Committer` over the SAME `session`/`shared`/`tables` `Arc`s the
    /// caller's engine/`ControlPlane` fixture already uses, with a no-op
    /// `emit` — most fixtures don't assert on the engine's own committed
    /// events; tests that need to (this file's own `mod tests`) build a
    /// recording `EventEmitter` directly and pass it to `Committer::new`
    /// themselves instead of calling this helper.
    pub fn test_committer(
        session: &Arc<Mutex<Session>>,
        shared: &Arc<SharedRt>,
        tables: &SharedGraphTables,
    ) -> Committer {
        Committer::new(
            session.clone(),
            shared.clone(),
            tables.clone(),
            Arc::new(Box::new(|_: &str, _: serde_json::Value| {}) as EventEmitter),
            // Its own private log (Task 17): a bare test `Committer` has no
            // `ControlPlane` to share one with, and a per-fixture log keeps
            // parallel tests from journaling into each other.
            Arc::new(history::HistoryLog::new()),
        )
    }
}

// ---------------------------------------------------------------------------
// Gesture IPC (Plan E Task 14, round-2 inventory row 31, ADR 0003)
// ---------------------------------------------------------------------------

/// The coalescing key a gesture folds by: the op's discriminant + the
/// `ObjectRef` it targets + the `PropPath` it targets (round-2 §4.4:
/// "coalesced by (op_kind, target, actor)" — the (kind, target) half; the
/// actor half is `OpenGesture::actor`/`GestureState::matches_actor`, checked
/// separately since it gates whether a commit folds AT ALL, not which key it
/// folds under). `path` is `Option` because a future non-`Set` op kind may
/// have no property path; today `for_op` only ever returns `Some(path)`,
/// since `Op::Set` is the only kind it builds a key for. Exported
/// (`pub(crate)`) — Task 17 imports this exact type to key its own
/// history-side merge; the name and the (kind, object, path) shape are
/// load-bearing, not cosmetic.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CoalesceKey {
    kind: &'static str,
    object: op::ObjectRef,
    path: Option<op::PropPath>,
}

impl CoalesceKey {
    /// Builds the key for `op`, or `None` for an op kind gesture folding
    /// doesn't (yet) handle. Only `Op::Set` today — the only op kind
    /// `ControlPlane::set_track_mix` (gesture folding's one wired caller)
    /// ever applies.
    fn for_op(op: &op::Op) -> Option<Self> {
        match op {
            op::Op::Set { object, path, .. } => {
                Some(Self { kind: "set", object: object.clone(), path: Some(*path) })
            }
            _ => None,
        }
    }

    /// The HISTORY-side key (Task 17), a deliberate SUPERSET of
    /// [`Self::for_op`]: everything a gesture folds, plus `Op::MidiSetNotes`
    /// keyed by its clip.
    ///
    /// Why the two differ, rather than one shared fn: `for_op` decides what
    /// a gesture folds INSIDE an open boundary, where folding also discards
    /// intermediate commits' effects — only `Op::Set`, the sole op kind
    /// `set_track_mix` (gesture folding's one wired caller) ever applies,
    /// is in scope there. The 350 ms fallback merges FINISHED, already
    /// committed batches, so it can safely cover the other §4.4
    /// value-replacement wrapper too: `midi_set_notes_core` deliberately
    /// keeps the FIXED label `"set midi notes"` (its own doc says so)
    /// precisely so a run of note edits on ONE clip collapses to one undo
    /// step instead of one per keystroke. `path: None` — a `MidiSetNotes`
    /// addresses the whole clip, not a property of it.
    ///
    /// Every other op kind is structural and returns `None`, which is what
    /// makes "a structural op breaks the merge" true by construction.
    fn for_history_op(op: &op::Op) -> Option<Self> {
        match op {
            op::Op::MidiSetNotes { clip, .. } => Some(Self {
                kind: "midiSetNotes",
                object: op::ObjectRef::MidiClip(clip.clone()),
                path: None,
            }),
            other => Self::for_op(other),
        }
    }
}

#[cfg(debug_assertions)]
thread_local! {
    /// Debug-only marker: set for the duration of the commit
    /// [`GestureState::commit_transient_and_fold`] runs, so
    /// [`debug_assert_transient_invariant`] can tell a MID-GESTURE transient
    /// fold (sanctioned — the gesture batch that closes over it supersedes
    /// it) from any other transient write. A thread-local rather than a flag
    /// on the call path for the same reason `session.rs`'s `IN_TX` is one:
    /// the commit reaches `Committer` through `ControlPlane::commit_with`,
    /// several frames and one closure away, and threading a debug-only
    /// parameter through the public commit API to serve an assertion would
    /// be worse than the assertion is good. Correct because the marker's
    /// whole lifetime is inside one `f()` call on one thread.
    static IN_GESTURE_FOLD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// THE `transient` INVARIANT, CHECKED (whole-branch review, M-3 —
/// previously a comment on `HistoryMode` and a bullet in
/// `docs/SIDE-CHANNEL-INVENTORY.md`, and nothing else).
///
/// Redo replays a `HistoryEntry`'s stored `ops` against whatever the
/// document is NOW, so any write that mutates the fields those ops address
/// WITHOUT leaving a history entry — which is exactly what
/// `TxMeta::transient` means — moves the ground under a pending redo
/// without invalidating it. The rule is therefore about what may be MARKED
/// transient, and the two sanctioned classes are:
///
/// * transport writes — `Op::Set` against `ObjectRef::Transport`, the only
///   object no history entry can ever address (transport ops are transient
///   by construction, everywhere: `ControlPlane::transport`, and the
///   engine's sample-rate / auto-stop / recording-state mirrors);
/// * mid-gesture folds — superseded by the gesture batch that closes over
///   them, and recognizable here by [`IN_GESTURE_FOLD`].
///
/// Anything else marked transient is the silent-redo-corruption bug this
/// catches. Debug-only: the check walks a batch that is already in hand and
/// is a no-op in release, so it costs nothing where it would matter.
fn debug_assert_transient_invariant(committed: &session::Committed) {
    #[cfg(debug_assertions)]
    {
        if !committed.meta.transient || IN_GESTURE_FOLD.with(|f| f.get()) {
            return;
        }
        for op in &committed.ops {
            debug_assert!(
                matches!(op, op::Op::Set { object: op::ObjectRef::Transport, .. }),
                "transient batch {:?} writes {op:?}, which addresses a document field a \
                 history entry's ops can also address — a pending redo would silently land on \
                 a different state (see HistoryMode's doc). Either drop `.transient()` or \
                 fold this write into an explicit gesture.",
                committed.meta.label,
            );
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = committed;
}

/// A gesture in progress: one explicit `gesture_begin`..`gesture_end` span
/// (round-2 §4.4's CLAP-style primitive; ADR 0003). `baselines`/`last` are
/// `Vec`s, not `HashMap`s — `PropPath` isn't `Hash` (op.rs's closed enum,
/// left untouched — `fold_ops` makes the same call for the same reason), and
/// a gesture's key count is small (a handful of track/property pairs at
/// most), so a linear scan is a non-issue.
struct OpenGesture {
    actor: op::Actor,
    /// One correlation id for the WHOLE gesture — each transient commit
    /// folded into it keeps its own tx's `rev`, but the synthesized history
    /// batch this gesture eventually produces carries this run id instead.
    run: String,
    /// First-seen inverse per coalesce key — the gesture's baseline (what
    /// each key was BEFORE the gesture began; never overwritten once set).
    baselines: Vec<(CoalesceKey, op::Op)>,
    /// Last-seen forward op per key — what each key IS as of the most
    /// recent fold (overwritten every time the key reappears).
    last: Vec<(CoalesceKey, op::Op)>,
    label: String,
}

/// Holds the (at most one) open gesture. `ControlPlane` owns one instance;
/// `gesture_begin`/`gesture_end` and `set_track_mix`'s transient-fold check
/// all go through it. One gesture at a time is the product reality: a
/// second `gesture_begin` before the first one's `gesture_end` auto-closes
/// the stale gesture first (its accumulated batch is still committed, via
/// `begin`'s return value) — a missed pointerup (pointercancel not wired, a
/// webview reload mid-drag, ...) can never wedge the channel shut for good.
///
/// LOCK ORDER (fix round 1, Finding 2 — binding for every method here and
/// every caller): this mutex is always acquired BEFORE the session lock,
/// never after. `commit_transient_and_fold` is the one method that holds it
/// across a nested session-lock acquisition (via the commit closure it
/// runs) — that nesting (gesture, then session) is the only direction that
/// is safe. No path may take THIS mutex while the session lock is already
/// held: a `Session::transact` closure (`Tx`) has no access to
/// `GestureState` at all, so that direction is structurally impossible
/// today, not just a convention to remember.
pub struct GestureState(Mutex<Option<OpenGesture>>);

impl GestureState {
    fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// Opens a new gesture. If one was already open, it's taken (closed)
    /// and handed back to the caller to finish committing — `GestureState`
    /// has no `ControlPlane` handle of its own to synthesize/emit the
    /// auto-closed batch.
    fn begin(&self, label: String, actor: op::Actor) -> Option<OpenGesture> {
        let mut guard = self.0.lock();
        let stale = guard.take();
        *guard =
            Some(OpenGesture { actor, run: uuid::Uuid::new_v4().to_string(), baselines: Vec::new(), last: Vec::new(), label });
        stale
    }

    /// Closes the open gesture (if any), handing it back to the caller to
    /// synthesize/commit. `None` if nothing was open.
    fn end(&self) -> Option<OpenGesture> {
        self.0.lock().take()
    }

    /// Runs `f` — a commit — with THIS mutex held across the WHOLE
    /// check -> commit -> fold sequence, when a gesture is open matching
    /// `actor`; returns `None` (without calling `f` at all) when no gesture
    /// is open, or the open one is a different actor class, so the caller
    /// falls back to its own non-transient `commit` path instead.
    ///
    /// Fix round 1, Finding 2: this replaces a prior three-step sequence —
    /// a `matches_actor` check (gesture lock taken and released), then
    /// `commit_with` (no gesture lock held), then a separate `fold_in`
    /// re-acquiring the lock — which had a TOCTOU: a concurrent
    /// `gesture_end` (also takes this same mutex — see `end`) could close
    /// the gesture in the window between "a gesture is open" and "fold the
    /// result in", silently losing `f`'s commit from BOTH the gesture batch
    /// (already closed without it) and history (it ran transient, no
    /// `project://changed`, and nothing folds a transient commit into
    /// history once its gesture is gone). Reachable in practice: the
    /// frontend fires `setGain`/`setPan` (async, not awaited by the
    /// pointermove handler) and `gestureEnd` (on pointerup) essentially
    /// concurrently. Holding this mutex for the whole sequence closes the
    /// window: a concurrent `gesture_end` either runs entirely before this
    /// call starts (so `f` sees no gesture open and the caller commits
    /// normally, getting its own `project://changed`) or is blocked on this
    /// mutex until `f`'s result is already folded in (so it closes a
    /// gesture that includes it) — never in between.
    ///
    /// `f` is expected to (transitively, via `ControlPlane::commit_with`)
    /// take the session lock WHILE this mutex is held — see this struct's
    /// LOCK ORDER doc for why that nesting direction is the only safe one.
    fn commit_transient_and_fold<F>(
        &self,
        actor: &op::Actor,
        f: F,
    ) -> Option<Result<session::Committed, String>>
    where
        F: FnOnce() -> Result<session::Committed, String>,
    {
        let mut guard = self.0.lock();
        match guard.as_ref() {
            Some(g) if &g.actor == actor => {}
            _ => return None,
        }
        // M-3: mark the fold so `debug_assert_transient_invariant` can tell
        // this sanctioned transient write from an unsanctioned one. Reset
        // unconditionally afterwards — `f` returns a `Result`, it does not
        // unwind on a rejected commit, and a panicking `f` takes the whole
        // commit path down anyway.
        #[cfg(debug_assertions)]
        IN_GESTURE_FOLD.with(|g| g.set(true));
        let result = f();
        #[cfg(debug_assertions)]
        IN_GESTURE_FOLD.with(|g| g.set(false));
        if let Ok(committed) = &result {
            let g = guard
                .as_mut()
                .expect("this mutex was held for the whole call — the gesture can't have closed underneath it");
            Self::fold_committed(g, committed);
        }
        Some(result)
    }

    /// Folds one just-committed, already per-tx-folded batch (`committed.
    /// ops`/`.inverses` — `fold_ops`'s output, round-2 §4's commit-time
    /// fold) into `g`'s accumulator: the LAST forward op per key overwrites
    /// `last`; the FIRST-seen inverse per key is recorded into `baselines`
    /// once and never overwritten again (it's what the gesture restores on
    /// undo). Private, `&mut OpenGesture`-taking helper — the only caller
    /// is `commit_transient_and_fold`, which already holds this mutex.
    fn fold_committed(g: &mut OpenGesture, committed: &session::Committed) {
        for (op, inv) in committed.ops.iter().zip(committed.inverses.iter()) {
            let Some(key) = CoalesceKey::for_op(op) else { continue };
            if !g.baselines.iter().any(|(k, _)| *k == key) {
                g.baselines.push((key.clone(), inv.clone()));
            }
            match g.last.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = op.clone(),
                None => g.last.push((key, op.clone())),
            }
        }
    }
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

/// `set_track_mix`'s per-tx body, factored into a free function (Plan E
/// Task 14 fix round 1) so it can be rebuilt as a fresh `|tx| ...` closure
/// at each of `set_track_mix`'s two call sites (the gesture-transient path
/// and the plain non-gesture path) without fighting the borrow/move rules
/// of sharing ONE closure value across a branch that may or may not run.
fn apply_mix_changes(changes: &[TrackMixChange], tx: &mut session::Tx<'_>) -> Result<(), String> {
    for c in changes {
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
        log: Arc<history::HistoryLog>,
    ) -> Self {
        let latest_meters = Arc::new(Mutex::new(None));
        engine.send(ControlMsg::Subscribe(Box::new(LatestMeterCache(
            latest_meters.clone(),
        ))));
        // `emit` is promoted to `Arc` once here (Task 13) so both
        // `ControlPlane` and its `committer` can hold a clone of the SAME
        // closure instance — see `Committer`'s doc.
        let emit: Arc<EventEmitter> = Arc::new(emit);
        // `log` is the SAME `Arc<HistoryLog>` the engine's own `Committer`
        // holds (Task 17) — `audio::init` creates it and
        // `AudioState::history_log` hands it here, so the engine's
        // non-transient commits land in this history.
        let committer =
            Committer::new(session.clone(), shared.clone(), tables.clone(), emit.clone(), log);
        Self {
            session,
            shared,
            tables,
            engine,
            jobs,
            latest_meters,
            emit,
            committer,
            gesture: GestureState::new(),
            last_gesture_batch: Mutex::new(None),
        }
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
                // either way (§4.2) — `request` blocks THIS (calling)
                // thread until the engine control thread's own `handle`
                // call returns, and `stop_recording`'s finalize commit
                // (`Control::commit_recording_finalize`, Task 13) runs
                // entirely INSIDE that call, before its reply is sent — so
                // by the time `request` returns here, "stopped" is already
                // committed as an `Actor::Engine` tx. The Set below then
                // commits "stopped" a SECOND time (this thread's own
                // `Actor::User` tx) — harmless: the value is already
                // "stopped", `write_transport_prop` accepts it idempotently,
                // and the resulting extra `rev` bump/journal entry is the
                // ordinary cost of two independent, correctly-ordered
                // commits, not a race. (Committer's deadlock audit: this
                // `request` is the "only blocking request-reply INTO the
                // engine" case (b) describes — the reply is never held
                // across the engine's own commit.)
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
        // Plan E Task 14 (fix round 1, Finding 2): the actor-match check,
        // the transient commit, and the fold into the gesture's accumulator
        // now happen while `GestureState::commit_transient_and_fold` holds
        // ITS OWN mutex across the whole sequence — closing a TOCTOU where
        // a concurrent `gesture_end` could close the gesture in the window
        // between "is one open" and "fold the result in", silently losing
        // this commit from both the gesture batch and history. See that
        // method's doc for the full race and the lock-order rule it
        // depends on. `meta.clone()` is cheap (two Strings + an enum) and
        // keeps the ORIGINAL `meta` available for the non-gesture fallback
        // below — whether `commit_transient_and_fold`'s inner closure ever
        // runs is a RUNTIME decision this code can't make ahead of time.
        let gesture_meta = meta.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_with(gesture_meta.transient(), |tx| apply_mix_changes(&changes, tx), false)
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| apply_mix_changes(&changes, tx))?;
            }
        }
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

    // ---- plugin document reads (Task 9) ----------------------------------
    // Thin session reads for the `plugins::` command wrappers — mirrors
    // `get_track` above. Mutations go through `commit` (`Op::PluginAdd` /
    // `PluginRemove` / `PluginSetState` / `Set{Plugin, Param}`), never here.

    pub fn plugin_rows(&self) -> Vec<crate::plugins::PluginInstanceInfo> {
        self.session.lock().plugins.instances.clone()
    }

    pub fn plugin_row(&self, instance_id: &str) -> Option<crate::plugins::PluginInstanceInfo> {
        self.session.lock().plugins.instances.iter().find(|r| r.id == instance_id).cloned()
    }

    pub fn plugin_params(&self, instance_id: &str) -> Option<Vec<crate::plugins::ParamInfo>> {
        self.session.lock().plugins.params.get(instance_id).cloned()
    }

    /// True when `instance_id` names a registered plugin instance row (used
    /// by `set_track_instrument`'s `plugin:` ref validation).
    pub fn plugin_exists(&self, instance_id: &str) -> bool {
        self.session.lock().plugins.instances.iter().any(|r| r.id == instance_id)
    }

    /// Prime `session.plugins.pending_state[instance_id]` with bytes a
    /// caller ALREADY obtained from the live host (`HostStateBridge::
    /// save_state`) — a direct write, bypassing `commit`/the op log, used
    /// ONLY to seed accurate "current truth" before a `PluginSetState`
    /// commit whose inverse `apply_raw` computes FROM that truth (`zyn_
    /// load_patch`: apply_raw itself never round-trips the host — [C1] —
    /// so this is how the wrapper hands it the real previous blob instead
    /// of a stale/absent one).
    pub fn set_plugin_pending_state(&self, instance_id: &str, bytes: Vec<u8>) {
        self.session.lock().plugins.pending_state.insert(instance_id.to_string(), bytes);
    }

    /// Thin wrapper over `Committer::commit_with_rebuild` (Plan E Task 13
    /// pulled `commit`/`commit_with`'s full body out into the `Committer`
    /// so the engine control thread can share it — see `Committer`'s doc
    /// for the moved implementation and the full per-step rationale).
    /// `project://changed`'s frozen event contract, the `[C1]` lock-order
    /// guarantee, and every other behavior below are unchanged — only
    /// WHERE the code lives moved.
    pub fn commit<F>(&self, meta: op::TxMeta, f: F) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
    {
        self.commit_with(meta, f, true)
    }

    /// Same as [`Self::commit`], but the caller controls whether the frozen
    /// `project://changed` event fires (Plan E Task 12). `commit` delegates
    /// here with `emit_project_changed: true`; `ControlPlane::transport`
    /// passes `false`. `do_rebuild` here is `ControlPlane`'s half of Task
    /// 13's split: a `ControlPlane`-driven commit reaches its engine by
    /// sending `ControlMsg::Rebuild` over the channel (the engine control
    /// thread's OWN commits instead call `Control::rebuild` directly — see
    /// `Committer`'s deadlock audit for why that distinction matters).
    pub fn commit_with<F>(
        &self,
        meta: op::TxMeta,
        f: F,
        emit_project_changed: bool,
    ) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
    {
        self.committer.commit_with_rebuild(meta, f, emit_project_changed, || {
            self.engine.send(ControlMsg::Rebuild)
        })
    }

    // ---- undo / redo (Plan E Task 17 — the log turns on) -----------------

    /// Undo the most recent history step. `Ok(None)` when there is nothing
    /// to undo — an empty history is not an error, the UI just has a greyed
    /// menu item.
    ///
    /// The entry's `inverses` are applied through the NORMAL commit path
    /// (`Session::transact` -> effects -> journal), so an undo is a
    /// first-class transaction: it bumps `rev`, computes its own fresh
    /// effect, rebuilds the graph if it must, persists, emits
    /// `project://changed`, and is JOURNALED (a replay must see it — it IS
    /// a mutation). What it does NOT do is create a new history entry:
    /// `HistoryMode::Replay` suppresses that, and the ORIGINAL entry
    /// migrates, unchanged, onto the redo stack.
    ///
    /// Why migrate the entry rather than derive a new one from the undo
    /// commit: an entry is the exact op/inverse pair `Session::transact`
    /// (or `close_gesture`) produced for that edit, so moving it between
    /// stacks cannot drift. Redoing applies `entry.ops`, which is sound
    /// because EVERY op in the vocabulary is absolute-valued rather than a
    /// delta (`Set` carries `to`; `TempoSet`/`MidiSetNotes`/
    /// `AutomationSetLane`/`PluginSetState` are whole-value replacements;
    /// structural ops carry their full row) — see
    /// `tests/figma_invariant.rs`, which proves the whole cycle
    /// byte-identical.
    ///
    /// `meta`: `Actor::User` (a person asked for this undo, whoever made
    /// the original edit) but the ORIGINAL entry's `run` is PRESERVED, so
    /// the journal can correlate an edit with its own undo — attribution
    /// (§7 test 5) is about tracing a run, and an undo belongs to the run
    /// it reverses.
    ///
    /// On a failed commit the entry goes back on the undo stack untouched:
    /// a rejected undo must not silently consume a history step.
    pub fn undo(&self) -> Result<Option<String>, String> {
        let Some(entry) = self.committer.log().pop_undo() else { return Ok(None) };
        let meta = op::TxMeta {
            actor: op::Actor::User,
            run: entry.run.clone(),
            label: format!("undo: {}", entry.label),
            transient: false,
        };
        let ops = entry.inverses.clone();
        match self.commit_replay(meta, ops) {
            Ok(()) => {
                let label = entry.label.clone();
                self.committer.log().push_redo(entry);
                Ok(Some(label))
            }
            Err(e) => {
                self.committer.log().push_undo_unchanged(entry);
                Err(e)
            }
        }
    }

    /// Redo the most recently undone step — [`Self::undo`]'s mirror in
    /// every respect: `entry.ops` through the normal commit path, journaled,
    /// no new history entry, and the same entry migrates back onto the undo
    /// stack (via `push_undo_unchanged`, which does NOT clear the redo
    /// stack — only a genuinely new edit does that).
    pub fn redo(&self) -> Result<Option<String>, String> {
        let Some(entry) = self.committer.log().pop_redo() else { return Ok(None) };
        let meta = op::TxMeta {
            actor: op::Actor::User,
            run: entry.run.clone(),
            label: format!("redo: {}", entry.label),
            transient: false,
        };
        let ops = entry.ops.clone();
        match self.commit_replay(meta, ops) {
            Ok(()) => {
                let label = entry.label.clone();
                self.committer.log().push_undo_unchanged(entry);
                Ok(Some(label))
            }
            Err(e) => {
                self.committer.log().push_redo(entry);
                Err(e)
            }
        }
    }

    /// Apply a recorded op list through the normal commit path in
    /// `HistoryMode::Replay` — shared by [`Self::undo`] and [`Self::redo`].
    fn commit_replay(&self, meta: op::TxMeta, ops: Vec<op::Op>) -> Result<(), String> {
        self.committer
            .commit_with_rebuild_mode(
                meta,
                |tx| {
                    for op in ops {
                        tx.apply(op)?;
                    }
                    Ok(())
                },
                true,
                || self.engine.send(ControlMsg::Rebuild),
                history::HistoryMode::Replay,
            )
            .map(|_| ())
    }

    /// `(undo_depth, redo_depth)` — what the `undo`/`redo` commands return
    /// alongside their result so the UI can enable/disable menu items
    /// without a second round trip.
    pub fn history_depths(&self) -> (usize, usize) {
        self.committer.log().depths()
    }

    // ---- gestures (Plan E Task 14) ---------------------------------------

    /// Opens a gesture boundary — the CLAP-style primitive round-2 §4.4 /
    /// ADR 0003 describe. While open, matching commits (today: only
    /// `set_track_mix` checks — same actor class as this gesture's) run
    /// TRANSIENT and fold into this gesture's accumulator instead of
    /// reaching history directly; `gesture_end` synthesizes the ONE
    /// history-bound batch the whole drag reduces to. One gesture at a time
    /// is the product reality: a second `gesture_begin` before the first
    /// one's `gesture_end` auto-closes the stale gesture first (committing
    /// whatever it had accumulated) — a missed pointerup (pointercancel not
    /// wired, a webview reload mid-drag, ...) can never wedge the channel
    /// shut for good. Always `Actor::User` — gestures are a frontend-only
    /// concept today; no MCP tool opens one.
    pub fn gesture_begin(&self, label: String) -> Result<(), String> {
        if let Some(stale) = self.gesture.begin(label, op::Actor::User) {
            self.close_gesture(stale);
        }
        Ok(())
    }

    /// Closes the open gesture, synthesizing and committing its one
    /// history-bound batch (see `close_gesture`). A no-op — not an error —
    /// when nothing is open: `pointerup`/`pointercancel` firing without a
    /// matching `pointerdown`, or a double-fire, must never error the IPC
    /// channel.
    pub fn gesture_end(&self) -> Result<(), String> {
        if let Some(g) = self.gesture.end() {
            self.close_gesture(g);
        }
        Ok(())
    }

    /// Synthesizes and finalizes a closed gesture's ONE `Committed`-shaped
    /// history entry: `ops` = last-forward op per key (first-seen order),
    /// `inverses` = first-baseline op per key, REVERSED (ready to apply in
    /// undo order — mirrors `Session::transact`'s own `inverses.reverse()`);
    /// `meta` carries the gesture's own `run`/`label`, non-transient (this
    /// IS the history-bound commit the whole gesture reduces to). `rev`/
    /// `epoch` are read fresh off the session — this synthesized entry
    /// isn't produced by `Session::transact` (its ops already landed, one
    /// transient tx at a time, while the gesture was open), so there is no
    /// single tx's `Committed` to borrow them from; the session's CURRENT
    /// values are the correct ones to stamp a history entry describing
    /// "the document as of gesture-close" with.
    ///
    /// Task 17: HISTORY IS NOW THE DIRECT CONSUMER. The batch is handed to
    /// `HistoryLog::record_gesture` right here, inside this function,
    /// BEFORE the `project://changed` emit — so there is no window in which
    /// a closed gesture exists but is not yet undoable (the single-slot
    /// `take_last_gesture_batch` park that stood in for this until now is
    /// kept, and kept a test surface only; see its own doc). Recorded, not
    /// re-committed: the ops already ran, one transient commit at a time,
    /// while the gesture was open — this is the one place a `Committed`
    /// reaches history without passing through `commit_with_rebuild`, and
    /// it is exactly why `close_gesture`'s `effect` is a documented
    /// placeholder.
    ///
    /// LOCK ORDER: `GestureState`'s mutex is NOT held here —
    /// `gesture_end`/`gesture_begin` take it, `take`/`end` the gesture out
    /// of it, and drop it before calling this. So the acquisition here is
    /// session (briefly, for `rev`/`epoch`) and then the history/journal
    /// leaf mutexes, never the reverse, never nested with the gesture lock.
    ///
    /// Also emits exactly ONE
    /// `project://changed` — every transient commit folded into this
    /// gesture emitted none of its own (`commit_with(..., false)`), so this
    /// is the gesture's only announcement, keeping an "N invokes -> 1
    /// event" contract even though N `set_track_mix` calls landed under the
    /// hood.
    ///
    /// A gesture that never folded anything (`last` empty — e.g. a
    /// `pointerdown`/`pointerup` with no drag in between) produces no batch
    /// and no emit: nothing changed, so there is nothing for history or the
    /// UI to hear about.
    fn close_gesture(&self, gesture: OpenGesture) {
        if gesture.last.is_empty() {
            return;
        }
        let ops: Vec<op::Op> = gesture.last.into_iter().map(|(_, op)| op).collect();
        let mut inverses: Vec<op::Op> = gesture.baselines.into_iter().map(|(_, op)| op).collect();
        inverses.reverse();
        let meta = op::TxMeta { actor: gesture.actor, run: gesture.run, label: gesture.label, transient: false };
        let (rev, epoch) = {
            let session = self.session.lock();
            (session.rev, session.epoch)
        };
        let committed = session::Committed {
            rev,
            epoch,
            ops,
            inverses,
            // PLACEHOLDER, not a real effect description (fix round 1,
            // Finding 1): the ops this batch carries already ran — each one
            // executed its OWN effect (param writes, RT atomics, ...) as
            // its own transient commit while the gesture was open. There is
            // nothing left to execute for THIS synthesized entry; it exists
            // for history (undo/redo) only. A history consumer (Task 17)
            // must replay `ops`/`inverses` through a NEW `commit`/
            // `commit_with` call (which computes its OWN fresh effect from
            // the replayed op) — it must NEVER read or execute this
            // `effect` field, which would silently do nothing on redo.
            effect: session::EngineEffect::default(),
            meta: meta.clone(),
        };
        // Task 17: the direct sink. No drop-window — the gesture is
        // undoable the instant it closes.
        self.committer.log().record_gesture(
            committed.rev,
            committed.epoch,
            &committed.meta,
            &committed.ops,
            &committed.inverses,
        );
        *self.last_gesture_batch.lock() = Some(committed);

        // Same payload shape `Committer::commit_with_rebuild` emits (Task
        // 13's frozen `project://changed` contract) — the full `Project`
        // shape plus rev/label/actor as additive top-level fields.
        let mut payload = serde_json::to_value(self.committer.project_changed_payload())
            .unwrap_or_else(|_| serde_json::json!({}));
        if let serde_json::Value::Object(map) = &mut payload {
            map.insert("rev".into(), serde_json::json!(rev));
            map.insert("label".into(), serde_json::json!(meta.label));
            map.insert("actor".into(), serde_json::to_value(&meta.actor).unwrap_or_default());
        }
        (self.emit)("project://changed", payload);
    }

    /// TEST SURFACE (Task 17 settled its status): takes (removes) the last
    /// gesture batch `close_gesture` parked — either from an explicit
    /// `gesture_end` or an auto-close inside `gesture_begin`. History is no
    /// longer a consumer of this slot: `close_gesture` records into
    /// `HistoryLog` DIRECTLY (see its doc), which is what closes the
    /// drop-window this park used to stand in for. The slot is retained
    /// because it is the only way a test can inspect the exact synthesized
    /// batch — its `ops`/`inverses`/`meta` — without reaching into the
    /// history stacks. Draining promptly
    /// matters: a second closing gesture overwrites this slot, so a caller
    /// that wants BOTH an auto-closed batch and the one that follows it
    /// must take the first before triggering the second (this crate's own
    /// auto-close test does exactly that).
    ///
    /// The returned batch's `effect` is a PLACEHOLDER (fix round 1, Finding
    /// 1) — already executed, one transient commit at a time, while the
    /// gesture was open. A history consumer replays `ops`/`inverses`
    /// through a NEW commit; it never reads or executes this `effect`
    /// field directly (see `close_gesture`'s doc on that field).
    // Read only by this crate's tests (Task 17 made history the direct
    // consumer at `close_gesture`), so the compiler sees no production
    // caller in a non-test build.
    #[allow(dead_code)]
    pub(crate) fn take_last_gesture_batch(&self) -> Option<session::Committed> {
        self.last_gesture_batch.lock().take()
    }

    /// Test-only accessor to the shared session lock, for tests that need
    /// to assert on/mutate store state directly around a `commit`-driven
    /// call (Task 7 brief).
    #[cfg(test)]
    pub fn session(&self) -> &Arc<Mutex<Session>> {
        &self.session
    }

    /// Test-only accessor to the `Committer` (review round 1, Important-2):
    /// `ControlPlane` no longer has its own `execute_persist` — the only
    /// production caller is `Committer::commit_with_rebuild` itself, calling
    /// `self.execute_persist` where `self: &Committer`. Direct test callers
    /// that used to write `cp.execute_persist(...)` now write
    /// `cp.committer().execute_persist(...)` — a real accessor instead of a
    /// forwarding method that existed for tests alone (which is exactly the
    /// shape that reads as dead code to the compiler once nothing in
    /// production calls it).
    #[cfg(test)]
    pub(crate) fn committer(&self) -> &Committer {
        &self.committer
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
        let new_epoch;
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

            // epoch boundary: Task 17's history-clear + journal rotation runs
            // just below, once this guard drops (the journal writes to disk,
            // and no I/O may happen under the session lock — round-2 §4).
            // Fix round 1 (Task 7 review finding 2): bump the document-swap
            // epoch counter here — see `Session::epoch`'s doc.
            session.epoch += 1;
            new_epoch = session.epoch;

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
        // epoch boundary (Task 17): document birth — history and redo are
        // cleared (they describe a document that is no longer open) and the
        // journal rotates onto the new project dir, where its first record
        // is this boundary.
        self.committer.log().epoch_boundary(&dir, history::EpochEvent::Create, new_epoch);
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
        let new_epoch;
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
            // epoch boundary: Task 17's history-clear + journal rotation runs
            // below, after this guard drops (journal I/O never under the
            // session lock).
            // Fix round 1 (Task 7 review finding 2): bump the document-swap
            // epoch counter here — see `Session::epoch`'s doc.
            session.epoch += 1;
            new_epoch = session.epoch;
            // Eager midi adopt (Task 6: no more lazy resync on the first
            // midi command after an open) — same lock as the store swap
            // above, no separate re-acquisition.
            let bpm = session.store.transport.tempo_bpm;
            crate::midi::adopt_midi_from_dir(&mut session.midi, &dir, bpm);
        }
        // ---- session lock released; host round-trips + rebuild + emit below ----
        // epoch boundary (Task 17): document swap = history root. Undoing
        // across it would apply this project's inverses to a DIFFERENT
        // document (ruling 4), so both stacks are cleared and the journal
        // rotates onto the newly opened project's own file — appending, so
        // that project's earlier sessions stay in its log.
        self.committer.log().epoch_boundary(&dir, history::EpochEvent::Open, new_epoch);
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
        let new_epoch;
        let (project, midi_snapshot) = {
            let mut session = self.session.lock();
            session.store.project_dir = Some(dir.to_path_buf());
            session.store.project_name = Some(name);
            session.store.created_at = created_at;
            // epoch boundary: Task 17's history-clear + journal rotation runs
            // below, after this guard drops (journal I/O never under the
            // session lock).
            // Fix round 1 (Task 7 review finding 2): bump the document-swap
            // epoch counter here — see `Session::epoch`'s doc.
            session.epoch += 1;
            new_epoch = session.epoch;
            // Mark the midi store as belonging to `dir` NOW (under the same
            // lock as the store swap); the snapshot taken alongside it is
            // written to disk below, AFTER the lock drops.
            session.midi.loaded_dir = Some(dir.to_path_buf());
            let project = project::from_store(&session.store, position, rate)?;
            let midi_snapshot = session.midi_snapshot();
            (project, midi_snapshot)
        };
        // ---- session lock released; all disk I/O below ----
        // epoch boundary (Task 17): the session just acquired an identity.
        // History is cleared for the same reason as the other swaps — the
        // pre-save-as entries describe an UNSAVED document that no longer
        // exists as such — and the journal opens, for the first time in an
        // unsaved session's life, in the freshly minted dir.
        self.committer.log().epoch_boundary(dir, history::EpochEvent::SaveAs, new_epoch);
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
        let (project, dir, epoch) = {
            let session = self.session.lock();
            let dir = session.store.project_dir.clone().ok_or("no project open")?;
            let project = project::from_store(&session.store, position, rate)?;
            (project, dir, session.epoch)
        };
        // epoch boundary: no document swap here (same project, same
        // in-memory content) — so history is NOT cleared and the journal is
        // NOT rotated. Task 17 journals a "save" MARK record instead: it
        // tells a replay where the on-disk snapshot caught up with the log,
        // which is the whole difference between a snapshot mark and an
        // epoch (ruling 4).
        project::save(&dir, &project)?;
        self.committer.log().snapshot_mark(epoch);
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
                let dir =
                    PathBuf::from(project.path.clone().expect("just-created project has a path"));
                // epoch boundary (Task 17). The store swap itself lives in
                // `project::ensure_default_project` (audio/project.rs, which
                // has no `ControlPlane` and must not grow one), so the
                // history/journal half hooks in HERE — the one place both
                // callers of that swap funnel through: this fn is what the
                // engine's `ensure_project` invokes via the closure lib.rs
                // installs, and it is also the standalone MCP/command entry
                // point. `Some(project)` means the swap actually happened
                // (`None` = a project was already open, nothing to rotate),
                // so this is exactly as often as the epoch counter bumped.
                let epoch = self.session.lock().epoch;
                self.committer.log().epoch_boundary(&dir, history::EpochEvent::Ensure, epoch);
                (self.emit)(
                    "project://changed",
                    serde_json::to_value(&project).unwrap_or_default(),
                );
                Ok(dir)
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
        // tracks so a failure leaves no half-bound state (None = PolySynth).
        let zyn = try_seed_zyn_demo_instruments(&self.session);

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
        // job here — `try_seed_zyn_demo_instruments` writes the document
        // rows DIRECTLY into `session.plugins`, bypassing `commit`/the op
        // log, so `execute_persist`'s plugin branch never sees this write),
        // so a save/open cycle replays the same demo through the same
        // patches (zone P4 restore path). A no-op when there's no open
        // project dir or no Zyn instances. Task 9: `persist_after_mutation`
        // is gone — snapshot the doc under a short lock and write it
        // directly (mirrors `execute_persist`'s own plugin branch).
        if zyn.is_some() {
            let dir = self.session.lock().store.project_dir.clone();
            if let Some(d) = &dir {
                let doc = self.session.lock().plugin_snapshot();
                // `with_host_state: true` here always takes the `fresh`
                // branch of the persist ladder for these brand-new
                // instances, so there's nothing in `dirty_state` to clear
                // (seed_demo writes rows directly, bypassing `apply_raw`
                // entirely — see above).
                if let Err(e) = crate::plugins::state::save_snapshot_into_project(d, &doc, true) {
                    log::warn!("seed demo: persisting plugins failed: {e}");
                }
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
/// Partial failures roll back (no orphan instances, no orphan rows).
///
/// Task 9: writes the document rows DIRECTLY into `session.plugins`
/// (bypassing `commit`/the op log), matching `seed_demo_project`'s own
/// direct-session-mutation style for the rest of the demo's bootstrap —
/// this pre-track-creation, best-effort batch is not itself a user-visible
/// edit yet (no track references these ids until the caller binds them),
/// so there is nothing meaningful to make undoable here.
fn try_seed_zyn_demo_instruments(session: &Arc<Mutex<Session>>) -> Option<[String; 3]> {
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
            Ok((info, params)) => {
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
                {
                    let mut s = session.lock();
                    s.plugins.instances.push(info.clone());
                    s.plugins.params.insert(info.id.clone(), params);
                }
                ids.push(info.id);
            }
            Err(e) => {
                log::warn!("seed demo: Zyn instantiation failed ({e}); PolySynth fallback");
                {
                    // Session lock dropped BEFORE the host call below ([C1]:
                    // never call a host while holding the session lock —
                    // `unregister_instance` is fire-and-forget/non-blocking,
                    // but this stays disciplined regardless).
                    let mut s = session.lock();
                    for id in &ids {
                        s.plugins.instances.retain(|r| &r.id != id);
                        s.plugins.params.remove(id);
                    }
                }
                for id in &ids {
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

/// Open a gesture boundary (Plan E Task 14 — round-2 inventory row 31, ADR
/// 0003) — thin delegate over [`ControlPlane::gesture_begin`]. The frontend
/// calls this on `pointerdown` of a fader/pan control; matching mid-gesture
/// `set_track_mix` calls fold backend-side until `gesture_end` closes the
/// boundary.
#[tauri::command]
pub fn gesture_begin(label: String, control: State<'_, Arc<ControlPlane>>) -> Result<(), String> {
    control.gesture_begin(label)
}

/// Close the open gesture boundary (Plan E Task 14) — thin delegate over
/// [`ControlPlane::gesture_end`]. The frontend calls this on `pointerup`/
/// `pointercancel`; a no-op (never an error) if nothing is open.
#[tauri::command]
pub fn gesture_end(control: State<'_, Arc<ControlPlane>>) -> Result<(), String> {
    control.gesture_end()
}

/// What `undo`/`redo` hand back: the label of the step that moved (or
/// `null` when the stack was empty — an empty history is not an error) plus
/// both stack depths, so the UI can update its menu items from ONE round
/// trip. Additive command, additive payload (D-06: readers ignore fields
/// they don't recognize).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryStep {
    /// The undone/redone step's ORIGINAL label (not the `"undo: …"` one the
    /// commit carries) — that is what a UI shows in "Undo <label>".
    pub label: Option<String>,
    pub undo_depth: usize,
    pub redo_depth: usize,
}

/// Undo the most recent history step (Plan E Task 17) — thin delegate over
/// [`ControlPlane::undo`]. Additive command.
#[tauri::command]
pub fn undo(control: State<'_, Arc<ControlPlane>>) -> Result<HistoryStep, String> {
    let label = control.undo()?;
    let (undo_depth, redo_depth) = control.history_depths();
    Ok(HistoryStep { label, undo_depth, redo_depth })
}

/// Redo the most recently undone step (Plan E Task 17) — thin delegate over
/// [`ControlPlane::redo`]. Additive command.
#[tauri::command]
pub fn redo(control: State<'_, Arc<ControlPlane>>) -> Result<HistoryStep, String> {
    let label = control.redo()?;
    let (undo_depth, redo_depth) = control.history_depths();
    Ok(HistoryStep { label, undo_depth, redo_depth })
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
            std::sync::Arc::new(crate::control::HistoryLog::new()),
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
            std::sync::Arc::new(crate::control::HistoryLog::new()),
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
            crate::control::testutil::test_committer(&session, &shared, &tables),
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
            std::sync::Arc::new(crate::control::HistoryLog::new()),
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

    /// I-3 (Plan E whole-branch review): `execute_host_forward`'s Instantiate
    /// writeback used to re-lock the session and write `status`/`params` with
    /// no epoch guard, so a project swap in flight got another project's
    /// plugin state written into it. Same guard shape `execute_persist` uses.
    #[test]
    fn instantiate_writeback_lands_when_the_epoch_is_unchanged() {
        let (cp, _events, _engine) = recording_control_plane();
        let row = crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(),
            uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "stub".into(),
            track_id: None,
        };
        let epoch = {
            let mut s = cp.session().lock();
            s.plugins.instances.push(row);
            s.epoch
        };
        let params = vec![crate::plugins::ParamInfo {
            id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
            default: 0.5, value: 0.25, steps: 0,
        }];
        cp.committer().apply_instantiate_writeback("inst-1", params, epoch);

        let s = cp.session().lock();
        assert_eq!(s.plugins.instances[0].status, "active");
        assert_eq!(s.plugins.params["inst-1"].len(), 1);
        assert_eq!(s.plugins.params["inst-1"][0].value, 0.25);
    }

    #[test]
    fn instantiate_writeback_is_skipped_when_the_epoch_moved_under_it() {
        let (cp, _events, _engine) = recording_control_plane();
        let row = crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(),
            uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "stub".into(),
            track_id: None,
        };
        let stale_epoch = {
            let mut s = cp.session().lock();
            s.plugins.instances.push(row);
            let e = s.epoch;
            s.epoch += 1; // an epoch function swapped the document meanwhile
            e
        };
        let params = vec![crate::plugins::ParamInfo {
            id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
            default: 0.5, value: 0.25, steps: 0,
        }];
        cp.committer().apply_instantiate_writeback("inst-1", params, stale_epoch);

        let s = cp.session().lock();
        assert_eq!(s.plugins.instances[0].status, "stub", "status must not be written");
        assert!(
            !s.plugins.params.contains_key("inst-1"),
            "the params mirror must not be CREATED for a document this commit no longer describes"
        );
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

    // ---- Plan E Task 14: gesture IPC ---------------------------------------

    /// The brief's core TDD case: begin -> 5x `set_track_mix` gain on ONE
    /// track -> end produces exactly ONE history-bound batch whose inverse
    /// restores the pre-gesture gain, and the 5 mid-gesture commits' would-
    /// be `project://changed` spam is reduced to ONE final emit.
    #[test]
    fn gesture_folds_five_gain_sets_into_one_batch_with_one_final_emit() {
        let (plane, _engine_rx, events) = test_plane_with_tracks(&["t-1"]);
        plane.gesture_begin("gain drag".into()).unwrap();

        for i in 1..=5 {
            plane
                .set_track_mix(
                    vec![TrackMixChange { gain_db: Some(-3.0 * i as f64), ..TrackMixChange::new("t-1") }],
                    TxMeta::user("set gain"),
                )
                .unwrap();
        }

        assert!(
            events.lock().iter().all(|(name, _)| name != "project://changed"),
            "mid-gesture transient commits must not emit project://changed"
        );

        plane.gesture_end().unwrap();

        let emits = events.lock().iter().filter(|(name, _)| name == "project://changed").count();
        assert_eq!(emits, 1, "gesture_end must emit exactly one project://changed");

        let batch = plane.take_last_gesture_batch().expect("gesture_end must produce a batch");
        assert!(!batch.meta.transient, "the history-bound batch is non-transient");
        assert_eq!(batch.ops.len(), 1, "one folded Set for the one (track, path) key touched");
        assert_eq!(batch.inverses.len(), 1);

        match &batch.ops[0] {
            Op::Set { object: ObjectRef::Track(id), path: PropPath::Gain, to, .. } => {
                assert_eq!(id, "t-1");
                assert_eq!(*to, serde_json::json!(-15.0), "last-forward value wins");
            }
            other => panic!("expected a Track Gain Set, got {other:?}"),
        }
        match &batch.inverses[0] {
            Op::Set { object: ObjectRef::Track(id), path: PropPath::Gain, to, .. } => {
                assert_eq!(id, "t-1");
                assert_eq!(*to, serde_json::json!(0.0), "inverse restores the pre-gesture (baseline) gain");
            }
            other => panic!("expected a Track Gain Set inverse, got {other:?}"),
        }
    }

    /// Two tracks touched during one gesture: the batch carries ONE folded
    /// Set per (track, path) key, not one per `set_track_mix` call.
    #[test]
    fn gesture_folds_one_set_per_track_path_key_across_two_tracks() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1", "t-2"]);
        plane.gesture_begin("gain drag".into()).unwrap();

        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-3.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set gain"),
            )
            .unwrap();
        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-4.0), ..TrackMixChange::new("t-2") }],
                TxMeta::user("set gain"),
            )
            .unwrap();
        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-6.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set gain"),
            )
            .unwrap();

        plane.gesture_end().unwrap();
        let batch = plane.take_last_gesture_batch().expect("gesture_end must produce a batch");
        assert_eq!(batch.ops.len(), 2, "one folded Set per (track, path) key, not one per call");

        let mut finals: Vec<(String, f64)> = batch
            .ops
            .iter()
            .map(|op| match op {
                Op::Set { object: ObjectRef::Track(id), path: PropPath::Gain, to, .. } => {
                    (id.to_string(), to.as_f64().unwrap())
                }
                other => panic!("expected a Track Gain Set, got {other:?}"),
            })
            .collect();
        finals.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(finals, vec![("t-1".to_string(), -6.0), ("t-2".to_string(), -4.0)]);
    }

    /// `gesture_begin` while one is already open auto-closes the stale
    /// gesture first (committing its batch) instead of discarding it — a
    /// missed `pointerup` must not wedge the channel. Both batches are
    /// retrievable, in order, by draining `take_last_gesture_batch` between
    /// the auto-close and the second, explicit `gesture_end`.
    #[test]
    fn gesture_begin_while_open_auto_closes_the_stale_gesture_first() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);

        plane.gesture_begin("gain drag".into()).unwrap();
        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-3.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set gain"),
            )
            .unwrap();

        // No matching `gesture_end` — a second `begin` must auto-close it.
        plane.gesture_begin("pan drag".into()).unwrap();
        let first = plane.take_last_gesture_batch().expect("auto-close must commit a batch");
        assert_eq!(first.meta.label, "gain drag");

        plane
            .set_track_mix(
                vec![TrackMixChange { pan: Some(0.5), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set pan"),
            )
            .unwrap();
        plane.gesture_end().unwrap();
        let second = plane.take_last_gesture_batch().expect("gesture_end must commit a batch");
        assert_eq!(second.meta.label, "pan drag");

        assert_ne!(first.meta.run, second.meta.run, "two independent batches, two independent run ids");
    }

    /// A gesture that never folds anything (begin immediately followed by
    /// end, no `set_track_mix` in between — e.g. a click with no drag)
    /// produces no batch and no emit.
    #[test]
    fn gesture_end_with_nothing_folded_produces_no_batch_and_no_emit() {
        let (plane, _engine_rx, events) = test_plane_with_tracks(&["t-1"]);
        plane.gesture_begin("gain drag".into()).unwrap();
        plane.gesture_end().unwrap();

        assert!(plane.take_last_gesture_batch().is_none());
        assert!(events.lock().iter().all(|(name, _)| name != "project://changed"));
    }

    /// `gesture_end` without a matching `gesture_begin` (a stray
    /// `pointerup`/`pointercancel`) is a safe no-op, never an error — the
    /// IPC channel must not wedge on a mismatched event pair.
    #[test]
    fn gesture_end_without_a_matching_begin_is_a_safe_no_op() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        assert!(plane.gesture_end().is_ok());
        assert!(plane.take_last_gesture_batch().is_none());
    }

    /// `take_last_gesture_batch` is a single slot, not a queue (documented
    /// on the fn itself and on `GestureState`'s doc) — pins that limitation
    /// with a direct repro: two gestures close back-to-back with no drain
    /// between them, and the FIRST batch is simply gone, overwritten by the
    /// second, never queued up behind it.
    #[test]
    fn take_last_gesture_batch_is_a_single_slot_not_a_queue() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);

        plane.gesture_begin("gain drag".into()).unwrap();
        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-3.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set gain"),
            )
            .unwrap();
        plane.gesture_end().unwrap();
        // First ("gain drag") batch parked — deliberately NOT drained here.

        plane.gesture_begin("pan drag".into()).unwrap();
        plane
            .set_track_mix(
                vec![TrackMixChange { pan: Some(0.5), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set pan"),
            )
            .unwrap();
        plane.gesture_end().unwrap();
        // Second close overwrites the slot before anyone drained the first.

        let batch = plane.take_last_gesture_batch().expect("the slot holds the SECOND batch");
        assert_eq!(
            batch.meta.label, "pan drag",
            "the single slot is overwritten by the second close — the first (\"gain drag\") \
             batch is gone, never queued"
        );
        assert!(
            plane.take_last_gesture_batch().is_none(),
            "draining again finds nothing — only one batch was ever retrievable"
        );
    }

    /// Fix round 1, Finding 2's regression test: a real concurrent race
    /// between a mid-gesture `set_track_mix` and a `gesture_end` on the
    /// SAME open gesture. Before the fix, this window could silently drop
    /// the mid-gesture value from both the gesture batch (already closed
    /// without it) and history (it ran transient, no `project://changed`,
    /// and nothing folds a transient commit into history once its gesture
    /// is gone) — reachable in practice because the frontend fires
    /// `setGain` (async, not awaited by the pointermove handler) and
    /// `gestureEnd` (on pointerup) essentially concurrently.
    ///
    /// `commit_transient_and_fold` now holds `GestureState`'s mutex across
    /// the whole check -> commit -> fold sequence, so however the OS
    /// actually schedules the two threads, only two outcomes are possible
    /// — never a silent third: (a) the mixer thread's commit sees the
    /// gesture still open, folds into it, and `gesture_end` (blocked on the
    /// same mutex meanwhile) closes a batch that includes it; or (b)
    /// `gesture_end` runs first and closes an empty-of-this-value gesture,
    /// so the mixer thread's commit finds no gesture open and falls back to
    /// a plain, non-transient commit — which emits its OWN
    /// `project://changed`. This test asserts the value shows up in EXACTLY
    /// one of those two places (never neither), across a real thread race
    /// (a `Barrier` only synchronizes the threads' START; which one
    /// actually wins the mutex is still up to the OS scheduler — both
    /// outcomes are valid and both are checked for, so the test is not
    /// flaky).
    #[test]
    fn gesture_fold_and_close_race_never_silently_loses_a_mid_gesture_commit() {
        let (plane, events, engine) = recording_control_plane();
        let track = plane.add_track(Some("Race".into()), None, TxMeta::user("add track")).unwrap();
        events.lock().clear();

        plane.gesture_begin("gain drag".into()).unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (b1, b2) = (barrier.clone(), barrier.clone());
        let (plane1, plane2) = (plane.clone(), plane.clone());
        let track_id = track.id.as_str().to_string();

        let mixer = std::thread::spawn(move || {
            b1.wait();
            plane1.set_track_mix(
                vec![TrackMixChange { gain_db: Some(-9.0), ..TrackMixChange::new(track_id.as_str()) }],
                TxMeta::user("set gain"),
            )
        });
        let closer = std::thread::spawn(move || {
            b2.wait();
            plane2.gesture_end()
        });

        assert!(mixer.join().unwrap().is_ok(), "the mid-gesture set_track_mix must still succeed");
        assert!(closer.join().unwrap().is_ok(), "gesture_end must still succeed");

        let in_gesture_batch = plane
            .take_last_gesture_batch()
            .map(|b| {
                b.ops.iter().any(|op| {
                    matches!(
                        op,
                        Op::Set { object: ObjectRef::Track(id), path: PropPath::Gain, to, .. }
                            if id == track.id.as_str() && *to == serde_json::json!(-9.0)
                    )
                })
            })
            .unwrap_or(false);
        let in_post_gesture_emit = events.lock().iter().any(|(name, payload)| {
            name == "project://changed"
                && payload["tracks"]
                    .as_array()
                    .map(|ts| {
                        ts.iter().any(|t| t["id"] == track.id.as_str() && t["gainDb"] == -9.0)
                    })
                    .unwrap_or(false)
        });
        assert!(
            in_gesture_batch || in_post_gesture_emit,
            "the mid-gesture gain change must land in the gesture's closing batch OR as its own \
             post-gesture commit — it must never be silently lost from both"
        );

        engine.send(crate::audio::engine::ControlMsg::Shutdown);
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

    /// A bare `Committer` over a fresh session/shared/tables, plus an
    /// `AtomicUsize` the caller passes as `do_rebuild` to count how many
    /// times a commit's folded effect actually asked for a rebuild — the
    /// engine's own sites 1-4 (Plan E Task 13) call `self.rebuild()`
    /// directly from that closure instead of round-tripping through
    /// `ControlMsg::Rebuild`; these tests exercise the SAME
    /// `commit_with_rebuild` primitive engine.rs calls, just with a
    /// counting closure standing in for `Control::rebuild`.
    fn test_committer() -> (Committer, Arc<Mutex<Session>>) {
        let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
        let shared = Arc::new(SharedRt::default());
        let tables = empty_tables();
        let committer =
            Committer::new(session.clone(), shared, tables, Arc::new(Box::new(|_: &str, _: serde_json::Value| {}) as EventEmitter), std::sync::Arc::new(crate::control::HistoryLog::new()));
        (committer, session)
    }

    /// Plan E Task 13's TDD step 1, corrected by review round 1
    /// (Important-1): recording finalize is now TWO commits, not one —
    /// `Control::commit_recording_finalize`'s exact shape (ClipAdd x n
    /// only) followed by `Control::commit_recording_stopped_state`'s
    /// (the transport-state Set, transient, separate). Bundling the state
    /// flip into the ClipAdd tx would make it part of THAT tx's inverse:
    /// once Task 17 lands undo history, undoing "stop recording" would
    /// restore `state = "recording"` in the same step that un-registers the
    /// clips — a document claiming a take is running while nothing
    /// records. This test pins both commits and their independent shapes:
    /// the non-transient one carries ONLY `ClipAdd`s (with exactly one
    /// rebuild — `EngineEffect::rebuild` folds to one flag even though TWO
    /// `ClipAdd`s are applied in the same transaction; "at most one
    /// `Rebuild` per transaction" is a claim about ALL rebuilds,
    /// engine-originated ones included), the transient one carries ONLY the
    /// state `Set` (no rebuild — `Op::Set{Transport, ...}` never sets
    /// `effect.rebuild`, Task 12's transport family).
    #[test]
    fn recording_finalize_commits_as_actor_engine_with_clip_add_ops_and_one_rebuild() {
        let (committer, session) = test_committer();
        let clip_a = crate::audio::types::testutil::test_clip("c-1", "t-1");
        let clip_b = crate::audio::types::testutil::test_clip("c-2", "t-1");
        let clips = vec![clip_a.clone(), clip_b.clone()];
        let rebuilds = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        // Commit 1: ClipAdd x n only, non-transient.
        let rebuilds2 = rebuilds.clone();
        let clip_committed = committer
            .commit_with_rebuild(
                TxMeta::engine("stop recording"),
                |tx| {
                    for clip in &clips {
                        let idx = tx.store().clips.len();
                        tx.apply(Op::ClipAdd { clip: clip.clone(), index: idx })?;
                    }
                    Ok(())
                },
                true,
                move || {
                    rebuilds2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                },
            )
            .unwrap();
        assert!(
            matches!(clip_committed.meta.actor, crate::control::op::Actor::Engine),
            "finalize must be attributed to Actor::Engine, got {:?}",
            clip_committed.meta.actor
        );
        assert!(!clip_committed.meta.transient, "clip registration is a real document edit, not transient");
        assert!(
            clip_committed.ops.iter().all(|op| matches!(op, Op::ClipAdd { .. })),
            "the clip-registration commit must carry ONLY ClipAdd ops, got {:?}",
            clip_committed.ops
        );
        let clip_ids: Vec<&str> = clip_committed
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::ClipAdd { clip, .. } => Some(clip.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(clip_ids, vec!["c-1", "c-2"], "both clips registered, in order");
        assert_eq!(rebuilds.load(std::sync::atomic::Ordering::Relaxed), 1, "exactly one rebuild");
        assert_eq!(session.lock().store.clips.len(), 2, "clips landed in the store");

        // Commit 2: the transport-state Set, its OWN transient commit,
        // submitted immediately after (mirroring `stop_recording`'s call
        // order).
        let rebuilds3 = rebuilds.clone();
        let state_committed = committer
            .commit_with_rebuild(
                TxMeta::engine("stop recording").transient(),
                |tx| {
                    tx.apply(Op::Set {
                        object: ObjectRef::Transport,
                        path: PropPath::TransportState,
                        from: serde_json::Value::Null,
                        to: serde_json::json!("stopped"),
                    })
                },
                false,
                move || {
                    rebuilds3.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                },
            )
            .unwrap();
        assert!(
            matches!(state_committed.meta.actor, crate::control::op::Actor::Engine),
            "state flip must also be attributed to Actor::Engine, got {:?}",
            state_committed.meta.actor
        );
        assert!(state_committed.meta.transient, "the state mirror is transient, like the rest of the transport family");
        assert!(
            state_committed.ops.iter().all(|op| matches!(op, Op::Set { path: PropPath::TransportState, .. })),
            "the state commit must carry ONLY the TransportState Set, got {:?}",
            state_committed.ops
        );
        assert_eq!(
            rebuilds.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the state commit adds no further rebuild (Transport Set never sets effect.rebuild)"
        );
        assert_eq!(session.lock().store.transport.state, "stopped");
    }

    /// Plan E Task 13's TDD step 1: auto-stop (`Control::commit_auto_stop`'s
    /// exact shape) produces a TRANSIENT `Actor::Engine` tx, and — because
    /// `Op::Set{Transport, ...}` never sets `effect.rebuild` (Task 12's
    /// transport family, session.rs) — `do_rebuild` is never invoked; the
    /// caller (the real `apply_end_policy`) relies on its own already-taken
    /// RT-atomic path, not a rebuild, for this state change.
    #[test]
    fn auto_stop_commits_a_transient_actor_engine_tx_with_no_rebuild() {
        let (committer, session) = test_committer();
        let rebuilds = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rebuilds2 = rebuilds.clone();
        let committed = committer
            .commit_with_rebuild(
                TxMeta::engine("auto-stop at end").transient(),
                |tx| {
                    tx.apply(Op::Set {
                        object: ObjectRef::Transport,
                        path: PropPath::TransportState,
                        from: serde_json::Value::Null,
                        to: serde_json::json!("stopped"),
                    })
                },
                false,
                move || {
                    rebuilds2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                },
            )
            .unwrap();
        assert!(
            matches!(committed.meta.actor, crate::control::op::Actor::Engine),
            "auto-stop must be attributed to Actor::Engine, got {:?}",
            committed.meta.actor
        );
        assert!(committed.meta.transient, "auto-stop is transient, like the rest of the transport family");
        assert_eq!(rebuilds.load(std::sync::atomic::Ordering::Relaxed), 0, "Transport Set never rebuilds");
        assert_eq!(session.lock().store.transport.state, "stopped");
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
            crate::control::testutil::test_committer(&session, &shared, &tables),
        );
        let cp = ControlPlane::new(
            session,
            shared.clone(),
            tables,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
            Box::new(|_, _| {}),
            std::sync::Arc::new(crate::control::HistoryLog::new()),
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
            crate::control::testutil::test_committer(&session, &shared, &tables),
        );
        let cp = ControlPlane::new(
            session.clone(),
            shared.clone(),
            tables,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
            Box::new(|_, _| {}),
            std::sync::Arc::new(crate::control::HistoryLog::new()),
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
        crate::midi::playback::append_from(&midi, &store, &crate::control::session::PluginDoc::default(), &slots, 48_000, None, &mut nodes, &mut out);
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
        // The seeder resolves the scan cache through the registered
        // app-global registry (register-once semantics shared across the
        // test process); the document rows land in a local session (Task
        // 9: the registry no longer holds instance rows).
        crate::plugins::register_registry(Arc::new(Mutex::new(
            crate::plugins::PluginRegistry::default(),
        )));
        let session = Arc::new(Mutex::new(Session::new(
            crate::audio::types::Store::default(),
            crate::midi::MidiStore::default(),
        )));
        let Some(ids) = try_seed_zyn_demo_instruments(&session) else {
            eprintln!("skipping: ZynAddSubFX or its banks not installed");
            return;
        };
        for id in &ids {
            let info = session
                .lock()
                .plugins
                .instances
                .iter()
                .find(|r| &r.id == id)
                .cloned()
                .expect("registered");
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
        let doc = session.lock().plugin_snapshot();
        crate::midi::playback::append_from(&midi, &store, &doc, &slots, 48_000, None, &mut nodes, &mut out);
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

        // Cleanup: drop the demo instances from the session/host.
        for id in &ids {
            session.lock().plugins.instances.retain(|r| &r.id != id);
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
        cp.committer().execute_persist(&PersistEffect { midi: true, ..PersistEffect::default() }, epoch);

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

        cp.committer().execute_persist(&PersistEffect { midi: true, ..PersistEffect::default() }, committed.epoch);

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
        cp.committer().execute_persist(&PersistEffect { automation: true, ..PersistEffect::default() }, epoch);

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
