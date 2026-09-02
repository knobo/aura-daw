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

pub mod clipboard;
/// The Composer (Plan H1): the ONE stateful seam over the pure `theory`
/// library — harmony document ops, the palette, suggestions, generation.
pub mod composer;
pub mod import;
pub mod hum;
pub mod export;
pub mod history;
pub mod op;
pub mod ops;
pub mod pitch_coach;
pub mod replay;
pub mod loopjam;
pub mod session;
pub mod snapshot;
pub mod vergraph;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::audio::engine::{ControlMsg, EngineHandle, MeterSink};
use crate::audio::rt::{SharedGraphTables, SharedRt, FLAG_MUTE, FLAG_SOLO};
use crate::ids::{PlayerId, TrackId};
use crate::audio::types::{Clip, MeterFrame, Project, TrackState, TransportState};
use crate::audio::project;
use crate::sidecars::jobs::{EventSink, JobManager};

pub use history::{EpochEvent, History, HistoryEntry, HistoryLog, HistoryMode, JournalWriter, UndoPath};
pub use ops::{LaneArrangement, TrackMixChange};
pub use session::{Committed, EngineEffect, PersistEffect, Session, Tx};
pub use snapshot::{ChangeSet, MidiSnapshot, SessionSnapshot};
pub use vergraph::{VersionGraph, VersionItem, VersionNode, VersionStats};

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
    /// The Composer's harmony document (Plan H1, additive). Ships from cold
    /// start for the same reason the section table does: the panel and the
    /// piano-roll tint need it before anyone calls a composer command.
    pub harmony: crate::theory::HarmonyDoc,
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

/// One clip's new placement in a `move_clips` batch. Externally tagged by
/// `kind` because audio clips are placed in SAMPLES and MIDI clips in TICKS
/// — two different units that must not be confusable on the wire, and two
/// different stores (`store.clips` / `midi.clips`) resolved by two different
/// lookups. A batch mixes both freely; the channel is cross-store atomic.
///
/// `length_ticks` / `content_length_ticks` are additive `#[serde(default)]`
/// fields carrying the group loop-length adjust. `None` means UNCHANGED, not
/// "clear" — clearing `content_length_ticks` back to "same as length" keeps
/// going through the existing `midi_set_clip_bounds(…, null)` command
/// (plan scope ruling H).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClipPlacement {
    Audio { clip_id: String, timeline_start_samples: u64 },
    Midi {
        clip_id: String,
        timeline_start_ticks: u64,
        #[serde(default)]
        length_ticks: Option<u64>,
        #[serde(default)]
        content_length_ticks: Option<u64>,
    },
}

impl ClipPlacement {
    fn clip_id(&self) -> &str {
        match self {
            Self::Audio { clip_id, .. } | Self::Midi { clip_id, .. } => clip_id,
        }
    }
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
/// V-19's voice cap: how many pads may be sounding, or waiting on a beat
/// to sound, at once. The owner's answer to the design doc's §8 question 1,
/// with stealing oldest-first.
///
/// A cap is what makes the deck honest: each voice is a clock and a mixer
/// slot, and "unbounded" would mean a press that allocates. Counted over
/// PLAYER clocks only — a scene is a region of the arrangement that a pad
/// borrowed, not a voice on the deck.
pub const VOICE_CAP: usize = 32;

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
    ///
    /// `Arc`-shared with the engine control thread's `Control` (automation
    /// Task 7), which needs the READ-ONLY
    /// [`GestureState::is_track_gain_touched`] to tell Touch/Latch whether
    /// the user's hand is on a fader right now. Same sharing shape as
    /// `session`/`shared`/`tables`/the history log: `AudioState` mints the
    /// one `Arc` and hands a clone to each side.
    gesture: Arc<GestureState>,
    /// The last gesture batch `close_gesture` synthesized, parked for
    /// `take_last_gesture_batch`. TEST-ONLY as of Task 17: history is now
    /// fed DIRECTLY by `close_gesture` (see its doc), so nothing in
    /// production reads this slot — it exists so a test can inspect the
    /// exact synthesized `ops`/`inverses`/`meta` without reaching into the
    /// history stacks.
    last_gesture_batch: Mutex<Option<session::Committed>>,
    /// The hardware MIDI-input manager (Task 3), attached once by lib.rs
    /// setup AFTER `.manage` — every unit test constructs a `ControlPlane`
    /// without calling `attach_midi_input`, so `select_midi_input_port`
    /// must error rather than panic while this is unset. Mirrors the
    /// `EngineHandle::for_tests()` "not really wired" shape, just for a
    /// seam this task adds rather than one already threaded through `new`.
    midi_input: std::sync::OnceLock<Arc<crate::midi_input::MidiInputManager>>,
    /// The `aura-midi-out` driver (Task 7), same carve-out shape as
    /// `midi_input` above: attached once by lib.rs setup AFTER `.manage`,
    /// so unit-test `ControlPlane`s (never routed through `lib.rs::run`)
    /// have this unset and the MIDI-out routing/port/clock methods error
    /// rather than panic.
    midi_out: std::sync::OnceLock<Arc<crate::midi_out::MidiOut>>,
    /// Serializes undo/redo pop→commit→push. `spawn_blocking` lets two
    /// Ctrl+Z keydowns run at once; without this gate they can pop two
    /// entries and apply inverses out of order (I-6).
    history_gate: Mutex<()>,
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
            time_signature: Some(
                session
                    .midi
                    .meter_events
                    .first()
                    .map(|e| (e.num, e.den))
                    .unwrap_or((4, 4)),
            ),
            tracks: s.tracks.clone(),
            clips: s.clips.clone(),
            players: s.players.clone(),
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
        self.commit_with_rebuild_full(meta, f, emit_project_changed, do_rebuild, history_mode, false)
    }

    /// [`Self::commit_with_rebuild_mode`] with an explicit `defer_persist`
    /// (I-8): when `true`, the persist described by this commit's
    /// `PersistEffect` is NOT executed here — the caller (an open gesture,
    /// via `ControlPlane::commit_transient_for_gesture`) accumulates it
    /// instead and executes the union once at `close_gesture`. Every other
    /// existing caller goes through `commit_with_rebuild_mode`'s delegate
    /// above, which passes `false` — byte-identical behaviour to before
    /// this fn existed.
    pub(crate) fn commit_with_rebuild_full<F, R>(
        &self,
        meta: op::TxMeta,
        f: F,
        emit_project_changed: bool,
        do_rebuild: R,
        history_mode: history::HistoryMode,
        defer_persist: bool,
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
                    op::PropPath::Gain => tables.params.set_gain_pair_linear(slot, *value),
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
                    // Plan V (ruling V-1): same reasoning for `Raw`,
                    // `TriggerMode` and `PlayerSource` — the first two are
                    // structural or document-only, the third is structural.
                    // A player's Gain/Pan/Muted do NOT land here: they reuse
                    // the four arms above, because a player's compiled
                    // `MixNode::id` is its `PlayerId` borrowed into `TrackId`
                    // and its slot lives in the same `slots` map a track's
                    // does (Task 9, `derive_slots_with_players`).
                    // Rename: document-only, no ParamTable counterpart and
                    // no rebuild — `apply_raw` never pushes it here either.
                    op::PropPath::Name
                    | op::PropPath::Armed
                    | op::PropPath::AutomationMode
                    | op::PropPath::InstrumentId
                    | op::PropPath::Group
                    | op::PropPath::TimelineStartSamples
                    | op::PropPath::LengthSamples
                    | op::PropPath::OffsetSamples
                    | op::PropPath::TimelineStartTicks
                    | op::PropPath::LengthTicks
                    | op::PropPath::ContentLengthTicks
                    | op::PropPath::TransposeSemitones
                    | op::PropPath::VelocityOffset
                    | op::PropPath::TransportState
                    | op::PropPath::LoopEnabled
                    | op::PropPath::LoopStartSamples
                    | op::PropPath::LoopEndSamples
                    | op::PropPath::StopAtEnd
                    | op::PropPath::SampleRate
                    | op::PropPath::Param { .. }
                    | op::PropPath::Raw
                    | op::PropPath::TriggerMode
                    | op::PropPath::PlayerSource
                    | op::PropPath::Quantize
                    | op::PropPath::ChokeGroup
                    | op::PropPath::VelocityToGain => {}
                }
            }
            // Plan G2: send amounts, resolved through the CURRENT
            // `send_slots` for the same reason the track writes resolve
            // through `slots` — an id with no lane yet (the send was added
            // by a commit whose rebuild has not run) is skipped, and that
            // rebuild will populate the lane from the document anyway.
            for (send_id, amount) in &committed.effect.send_writes {
                let Some(&idx) = tables.send_slots.get(send_id) else { continue };
                tables.params.set_send_amount_linear(idx, *amount);
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
        if !defer_persist && committed.effect.persist != session::PersistEffect::default() {
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
            self.log.record_commit(&committed, history_mode);
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
    /// `Session::epoch`'s doc). (Plan F, 2026-08-14, ruling F-6): this is
    /// a skip, not a flush of the outgoing project. Direct test callers of
    /// this fn (that don't go through `commit_with_rebuild`) should pass
    /// the session's current epoch.
    pub(crate) fn execute_persist(&self, p: &session::PersistEffect, committed_epoch: u64) {
        let persist_gate = self.session.lock().persist_gate.clone();
        let _persist = persist_gate.lock();
        let (
            dir,
            epoch_now,
            midi_snapshot,
            project_snapshot,
            automation_snapshot,
            modulation_snapshot,
            plugin_snapshot,
        ) = {
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
                p.modulation.then(|| s.modulation.clone()),
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
                self.clear_midi_dirty_if_unchanged(&m);
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
        // Persist policy (Task 7): `session.modulation` is the source of
        // truth. `modulation::persist::save_into_project` is one-way — it
        // writes `modulation{}` at schemaVersion 4 and DROPS `automation[]`.
        // Writing both paths in one persist would let the old lane save
        // undo the v4 upgrade (or the reverse). So: when a modulation
        // snapshot is present, skip the old `automation[]` write. A leftover
        // persist.automation-only effect (tests poking the old flag, any
        // arm not yet routed through the facade) still uses the lane path.
        if let Some(doc) = modulation_snapshot {
            if let Err(e) = crate::modulation::persist::save_into_project(&dir, &doc) {
                log::warn!("modulation persist failed: {e}");
            }
        } else if let Some(lanes) = automation_snapshot {
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
                    // under it. M-1 (Task 3, whole-branch review): guarded
                    // against a `PluginSetState` landing between the
                    // snapshot above and this re-lock — see
                    // `clear_dirty_state_matching`'s doc.
                    self.clear_dirty_state_matching(&cleared, &doc);
                }
                Ok(_) => {}
                Err(e) => log::warn!("plugins persist failed: {e}"),
            }
        }
    }

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
        // snapshot republish: R-4 — `status` and the param mirror are
        // document content, and this writeback is a carve-out that bypasses
        // `transact` (see this fn's doc), so nothing else would publish it.
        // Same lock as the writes, so the intermediate is never observable.
        // The structural twin of `plugins::state::reactivate_restored_with`'s
        // post-host writeback, which republishes for the same reason.
        s.republish_full();
    }

    /// Clear `dirty_state` ONLY for ids whose live pending bytes still equal
    /// the bytes this persist actually wrote (M-1, whole-branch review): a
    /// concurrent `PluginSetState` landing between the snapshot (taken under
    /// the FIRST session lock of the persist call this helper closes out)
    /// and this re-lock must keep its dirty flag, or its bytes would
    /// silently never persist — the same `PluginRemove`/`PluginSetState`
    /// hazard Task 9's Critical-2 fixed one level down (a fresher write
    /// beating a merely-existing file); this closes the analogous window
    /// one level UP, between "bytes chosen to write" and "flag cleared".
    /// Called from `execute_persist`, `save_project_mark`'s M-2 flush, and
    /// `save_project_as_epoch`'s Save-As write — every site that calls
    /// `plugins::state::save_snapshot_into_project` and then wants to clear
    /// the ids it returned.
    ///
    /// Returns `true` when every written id still matches. A mismatch
    /// re-inserts the id: a later persist may already have cleared dirty,
    /// and leaving it false after a stale write would never flush again.
    fn clear_dirty_state_matching(&self, written: &[String], snapshot: &session::PluginDoc) -> bool {
        let mut s = self.session.lock();
        let mut all_matched = true;
        for id in written {
            if s.plugins.pending_state.get(id) == snapshot.pending_state.get(id) {
                s.plugins.dirty_state.remove(id);
            } else {
                s.plugins.dirty_state.insert(id.clone());
                all_matched = false;
            }
        }
        all_matched
    }

    /// Clear `midi.dirty` only when the live store still matches the
    /// snapshot that just landed on disk. A `MidiSetNotes` between the
    /// write and this re-lock must keep the flag, or the newer notes
    /// never persist (same window `clear_dirty_state_matching` closes
    /// for plugin blobs).
    ///
    /// Returns `true` on match. Mismatch re-dirties: a stale writer can
    /// overwrite newer bytes after a later persist already cleared the flag.
    fn clear_midi_dirty_if_unchanged(&self, written: &crate::midi::persist::V3Data) -> bool {
        let mut s = self.session.lock();
        if &s.midi_snapshot() == written {
            s.midi.dirty = false;
            true
        } else {
            s.midi.dirty = true;
            false
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
                    if let Some(format) = format {
                        forward_param_to_host(instance, &format, *index, *value);
                    }
                }
                HostForward::Instantiate { instance } => {
                    let (row, as_effect) = {
                        let s = self.session.lock();
                        let row = s.plugins.instances.iter().find(|r| &r.id == instance).cloned();
                        let as_effect = row
                            .as_ref()
                            .is_some_and(|r| s.instance_is_insert(&r.id));
                        (row, as_effect)
                    };
                    let Some(row) = row else { continue }; // row vanished meanwhile
                    // Idempotent by construction (doc on `HostForward::
                    // Instantiate`): if the host already has this id live
                    // (the prepare-outside fresh-instantiate path), re-sync
                    // params via a plain read instead of re-instantiating —
                    // re-registering a live id would reset its voice state.
                    // Insert membership (already applied) picks Effect vs
                    // Instrument — undo/replay/open of insert FX must not
                    // re-negotiate as a note-port instrument.
                    let hosted = match row.format.as_str() {
                        "clap" => match clap_host::has_instance(instance) {
                            Ok(true) => clap_host::get_params(instance),
                            Ok(false) if as_effect => {
                                clap_host::instantiate_effect(instance, &row.uid)
                            }
                            Ok(false) => clap_host::instantiate(instance, &row.uid),
                            Err(e) => Err(e),
                        },
                        "lv2" => {
                            let host = lv2_host::global();
                            match host.has_instance(instance) {
                                Ok(true) => host.get_params(instance),
                                Ok(false) if as_effect => {
                                    host.register_instance_effect(instance, &row.uid)
                                }
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

    /// Put `gesture` into the state an open fader drag on `track_id` leaves
    /// behind — a gesture open with a gain `Set` already folded into it, so
    /// [`GestureState::is_track_gain_touched`] reads `true`. For fixtures
    /// that have no `ControlPlane` to run a real `gesture_begin` +
    /// `set_track_mix` through (the engine's `bare_control`, whose
    /// automation Touch/Latch tests need exactly this and nothing else).
    pub fn touch_track_gain(gesture: &GestureState, track_id: &str) {
        let op = op::Op::Set {
            object: op::ObjectRef::Track(track_id.into()),
            path: op::PropPath::Gain,
            from: serde_json::Value::Null,
            to: serde_json::json!(0.0),
        };
        let key = CoalesceKey::for_op(&op).expect("a gain Set folds by key");
        let _stale = gesture.begin("test fader drag".into(), op::Actor::User, 0);
        gesture
            .0
            .lock()
            .as_mut()
            .expect("just opened")
            .last
            .push((key, op));
    }

    /// Close whatever [`touch_track_gain`] opened — the pointerup half.
    pub fn release_gesture(gesture: &GestureState) {
        let _closed = gesture.end(None);
    }
}

// ---------------------------------------------------------------------------
// Gesture IPC (Plan E Task 14, round-2 inventory row 31, ADR 0003)
// ---------------------------------------------------------------------------

/// What a `CoalesceKey` addresses. Internal to this module (`CoalesceKey`
/// is `pub(crate)` and is never serialized), which is exactly why an
/// automation lane can get a coalesce target here without adding a
/// variant to the JOURNALED `op::ObjectRef` enum.
#[derive(Debug, Clone, PartialEq)]
enum CoalesceTarget {
    Object(op::ObjectRef),
    /// `AutomationLane::id` — lanes have a string id, not a struct key.
    AutomationLane(String),
    /// Modulation graph keys (`Curve::id` / `Binding::id` / `AutomationClip::id`).
    ModulationKey(String),
    /// `SendSlot::id` (Plan G2) — keyed by the SEND, not by its track: two
    /// sends on one track are two independent knobs and must not fold into
    /// each other.
    Send(String),
}

/// The coalescing key a gesture folds by: the op's discriminant + the
/// target it addresses + the `PropPath` it targets (round-2 §4.4:
/// "coalesced by (op_kind, target, actor)" — the (kind, target) half; the
/// actor half is `OpenGesture::actor`/`GestureState::matches_actor`, checked
/// separately since it gates whether a commit folds AT ALL, not which key it
/// folds under). `path` is `Option` because a non-`Set` op kind (e.g.
/// `Op::AutomationSetLane`) has no property path — it addresses a whole
/// lane, not a property of one. Exported (`pub(crate)`) — Task 17 imports
/// this exact type to key its own history-side merge; the name and the
/// (kind, target, path) shape are load-bearing, not cosmetic.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CoalesceKey {
    kind: &'static str,
    target: CoalesceTarget,
    path: Option<op::PropPath>,
}

impl CoalesceKey {
    /// Builds the key for `op`, or `None` for an op kind gesture folding
    /// doesn't (yet) handle. `Op::Set` and `Op::AutomationSetLane` today —
    /// the op kinds `ControlPlane::set_track_mix`, `set_plugin_params`, and
    /// `set_automation_lane` (gesture folding's wired callers) apply.
    fn for_op(op: &op::Op) -> Option<Self> {
        match op {
            op::Op::Set { object, path, .. } => Some(Self {
                kind: "set",
                target: CoalesceTarget::Object(object.clone()),
                path: Some(*path),
            }),
            // §4.4 value-replacement wrapper: a lane drag is a run of
            // whole-lane replaces of ONE lane; folding them by lane id is
            // what makes the drag one undo entry AND (with Task 2's
            // deferral) one automation persist.
            op::Op::AutomationSetLane { key, .. } => Some(Self {
                kind: "automationSetLane",
                target: CoalesceTarget::AutomationLane(key.clone()),
                path: None,
            }),
            op::Op::ModulationSetCurve { key, .. } => Some(Self {
                kind: "modulationSetCurve",
                target: CoalesceTarget::ModulationKey(key.clone()),
                path: None,
            }),
            op::Op::ModulationSetBinding { key, .. } => Some(Self {
                kind: "modulationSetBinding",
                target: CoalesceTarget::ModulationKey(key.clone()),
                path: None,
            }),
            op::Op::AutomationClipSet { key, .. } => Some(Self {
                kind: "automationClipSet",
                target: CoalesceTarget::ModulationKey(key.clone()),
                path: None,
            }),
            // Transport-bar tempo/meter slider: one TempoSet per project,
            // so a drag folds to one undo. Kind is distinct from a
            // transport `Set` against `ObjectRef::Transport`.
            op::Op::TempoSet { .. } => Some(Self {
                kind: "tempoSet",
                target: CoalesceTarget::Object(op::ObjectRef::Transport),
                path: None,
            }),
            // Plan G2: a send knob drags like a fader, so it folds like one
            // — a run of amount writes on ONE send is one undo step. The
            // op is absolute-valued (it carries `amount_db`), which is what
            // makes discarding the intermediates sound.
            op::Op::SendSetAmount { send_id, .. } => Some(Self {
                kind: "sendSetAmount",
                target: CoalesceTarget::Send(send_id.clone()),
                path: None,
            }),
            _ => None,
        }
    }

    /// The HISTORY-side key (Task 17), a deliberate SUPERSET of
    /// [`Self::for_op`]: everything a gesture folds, plus `Op::MidiSetNotes`
    /// keyed by its clip.
    ///
    /// Why the two differ, rather than one shared fn: `for_op` decides what
    /// a gesture folds INSIDE an open boundary, where folding also discards
    /// intermediate commits' effects — only the op kinds `set_track_mix`,
    /// `set_plugin_params`, and `set_automation_lane` (gesture folding's
    /// wired callers) ever apply are in scope there. The 350 ms fallback
    /// merges FINISHED, already committed batches, so it can safely cover
    /// the other §4.4 value-replacement wrapper too: `midi_set_notes_core`
    /// deliberately keeps the FIXED label `"set midi notes"` (its own doc
    /// says so) precisely so a run of note edits on ONE clip collapses to
    /// one undo step instead of one per keystroke. `path: None` — a
    /// `MidiSetNotes` addresses the whole clip, not a property of it.
    ///
    /// Every other op kind is structural and returns `None`, which is what
    /// makes "a structural op breaks the merge" true by construction.
    fn for_history_op(op: &op::Op) -> Option<Self> {
        match op {
            op::Op::MidiSetNotes { clip, .. } => Some(Self {
                kind: "midiSetNotes",
                target: CoalesceTarget::Object(op::ObjectRef::MidiClip(clip.clone())),
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
    /// Track faders this gesture controls as Write/Touch/Latch input. They
    /// are live side-channel writes, deliberately absent from `last`: the
    /// persisted base fader must not move while automation is recorded.
    live_gain_tracks: Vec<String>,
    /// Automation pass active when the gesture opened.
    automation_pass: u64,
    /// Union of the persist effects of every commit folded into this
    /// gesture, executed ONCE at `close_gesture` (I-8). Deferring is what
    /// turns "one project.json write per rAF batch" into "one per drag".
    persist: session::PersistEffect,
    /// `Committed.epoch` of the LAST folded commit — what `execute_persist`
    /// checks against the current session epoch at close. An epoch boundary
    /// mid-gesture swaps the document out from under the accumulated
    /// snapshot, and the epoch's own save owns durability from there.
    epoch: u64,
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

/// Mirrors `HistoryLog`'s pair — `new` is the name every call site uses; this
/// exists so a `pub fn new()` on a public type is not a clippy wart.
impl Default for GestureState {
    fn default() -> Self {
        Self::new()
    }
}

impl GestureState {
    /// `pub` rather than private because the ONE instance is minted by
    /// `AudioState::default` — `audio::init` builds the engine's `Control`
    /// before `ControlPlane` exists, so the `Arc` both sides share has to be
    /// born outside this module (exactly the carve-out
    /// `AudioState::log`/`HistoryLog` already documents) — and because
    /// `ControlPlane::new`/`engine::start` are themselves `pub` and now take
    /// one, so the crate's integration tests must be able to mint it too.
    /// Minting a SECOND `GestureState` in production would be the bug: the
    /// engine would then watch a gesture slot the UI never opens.
    pub fn new() -> Self {
        Self(Mutex::new(None))
    }

    /// True while there is an open gesture that has already folded a gain
    /// write for `track_id` — i.e. the user has an active fader drag on this
    /// track right now. Read-only: it peeks at the open gesture's `last`
    /// accumulator and neither mutates nor closes anything, and it takes no
    /// lock but this one, so it cannot participate in the gesture-before-
    /// session order this type's doc pins down.
    ///
    /// Note the "already folded" half: a gesture that has begun but not yet
    /// committed anything reads as untouched, which is the honest answer for
    /// Touch/Latch — the recorder arms on the first value the user actually
    /// moved, not on the pointerdown that preceded it.
    ///
    /// Consumed by the automation recorder (Touch/Latch) on the ENGINE
    /// CONTROL THREAD, which is why this exists at all: that thread holds a
    /// clone of the same `Arc<GestureState>` `ControlPlane` does.
    pub fn track_gain_touch_pass(&self, track_id: &str) -> Option<u64> {
        let guard = self.0.lock();
        let g = guard.as_ref()?;
        let touched = g.live_gain_tracks.iter().any(|id| id == track_id)
            || g.last.iter().any(|(key, _)| {
                key.kind == "set"
                    && key.path == Some(op::PropPath::Gain)
                    && matches!(&key.target, CoalesceTarget::Object(op::ObjectRef::Track(id)) if id.as_str() == track_id)
            });
        touched.then_some(g.automation_pass)
    }

    pub fn is_track_gain_touched(&self, track_id: &str) -> bool {
        self.track_gain_touch_pass(track_id).is_some()
    }

    /// Run live automation-control writes while the matching gesture mutex
    /// is held. This makes mark + RT-table write atomic against gesture_end:
    /// the engine can never receive a Touch-finish before the final fader
    /// value has landed. Returns false when there is no matching gesture.
    fn control_live_track_gains<F>(&self, actor: &op::Actor, track_ids: &[String], f: F) -> bool
    where
        F: FnOnce(u64),
    {
        let mut guard = self.0.lock();
        let Some(g) = guard.as_mut().filter(|g| &g.actor == actor) else {
            return false;
        };
        f(g.automation_pass);
        for id in track_ids {
            if !g.live_gain_tracks.contains(id) {
                g.live_gain_tracks.push(id.clone());
            }
        }
        true
    }

    /// Opens a new gesture. If one was already open, it's taken (closed)
    /// and handed back to the caller to finish committing — `GestureState`
    /// has no `ControlPlane` handle of its own to synthesize/emit the
    /// auto-closed batch.
    fn begin(&self, label: String, actor: op::Actor, automation_pass: u64) -> (String, Option<OpenGesture>) {
        let mut guard = self.0.lock();
        let stale = guard.take();
        let run = uuid::Uuid::new_v4().to_string();
        *guard = Some(OpenGesture {
            actor,
            run: run.clone(),
            baselines: Vec::new(),
            last: Vec::new(),
            label,
            live_gain_tracks: Vec::new(),
            automation_pass,
            persist: session::PersistEffect::default(),
            epoch: 0,
        });
        (run, stale)
    }

    /// Closes the open gesture (if any), handing it back to the caller to
    /// synthesize/commit. `None` if nothing was open. When `id` is `Some`
    /// and does not match the open gesture's run id, leaves it open —
    /// a late end from a different begin must not close this one.
    fn end(&self, id: Option<&str>) -> Option<OpenGesture> {
        let mut guard = self.0.lock();
        if let (Some(want), Some(g)) = (id, guard.as_ref()) {
            if g.run != want {
                return None;
            }
        }
        guard.take()
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
        g.persist.merge(&committed.effect.persist);
        g.epoch = committed.epoch;
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

/// Forward one already-clamped param value to whichever host owns
/// `instance`. Two callers: `Committer::execute_host_forward`'s `ParamWrite`
/// arm (a document edit's host effect) and the engine control thread's
/// automation driver (Track D — an RT-visible override that never touches
/// the document; see `plugins::automation::ParamAutomationDriver`'s doc and
/// `docs/SIDE-CHANNEL-INVENTORY.md`). Taking `format` as an argument, rather
/// than looking it up, is what lets the driver run with zero session locks
/// on its 2 ms tick.
pub(crate) fn forward_param_to_host(instance: &str, format: &str, index: u32, value: f32) {
    forward_params_to_host(instance, format, &[(index, value)]);
}

/// The same, for a whole BATCH of params on one instance — one host call
/// instead of one per param (Task 9 review, I-2), and posted rather than run
/// so neither caller ever waits on the plugin-main thread's queue. This
/// matters most for CLAP: `clap_host::set_params` is a blocking
/// `plugin_main().run(…)` round-trip on the thread that also serves the
/// param panel, instantiate and `save_state`, so a handful of automated
/// params on a 2 ms tick would otherwise be thousands of blocking hops a
/// second. `clap_host::post_params` is the fire-and-forget sibling that
/// closes that gap — both callers already discard the confirmed value
/// `set_params` returned (this function has always returned nothing), so
/// nothing downstream depended on the wait. LV2 was already posted this way
/// via `lv2_host::set_params`; the engine's automation driver hands its
/// writes out grouped by instance precisely so this is one call per plugin
/// per tick regardless of format.
pub(crate) fn forward_params_to_host(instance: &str, format: &str, changes: &[(u32, f32)]) {
    use crate::plugins::{clap_host, lv2_host};
    if changes.is_empty() {
        return;
    }
    match format {
        "lv2" => {
            if let Some(host) = lv2_host::try_global() {
                host.set_params(instance, changes.to_vec());
            }
        }
        "clap" => {
            let changes = changes
                .iter()
                .map(|(id, v)| crate::plugins::ParamChange { id: *id, value: *v as f64 })
                .collect();
            clap_host::post_params(instance, changes);
        }
        _ => {}
    }
}

fn records_automation(mode: crate::audio::types::AutomationMode) -> bool {
    matches!(
        mode,
        crate::audio::types::AutomationMode::Write
            | crate::audio::types::AutomationMode::Touch
            | crate::audio::types::AutomationMode::Latch
    )
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

/// Validate then emit one `TempoSet` (optional meter already resolved).
fn apply_tempo_map(
    tx: &mut session::Tx<'_>,
    ppq: Option<u32>,
    events: &[crate::midi::types::TempoEvent],
    meter: &Option<Vec<crate::midi::types::MeterEvent>>,
) -> Result<(), String> {
    let resolved_ppq = ppq.unwrap_or(tx.midi().ppq);
    let meter = match meter {
        Some(m) => {
            crate::midi::tempo::MeterMap::new(m.clone())?;
            m.clone()
        }
        None => tx.midi().meter_events.clone(),
    };
    // Validate the tempo map before apply so a bad bpm cannot land.
    crate::midi::build_tempo_map_state(resolved_ppq, events, &meter)?;
    tx.apply(op::Op::TempoSet {
        ppq: resolved_ppq,
        events: events.to_vec(),
        meter,
    })
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
        if let Some(m) = c.automation_mode {
            tx.apply(set_prop(&c.track_id, op::PropPath::AutomationMode, serde_json::json!(m)))?;
        }
    }
    Ok(())
}

/// Apply one `move_clips` batch inside an open transaction. Pure op
/// emission — every id was validated by the caller before the lock, so this
/// never needs to reject anything and never panics.
fn apply_clip_placements(
    placements: &[ClipPlacement],
    tx: &mut session::Tx<'_>,
) -> Result<(), String> {
    for p in placements {
        match p {
            ClipPlacement::Audio { clip_id, timeline_start_samples } => {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Clip(clip_id.as_str().into()),
                    path: op::PropPath::TimelineStartSamples,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(timeline_start_samples),
                })?;
            }
            ClipPlacement::Midi {
                clip_id,
                timeline_start_ticks,
                length_ticks,
                content_length_ticks,
            } => {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::MidiClip(clip_id.as_str().into()),
                    path: op::PropPath::TimelineStartTicks,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(timeline_start_ticks),
                })?;
                if let Some(len) = length_ticks {
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::MidiClip(clip_id.as_str().into()),
                        path: op::PropPath::LengthTicks,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(len),
                    })?;
                }
                if let Some(cl) = content_length_ticks {
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::MidiClip(clip_id.as_str().into()),
                        path: op::PropPath::ContentLengthTicks,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(cl),
                    })?;
                }
            }
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

/// Open-path load of the modulation document.
///
/// A v4 `modulation{}` file is decoded as-is. A still-v3 `automation[]`
/// file is remigrated with the live plugin param table when that table
/// has rows (so plugin points normalize and `rangeSnapshot` is recorded);
/// otherwise `load_from_project`'s in-memory migrate (`|_, _| None`) is
/// acceptable — plugin points stay `domain: native`.
fn load_modulation_for_open(
    dir: &Path,
    params: &std::collections::HashMap<String, Vec<crate::plugins::ParamInfo>>,
) -> crate::modulation::ModulationDoc {
    let still_v3 = std::fs::read(dir.join("project.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .is_some_and(|v| v.get("modulation").is_none() && v.get("automation").is_some());
    if still_v3 && !params.is_empty() {
        if let Ok(Some(lanes)) = crate::plugins::automation::load_lanes(dir) {
            let lookup = |inst: &str, id: u32| {
                params.get(inst).and_then(|ps| {
                    ps.iter().find(|p| p.id == id).and_then(|p| {
                        let min = p.min as f32;
                        let max = p.max as f32;
                        (max > min && min.is_finite() && max.is_finite()).then_some((min, max))
                    })
                })
            };
            return crate::modulation::persist::migrate_v3_lanes(&lanes, &lookup);
        }
    }
    crate::modulation::persist::load_from_project(dir).unwrap_or_else(|e| {
        log::warn!("modulation: cannot load from {}: {e}", dir.display());
        crate::modulation::ModulationDoc::default()
    })
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
        gesture: Arc<GestureState>,
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
            // The SAME `Arc` the engine's `Control` holds (automation Task 7):
            // `AudioState` minted it before `audio::init` started the control
            // thread, for the same reason `log` is minted there.
            gesture,
            last_gesture_batch: Mutex::new(None),
            midi_input: std::sync::OnceLock::new(),
            midi_out: std::sync::OnceLock::new(),
            history_gate: Mutex::new(()),
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
            harmony: midi.harmony.clone(),
        }
    }

    pub fn transport_state(&self) -> TransportState {
        ops::transport_snapshot(&self.session.lock().store, &self.shared)
    }

    pub fn emit_launch_changed(&self) {
        let snap = {
            let s = self.session.lock();
            crate::midi::launch::LaunchSnapshot {
                maps: {
                    let mut maps = s.midi.launch_maps.clone();
                    crate::midi::launch::ensure_maps(&mut maps);
                    maps
                },
            }
        };
        (self.emit)(
            "launch://changed",
            serde_json::to_value(&snap).unwrap_or_default(),
        );
    }

    pub fn emit_launch_fired(&self, fired: crate::midi::launch::LaunchFired) {
        (self.emit)(
            "launch://fired",
            serde_json::to_value(&fired).unwrap_or_default(),
        );
    }

    /// The clock a scene binding fires, or `None` when the graph has not been
    /// rebuilt since the binding was added. A missing clock drops the fire
    /// with a warn rather than firing the wrong one — the same "unknown index
    /// means drop the write" rule `ParamTable`'s setters use.
    pub fn scene_clock_for(&self, binding_id: &str) -> Option<u32> {
        self.tables.lock().scene_clocks.get(binding_id).copied()
    }

    /// Start one scene: point the tracks its region names at ITS clock, then
    /// fire that clock. Returns whether the scene actually has one.
    ///
    /// This is the whole fire path now, for every `FireOrigin`. It never
    /// touches the transport (design §2.2's defect) and never touches another
    /// scene's clock, which is what lets two scenes sound at once — the
    /// single overlay this replaces could express neither.
    ///
    /// The release-then-bind pass is per-clock and deliberate: re-firing a
    /// binding whose region has since lost a track must not leave that track
    /// stranded on this scene's playhead. It releases only what THIS clock
    /// still owns (V-14), so a track another scene has claimed in the
    /// meantime stays with that scene.
    /// `at` is the transport position a QUANTIZED press waits for, or `None`
    /// for "now" — the same `Option` a player's press carries, resolved by
    /// the same [`ControlPlane::quantize_target`], so "Q 1/4" means one
    /// thing across both kinds of pad.
    ///
    /// The tracks are bound NOW even when the fire waits, and that is not an
    /// oversight: a slot on a scene clock that is off falls back to the
    /// arrangement (`mixer::node_playhead`'s fourth case), so the borrowed
    /// tracks keep playing the song until the beat arrives, and the binding
    /// is already in place when it does. Binding from `arm_pending` instead
    /// is not available: that runs on the audio thread, and which slots a
    /// scene names is a control-side map.
    pub fn fire_scene(
        &self,
        binding_id: &str,
        track_ids: &[String],
        start: u64,
        end: u64,
        at: Option<u64>,
    ) -> bool {
        let tables = self.tables.lock();
        let Some(&clock) = tables.scene_clocks.get(binding_id) else {
            log::warn!("launch: no clock for binding {binding_id} — dropping the fire");
            return false;
        };
        for slot in 0..tables.params.len() {
            tables.clocks.release_slot_if(slot, clock);
        }
        for id in track_ids {
            if let Some(&slot) = tables.slots.get(&TrackId::from(id.as_str())) {
                tables.clocks.bind_slot(slot, clock);
            }
        }
        // A scene carries no velocity: unity, as V2 fired it. V-18's gain is
        // a PAD's, and a scene borrows real arrangement tracks — turning
        // those down because someone tapped softly is a different feature,
        // and not one anybody has asked for.
        tables.clocks.fire_maybe_at(clock, at, start, end, false, 1.0);
        true
    }

    /// Cut one scene: stop its clock, and leave every slot it owns bound.
    ///
    /// This is the brief's `stop_scene`, RENAMED when the release moved out
    /// of it, because a method called "stop" invites putting the release
    /// back — and the release is exactly what must not happen here.
    /// `ClockTable::stop` latches one discontinuity for the live nodes bound
    /// to this clock (the `all_notes_off` a cut note needs, or the voice
    /// hangs with nothing left to release it), and a slot released in the
    /// same breath never reads it. `release_finished_scenes` does the
    /// release, once a rendered block has actually delivered the flush.
    ///
    /// Every ending goes through here — a clip running out, a Gate note-off
    /// lifting mid-clip, stop-all, and the transport stopping. The Gate path
    /// is why the old "the drive thread only releases a poll after the clock
    /// already stopped" reasoning was not enough: it cuts a RUNNING clock.
    ///
    /// Returns whether this call is the one that stopped a running clock.
    /// `ClockTable::stop`'s own return value is the answer, not an `is_on`
    /// read beside it: `advance` can turn the clock off between the two, and
    /// the pair would then report "something was sounding" for a scene that
    /// had already ended.
    pub fn cut_scene(&self, binding_id: &str) -> bool {
        let tables = self.tables.lock();
        let Some(&clock) = tables.scene_clocks.get(binding_id) else { return false };
        tables.clocks.stop(clock)
    }

    /// Cut every scene, and forget the endings we owed the frontend — the
    /// transport-stop path (`TransportAction::Stop`) and nothing else:
    /// stopping the song ends the scenes with it, and the UI learns that from
    /// `transport://state`, not from a `LaunchFired` per scene.
    ///
    /// Cut, NOT released: same reason as `cut_scene`. Before Task 8's fix
    /// round this stopped and released in one breath, so a note sounding when
    /// the user pressed Stop was left hanging in the live node.
    pub fn clear_launch_audible(&self) {
        crate::midi::launch::runtime().clear_sounding();
        let tables = self.tables.lock();
        // SCENES ONLY, and that is a ruling, not an oversight. A scene is a
        // region of the arrangement that a pad borrowed, so stopping the song
        // ends it. A PLAYER is not in the song at all (V-2), and cutting a
        // performance because someone stopped the transport is the deck
        // going quiet mid-set. `stop_launch_overlay` (Escape / stop-all) is
        // the call that cuts pads; see `docs/backlog/plan-v-players.md`.
        for &clock in tables.scene_clocks.values() {
            tables.clocks.stop(clock);
        }
        // A pad quantized to a beat is waiting for a transport position that
        // is not coming any more (V-21). Cancelling is the only alternative
        // to a press that sounds whenever the song is next played past that
        // point — which is not "the deck kept playing", it is a pad that
        // fires on its own, minutes later. A pad already SOUNDING is
        // untouched: that is the ruling this method exists for.
        tables.clocks.cancel_pending();
    }

    /// Cut everything sounding — every scene AND every player (Escape /
    /// stop-all). Ends them exactly the way reaching a clip's end does, so
    /// the drive thread's own release edge announces each ending and
    /// `release_finished_scenes` hands the scenes' tracks back — one code
    /// path, one behaviour. Returns true when something was actually
    /// sounding.
    ///
    /// PLAYERS were missing here until fix round 1, and their absence made
    /// this the only escape from a hung deck. `TriggerMode::Loop` fires a
    /// looping clock; a looping clock never ends itself
    /// (`ClockTable::advance` wraps it instead), and `any_running()` keeps
    /// the output callback rendering with the transport stopped — so a
    /// looping pad sounded indefinitely with nothing in reach able to cut
    /// it. `player_stop(id)` could, but no caller exists yet, and this
    /// method's own doc already claimed to stop everything.
    ///
    /// This is NOT the transport-stop path. Stopping the song deliberately
    /// leaves pads sounding (V-2: a pad is not arrangement material, and
    /// stopping the arrangement should not cut a performance) — see
    /// `clear_launch_audible`, and `docs/backlog/plan-v-players.md` for the
    /// ruling.
    ///
    /// It deliberately does NOT release the slots itself, for the reason
    /// spelled out on `cut_scene`: the discontinuity `ClockTable::stop`
    /// latches has to reach the live nodes still bound here first. A
    /// player's slot is never released at all — it owns it for the life of
    /// the graph.
    ///
    /// `ClockTable::stop`'s own return value is the answer, not an `is_on`
    /// read beside it: `advance` can turn a clock off between the two, and
    /// the pair would then report "something was sounding" for a scene that
    /// had already ended.
    pub fn stop_launch_overlay(&self) -> bool {
        let tables = self.tables.lock();
        // Fold with `|`, not `any`: every scene and every pad must be cut,
        // and `any` short-circuits on the first one that was running.
        tables
            .scene_clocks
            .values()
            .chain(tables.player_clocks.values())
            .fold(false, |acc, &clock| tables.clocks.stop(clock) | acc)
    }

    // ---- Plan V — V2: players (ruling V-1, Task 9) --------------------

    /// Every player in the document. PURE session-lock read.
    pub fn players(&self) -> Vec<crate::audio::player::Player> {
        self.session.lock().store.players.clone()
    }

    /// The clock this player fires, or `None` when the graph has not been
    /// rebuilt since the player was added. Same contract — and the same
    /// reasoning — as [`ControlPlane::scene_clock_for`]: a missing clock
    /// drops the fire rather than firing whichever clock sits at that index.
    pub fn player_clock_for(&self, id: &str) -> Option<u32> {
        self.tables.lock().player_clocks.get(&PlayerId::from(id)).copied()
    }

    /// How many samples a fired player sounds for.
    ///
    /// The PLACEMENT's length, not the source file's: `PlayerSource` names a
    /// `ClipId` precisely because the placement is what carries the trim
    /// (`offset_samples`/`length_samples`), and V-16 defines raw playback in
    /// exactly those terms. `PlayerSource::None` is a knobs-only pad (R5) and
    /// has nothing to sound, so it is 0 rather than an error — a control pad
    /// that reports a failure every time it is pressed is worse than one that
    /// silently does what it is.
    /// The MIDI arm reads `session.midi`, which is why this takes the whole
    /// session rather than the store: a MIDI player's placement is a
    /// `MidiClip`, and its length is TICKS. `rate` is the engine's, and a
    /// rate of 0 (no device yet) yields 0 — the same "nothing to sound"
    /// answer a knobs-only pad gives, rather than a length computed at a
    /// fabricated rate.
    fn player_source_length(
        session: &Session,
        p: &crate::audio::player::Player,
        rate: u32,
    ) -> Result<u64, String> {
        match &p.source {
            crate::audio::player::PlayerSource::AudioClip { clip_id } => session
                .store
                .clips
                .iter()
                .find(|c| &c.id == clip_id)
                .map(|c| c.length_samples)
                .ok_or_else(|| format!("player {}: unknown clip {clip_id}", p.id)),
            // The PLACEMENT's span, tick-converted through the tempo map the
            // arrangement uses — the difference of the two ends rather than
            // `length_ticks` converted from zero, so a placement under a
            // tempo change is as long as it actually sounds there.
            crate::audio::player::PlayerSource::MidiClip { clip_id, .. } => {
                let clip = session
                    .midi
                    .clips
                    .iter()
                    .find(|c| &c.id == clip_id)
                    .ok_or_else(|| format!("player {}: unknown midi clip {clip_id}", p.id))?;
                if rate == 0 {
                    return Ok(0);
                }
                let map = crate::midi::TempoMap::new(
                    session.midi.ppq,
                    session.midi.tempo_events.clone(),
                    rate,
                )
                .map_err(|e| format!("player {}: {e}", p.id))?;
                let start = map.tick_to_samples(clip.timeline_start_ticks);
                let end =
                    map.tick_to_samples(clip.timeline_start_ticks + clip.length_ticks);
                Ok(end.saturating_sub(start))
            }
            crate::audio::player::PlayerSource::None => Ok(0),
        }
    }

    /// Fire a player: start ITS clock, at 0, for its source's length.
    ///
    /// TRANSIENT by construction — a press is not a document change, so it
    /// commits no op and takes no undo entry, the same reasoning that keeps
    /// transport actions out of the history. It is also two atomic stores and
    /// nothing else: the slot binding was made at graph build, so a pad press
    /// never rebuilds the graph (the RT contract).
    ///
    /// It never touches the transport, and never touches another player's or
    /// scene's clock — which is what lets a pad fire under a rolling
    /// arrangement, and two pads sound at once (V-4).
    pub fn player_fire(&self, id: &str) -> Result<(), String> {
        self.player_fire_with_velocity(id, crate::audio::player::FULL_VELOCITY)
    }

    /// [`ControlPlane::player_fire`] with the press's velocity (V-18). The
    /// no-velocity entry point above is this one at
    /// [`crate::audio::player::FULL_VELOCITY`], which is unity at every
    /// depth — so a mouse press, and every V2-era caller, sounds exactly as
    /// it did.
    ///
    /// V3 adds three things to V2's two atomic stores, and all three are
    /// resolved HERE, off the live document, not off the compiled graph
    /// (which is why none of them rebuilds):
    ///
    /// * the **choke group** — but the cut itself is `ClockTable`'s, because
    ///   a quantized press has to choke when it STARTS, not when it is
    ///   pressed (V-20);
    /// * the **voice cap** (V-19) — 32 sounding-or-pending pads, and the
    ///   33rd press steals the oldest;
    /// * the **quantize division** (V-21) — with a grid to land on, the fire
    ///   is armed for a transport position instead of taken now.
    pub fn player_fire_with_velocity(&self, id: &str, velocity: u8) -> Result<(), String> {
        // Read before the lock: an atomic load has no business under it.
        let rate = self.shared.sample_rate.load(Relaxed);
        let (len, looping, gain, quantize) = {
            let session = self.session.lock();
            let p = session
                .store
                .players
                .iter()
                .find(|p| p.id.as_str() == id)
                .ok_or_else(|| format!("unknown player: {id}"))?;
            (
                Self::player_source_length(&session, p, rate)?,
                // `Gate` and `OneShot` are byte-identical here, by design:
                // the design's `gate` ("sounds while held; release cuts it")
                // is entirely a matter of WHO calls `player_stop` and WHEN —
                // a pointerup, not anything the engine can see — so this is
                // the one and only place trigger mode does anything at all.
                p.trigger.mode == crate::audio::player::TriggerMode::Loop,
                p.gain_for_velocity(velocity) as f32,
                p.trigger.quantize,
            )
        };
        // BEFORE the clock lookup, and that order is the whole point (fix
        // round 1). A zero-length source (a knobs-only pad, or a MIDI one
        // before Task 10) would fire a clock that ends on the block it
        // started — `ClockTable::fire` widens `end` to `start + 1` rather
        // than divide by zero — so there is nothing to fire. Below the
        // lookup, a knobs-only pad the graph had not been rebuilt for
        // returned `Err("player has no clock yet")` instead of the
        // documented no-op: an R5 control pad reporting a failure for
        // being exactly what it is.
        if len == 0 {
            return Ok(());
        }
        // V-21. Computed BEFORE the tables lock — it reads the session and
        // the tempo map, and session-under-tables is the wrong lock order
        // [C1]. `None` is "there is no grid to wait for": quantize off, or
        // the transport stopped.
        let at = self.quantize_target(quantize);
        let stolen = {
            let tables = self.tables.lock();
            let clock = tables
                .player_clocks
                .get(&PlayerId::from(id))
                .copied()
                .ok_or_else(|| format!("player has no clock yet: {id}"))?;
            // V-19. A pad that is already sounding is RETRIGGERED, not a
            // second voice — the clock it would take is the one it already
            // holds — so the cap only bites when the press needs a voice
            // that is not already spent. Without this a 32-pad deck could
            // steal a pad to make room for itself.
            let stolen = if tables.clocks.is_live(clock)
                || tables.clocks.voices_in_use() < VOICE_CAP
            {
                None
            } else {
                // `oldest_voice` cannot answer `clock` here: that branch is
                // the one above.
                tables.clocks.oldest_voice().inspect(|&victim| {
                    tables.clocks.stop(victim);
                })
            };
            tables.clocks.fire_maybe_at(clock, at, 0, len, looping, gain);
            stolen.and_then(|victim| {
                tables
                    .player_clocks
                    .iter()
                    .find(|(_, &c)| c == victim)
                    .map(|(id, _)| id.to_string())
            })
        };
        // Outside the lock: an emit is a frontend round trip, and V-19 asks
        // for stealing to be VISIBLE, not for it to be visible from under
        // the tables lock.
        if let Some(stolen) = stolen {
            (self.emit)("player://stolen", serde_json::json!({ "playerId": stolen }));
        }
        Ok(())
    }

    /// The transport position a press quantized to `quantize` should start
    /// at, or `None` for "start now" (V-21).
    ///
    /// `None` covers three cases that are one case musically — there is no
    /// grid to land on:
    ///
    /// * `Quantize::Off`, the default and V2's only behaviour;
    /// * the transport is STOPPED, so `base_pos` is never going to reach any
    ///   target and the alternative to firing now is a pad that never sounds;
    /// * the tempo map cannot be built (no device, so no rate), or the
    ///   boundary converts back to a position that is not actually ahead of
    ///   the playhead.
    pub(crate) fn quantize_target(&self, quantize: crate::audio::player::Quantize) -> Option<u64> {
        if quantize == crate::audio::player::Quantize::Off {
            return None;
        }
        if !matches!(self.transport_state().state.as_str(), "playing" | "recording") {
            return None;
        }
        let rate = self.shared.sample_rate.load(Relaxed);
        if rate == 0 {
            return None;
        }
        let pos = self.shared.position.load(Relaxed);
        let session = self.session.lock();
        let ppq = session.midi.ppq;
        let map = crate::midi::TempoMap::new(ppq, session.midi.tempo_events.clone(), rate).ok()?;
        let tick = map.samples_to_tick(pos);
        // A bar's length is the METER's, and it is measured from the
        // signature change that governs it — the one thing here that cannot
        // be anchored at tick 0. Every other division is a fixed number of
        // quarters, and quarters line up with tick 0 wherever the meter
        // sits.
        let (origin, grid) = match quantize.quarters() {
            Some(q) => (0u64, ((q * f64::from(ppq)).round() as u64).max(1)),
            None => {
                let e = session
                    .midi
                    .meter_events
                    .iter()
                    .filter(|e| e.tick <= tick)
                    .max_by_key(|e| e.tick)
                    .copied()
                    .unwrap_or(crate::midi::MeterEvent { tick: 0, num: 4, den: 4 });
                let bar = (u64::from(e.num) * u64::from(ppq) * 4 / u64::from(e.den).max(1)).max(1);
                (e.tick, bar)
            }
        };
        let next = origin + ((tick.saturating_sub(origin)) / grid + 1) * grid;
        let target = map.tick_to_samples(next);
        (target > pos).then_some(target)
    }

    /// Cut a player: stop its clock, leaving one discontinuity behind for the
    /// nodes bound to it. The slot stays bound — a player OWNS its slot for
    /// the life of the graph, so there is nothing to release (unlike
    /// `cut_scene`, which borrows a timeline track's).
    ///
    /// Returns `Ok(())` for a pad that was not sounding: stop-all presses
    /// every pad unconditionally, and `ClockTable::stop`'s guard already
    /// makes an idle stop fabricate no flush.
    ///
    /// The session is consulted only on the FAILURE path, and deliberately so.
    /// The clock map alone cannot tell "no such pad" from "a pad the graph has
    /// not been rebuilt for yet", and reporting the first for the second sends
    /// the reader hunting a document bug that is not there — the press side
    /// already distinguishes them (`player_fire`). Doing it here costs the
    /// happy path nothing: a release takes one uncontended table lock and no
    /// session lock at all, and the two locks are never held together.
    pub fn player_stop(&self, id: &str) -> Result<(), String> {
        {
            let tables = self.tables.lock();
            if let Some(&clock) = tables.player_clocks.get(&PlayerId::from(id)) {
                tables.clocks.stop(clock);
                return Ok(());
            }
        }
        Err(
            if self.session.lock().store.players.iter().any(|p| p.id.as_str() == id) {
                format!("player has no clock yet: {id}")
            } else {
                format!("unknown player: {id}")
            },
        )
    }

    /// Set a player's trigger mode through the transaction channel
    /// (`Op::Set { path: PropPath::TriggerMode, .. }`) — the seam a user
    /// needs to reach anything but the `OneShot` default (fix round 1: the
    /// plan otherwise shipped Loop and Gate unreachable by any caller).
    /// Document-only (Task 3's ruling: `player_fire` reads `p.trigger.mode`
    /// off the live session on every press, not off the compiled graph), so
    /// this never rebuilds and never touches `GraphTables`.
    pub fn set_trigger_mode(
        &self,
        id: &str,
        mode: crate::audio::player::TriggerMode,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        self.commit(meta, |tx| {
            tx.apply(op::Op::Set {
                object: op::ObjectRef::Player(PlayerId::from(id)),
                path: op::PropPath::TriggerMode,
                from: serde_json::Value::Null,
                to: serde_json::to_value(mode).unwrap(),
            })
        })?;
        Ok(())
    }

    /// Set one V3 player property through the transaction channel. Same
    /// shape and same reasoning as [`ControlPlane::set_trigger_mode`]: all
    /// three are read off the live document on every press
    /// ([`ControlPlane::player_fire_with_velocity`]), never off the
    /// compiled graph, so none of them rebuilds and none of them touches
    /// `GraphTables`.
    fn set_player_prop(
        &self,
        id: &str,
        path: op::PropPath,
        to: serde_json::Value,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        self.commit(meta, |tx| {
            tx.apply(op::Op::Set {
                object: op::ObjectRef::Player(PlayerId::from(id)),
                path,
                from: serde_json::Value::Null,
                to,
            })
        })?;
        Ok(())
    }

    pub fn set_quantize(
        &self,
        id: &str,
        quantize: crate::audio::player::Quantize,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        self.set_player_prop(id, op::PropPath::Quantize, serde_json::to_value(quantize).unwrap(), meta)
    }

    /// `None` takes the pad out of every group — the state a migrated V2
    /// player has, and the only way back to it.
    pub fn set_choke_group(
        &self,
        id: &str,
        group: Option<u8>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        let to = match group {
            Some(g) => serde_json::json!(g),
            None => serde_json::Value::Null,
        };
        self.set_player_prop(id, op::PropPath::ChokeGroup, to, meta)
    }

    pub fn set_velocity_to_gain(
        &self,
        id: &str,
        depth: f64,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        self.set_player_prop(id, op::PropPath::VelocityToGain, serde_json::json!(depth), meta)
    }

    /// Add a player through the transaction channel (`Op::PlayerAdd`), so it
    /// is undoable, journaled and persisted exactly as `add_track` is. A
    /// player is a document object (V-1); it is NOT a track (V-2), which is
    /// why this is its own op rather than a `TrackAdd` with a flag.
    pub fn add_player(
        &self,
        name: Option<String>,
        source: crate::audio::player::PlayerSource,
        raw: bool,
        meta: op::TxMeta,
    ) -> Result<crate::audio::player::Player, String> {
        let index = self.session.lock().store.players.len();
        let name = name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("PAD {}", index + 1));
        let mut player = crate::audio::player::Player::new(PlayerId::mint(), name);
        player.source = source;
        player.raw = raw;
        self.commit(meta, |tx| {
            tx.apply(op::Op::PlayerAdd { player: player.clone(), index })
        })?;
        Ok(player)
    }

    /// Remove a player through the transaction channel (`Op::PlayerRemove`).
    /// The op takes its payload from store truth, so undo restores the pad
    /// byte-identically.
    ///
    /// `index` is passed as `usize::MAX` rather than `0` on purpose. The
    /// `apply_raw` arm ignores the caller's value entirely and finds the real
    /// position by id — a literal `0` in a declared field reads like an
    /// assertion that the pad is first, and would be believed by the next
    /// person to touch this. `usize::MAX` cannot be mistaken for a claim.
    /// The inverse op the arm returns carries the TRUE index, which is what
    /// undo re-inserts at.
    pub fn remove_player(&self, id: &str, meta: op::TxMeta) -> Result<(), String> {
        let player = {
            let session = self.session.lock();
            session
                .store
                .players
                .iter()
                .find(|p| p.id.as_str() == id)
                .cloned()
                .ok_or_else(|| format!("unknown player: {id}"))?
        };
        self.commit(meta, |tx| {
            tx.apply(op::Op::PlayerRemove { player: player.clone(), index: usize::MAX })
        })?;
        Ok(())
    }

    /// All automation lanes (Plan E Task 10). PURE session-lock read — no
    /// sync, no `loaded_dir`, no disk — `automation_get`'s entire body.
    pub fn automation_lanes(&self) -> Vec<crate::plugins::automation::AutomationLane> {
        self.session.lock().automation.lanes.clone()
    }

    /// Track F: the modulation document. PURE session-lock read — no disk.
    pub fn modulation_doc(&self) -> crate::modulation::ModulationDoc {
        self.session.lock().modulation.clone()
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
    ///   The same decision also skips the post-commit `playing` store:
    ///   `transport_snapshot` prefers that RT atomic over the store label,
    ///   so arming it on the guard path would report `"playing"` even
    ///   though the document (and a running take) still own `"recording"`.
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
                // Captured inside the commit so the RT write below follows
                // the same in-transaction decision as the document guard —
                // a post-commit re-read of the store would reopen a window
                // where Stop could land and this Play would then arm
                // playback after the take had already finished.
                let mut arm_playing = true;
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
                            arm_playing = false;
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
                if arm_playing {
                    self.shared.playing.store(true, Relaxed);
                }
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
                if self.shared.recording.load(Relaxed) || self.shared.countin_left.load(Relaxed) > 0
                {
                    self.engine
                        .request::<Vec<Clip>>(|reply| ControlMsg::StopRecording { reply })?;
                }
                // Freeze transport first, then capture the automation boundary.
                // A repeated Stop therefore carries `active_pass = false` and
                // cannot mint a Write point or undo entry.
                let automation_active = self.shared.playing.swap(false, Relaxed);
                let automation_stop_at = self.shared.position.load(Relaxed);
                let stopped_pass = automation_active
                    .then(|| crate::audio::rt::advance_automation_pass(&self.shared.automation_pass));
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
                self.engine.send(ControlMsg::FinishAutomationStop {
                    at: automation_stop_at,
                    active_pass: automation_active,
                    stopped_pass,
                });
                self.clear_launch_audible();
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
        // X1: MIDI tracks with a return source become audio takes on that
        // device. The engine does not talk to MidiOut (ruling: midi_out
        // never touches engine.rs); we pass the map in on the message.
        let return_sources = self
            .midi_out
            .get()
            .map(|out| out.return_sources())
            .unwrap_or_default();
        self.engine.request::<Vec<String>>(|reply| ControlMsg::StartRecording {
            track_ids,
            return_sources,
            reply,
        })?;
        let snap = self.transport_state();
        self.emit_transport(&snap);
        Ok(snap)
    }

    /// See `Control::stop_recording`: on `Err` the take HAS been committed
    /// (one undo entry) and the transport has stopped — the error reports a
    /// failed WAV write, not a failed registration. Never a retry signal.
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

    // ---- MIDI-in selection -------------------------------------------------
    // MIDI slice 2, Task 5 (§4.5 config carve-out, same shape as the device
    // selection pair above): hardware MIDI-in port selection and the
    // target-track routing choice are both app config, not document state —
    // no `Op`, no `commit`. Moving them behind `ControlPlane` methods is
    // purely so the ACTOR/LABEL attribution is captured at the one front
    // door both Tauri and MCP call through.

    /// lib.rs setup calls this once, after `.manage` — before that, every
    /// unit test's `ControlPlane` (built via `ControlPlane::new` directly,
    /// never through `lib.rs::run`) has an unattached `midi_input`, and
    /// `select_midi_input_port` errors instead of panicking.
    pub fn attach_midi_input(&self, mgr: Arc<crate::midi_input::MidiInputManager>) {
        // `OnceLock::set` returning `Err` (already attached) is silently
        // ignored — lib.rs calls this exactly once in `setup`, and a
        // hypothetical second call losing is harmless (first attach wins).
        let _ = self.midi_input.set(mgr);
    }

    /// Select the hardware MIDI-in port, logging the attribution before
    /// delegating to the attached `MidiInputManager` — same shape as
    /// `select_input_device`, but there is no engine `ControlMsg` for this;
    /// the manager owns the connection directly (Task 3).
    pub fn select_midi_input_port(
        &self,
        port_id: Option<String>,
        monitor: bool,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        log::info!(
            "select_midi_input_port: actor={:?} label={:?} port={port_id:?} monitor={monitor}",
            meta.actor,
            meta.label
        );
        let mgr = self
            .midi_input
            .get()
            .ok_or_else(|| "midi input manager not attached".to_string())?;
        mgr.select_port(port_id, monitor)
    }

    /// Route incoming MIDI to a track's instrument (ruling 1: app config,
    /// not document state). `None` clears the routing. `Some(id)` must name
    /// an existing `kind: "midi"` track — validated under a SHORT session
    /// read that is dropped BEFORE touching the hub (never held across the
    /// hub call).
    pub fn select_midi_input_track(
        &self,
        track_id: Option<String>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if let Some(id) = &track_id {
            let session = self.session.lock();
            let t = session
                .store
                .tracks
                .iter()
                .find(|t| t.id.as_str() == id)
                .ok_or_else(|| format!("unknown track: {id}"))?;
            if t.kind != "midi" {
                return Err(format!(
                    "track {id} is kind \"{}\" (midi input needs a midi track)",
                    t.kind
                ));
            }
        }
        log::info!(
            "select_midi_input_track: actor={:?} label={:?} track={track_id:?}",
            meta.actor,
            meta.label
        );
        crate::audio::midi_in::hub().set_target_track(track_id);
        Ok(())
    }

    // ---- MIDI-out routing / clock --------------------------------------
    // Which track/clip goes to which port+channel, and whether a port is
    // clock-slaved, are all app config, not document state — no `Op`, no
    // `commit` (ruling 10, extended from slice 2's single-track carve-out
    // to per-track/per-clip routing across multiple open ports). Moving
    // them behind `ControlPlane` methods is purely so the ACTOR/LABEL
    // attribution is captured at the one front door both Tauri and MCP
    // call through, and so every mutation re-persists the per-machine
    // routing file (`midi_out::persist`) for the CURRENT project path.

    /// lib.rs setup calls this once, after `.manage` — before that, every
    /// unit test's `ControlPlane` has an unattached `midi_out`, and the
    /// methods below error instead of panicking.
    pub fn attach_midi_out(&self, out: Arc<crate::midi_out::MidiOut>) {
        // Every change to the routing table — from here, from the panel, from
        // a project adopt, from the port thread's own self-heal — has to reach
        // the engine, because routing is app config and never rides a commit.
        // One hook installed here does that, instead of a `publish` call
        // remembered at each of six sites (which is how two delete paths came
        // to be missed; see `midi_out::RoutesChanged`).
        //
        // WEAK, deliberately: the hook lives inside `MidiOut`, so a strong
        // reference back to it would be a cycle that never drops.
        let engine = self.engine.clone();
        let weak = Arc::downgrade(&out);
        out.set_routes_changed_hook(std::sync::Arc::new(move || {
            if let Some(out) = weak.upgrade() {
                engine.send(ControlMsg::SetExternalRouting(std::sync::Arc::new(
                    out.routed_out(),
                )));
            }
        }));
        // `OnceLock::set` returning `Err` (already attached) is silently
        // ignored — lib.rs calls this exactly once in `setup`, and a
        // hypothetical second call losing is harmless (first attach wins).
        let _ = self.midi_out.set(out);
        // Seed the engine with whatever the table already holds; the hook only
        // fires on later changes.
        self.publish_external_routing();
    }

    fn midi_out(&self) -> Result<&Arc<crate::midi_out::MidiOut>, String> {
        self.midi_out.get().ok_or_else(|| "midi output driver not attached".to_string())
    }

    /// Push the current routing table to the engine, so a routed track's
    /// internal instrument stops (or resumes) sounding.
    ///
    /// Normal changes do NOT come through here — `attach_midi_out` installs a
    /// `RoutesChanged` hook on `MidiOut` that fires on every mutation, which is
    /// the only way to cover the port thread's self-heal as well. This is the
    /// seeding call for attach time, and the answer for a `ControlPlane` with no
    /// `MidiOut` attached (every unit test): an empty table, which is correct.
    fn publish_external_routing(&self) {
        let routed = self
            .midi_out
            .get()
            .map(|out| out.routed_out())
            .unwrap_or_default();
        self.engine
            .send(ControlMsg::SetExternalRouting(std::sync::Arc::new(routed)));
    }

    /// The current project's directory, if it has one yet (an unsaved
    /// project has none, and its routing simply isn't persisted — see
    /// `midi_out::MidiOut::persist`'s doc).
    fn current_project_dir(&self) -> Option<std::path::PathBuf> {
        self.session.lock().store.project_dir.clone()
    }

    /// Open one more hardware MIDI-out port — additive, does not affect any
    /// other port already open.
    pub fn open_midi_output_port(&self, port_id: String, meta: op::TxMeta) -> Result<(), String> {
        log::info!(
            "open_midi_output_port: actor={:?} label={:?} port={port_id:?}",
            meta.actor,
            meta.label
        );
        let out = self.midi_out()?;
        out.open_port(port_id)?;
        out.persist(self.current_project_dir().as_deref());
        Ok(())
    }

    /// Close one open hardware MIDI-out port. Any route still pointing at
    /// it is dropped too — a route silently pointing at nothing forever
    /// would be confusing; the user re-routes explicitly if they meant to
    /// swap devices.
    pub fn close_midi_output_port(&self, port_id: String, meta: op::TxMeta) -> Result<(), String> {
        log::info!(
            "close_midi_output_port: actor={:?} label={:?} port={port_id:?}",
            meta.actor,
            meta.label
        );
        let out = self.midi_out()?;
        out.close_port(&port_id)?;
        out.clear_routes_for_port(&port_id);
        out.persist(self.current_project_dir().as_deref());
        Ok(())
    }

    /// Toggle whether a specific open port's `aura-midi-out-<n>` thread
    /// emits clock/transport bytes. Leaves the thread and the connection
    /// alive either way (note-out keeps working).
    pub fn set_midi_output_clock_enabled(
        &self,
        port_id: String,
        enabled: bool,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        log::info!(
            "set_midi_output_clock_enabled: actor={:?} label={:?} port={port_id:?} enabled={enabled}",
            meta.actor,
            meta.label
        );
        let out = self.midi_out()?;
        out.set_clock_enabled(&port_id, enabled)?;
        out.persist(self.current_project_dir().as_deref());
        Ok(())
    }

    /// Route a MIDI track's notes to external gear, or (on `port_id: None`)
    /// clear its routing (ruling 10: app config — no document field, no
    /// `Op`). The track must exist and be `kind: "midi"`, validated under a
    /// SHORT session read dropped BEFORE touching `MidiOut` — mirrors
    /// `select_midi_input_track`'s validation shape. A clip belonging to
    /// this track with its OWN route is unaffected — a clip override
    /// always wins over its track's route (see `midi_out`'s module doc).
    pub fn set_midi_track_route(
        &self,
        track_id: String,
        port_id: Option<String>,
        channel: Option<u8>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        // Only validate when actually routing to a track: a leftover route
        // to a track the document no longer has must stay clearable
        // (`port_id: None`) even though the track itself fails this lookup
        // — that is exactly the orphan case the PATCH panel's "forget"
        // button exists for.
        if port_id.is_some() {
            let session = self.session.lock();
            let t = session
                .store
                .tracks
                .iter()
                .find(|t| t.id.as_str() == track_id)
                .ok_or_else(|| format!("unknown track: {track_id}"))?;
            if t.kind != "midi" {
                return Err(format!(
                    "track {track_id} is kind \"{}\" (midi output needs a midi track)",
                    t.kind
                ));
            }
        }
        log::info!(
            "set_midi_track_route: actor={:?} label={:?} track={track_id} port={port_id:?} channel={channel:?}",
            meta.actor,
            meta.label
        );
        let out = self.midi_out()?;
        out.set_route(
            crate::midi_out::RouteScope::Track(track_id),
            port_id.map(|port_id| crate::midi_out::RouteTarget::new(port_id, channel)),
        );
        out.persist(self.current_project_dir().as_deref());
        Ok(())
    }

    /// Pick (or clear) the audio-return input device on a routed MIDI track.
    /// App config, same file as the route — no `Op`. The track must already
    /// be routed (`MidiOut::set_return` rejects otherwise).
    pub fn set_midi_track_return(
        &self,
        track_id: String,
        device_id: Option<String>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        {
            let session = self.session.lock();
            let t = session
                .store
                .tracks
                .iter()
                .find(|t| t.id.as_str() == track_id)
                .ok_or_else(|| format!("unknown track: {track_id}"))?;
            if t.kind != "midi" {
                return Err(format!(
                    "track {track_id} is kind \"{}\" (a return hangs on a midi track)",
                    t.kind
                ));
            }
        }
        log::info!(
            "set_midi_track_return: actor={:?} label={:?} track={track_id} device={device_id:?}",
            meta.actor,
            meta.label
        );
        let out = self.midi_out()?;
        out.set_return(&track_id, device_id)?;
        out.persist(self.current_project_dir().as_deref());
        Ok(())
    }

    /// Route one MIDI clip's notes to external gear, overriding its
    /// track's route (or, on `port_id: None`, clear the override so the
    /// clip falls back to its track's routing). The clip must exist.
    pub fn set_midi_clip_route(
        &self,
        clip_id: String,
        port_id: Option<String>,
        channel: Option<u8>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        // Same asymmetry as `set_midi_track_route`: clearing a leftover
        // route to a clip that is already gone must not fail the existence
        // check that only makes sense when actually routing to a clip.
        if port_id.is_some() {
            let session = self.session.lock();
            if !session.midi.clips.iter().any(|c| c.id.as_str() == clip_id) {
                return Err(format!("unknown midi clip: {clip_id}"));
            }
        }
        log::info!(
            "set_midi_clip_route: actor={:?} label={:?} clip={clip_id} port={port_id:?} channel={channel:?}",
            meta.actor,
            meta.label
        );
        let out = self.midi_out()?;
        out.set_route(
            crate::midi_out::RouteScope::Clip(clip_id),
            port_id.map(|port_id| crate::midi_out::RouteTarget::new(port_id, channel)),
        );
        out.persist(self.current_project_dir().as_deref());
        Ok(())
    }

    pub fn set_midi_track_virtual_output(
        &self,
        track_id: String,
        enabled: bool,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        let name = {
            let session = self.session.lock();
            let track = session.store.tracks.iter().find(|t| t.id.as_str() == track_id)
                .ok_or_else(|| format!("unknown track: {track_id}"))?;
            if track.kind != "midi" {
                return Err(format!("track {track_id} is not a MIDI track"));
            }
            track.name.clone()
        };
        log::info!("set_midi_track_virtual_output: actor={:?} label={:?} track={track_id} enabled={enabled}", meta.actor, meta.label);
        let out = self.midi_out()?;
        out.set_virtual_route(crate::midi_out::RouteScope::Track(track_id), &name, enabled)?;
        out.persist(self.current_project_dir().as_deref());
        Ok(())
    }

    pub fn set_midi_clip_virtual_output(
        &self,
        clip_id: String,
        enabled: bool,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        let name = {
            let session = self.session.lock();
            session.midi.clips.iter().find(|c| c.id.as_str() == clip_id)
                .ok_or_else(|| format!("unknown midi clip: {clip_id}"))?.name.clone()
        };
        log::info!("set_midi_clip_virtual_output: actor={:?} label={:?} clip={clip_id} enabled={enabled}", meta.actor, meta.label);
        let out = self.midi_out()?;
        out.set_virtual_route(crate::midi_out::RouteScope::Clip(clip_id), &name, enabled)?;
        out.persist(self.current_project_dir().as_deref());
        Ok(())
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
            tx.apply(op::Op::TrackAdd { track: track.clone(), index, clips: vec![], clip_indices: vec![], automation_clips: vec![], bindings: vec![] })
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
        // Capture insert-instance rows (blob + params) BEFORE the commit —
        // same prepare pattern as `plugin_remove`. After TrackRemove the
        // slots leave with the row; PluginRemove then drops the instances
        // (G-10 sweep is a no-op). Undo applies inverses in reverse:
        // PluginAdds then TrackAdd (with inserts).
        let (track, clip_ids, insert_removes, send_removes, output_resets) = {
            let session = self.session.lock();
            let track = session
                .store
                .tracks
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| format!("unknown track: {id}"))?;
            let clip_ids: Vec<String> = session
                .midi
                .clips
                .iter()
                .filter(|c| c.track_id.as_str() == id)
                .map(|c| c.id.to_string())
                .collect();
            let mut insert_removes = Vec::new();
            for slot in &track.inserts {
                let Some(row) =
                    session.plugins.instances.iter().find(|r| r.id == slot.instance_id).cloned()
                else {
                    continue;
                };
                let blob = crate::plugins::state::registered_state_bridge()
                    .and_then(|b| b.save_state(&row.id).ok().flatten())
                    .map(|blob| crate::plugins::state::encode_state(&row.uid, &blob));
                let params = session.plugins.params.get(&row.id).cloned().unwrap_or_default();
                insert_removes.push((row, blob, params));
            }
            // Plan G2: every send POINTING AT this track. Removing a bus
            // must not leave dangling wires behind — the compiler would drop
            // them silently, but the send list on the source track would go
            // on showing a row whose destination no longer exists. Removed
            // in the SAME commit so one undo brings the bus and its wiring
            // back together.
            let mut send_removes = Vec::new();
            let mut output_resets = Vec::new();
            for t in &session.store.tracks {
                for snd in &t.sends {
                    if snd.dest.as_str() == id {
                        send_removes.push((t.id.clone(), snd.clone()));
                    }
                }
                // Same argument for the OUTPUT edge: a track routed into a
                // bus that is going away must fall back to the master here,
                // in this commit, so one undo restores both. Left alone it
                // would compile to the master anyway — but silently, and
                // the document would keep naming a track that is gone.
                if t.output.as_ref().is_some_and(|o| o.as_str() == id) {
                    output_resets.push(t.id.clone());
                }
            }
            (track, clip_ids, insert_removes, send_removes, output_resets)
        };
        self.commit(meta, |tx| {
            for track_id in output_resets {
                tx.apply(op::Op::TrackSetOutput { track_id, output: None })?;
            }
            for (track_id, slot) in send_removes {
                tx.apply(op::Op::SendRemove { track_id, slot, index: 0 })?;
            }
            tx.apply(op::Op::TrackRemove {
                track,
                index: 0,
                clips: vec![],
                clip_indices: vec![],
            })?;
            for (row, state, params) in insert_removes {
                tx.apply(op::Op::PluginRemove {
                    row,
                    index: 0,
                    state,
                    params,
                })?;
            }
            Ok(())
        })?;
        self.clear_midi_routing_for(id, &clip_ids);
        Ok(())
    }

    /// Lanes UX: rename one track. Property-addressed (`Op::Set` +
    /// `PropPath::Name`), so it is undoable, journaled and coalescable like
    /// every other track property — there is no bespoke rename channel.
    /// Trim/empty/length validation lives on the WRITE side
    /// (`session::write_prop`), so what comes back here is the value that
    /// was actually stored.
    pub fn set_track_name(
        &self,
        id: &str,
        name: String,
        meta: op::TxMeta,
    ) -> Result<TrackState, String> {
        self.set_track_prop(id, op::PropPath::Name, serde_json::Value::String(name), meta)
    }

    /// Lanes UX: put one track in a lane group (`None` = ungrouped). See
    /// [`Self::arrange_lanes`] for the drag-shaped path that also moves the
    /// row — this one is the plain "set the label" case.
    ///
    /// Deliberately does NOT reorder: `buildLaneLayout` (frontend) treats a
    /// group as the maximal CONTIGUOUS run of tracks sharing its name, so
    /// calling this on a track outside its target group's run produces a
    /// track whose `group` is set but is not adjacent to the rest of that
    /// group — a split run the UI was built to never paint. No UI path
    /// calls this today (every user-facing group change goes through
    /// `arrange_lanes`, which keeps runs contiguous by construction); if you
    /// are adding one, prefer `arrange_lanes` unless you also handle
    /// contiguity at the call site.
    pub fn set_track_group(
        &self,
        id: &str,
        group: Option<String>,
        meta: op::TxMeta,
    ) -> Result<TrackState, String> {
        let to = match group {
            Some(g) => serde_json::Value::String(g),
            None => serde_json::Value::Null,
        };
        self.set_track_prop(id, op::PropPath::Group, to, meta)
    }

    /// Shared body of the two setters above: one `Op::Set` against a track,
    /// then read the committed row back. Validating the id here (rather
    /// than letting `apply_raw` fail) keeps the error message the same
    /// shape as `set_track_mix`'s pre-check.
    fn set_track_prop(
        &self,
        id: &str,
        path: op::PropPath,
        to: serde_json::Value,
        meta: op::TxMeta,
    ) -> Result<TrackState, String> {
        {
            let session = self.session.lock();
            if !session.store.tracks.iter().any(|t| t.id.as_str() == id) {
                return Err(format!("unknown track: {id}"));
            }
        }
        let object = op::ObjectRef::Track(id.into());
        self.commit(meta, |tx| {
            // `from` is advisory — `apply_raw` reads store truth for the
            // inverse — so `Null` here is correct, not a placeholder bug.
            tx.apply(op::Op::Set { object, path, from: serde_json::Value::Null, to })
        })?;
        let session = self.session.lock();
        session
            .store
            .tracks
            .iter()
            .find(|t| t.id.as_str() == id)
            .cloned()
            // Same race `set_track_mix` documents: a concurrent
            // `remove_track` can land between commit and this lock.
            .ok_or_else(|| format!("unknown track: {id}"))
    }

    /// Lanes UX: apply a whole lane arrangement — display order plus each
    /// row's group — in ONE transaction.
    ///
    /// Coarse on purpose. Every lane gesture the UI offers (drag to
    /// reorder, drag into or out of a group, rename a group, collapse-driven
    /// regroup) changes order and membership together, and a user who drags
    /// one lane into a group expects ONE Ctrl+Z to put it back. Splitting
    /// this into `reorder` + N × `set_track_group` would make that two to
    /// N+1 undo steps and would publish intermediate arrangements where a
    /// group is momentarily non-contiguous.
    ///
    /// `lanes` must list EVERY track exactly once, in the new display
    /// order; `Op::TrackReorder` enforces that and rejects the whole
    /// transaction otherwise. Group writes are emitted only where the value
    /// actually changes, so a pure reorder journals one op, not N+1.
    pub fn arrange_lanes(
        &self,
        lanes: Vec<LaneArrangement>,
        meta: op::TxMeta,
    ) -> Result<Vec<TrackState>, String> {
        // Snapshot current groups before the commit so the diff below only
        // emits `Set`s that change something.
        let current: std::collections::HashMap<String, Option<String>> = {
            let session = self.session.lock();
            session
                .store
                .tracks
                .iter()
                .map(|t| (t.id.to_string(), t.group.clone()))
                .collect()
        };
        let order: Vec<crate::ids::TrackId> =
            lanes.iter().map(|l| l.track_id.as_str().into()).collect();
        self.commit(meta, |tx| {
            // Reorder FIRST: it is the op that validates the id set, so a
            // bogus arrangement fails before any group label is rewritten.
            tx.apply(op::Op::TrackReorder { order })?;
            for lane in &lanes {
                // Normalize here as well as in `write_prop` so the
                // "unchanged" comparison sees the same shape the store
                // holds — otherwise `Some("")` from a caller would look
                // different from the stored `None` and emit a no-op Set.
                let want = lane.group.as_deref().map(str::trim).filter(|g| !g.is_empty());
                let have = current.get(&lane.track_id).cloned().flatten();
                if want == have.as_deref() {
                    continue;
                }
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Track(lane.track_id.as_str().into()),
                    path: op::PropPath::Group,
                    from: serde_json::Value::Null,
                    to: match want {
                        Some(g) => serde_json::Value::String(g.to_string()),
                        None => serde_json::Value::Null,
                    },
                })?;
            }
            Ok(())
        })?;
        let session = self.session.lock();
        Ok(session.store.tracks.clone())
    }

    /// Plan G1: document half of insert-add — `PluginAdd` then `InsertAdd`
    /// in one commit. Host instantiate is prepare-outside (the Tauri
    /// command); callers pass the already-built row + slot.
    pub fn insert_add(
        &self,
        track_id: &str,
        row: crate::plugins::PluginInstanceInfo,
        slot: crate::audio::types::InsertSlot,
        index: usize,
        meta: op::TxMeta,
    ) -> Result<crate::audio::types::InsertSlot, String> {
        let out = slot.clone();
        self.commit(meta, |tx| {
            tx.apply(op::Op::PluginAdd {
                row,
                index: usize::MAX,
            })?;
            tx.apply(op::Op::InsertAdd {
                track_id: track_id.into(),
                slot,
                index,
            })
        })?;
        Ok(out)
    }

    /// Plan G1: remove one insert slot and its plugin instance in one commit.
    pub fn insert_remove(
        &self,
        track_id: &str,
        slot_id: &str,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        let (slot, row, blob, params) = {
            let session = self.session.lock();
            let track = session
                .store
                .tracks
                .iter()
                .find(|t| t.id.as_str() == track_id)
                .ok_or_else(|| format!("unknown track: {track_id}"))?;
            let slot = track
                .inserts
                .iter()
                .find(|s| s.id == slot_id)
                .cloned()
                .ok_or_else(|| format!("unknown insert slot: {slot_id}"))?;
            let row = session
                .plugins
                .instances
                .iter()
                .find(|r| r.id == slot.instance_id)
                .cloned()
                .ok_or_else(|| format!("unknown plugin instance: {}", slot.instance_id))?;
            let blob = crate::plugins::state::registered_state_bridge()
                .and_then(|b| b.save_state(&row.id).ok().flatten())
                .map(|blob| crate::plugins::state::encode_state(&row.uid, &blob));
            let params = session.plugins.params.get(&row.id).cloned().unwrap_or_default();
            (slot, row, blob, params)
        };
        self.commit(meta, |tx| {
            tx.apply(op::Op::InsertRemove {
                track_id: track_id.into(),
                slot,
                index: 0,
            })?;
            tx.apply(op::Op::PluginRemove {
                row,
                index: 0,
                state: blob,
                params,
            })
        })?;
        Ok(())
    }

    /// Plan G1: reorder one insert slot on a track.
    pub fn insert_reorder(
        &self,
        track_id: &str,
        slot_id: &str,
        to_index: usize,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        self.commit(meta, |tx| {
            tx.apply(op::Op::InsertReorder {
                track_id: track_id.into(),
                slot_id: slot_id.into(),
                from: 0,
                to: to_index,
            })
        })?;
        Ok(())
    }

    /// Plan G1: set bypass on one insert slot.
    pub fn insert_set_bypass(
        &self,
        track_id: &str,
        slot_id: &str,
        bypassed: bool,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        self.commit(meta, |tx| {
            tx.apply(op::Op::InsertSetBypass {
                track_id: track_id.into(),
                slot_id: slot_id.into(),
                bypassed,
            })
        })?;
        Ok(())
    }

    /// Plan G2: add a send from `track_id` into the bus `dest`. The id is
    /// minted here (a uuid, stable across reorder — `SCALABILITY` §2), and
    /// the send lands at unity so adding it is audible; `send_set_amount`
    /// dials it from there. Returns the row that was stored.
    pub fn send_add(
        &self,
        track_id: &str,
        dest: &str,
        meta: op::TxMeta,
    ) -> Result<crate::audio::types::SendSlot, String> {
        let slot = crate::audio::types::SendSlot {
            id: uuid::Uuid::new_v4().to_string(),
            dest: dest.into(),
            amount_db: 0.0,
            pre_fader: false,
        };
        let out = slot.clone();
        self.commit(meta, |tx| {
            tx.apply(op::Op::SendAdd {
                track_id: track_id.into(),
                slot,
                index: usize::MAX,
            })
        })?;
        Ok(out)
    }

    /// Plan G2: remove one send edge.
    pub fn send_remove(&self, track_id: &str, send_id: &str, meta: op::TxMeta) -> Result<(), String> {
        let slot = {
            let session = self.session.lock();
            session
                .store
                .tracks
                .iter()
                .find(|t| t.id.as_str() == track_id)
                .ok_or_else(|| format!("unknown track: {track_id}"))?
                .sends
                .iter()
                .find(|s| s.id == send_id)
                .cloned()
                .ok_or_else(|| format!("unknown send: {send_id}"))?
        };
        self.commit(meta, |tx| {
            tx.apply(op::Op::SendRemove { track_id: track_id.into(), slot, index: 0 })
        })?;
        Ok(())
    }

    /// Plan G2: set a send's amount in dB. A MIX change — the commit
    /// resolves it into `ParamTable::send_amount` and schedules NO rebuild.
    pub fn send_set_amount(
        &self,
        track_id: &str,
        send_id: &str,
        amount_db: f64,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        let make_op = || op::Op::SendSetAmount {
            track_id: track_id.into(),
            send_id: send_id.into(),
            amount_db,
        };
        // Same shape as `set_track_mix`'s fader path: inside an open
        // gesture the write is transient and folds into the batch, so a
        // knob DRAG is one undo step and one persist; outside one it is an
        // ordinary commit. Whether a gesture is open is a runtime fact, so
        // both closures have to exist.
        let gesture_meta = meta.clone();
        let folded = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| tx.apply(make_op()))
        });
        match folded {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| tx.apply(make_op()))?;
            }
        }
        Ok(())
    }

    /// Plan G2: point a track's output at a bus, or back at the master
    /// (`output: None`). A MOVE, not a copy — the track stops reaching the
    /// master. Rejected if it would close a routing loop.
    pub fn track_set_output(
        &self,
        track_id: &str,
        output: Option<&str>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        let output = output.map(crate::ids::TrackId::from);
        self.commit(meta, |tx| {
            tx.apply(op::Op::TrackSetOutput { track_id: track_id.into(), output })
        })?;
        Ok(())
    }

    /// Plan G2: move a send's tap between post-fader and pre-fader
    /// (structural — it changes where the wire leaves the strip).
    pub fn send_set_pre_fader(
        &self,
        track_id: &str,
        send_id: &str,
        pre_fader: bool,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        self.commit(meta, |tx| {
            tx.apply(op::Op::SendSetPreFader {
                track_id: track_id.into(),
                send_id: send_id.into(),
                pre_fader,
            })
        })?;
        Ok(())
    }

    /// The MIDI-in target and every MIDI-out route (track- or clip-scoped)
    /// are app config holding track/clip ids (rulings 1 and 10), so nothing
    /// in the document model retires them when the track goes away. Called
    /// AFTER the commit succeeds: a failed removal must leave the routing
    /// exactly as it was.
    ///
    /// This explicit delete path (and `clear_midi_route_for_clip` below, for
    /// `midi::midi_remove_clip_core`) clears eagerly; the OUTPUT side also
    /// has a second line of defense that covers every other case (undoing a
    /// `TrackAdd`/`MidiClipAdd`, or an `Op::MidiClipRemove` applied directly
    /// without going through either wrapper — e.g. inside `midi_add_clip_
    /// core`'s own undo path) — `midi_out::run_thread` re-validates every
    /// route it owns against the live document on each 250 ms window and
    /// drops whatever no longer exists (self-healing, see that module's
    /// doc). On the INPUT side that second line is `refresh_target`
    /// resolving an unknown id to `NO_SLOT`,
    /// so events are simply dropped. On the OUTPUT side it is only
    /// survivable because of the whole-track review's Critical 1 fix: the
    /// vanished track's events snapshot becomes EMPTY under an UNCHANGED
    /// track id, which is exactly the shape that used to strand the cursor
    /// and hang a
    /// sounding note — `run_thread` now keys its release+reseek on the
    /// snapshot's CONTENT, so the note is released instead. Do not restore
    /// an id-only comparison there on the assumption that this path is
    /// harmless. Either way, both selectors must keep tolerating an id the
    /// store does not have.
    fn clear_midi_routing_for(&self, id: &str, clip_ids: &[String]) {
        let hub = crate::audio::midi_in::hub();
        if hub.target_track().as_deref() == Some(id) {
            // Goes through `set_target_track`, so the outgoing target still
            // gets its `all_off` — a key held while the track is deleted
            // must not be left sounding.
            hub.set_target_track(None);
        }
        if let Some(out) = self.midi_out.get() {
            out.clear_routes_for_track(id, clip_ids);
            out.persist(self.current_project_dir().as_deref());
        }
    }

    /// Clear a single clip's MIDI-out route (if any) — the single-clip
    /// analogue of `clear_midi_routing_for`, called from `midi::
    /// midi_remove_clip_core` after its commit succeeds. A no-op if no
    /// `MidiOut` is attached (every unit test's `ControlPlane` by default).
    pub fn clear_midi_route_for_clip(&self, clip_id: &str) {
        if let Some(out) = self.midi_out.get() {
            out.clear_route_for_clip(clip_id);
            out.persist(self.current_project_dir().as_deref());
        }
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
        mut changes: Vec<TrackMixChange>,
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
        // While playback is writing automation, an open fader gesture is a
        // LIVE automation controller. The persisted gain remains the base
        // fader and the recorded lane becomes its relative multiplier;
        // committing this gain as a normal Set would both move the base and
        // write the same movement into the lane (double attenuation and two
        // undo entries). Non-gesture callers and stopped transport retain
        // the ordinary document-edit behaviour.
        let recording_gains: Vec<(String, f64)> = if self.shared.playing.load(Relaxed) {
            let session = self.session.lock();
            changes
                .iter()
                .filter_map(|c| {
                    let gain = c.gain_db?;
                    let track = session.store.tracks.iter().find(|t| t.id == c.track_id)?;
                    records_automation(track.automation_mode)
                        .then(|| (c.track_id.clone(), gain.clamp(-160.0, 24.0)))
                })
                .collect()
        } else {
            Vec::new()
        };
        let live_ids: Vec<String> = recording_gains.iter().map(|(id, _)| id.clone()).collect();
        let live_controlled = !recording_gains.is_empty()
            && self.gesture.control_live_track_gains(&meta.actor, &live_ids, |automation_pass| {
                let tables = self.tables.lock();
                for (track_id, gain_db) in &recording_gains {
                    if let Some(&slot) = tables.slots.get(track_id.as_str()) {
                        tables.params.set_gain_linear(
                            slot,
                            crate::audio::mixer::db_to_linear(*gain_db),
                        );
                        tables.params.set_gain_automation_owner(slot, Some(automation_pass));
                    }
                }
            });
        if live_controlled {
            for change in &mut changes {
                if live_ids.iter().any(|id| id == &change.track_id) {
                    change.gain_db = None;
                }
            }
        }
        let has_document_changes = changes.iter().any(|c| {
            c.gain_db.is_some()
                || c.pan.is_some()
                || c.muted.is_some()
                || c.soloed.is_some()
                || c.armed.is_some()
                || c.automation_mode.is_some()
        });

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
        if has_document_changes {
            let gesture_meta = meta.clone();
            let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
                self.commit_transient_for_gesture(gesture_meta, |tx| apply_mix_changes(&changes, tx))
            });
            match gesture_result {
                Some(result) => {
                    result?;
                }
                None => {
                    self.commit(meta, |tx| apply_mix_changes(&changes, tx))?;
                }
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

    /// Batched plugin-param writes through the transaction channel — one
    /// `Op::Set{Plugin, Param}` per change, applied atomically. Gesture-aware
    /// in exactly the shape `set_track_mix` uses (I-8: plugin knobs are the
    /// canonical CLAP gesture, round-2 §4.4, and until now they were the one
    /// drag surface that never consulted `GestureState` — so every rAF batch
    /// was its own undo entry AND its own `project.json` rewrite).
    ///
    /// LOCK ORDER: `commit_transient_and_fold` holds the gesture mutex
    /// across the nested session-lock acquisition; that direction (gesture,
    /// then session) is the only safe one and is the one used here.
    pub fn set_plugin_params(
        &self,
        instance_id: &str,
        changes: &[crate::plugins::ParamChange],
        meta: op::TxMeta,
    ) -> Result<(), String> {
        // Validate before any commit (the `transact` closure must not panic,
        // and an unknown instance must fail the whole batch atomically).
        {
            let s = self.session.lock();
            if !s.plugins.instances.iter().any(|r| r.id == instance_id) {
                return Err(format!("unknown plugin instance: {instance_id}"));
            }
        }
        let apply = |changes: &[crate::plugins::ParamChange], tx: &mut session::Tx<'_>| {
            for c in changes {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Plugin(instance_id.to_string()),
                    path: op::PropPath::Param { index: c.id },
                    from: serde_json::Value::Null,
                    to: serde_json::json!(c.value),
                })?;
            }
            Ok(())
        };
        let gesture_meta = meta.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| apply(changes, tx))
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| apply(changes, tx))?;
            }
        }
        Ok(())
    }

    /// Upsert (or delete, when the normalized lane has no points) ONE
    /// automation lane through the transaction channel — the §4.4
    /// value-replacement wrapper, gesture-aware in the same shape as
    /// `set_track_mix`/`set_plugin_params`. A lane drag therefore folds to
    /// one `Op::AutomationSetLane` (last write per lane id wins), one undo
    /// entry, and — with the gesture's deferred persist — one automation
    /// write to disk.
    pub fn set_automation_lane(
        &self,
        mut lane: crate::plugins::automation::AutomationLane,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if lane.id.is_empty() {
            lane.id = uuid::Uuid::new_v4().to_string();
        }
        // Validate/normalize BEFORE the transaction (the closure must not
        // panic, and a rejected lane must leave no document trace).
        crate::plugins::automation::normalize_lane(&mut lane)?;
        let key = lane.id.clone();
        let to_apply = if lane.points.is_empty() { None } else { Some(lane) };
        let gesture_meta = meta.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| {
                tx.apply(op::Op::AutomationSetLane { key: key.clone(), lane: to_apply.clone() })
            })
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| {
                    tx.apply(op::Op::AutomationSetLane { key: key.clone(), lane: to_apply.clone() })
                })?;
            }
        }
        Ok(())
    }

    /// Upsert (or delete when `curve` is `None`) one curve through the
    /// transaction channel — Track F, same gesture-aware shape as
    /// `set_automation_lane`.
    pub fn set_curve(
        &self,
        key: String,
        mut curve: Option<crate::modulation::Curve>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if let Some(c) = curve.as_mut() {
            c.id = key.clone();
            let domain = self.session.lock().modulation.domain_of(&key);
            crate::modulation::normalize_curve_in_domain(c, domain)?;
        }
        let to_apply = curve;
        let gesture_meta = meta.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| {
                tx.apply(op::Op::ModulationSetCurve {
                    key: key.clone(),
                    curve: to_apply.clone(),
                })
            })
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| {
                    tx.apply(op::Op::ModulationSetCurve {
                        key: key.clone(),
                        curve: to_apply.clone(),
                    })
                })?;
            }
        }
        Ok(())
    }

    /// Upsert (or delete when `binding` is `None`) one binding through the
    /// transaction channel — Track F.
    pub fn set_binding(
        &self,
        key: String,
        mut binding: Option<crate::modulation::Binding>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if let Some(b) = binding.as_mut() {
            b.id = key.clone();
            crate::modulation::validate_binding(b)?;
        }
        let to_apply = binding;
        let gesture_meta = meta.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| {
                tx.apply(op::Op::ModulationSetBinding {
                    key: key.clone(),
                    binding: to_apply.clone(),
                })
            })
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| {
                    tx.apply(op::Op::ModulationSetBinding {
                        key: key.clone(),
                        binding: to_apply.clone(),
                    })
                })?;
            }
        }
        Ok(())
    }

    /// Upsert (or delete when `clip` is `None`) one automation clip through
    /// the transaction channel — Track F.
    pub fn set_automation_clip(
        &self,
        key: String,
        mut clip: Option<crate::modulation::AutomationClip>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if let Some(c) = clip.as_mut() {
            c.id = key.clone();
            if c.id.is_empty() {
                return Err("automation clip id must not be empty".into());
            }
            if c.track_id.is_empty() {
                return Err("automation clip trackId must not be empty".into());
            }
            if c.curve_id.is_empty() {
                return Err("automation clip curveId must not be empty".into());
            }
        }
        let to_apply = clip;
        let gesture_meta = meta.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| {
                tx.apply(op::Op::AutomationClipSet {
                    key: key.clone(),
                    clip: to_apply.clone(),
                })
            })
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| {
                    tx.apply(op::Op::AutomationClipSet {
                        key: key.clone(),
                        clip: to_apply.clone(),
                    })
                })?;
            }
        }
        Ok(())
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

    /// Move (and, for MIDI, optionally resize) a BATCH of clips in ONE
    /// transaction — the group-drag counterpart of [`Self::move_clip`].
    /// Audio and MIDI clips mix freely: both stores live under the one
    /// session lock (round-2 §4.1's cross-store atomicity), so a mixed
    /// selection is one atomic batch and ONE undo entry.
    ///
    /// GESTURE-AWARE, and deliberately the same shape as
    /// [`Self::set_track_mix`]: when a gesture matching this actor is open,
    /// the batch commits TRANSIENT and folds into the gesture's accumulator
    /// through `GestureState::commit_transient_and_fold` — which holds the
    /// gesture mutex across the nested session-lock acquisition, the ONE
    /// safe nesting direction (Task 14, fix round 1 Finding 2). Do not
    /// reorder those locks. With no gesture open it falls back to a plain
    /// `commit`, which is still exactly one history entry.
    ///
    /// Every id is validated BEFORE any op is applied: a batch with one bad
    /// id fails whole, having moved nothing (`Session::transact` would roll
    /// back the applied ops, but pre-checking makes the failure atomic at
    /// the batch level and keeps the error message about the id the caller
    /// got wrong).
    pub fn move_clips(
        &self,
        placements: Vec<ClipPlacement>,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if placements.is_empty() {
            return Ok(());
        }
        {
            let session = self.session.lock();
            for p in &placements {
                let known = match p {
                    ClipPlacement::Audio { clip_id, .. } => {
                        session.store.clips.iter().any(|c| c.id == *clip_id)
                    }
                    ClipPlacement::Midi { clip_id, .. } => {
                        session.midi.clips.iter().any(|c| c.id == *clip_id)
                    }
                };
                if !known {
                    return Err(format!("unknown clip: {}", p.clip_id()));
                }
            }
        }
        let gesture_meta = meta.clone();
        let placements_for_gesture = placements.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_with(
                gesture_meta.transient(),
                |tx| apply_clip_placements(&placements_for_gesture, tx),
                false,
            )
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| apply_clip_placements(&placements, tx))?;
            }
        }
        Ok(())
    }

    /// Remove an audio clip through the transaction channel
    /// (`Op::ClipRemove`, `commit`) — the structural analogue of
    /// `remove_track`, scoped to a single clip. Looks the clip up via
    /// `tx.store()` INSIDE the closure (`Tx::store`'s documented TOCTOU
    /// rule), not a separate pre-commit lock. `apply_raw`'s `Op::ClipRemove`
    /// arm re-finds it by id (store truth wins over the caller's payload
    /// beyond `.id`) and computes the inverse `Op::ClipAdd`, so undo restores
    /// the clip byte-identically — same free-inverse shape as `remove_track`.
    pub fn remove_clip(&self, clip_id: &str, meta: op::TxMeta) -> Result<(), String> {
        self.commit(meta, |tx| {
            let clip = tx
                .store()
                .clips
                .iter()
                .find(|c| c.id == clip_id)
                .cloned()
                .ok_or_else(|| format!("unknown clip: {clip_id}"))?;
            tx.apply(op::Op::ClipRemove { clip, index: 0 })
        })?;
        Ok(())
    }

    /// Absolute on-disk path for an audio clip's source file — "open in
    /// external editor" (double-click an audio clip on the timeline, the
    /// audio-clip analogue of the MIDI clip's double-click-opens-piano-roll).
    /// Read-only: a plain re-lock, same shape as `set_track_instrument`'s
    /// read-back, not a `commit` (nothing is mutated). Errors when the clip
    /// is unknown, no project is open, or the source file is missing on disk
    /// — a moved/deleted source must surface as an error, not a silent no-op.
    pub fn clip_source_abs_path(&self, clip_id: &str) -> Result<PathBuf, String> {
        let (rel, dir) = {
            let session = self.session.lock();
            let clip = session
                .store
                .clips
                .iter()
                .find(|c| c.id == clip_id)
                .ok_or_else(|| format!("unknown clip: {clip_id}"))?;
            let rel = crate::audio::project::normalize_source_path(&clip.source_path)?;
            let dir = session
                .store
                .project_dir
                .clone()
                .ok_or_else(|| "no project directory open".to_string())?;
            (rel, dir)
        };
        let path = dir.join(rel);
        if !path.is_file() {
            return Err(format!("missing audio source: {}", path.display()));
        }
        Ok(path)
    }

    /// Bind (or unbind, with `instrument_id: None`) a track's instrument
    /// through the transaction channel (`Op::Set`, `PropPath::InstrumentId`)
    /// — Plan E Task 3 (round-2 inventory row 2). `set_track_instrument`
    /// (audio/mod.rs) is a thin wrapper over this, keeping its own
    /// track-kind/plugin-existence validation ahead of the call.
    ///
    /// Plan G1 Task 3 / R6: a `plugin:<id>` ref is rejected when the instance
    /// is already an insert (G-7) or its scanned descriptor is `!is_instrument`.
    /// No uid-suffix heuristic; if the scan cache is absent, G-7 still applies.
    pub fn set_track_instrument(
        &self,
        track_id: &str,
        instrument_id: Option<String>,
        meta: op::TxMeta,
    ) -> Result<TrackState, String> {
        if let Some(ref id) = instrument_id {
            if let Some(pid) = id.strip_prefix("plugin:") {
                self.reject_effect_as_instrument(pid)?;
            }
        }
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

    /// G-7 + R6: refuse binding an insert/effect instance as instrument.
    fn reject_effect_as_instrument(&self, instance_id: &str) -> Result<(), String> {
        let session = self.session.lock();
        for t in &session.store.tracks {
            if t.inserts.iter().any(|s| s.instance_id == instance_id) {
                return Err(format!(
                    "plugin instance {instance_id} is already an insert; cannot bind as instrument"
                ));
            }
        }
        let uid = session
            .plugins
            .instances
            .iter()
            .find(|r| r.id == instance_id)
            .map(|r| r.uid.clone());
        drop(session);
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
        Ok(())
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
        let mut session = self.session.lock();
        session.plugins.pending_state.insert(instance_id.to_string(), bytes);
        // snapshot republish: R-1 — `pending_state` is document content
        // (`SessionSnapshot::plugins` carries it), and this writer bypasses
        // `transact` entirely, so nothing else would publish it.
        session.republish_full();
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

    /// Replace the project tempo map (and optionally the meter). Gesture-
    /// aware in the same shape as [`Self::set_track_mix`]: a transport-bar
    /// slider drag opened with `gesture_begin` folds every tick into one
    /// `Op::TempoSet`. Outside a gesture this is one history entry.
    pub fn set_tempo_map(
        &self,
        ppq: Option<u32>,
        events: Vec<crate::midi::types::TempoEvent>,
        meter: Option<Vec<crate::midi::types::MeterEvent>>,
        meta: op::TxMeta,
    ) -> Result<crate::midi::TempoMapState, String> {
        let gesture_meta = meta.clone();
        let events_g = events.clone();
        let meter_g = meter.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| {
                apply_tempo_map(tx, ppq, &events_g, &meter_g)
            })
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| apply_tempo_map(tx, ppq, &events, &meter))?;
            }
        }
        let session = self.session.lock();
        crate::midi::build_tempo_map_state(
            session.midi.ppq,
            &session.midi.tempo_events,
            &session.midi.meter_events,
        )
    }

    /// The commit shape every gesture-folding caller uses (`set_track_mix`,
    /// `set_plugin_params`, `set_automation_lane`): TRANSIENT (no history
    /// entry, no journal line — the synthesized gesture batch is the
    /// history-bound one), `emit_project_changed: false` (the gesture emits
    /// exactly one at close), and `defer_persist: true` (I-8 — the persist
    /// rides `close_gesture`, once).
    ///
    /// Callers must only reach this from INSIDE
    /// `GestureState::commit_transient_and_fold`, which is what guarantees
    /// the deferred persist is actually accumulated by an open gesture
    /// rather than silently dropped.
    fn commit_transient_for_gesture<F>(
        &self,
        meta: op::TxMeta,
        f: F,
    ) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
    {
        // Fix round 1, Important-2: the doc comment above states the
        // contract; this enforces it. `IN_GESTURE_FOLD` only exists under
        // `#[cfg(debug_assertions)]` (see its declaration), so the whole
        // check — not just `debug_assert!`'s own internal `cfg!` gate — is
        // wrapped in `#[cfg(debug_assertions)]`, same as every other
        // `IN_GESTURE_FOLD` reference in this file.
        #[cfg(debug_assertions)]
        debug_assert!(
            IN_GESTURE_FOLD.with(|marker| marker.get()),
            "commit_transient_for_gesture called outside commit_transient_and_fold — \
             the deferred persist would be dropped"
        );
        self.committer.commit_with_rebuild_full(
            meta.transient(),
            f,
            false,
            || self.engine.send(ControlMsg::Rebuild),
            history::HistoryMode::Record,
            true,
        )
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
    ///
    /// M-4 — AN OPEN GESTURE IS CLOSED FIRST. Ctrl+Z with the pointer still
    /// down used to walk straight past the drag: its writes are transient
    /// folds that have reached no history entry yet, so the pop found the
    /// step BEFORE the gesture, undid that, and the drag's own value then
    /// landed as a separate step whenever the pointer finally came up. The
    /// auto-close is F-7's — the very same `end()` + `close_gesture` pair
    /// `gesture_begin` uses for a stale gesture — so the drag becomes the
    /// finished, undoable step it already looks like to the user, and the
    /// undo consumes THAT.
    ///
    /// LOCK ORDER is unchanged and unnested: `close_gesture` takes (and
    /// releases) the gesture and session locks before `pop_undo` is called;
    /// `pop_undo`/`push_*` take `epoch` -> `history` with NO session lock
    /// held (`commit_replay` returns before the push).
    pub fn undo(&self) -> Result<Option<String>, String> {
        if let Some(g) = self.gesture.end(None) {
            self.close_gesture(g);
        }
        let _gate = self.history_gate.lock();
        self.undo_step()
    }

    /// One undo step with the gate ALREADY HELD — the body of
    /// [`Self::undo`]. `history_gate` is a plain `parking_lot::Mutex` and is
    /// not reentrant, so a caller that holds it must never route back
    /// through [`Self::undo`].
    ///
    /// THE POP HERE IS UNCONDITIONAL, and that is right for a single step:
    /// Ctrl+Z means "undo whatever is on top NOW", so a commit that landed a
    /// moment ago is a legitimate thing to consume. A multi-step walk means
    /// something else — "undo these particular revisions" — and uses
    /// [`Self::undo_walk_step`] for exactly that reason (C-1).
    ///
    /// Everything the single-step contract promises still holds here: the
    /// entry's inverses go through the normal commit path (journaled, own
    /// rebuild, `project://changed`), `HistoryMode::Replay` suppresses a new
    /// history entry, the ORIGINAL entry migrates onto the redo stack, and a
    /// failed commit puts it back untouched.
    fn undo_step(&self) -> Result<Option<String>, String> {
        let Some((entry, popped_epoch)) = self.committer.log().pop_undo() else { return Ok(None) };
        let meta = op::TxMeta {
            actor: op::Actor::User,
            run: entry.run.clone(),
            label: format!("undo: {}", entry.label),
            transient: false,
        };
        let ops = entry.inverses.clone();
        match self.commit_replay(meta, ops, popped_epoch) {
            Ok(()) => {
                let label = entry.label.clone();
                self.committer.log().push_redo(entry, popped_epoch);
                Ok(Some(label))
            }
            Err(e) => {
                self.committer.log().push_undo_unchanged(entry, popped_epoch);
                Err(e)
            }
        }
    }

    /// Redo the most recently undone step — [`Self::undo`]'s mirror in
    /// every respect: `entry.ops` through the normal commit path, journaled,
    /// no new history entry, and the same entry migrates back onto the undo
    /// stack (via `push_undo_unchanged`, which does NOT clear the redo
    /// stack — only a genuinely new edit does that).
    ///
    /// Closes an open gesture first for [`Self::undo`]'s reason (M-4). The
    /// visible consequence differs from undo's: the close records a fresh
    /// edit, and a fresh edit CLEARS the redo stack (`History::record`), so
    /// a redo pressed mid-drag now finds nothing and returns `Ok(None)`
    /// instead of replaying a future the drag has already invalidated —
    /// which is the same answer it would have given a moment later, once
    /// the pointer came up.
    pub fn redo(&self) -> Result<Option<String>, String> {
        if let Some(g) = self.gesture.end(None) {
            self.close_gesture(g);
        }
        let _gate = self.history_gate.lock();
        let Some((entry, popped_epoch)) = self.committer.log().pop_redo() else { return Ok(None) };
        let meta = op::TxMeta {
            actor: op::Actor::User,
            run: entry.run.clone(),
            label: format!("redo: {}", entry.label),
            transient: false,
        };
        let ops = entry.ops.clone();
        match self.commit_replay(meta, ops, popped_epoch) {
            Ok(()) => {
                let label = entry.label.clone();
                self.committer.log().push_undo_unchanged(entry, popped_epoch);
                Ok(Some(label))
            }
            Err(e) => {
                self.committer.log().push_redo(entry, popped_epoch);
                Err(e)
            }
        }
    }

    /// Walk the linear undo ancestry back to `target_rev` — the Plan F
    /// carry-forward (e) "ordered next step".
    ///
    /// SEMANTICS: EXCLUSIVE. Every undo entry ABOVE `target_rev` is undone;
    /// `target_rev` itself stays applied, so the document ends as the one
    /// `materialize_version(target_rev)` describes — the document the
    /// HISTORY dock's detail pane is showing when the user picks that row.
    /// Picking the head is therefore a successful no-op.
    ///
    /// THE GUARDS, and what each is for:
    /// * `expected_epoch` — the document must still be the one the caller
    ///   read. Undoing across a document swap is corruption, not undo
    ///   (`History::clear`'s doc). Checked here once, and then ENFORCED
    ///   AGAIN AT EVERY STEP: [`Self::undo_walk_step`] compares the epoch
    ///   each entry was popped under against THIS value and aborts if they
    ///   differ, before anything is applied.
    ///
    ///   That per-step check is a real guard, not belt-and-braces (I-1,
    ///   whole-branch review). It replaces an inference that does not hold —
    ///   "a boundary clears the undo stack, so a step that still pops
    ///   something proves the epoch has not moved". A boundary followed by a
    ///   FRESH EDIT on the new document leaves a poppable entry again, and
    ///   `commit_replay` only compares that entry against the epoch IT was
    ///   popped under (both new, so it passes). The walk would then keep
    ///   undoing edits belonging to a document the user never targeted, one
    ///   step at a time, and report success. Comparing against the CALLER's
    ///   epoch is what makes "nothing applied after the swap" true.
    /// * `expected_head_rev` — the undo ancestry must not have moved. NOT
    ///   `Session::rev`: transient commits (transport play/stop, gesture
    ///   folds) bump that without touching this stack, so guarding on it
    ///   would abort because the user pressed play.
    /// * `target_rev` must be ON the current undo path. An already-undone
    ///   step (now on the redo stack) and a bottom-evicted one
    ///   (`UNDO_STACK_LIMIT`) are both simply absent, and both are refused
    ///   with the same message: the request describes an ancestry this
    ///   document does not have.
    ///
    /// ONE GATE FOR THE WHOLE WALK: `history_gate` is taken once and held
    /// across every step, so a concurrent `undo`/`redo` cannot interleave
    /// into the middle of a walk. The gate is not reentrant, hence
    /// [`Self::undo_walk_step`] rather than [`Self::undo`].
    ///
    /// THE WALK CONSUMES A ROUTE, NOT A COUNT (C-1, whole-branch review).
    /// The gate serialises this against `undo`/`redo` and nothing else — it
    /// does NOT gate ordinary commits, and this app has several that arrive
    /// off the UI thread: `commit_recording_finalize` commits `Actor::Engine`
    /// non-transiently from the engine control thread, MCP agent tools
    /// commit on their own threads, an automation write pass commits on
    /// release. Any of those landing between two steps pushes a fresh entry
    /// onto the back of the undo stack. A walk that trusted a precomputed
    /// step count would undo THAT entry — an edit nobody asked to undo —
    /// then stop one step short of the target and report success anyway,
    /// with the redo chain it had been building cleared by the same
    /// `History::record`. So the walk instead plans the exact revisions to
    /// consume, highest first, and every step pops CONDITIONALLY
    /// (`HistoryLog::pop_undo_if`): it consumes the revision it planned to,
    /// or nothing at all and the walk stops and says so.
    ///
    /// M-4, an open gesture is closed FIRST, exactly as [`Self::undo`] does
    /// — the drag becomes the finished step it already looks like. That
    /// close records a fresh entry, which MOVES the head, so a walk
    /// requested mid-drag then fails its own head guard and the user
    /// retries against the ancestry they can now see. Aborting is the right
    /// answer: the row they clicked was chosen before the drag existed.
    ///
    /// PARTIAL WALKS ARE REPORTED, NOT HIDDEN. Each step is a real
    /// committed transaction, so there is nothing to roll back to: if step
    /// `k` stops the walk — its commit failed, the epoch moved, or the undo
    /// stack no longer offers the revision that step planned to consume —
    /// the `k` steps before it STAY APPLIED, and the error says how many of
    /// how many were applied and why it stopped. The caller re-reads the
    /// overview AND re-pulls its stores, because the document moved
    /// (`projectops.undoTo`'s failure path does exactly that).
    pub fn undo_to(
        &self,
        target_rev: u64,
        expected_epoch: u64,
        expected_head_rev: Option<u64>,
    ) -> Result<UndoToOutcome, String> {
        if let Some(g) = self.gesture.end(None) {
            self.close_gesture(g);
        }
        let _gate = self.history_gate.lock();

        let path = self.committer.log().undo_path();
        if path.epoch != expected_epoch {
            return Err(format!(
                "the project was replaced under this request (epoch {expected_epoch} -> {}) \
                 — nothing applied",
                path.epoch
            ));
        }
        if path.head() != expected_head_rev {
            return Err(format!(
                "the edit history changed under this request (head {:?} -> {:?}) \
                 — nothing applied",
                expected_head_rev,
                path.head()
            ));
        }
        let Some(at) = path.revs.iter().position(|r| *r == target_rev) else {
            return Err(format!(
                "revision {target_rev} is not on the undo path — it was undone already, \
                 or dropped from the {} most recent steps history keeps",
                history::UNDO_STACK_LIMIT
            ));
        };
        // THE ROUTE: the revisions ABOVE the target, highest first — the
        // exact entries this walk intends to consume, in the order it
        // intends to consume them. Exclusive semantics, so `target_rev`
        // itself is not on it. This list, not its length, is what the loop
        // walks (C-1).
        let route: Vec<u64> = path.revs[at + 1..].iter().rev().copied().collect();
        let steps = route.len();

        let mut label = None;
        for (done, want) in route.iter().enumerate() {
            match self.undo_walk_step(*want, expected_epoch) {
                Ok(Some(l)) => label = Some(l),
                // M-2: "nothing popped" is no longer one specific story. The
                // stack did not offer `want` — a commit landed on top of it
                // mid-walk, or the entry was dropped on its way back by
                // `push_redo`'s epoch guard. Name the observation, not a
                // guess at the cause, and say what stayed applied.
                Ok(None) => {
                    return Err(format!(
                        "undo to revision {target_rev} stopped after {done} of {steps} steps: \
                         the undo stack no longer offers revision {want}, which this step \
                         planned to consume — the edit history moved under the walk. The \
                         {done} steps already applied stay applied."
                    ))
                }
                Err(e) => {
                    return Err(format!(
                        "undo to revision {target_rev} stopped after {done} of {steps} \
                         steps: {e}. The {done} steps already applied stay applied."
                    ))
                }
            }
        }
        Ok(UndoToOutcome { steps, label })
    }

    /// One step of an [`Self::undo_to`] walk: [`Self::undo_step`] for a
    /// PLANNED revision, with the gate already held.
    ///
    /// Two differences from `undo_step`, both of which exist only because a
    /// walk is several steps long while `history_gate` gates only other
    /// `undo`/`redo` commands (C-1):
    ///
    /// * THE POP IS CONDITIONAL. `pop_undo_if(want)` consumes the revision
    ///   this step planned to consume or nothing at all — never "whatever is
    ///   on the back now", which after a concurrent commit is a fresh edit
    ///   the user never asked to undo. `Ok(None)` is that refusal, and it is
    ///   deliberately distinguishable from a failed commit: the caller
    ///   reports "the history moved" rather than a commit error.
    /// * THE POPPED EPOCH IS CHECKED AGAINST THE CALLER'S. `commit_replay`
    ///   compares an entry against the epoch IT was popped under, which
    ///   cannot notice that the whole document was swapped mid-walk and then
    ///   edited: the new document's fresh entry pops under the new epoch and
    ///   agrees with itself. Comparing against the epoch `undo_to`
    ///   VALIDATED is what stops the walk at the boundary. The entry goes
    ///   straight back with `push_undo_unchanged` (whose own guard drops it
    ///   if it belongs to a document that is gone) — a refused step must not
    ///   consume a history entry.
    ///
    /// Everything the single-step contract promises still holds: the
    /// inverses go through the normal commit path (journaled, own rebuild,
    /// `project://changed`), `HistoryMode::Replay` suppresses a new history
    /// entry, the ORIGINAL entry migrates onto the redo stack, and a failed
    /// commit puts it back untouched.
    fn undo_walk_step(&self, want: u64, expected_epoch: u64) -> Result<Option<String>, String> {
        let Some((entry, popped_epoch)) = self.committer.log().pop_undo_if(want) else {
            return Ok(None);
        };
        if popped_epoch != expected_epoch {
            self.committer.log().push_undo_unchanged(entry, popped_epoch);
            return Err(format!(
                "the project was replaced under this walk (epoch {expected_epoch} -> \
                 {popped_epoch})"
            ));
        }
        let meta = op::TxMeta {
            actor: op::Actor::User,
            run: entry.run.clone(),
            label: format!("undo: {}", entry.label),
            transient: false,
        };
        let ops = entry.inverses.clone();
        match self.commit_replay(meta, ops, popped_epoch) {
            Ok(()) => {
                let label = entry.label.clone();
                self.committer.log().push_redo(entry, popped_epoch);
                Ok(Some(label))
            }
            Err(e) => {
                self.committer.log().push_undo_unchanged(entry, popped_epoch);
                Err(e)
            }
        }
    }

    /// Apply a recorded op list through the normal commit path in
    /// `HistoryMode::Replay` — shared by [`Self::undo`] and [`Self::redo`].
    ///
    /// `expected_epoch`: the epoch the entry was POPPED under
    /// (`HistoryLog::pop_undo`). The closure checks it FIRST, before any
    /// `tx.apply`, under the same session lock the writes go through — the
    /// dangerous half of the C-1 residual. `record_commit`'s guard drops a
    /// stale journal line and a stale history entry, but it runs after the
    /// effect phase: by then a stale undo's inverses have already been
    /// APPLIED. Re-opening the same project is routine and keeps every id,
    /// so those inverses apply CLEANLY against the wrong revision instead of
    /// failing loudly. Checking inside the closure is what makes "nothing
    /// applied" true rather than "nothing recorded".
    ///
    /// The mismatch returns `Err` — it never panics (a `transact` closure
    /// must not: `Session::transact` holds the session lock across it) — and
    /// on the `Err` path `transact` rolls back the inverses collected so
    /// far, which here is none: the check is the closure's first statement,
    /// so nothing was applied to roll back.
    fn commit_replay(
        &self,
        meta: op::TxMeta,
        ops: Vec<op::Op>,
        expected_epoch: u64,
    ) -> Result<(), String> {
        self.committer
            .commit_with_rebuild_mode(
                meta,
                |tx| {
                    if tx.epoch() != expected_epoch {
                        return Err(format!(
                            "document changed under undo/redo (epoch {expected_epoch} -> {}) — \
                             nothing applied",
                            tx.epoch()
                        ));
                    }
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

    /// What the version graph currently retains (Plan F Task 7). Reported
    /// alongside the undo depths by Task 11's browsing surface.
    pub fn version_stats(&self) -> vergraph::VersionStats {
        self.committer.log().version_stats()
    }

    pub fn version_overview(&self) -> (vergraph::VersionStats, Vec<vergraph::VersionItem>) {
        self.committer.log().version_overview()
    }

    /// The linear undo ancestry, for the browsing surface's `Undo to here`
    /// affordance and the guard pair it must hand back.
    pub fn undo_path(&self) -> history::UndoPath {
        self.committer.log().undo_path()
    }

    pub fn materialize_version(&self, rev: u64) -> Option<SessionSnapshot> {
        self.committer.log().materialize_version(rev)
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
    pub fn gesture_begin(&self, label: String) -> Result<String, String> {
        let (id, stale) = self.gesture.begin(label, op::Actor::User, self.shared.automation_pass.load(Relaxed));
        if let Some(stale) = stale {
            self.close_gesture(stale);
        }
        Ok(id)
    }

    /// Closes the open gesture, synthesizing and committing its one
    /// history-bound batch (see `close_gesture`). A no-op — not an error —
    /// when nothing is open: `pointerup`/`pointercancel` firing without a
    /// matching `pointerdown`, or a double-fire, must never error the IPC
    /// channel.
    pub fn gesture_end(&self) -> Result<(), String> {
        if let Some(g) = self.gesture.end(None) {
            self.close_gesture(g);
        }
        Ok(())
    }

    /// Close the open gesture only if `id` is the one `gesture_begin`
    /// returned for it. A mismatch is a no-op — not an error — so a late
    /// `end` from a promise continuation cannot close a different gesture
    /// that began while it was awaiting (Track D leftover).
    pub fn gesture_end_id(&self, id: &str) -> Result<(), String> {
        if let Some(g) = self.gesture.end(Some(id)) {
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
    /// `pointerdown`/`pointerup` with no drag in between) produces no
    /// history batch and no `project://changed` emit — nothing changed to
    /// this gesture's coalesced KEYS, so there is nothing for history or
    /// the UI to hear about. It can still owe a deferred PERSIST, though
    /// (see the early-return branch below) — `last` tracks coalesced ops,
    /// not persist flags, and the two can disagree.
    fn close_gesture(&self, gesture: OpenGesture) {
        // I-8: read BEFORE the fields below are moved out of `gesture` by
        // the destructuring that follows.
        let gesture_persist = gesture.persist;
        let gesture_epoch = gesture.epoch;
        if !gesture.live_gain_tracks.is_empty() {
            let release_sample = self.shared.position.load(Relaxed);
            let automation_pass = gesture.automation_pass;
            let final_values = {
                let tables = self.tables.lock();
                gesture
                    .live_gain_tracks
                    .iter()
                    .filter_map(|track_id| {
                        let &slot = tables.slots.get(track_id.as_str())?;
                        let (live, base) = tables.params.gain_pair_linear(slot);
                        let multiplier = crate::audio::rt::relative_gain_multiplier(live, base);
                        Some(crate::audio::engine::AutomationTouchEndpoint {
                            track_id: track_id.clone(),
                            value: multiplier,
                            sample: release_sample,
                            pass: automation_pass,
                        })
                    })
                    .collect()
            };
            self.engine.send(ControlMsg::FinishAutomationTouch(final_values));
        }
        if gesture.last.is_empty() {
            // Fix round 1, Important-1: a folded commit's OWN ops can net
            // to nothing (session.rs's `fold_ops` elides a same-key Set
            // pair whose `from == to` — e.g. a drag that ends back at its
            // starting value within one folded commit) while its
            // `EngineEffect::persist` was still computed `true` — the
            // persist flags are set in `apply_raw` unconditionally on a
            // successful write, before `fold_ops` ever runs. That commit's
            // persist was already merged into `gesture_persist` by
            // `fold_committed`, regardless of whether it left anything in
            // `last`. Returning early here without executing it would
            // silently drop a write this gesture already promised — so it
            // runs even on the "nothing to show history" path.
            if gesture_persist != session::PersistEffect::default() {
                self.committer.execute_persist(&gesture_persist, gesture_epoch);
            }
            return;
        }
        let ops: Vec<op::Op> = gesture.last.into_iter().map(|(_, op)| op).collect();
        let mut inverses: Vec<op::Op> = gesture.baselines.into_iter().map(|(_, op)| op).collect();
        inverses.reverse();
        let meta = op::TxMeta { actor: gesture.actor, run: gesture.run, label: gesture.label, transient: false };
        let (rev, epoch, snapshot) = {
            let session = self.session.lock();
            // Plan F Task 5: NOT a fresh capture — this entry is synthesized
            // from ops that ALREADY ran, each publishing its own image as a
            // transient commit while the gesture was open. The current
            // published image is exactly the document those ops produced, so
            // read it (leaf lock, pointer clone) alongside `rev`/`epoch`
            // under this same session lock rather than re-capturing an
            // identical one.
            let snapshot = session.published_handle().lock().clone();
            (session.rev, session.epoch, snapshot)
        };
        let snapshot_charge =
            snapshot::charge_of(&snapshot, &snapshot::ChangeSet::from_ops(&ops));
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
            // CORRECTION to this field's Task 5 comment, which said 0 to
            // avoid double-counting the transient folds' own captures. The
            // folds are TRANSIENT, so none of them ever reached
            // `record_commit` and none of them has a version node: this
            // synthesized batch is the drag's ONLY node, and a charge of 0
            // would make the graph's budget blind to the image it retains.
            // Charged from the NET ops, which is exactly the own-created
            // work the last fold's capture did.
            snapshot_charge,
            snapshot,
        };
        // Task 17: the direct sink. No drop-window — the gesture is
        // undoable the instant it closes.
        self.committer.log().record_gesture(&committed);
        *self.last_gesture_batch.lock() = Some(committed);

        // I-8: the whole drag's persist, once, here — never once per folded
        // commit. Before the emit, for the same reason `commit_with_rebuild_full`
        // persists before its own emit: the event announces durable truth.
        if gesture_persist != session::PersistEffect::default() {
            self.committer.execute_persist(&gesture_persist, gesture_epoch);
        }

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

    /// Shared session lock. Used by crate-internal modules (MIDI launch)
    /// and by tests that assert on store state around a `commit`.
    /// The RT-shared cell — transport position, play state, sample rate.
    /// Crate-visible for the same reason `session` is: `midi::launch` lives
    /// in another module and implements half of `ControlPlane`.
    pub(crate) fn shared(&self) -> &Arc<SharedRt> {
        &self.shared
    }

    pub(crate) fn session(&self) -> &Arc<Mutex<Session>> {
        &self.session
    }

    /// Test-only view of the published `GraphTables` — the clock table, the
    /// slot map and the scene-clock map a rebuild would have published. Tests
    /// that assert on what a fire DID (which clock is on, which slot reads
    /// it) need to see the same table the fire wrote into.
    ///
    /// Returns the guard, so a caller holding it must not call back into the
    /// control plane: every helper here takes this same lock.
    #[cfg(test)]
    pub(crate) fn tables_for_tests(&self) -> parking_lot::MutexGuard<'_, crate::audio::rt::GraphTables> {
        self.tables.lock()
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

    /// Load `session.modulation` from `dir` and refresh the derived lane
    /// view. After plugin adopt so a v3 file can remigrate with live
    /// param ranges; if the plugin table is still empty,
    /// `load_from_project`'s in-memory migrate (`|_, _| None`) is what
    /// we keep — plugin points stay `domain: native`.
    fn adopt_modulation_from_dir(&self, dir: &Path) {
        let params = self.session.lock().plugins.params.clone();
        let doc = load_modulation_for_open(dir, &params);
        let mut s = self.session.lock();
        s.modulation = doc;
        s.automation.lanes = crate::modulation::compat::lanes_from_doc(&s.modulation);
        // snapshot republish: adopt writes the graph and the derived lane
        // view AFTER the epoch swap's publish. Without this, rebuild and
        // the Plan F equivalence sweep see an empty automation half.
        s.republish_full();
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
            s.players.clear(); // blank-slate reset (V-1) — same reason as tracks/clips above
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
            session.midi.launch_maps = d0.launch_maps;
            crate::midi::launch::runtime().set_maps(Vec::new());
            // Finding 2: a stale `dirty = true` left over from a prior
            // auto-persist failure (M-5) must not survive into this fresh
            // project — otherwise the first midi mutation here persists a
            // BLANK store over this project's real midi (the guard added to
            // `with_synced_store` for finding 1 would otherwise be fooled:
            // `loaded_dir` is set correctly above, so it WOULD persist, and
            // dirty=true is only meant to block resync-from-disk, not writes).
            adopt_midi_dir(&mut session.midi, &dir);
            // snapshot republish: document swap (create) — a non-op writer,
            // so nothing captured this. Full re-derive before the guard
            // drops, so the published image is never behind the live doc.
            session.republish_full();
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
        if let Some(out) = self.midi_out.get() {
            out.adopt_project(&dir);
        }
        self.adopt_modulation_from_dir(&dir);
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
    ///
    /// PROGRESS (startup-progress task): this whole method runs on a
    /// `spawn_blocking` thread the frontend `await`s (`audio::mod.rs`'s
    /// `open_project` command) with nothing on screen otherwise — the
    /// silent stretch can run into seconds (a big `journal.ndjson`, several
    /// LV2 instances each triggering a first-touch `livi::World::new()`
    /// bundle scan, see `plugins::lv2_host`). `emit_progress` fires
    /// `project://open-progress` BEFORE each of the 9 stages so the
    /// frontend can show what is currently happening rather than a frozen
    /// spinner; `log::info!` lines below record the same stages' elapsed
    /// time PERMANENTLY (not scaffolding) so a future slow open is
    /// diagnosable from the log alone, without reproducing it live.
    pub fn open_project_epoch(&self, dir: &Path) -> Result<Project, String> {
        let t_open = std::time::Instant::now();
        // `step`/`index`/`total` are the frontend's exact wire contract —
        // see the event's doc at the call sites below. `total` is fixed at
        // 9 (8 real stages + the final `done` marker), so index and total
        // are both plain literals per call rather than derived.
        let emit_progress = |step: &str, index: u32, label: &str, detail: Option<&str>| {
            (self.emit)(
                "project://open-progress",
                serde_json::json!({
                    "step": step,
                    "index": index,
                    "total": 9,
                    "label": label,
                    "detail": detail,
                }),
            );
        };

        emit_progress("load", 1, "Reading project file", None);
        let t = std::time::Instant::now();
        let (project, dir) = project::load(dir)?;
        // Validate BEFORE mutating any in-memory state (review fix carried
        // over: a project with duplicate track ids must fail cleanly, not
        // after tracks/clips were replaced).
        project::validate(&project)?;
        log::info!("open: project loaded in {} ms", t.elapsed().as_millis());

        // Announced BEFORE the lock is taken, not inside it: `(self.emit)`
        // is an `AppHandle::emit` round-trip, and the session lock is the
        // one every other command contends on — nothing that touches the
        // webview belongs under it. The stage covers the whole locked
        // store-swap + midi adopt below.
        emit_progress("midi", 2, "Adopting MIDI state", None);
        let new_epoch;
        {
            let mut session = self.session.lock();
            session.store.tracks = project.tracks.clone();
            session.store.clips = project.clips.clone();
            // Plan V players (V-1): a separate list from tracks/clips (V-2),
            // swapped in the same place for the same reason — an open must
            // replace the in-memory document wholesale, not merge into it.
            session.store.players = project.players.clone();
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
            //
            // `force: true` — an explicit open must always re-read midi
            // state, the same as the store fields just above
            // (tracks/clips/PLAYERS), which are refreshed unconditionally.
            // `adopt_midi_from_dir`'s same-dir skip exists for the lazy
            // READ paths it used to serve (Task 6's own doc), not for an
            // explicit open of a document just freshly re-read and
            // validated above. Fix round 1, Critical 1: an EARLIER version
            // of this forced the reload by clearing `session.midi.loaded_dir`
            // itself, which ALSO defeated the `Ok(None)` branch's own,
            // unrelated `loaded_dir.is_some()` check inside
            // `adopt_midi_from_dir` — the guard that clears a PREVIOUS
            // project's clips/harmony/launch_maps when the newly-opened one
            // has never had a midi save (every project that has never been
            // midi-saved takes that branch). `force` bypasses only the
            // same-dir cache hit, leaving that check, and the `dirty`
            // guard, exactly as `adopt_midi_from_dir` already has them.
            let t = std::time::Instant::now();
            let bpm = session.store.transport.tempo_bpm;
            crate::midi::adopt_midi_from_dir(&mut session.midi, &dir, bpm, true);
            log::info!("open: midi adopted in {} ms", t.elapsed().as_millis());
            // Plan V — V2 Task 12: retires the launch overlay's `Clip`
            // targets in favor of players, now that both the just-adopted
            // launch maps and the just-swapped players/tracks are in place.
            // In-memory only, like the schema migrations `adopt_midi_from_dir`
            // itself performs (persist.rs's own doc) — the next save writes
            // it back; nothing here forces disk I/O under the session lock.
            let sess = &mut *session;
            let migrated = crate::midi::launch::migrate_clip_targets_to_players(
                &mut sess.midi.launch_maps,
                &sess.midi.clips,
                &sess.store.tracks,
                &mut sess.store.players,
            );
            if migrated > 0 {
                log::info!("launch: migrated {migrated} clip binding(s) to players on open");
                crate::midi::launch::runtime().set_maps(session.midi.launch_maps.clone());
            }
            // snapshot republish: document swap (open) — a non-op writer,
            // so nothing captured this. Full re-derive before the guard
            // drops, so the published image is never behind the live doc.
            session.republish_full();
        }
        // ---- session lock released; host round-trips + rebuild + emit below ----
        // epoch boundary (Task 17): document swap = history root. Undoing
        // across it would apply this project's inverses to a DIFFERENT
        // document (ruling 4), so both stacks are cleared and the journal
        // rotates onto the newly opened project's own file — appending, so
        // that project's earlier sessions stay in its log.
        // Plan F Task 9: the journal now has a reader, and this is its only
        // production call site — DETECTION ONLY (ruling F-8), no auto-apply,
        // no event, no UI. No lock is held here.
        //
        // ORDER IS LOAD-BEARING, and it is NOT the plan's (plan defect #17,
        // fix round 1): this must run BEFORE `epoch_boundary`, which appends
        // this open's own `{"epochEvent":"open","epoch":N}` — with N above
        // every epoch already in the file — to the file being read. After
        // the boundary, the newest epoch holds no batches and the tail is
        // always empty: File▸Open A, edit, File▸Open B, File▸Open A used to
        // report nothing with two unsaved batches sitting on disk. Reading
        // needs neither the boundary nor the adopts below.
        //
        // Progress-wise this reads the WHOLE journal.ndjson (7.6 MB in one
        // observed project) before `epoch_boundary` appends one line to it,
        // so both share the single "journal" stage rather than each getting
        // its own step in the 9-step contract.
        emit_progress("journal", 3, "Checking journal for unsaved changes", None);
        let t = std::time::Instant::now();
        replay::detect_unsaved_tail(&dir);
        self.committer.log().epoch_boundary(&dir, history::EpochEvent::Open, new_epoch);
        log::info!("open: journal checked in {} ms", t.elapsed().as_millis());

        // Plugins is the heaviest stage in practice: every restored instance
        // is instantiated SERIALLY and SYNCHRONOUSLY, and the first LV2 one
        // touches `livi::World::new()` (a full system bundle scan) — see
        // `plugins::state::reactivate_restored`'s doc. The progress callback
        // re-emits the SAME "plugins" stage with a per-instance `detail` so
        // the frontend can show which plugin is loading, not just that the
        // stage is running.
        emit_progress("plugins", 4, "Instantiating plugins", None);
        let t = std::time::Instant::now();
        crate::plugins::state::adopt_open_project_with_progress(&dir, &|done, total, name| {
            emit_progress(
                "plugins",
                4,
                "Instantiating plugins",
                Some(&format!("{name} ({}/{total})", done + 1)),
            );
        });
        log::info!("open: plugins adopted in {} ms", t.elapsed().as_millis());

        emit_progress("automation", 5, "Restoring automation", None);
        let t = std::time::Instant::now();
        crate::plugins::automation::adopt_open_project(&dir);
        log::info!("open: automation adopted in {} ms", t.elapsed().as_millis());

        emit_progress("midiOut", 6, "Reconnecting MIDI outputs", None);
        let t = std::time::Instant::now();
        if let Some(out) = self.midi_out.get() {
            out.adopt_project(&dir);
        }
        log::info!("open: midi outputs adopted in {} ms", t.elapsed().as_millis());

        emit_progress("modulation", 7, "Loading modulation routes", None);
        let t = std::time::Instant::now();
        self.adopt_modulation_from_dir(&dir);
        log::info!("open: modulation adopted in {} ms", t.elapsed().as_millis());

        // `Rebuild` is fire-and-forget (the real media-decode cost lands
        // later, silently, on the engine control thread's `ensure_loaded` —
        // see that method's own `project://media-progress` emits), so this
        // stage's timing is just dispatch cost, not the rebuild itself.
        emit_progress("rebuild", 8, "Rebuilding audio graph", None);
        let t = std::time::Instant::now();
        self.engine.send(ControlMsg::Rebuild);
        log::info!("open: rebuild dispatched in {} ms", t.elapsed().as_millis());

        (self.emit)("project://changed", serde_json::to_value(&project).unwrap_or_default());
        emit_progress("done", 9, "Ready", None);
        log::info!("open: project opened in {} ms total", t_open.elapsed().as_millis());
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
        let persist_gate = self.session.lock().persist_gate.clone();
        let _persist = persist_gate.lock();
        let name = dir
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        let created_at = project::load(dir).ok().and_then(|(p, _)| p.created_at);
        let rate = self.shared.sample_rate.load(Relaxed);
        let position = self.shared.position.load(Relaxed);
        let new_epoch;
        let (project, midi_snapshot, plugin_snapshot, automation_snapshot, modulation_snapshot) = {
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
            // I-1 fix (ruling F-6): the plugin doc + automation lanes are
            // snapshotted under this SAME short lock as the midi snapshot
            // above — the actual writes happen below, after the guard
            // drops (round-2 §4: no disk I/O under the session lock), using
            // the SAME helpers `execute_persist` calls. Before this fix,
            // Save-As wrote project.json + midi only: the new dir got no
            // `plugins[]`/state blobs and no `automation[]`/chunks, so the
            // next COLD OPEN of the Save-As'd project saw nothing on disk
            // and (after Task 1's I-7 adopt-clear fix) actively cleared
            // whatever plugins/automation the session had — a Save-As that
            // silently destroyed both.
            let plugin_snapshot = session.plugin_snapshot();
            let automation_snapshot = session.automation.lanes.clone();
            let modulation_snapshot = session.modulation.clone();
            // snapshot republish: document swap (save-as) — project meta +
            // the epoch bump are non-op writes; republish before the guard
            // drops. (`midi.loaded_dir` above is bookkeeping, outside the
            // equivalence contract — the epoch is what makes this a swap.)
            session.republish_full();
            (project, midi_snapshot, plugin_snapshot, automation_snapshot, modulation_snapshot)
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
            Ok(()) => {
                self.committer.clear_midi_dirty_if_unchanged(&midi_snapshot);
            }
            Err(e) => {
                self.session.lock().midi.dirty = true;
                log::warn!("save_project_as_epoch: persisting midi failed: {e}");
            }
        }
        // I-1 fix: plugin state blobs + `plugins[]`, same helper and
        // `with_host_state: false` reasoning `execute_persist` uses —
        // `pending_state` is already kept current by the op arms that
        // produced it, and Save-As must not round-trip live hosts. A failed
        // write here is degraded, not aborted (project.json + midi already
        // landed) — same `log::warn!`-not-fail policy as `execute_persist`.
        match crate::plugins::state::save_snapshot_into_project(dir, &plugin_snapshot, false) {
            Ok(cleared) if !cleared.is_empty() => {
                // Mirrors execute_persist's post-write re-lock (:581-596):
                // clear `dirty_state` for whichever ids' pending bytes just
                // landed on disk — now through `clear_dirty_state_matching`
                // (M-1, Task 3), which keeps a concurrent `PluginSetState`'s
                // dirty flag set if its bytes moved on since this snapshot.
                self.committer.clear_dirty_state_matching(&cleared, &plugin_snapshot);
            }
            Ok(_) => {}
            Err(e) => log::warn!("save_project_as_epoch: persisting plugin state failed: {e}"),
        }
        // Track F: a v4 write drops `automation[]`. Persist the graph when
        // it has content; if the session still only has Track D lanes
        // (I-1 tests, leftover facade-less state), migrate them first so
        // Save-As cannot stamp an empty `modulation{}` over live lanes.
        let modulation_to_write = if !modulation_snapshot.is_empty() {
            modulation_snapshot
        } else if !automation_snapshot.is_empty() {
            crate::modulation::persist::migrate_v3_lanes(&automation_snapshot, &|_, _| None)
        } else {
            modulation_snapshot
        };
        if let Err(e) = crate::modulation::persist::save_into_project(dir, &modulation_to_write) {
            log::warn!("save_project_as_epoch: persisting modulation failed: {e}");
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
        let persist_gate = self.session.lock().persist_gate.clone();
        let _persist = persist_gate.lock();
        let rate = self.shared.sample_rate.load(Relaxed);
        let position = self.shared.position.load(Relaxed);
        let (project, dir, epoch, midi_snapshot, plugin_snapshot, modulation) = {
            let session = self.session.lock();
            let dir = session.store.project_dir.clone().ok_or("no project open")?;
            let project = project::from_store(&session.store, position, rate)?;
            // M-2 (Task 3, whole-branch review): a prior auto-persist
            // (`execute_persist`) can fail and leave `midi.dirty`/
            // `plugins.dirty_state` set with nothing actually written —
            // Ctrl+S (this fn) is the user's explicit "save now" and, with
            // the journal ON, the mark it records below claims durability
            // for the whole document; it must recover any dirty stores
            // first, not just write `project.json`. Snapshots taken under
            // this SAME lock as the project snapshot above, written below
            // after the lock drops (round-2 §4: no disk I/O under the
            // session lock) — same helpers, same warn-not-fail policy, same
            // `clear_dirty_state_matching` guard `execute_persist` uses.
            // Automation is deliberately NOT covered here: lanes carry no
            // dirty flag today (an automation persist is an all-or-nothing
            // lane write, unlike midi/plugins' incremental dirty tracking),
            // so there is nothing for a failed auto-persist to have left
            // set — a future automation dirty flag would need the same
            // treatment added here.
            let midi_snapshot = session.midi.dirty.then(|| session.midi_snapshot());
            let plugin_snapshot =
                (!session.plugins.dirty_state.is_empty()).then(|| session.plugin_snapshot());
            (project, dir, session.epoch, midi_snapshot, plugin_snapshot, session.modulation.clone())
        };
        project::save(&dir, &project)?;
        let mut flush_ok = true;
        if let Some(m) = midi_snapshot {
            if let Err(e) = crate::midi::persist::save_snapshot_into_project(&dir, &m) {
                log::warn!("save_project_mark: midi persist failed: {e}");
                self.session.lock().midi.dirty = true;
                flush_ok = false;
            } else if !self.committer.clear_midi_dirty_if_unchanged(&m) {
                flush_ok = false;
            }
        }
        if let Some(doc) = plugin_snapshot {
            match crate::plugins::state::save_snapshot_into_project(&dir, &doc, false) {
                Ok(cleared) if !cleared.is_empty() => {
                    if !self.committer.clear_dirty_state_matching(&cleared, &doc) {
                        flush_ok = false;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    log::warn!("save_project_mark: plugins persist failed: {e}");
                    flush_ok = false;
                }
            }
        }
        // epoch boundary: no document swap here (same project, same
        // in-memory content) — so history is NOT cleared and the journal is
        // NOT rotated. Task 17 journals a "save" MARK record instead: it
        // tells a replay where the on-disk snapshot caught up with the log,
        // which is the whole difference between a snapshot mark and an
        // epoch (ruling 4). Position kept exactly here — after
        // `project::save`, before the emit — even though the dirty-store
        // flush above may now also have run: a save mark that also flushed
        // dirty stores is still one mark.
        // Task 7: an explicit save is the one-way v4 upgrade. The file
        // stays v3 `automation[]` until this write (or an edit persist).
        // Same migrate-if-lanes-only rule as Save-As: an empty graph must
        // not wipe leftover Track D lanes.
        let modulation_to_write = if !modulation.is_empty() {
            modulation
        } else {
            let lanes = self.session.lock().automation.lanes.clone();
            if lanes.is_empty() {
                crate::modulation::ModulationDoc::default()
            } else {
                crate::modulation::persist::migrate_v3_lanes(&lanes, &|_, _| None)
            }
        };
        if let Err(e) = crate::modulation::persist::save_into_project(&dir, &modulation_to_write) {
            log::warn!("save_project: modulation persist failed: {e}");
            flush_ok = false;
        }
        if flush_ok {
            self.committer.log().snapshot_mark(epoch);
        } else {
            log::warn!("save_project_mark: skipping journal mark — a dirty-store flush failed");
        }
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

        // Zyn upgrade path: PREPARE the three patched instances BEFORE the
        // tracks — all host I/O, all outside any lock/transaction
        // (prepare-outside) — so a failure leaves no half-bound state (None
        // = PolySynth) and touches NO session state at all (Task 10: R-3
        // closed, see `try_seed_zyn_demo_instruments`'s doc).
        let zyn = try_seed_zyn_demo_instruments();
        self.seed_demo_project_commit(zyn)
    }

    /// The commit half of [`Self::seed_demo_project`], split out so tests
    /// can drive it with a hand-built `zyn` fixture (the plugins tests'
    /// `FormatStateBridge`-fixture pattern) instead of a real Zyn host —
    /// `try_seed_zyn_demo_instruments` itself needs a live LV2 world plus
    /// the zynaddsubfx-lv2 plugin, which CI doesn't have.
    ///
    /// Task 7 + Task 10: one commit — 3x add_track_tx, the Zyn rows (if
    /// prepared, via `Op::PluginAdd`/`Op::PluginSetState`) and instrument
    /// bindings, and the 3 demo clips, all through the channel. The demo's
    /// plugin rows are now attributed, undoable in the same step as the
    /// rest of the demo, persisted via `PersistEffect` (no manual save
    /// needed), and cold-replayable from the journal. `persist.project`
    /// (set only by the InstrumentId `Set`s below, same as the pre-Task-7
    /// code's zyn-gated project::save) and `persist.midi` (set
    /// unconditionally by `MidiClipAdd`, same as the pre-Task-7 code's
    /// unconditional `save_into_project`) replace the manual saves;
    /// `commit` also emits `project://changed`, fixing this command's
    /// previously missing event.
    fn seed_demo_project_commit(
        &self,
        zyn: Option<[PreparedZynInstance; 3]>,
    ) -> Result<ProjectSnapshot, String> {
        self.commit(op::TxMeta::system("seed demo project"), |tx| {
            let pad = ops::add_track_tx(tx, Some("Demo Pad".into()), Some("midi".into()))?;
            let lead = ops::add_track_tx(tx, Some("Demo Lead".into()), Some("midi".into()))?;
            let bass = ops::add_track_tx(tx, Some("Demo Bass".into()), Some("midi".into()))?;

            if let Some(prepared) = &zyn {
                for (track_id, p) in
                    [(&pad.id, &prepared[0]), (&lead.id, &prepared[1]), (&bass.id, &prepared[2])]
                {
                    // `index: usize::MAX` — same "append" signal
                    // `plugin_instantiate`'s command path uses; `apply_raw`
                    // clamps it to the live length.
                    tx.apply(op::Op::PluginAdd { row: p.row.clone(), index: usize::MAX })?;
                    if let Some(state) = &p.state {
                        tx.apply(op::Op::PluginSetState {
                            instance: p.row.id.clone(),
                            state: state.clone(),
                        })?;
                    }
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::Track(track_id.clone()),
                        path: op::PropPath::InstrumentId,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(format!("plugin:{}", p.row.id)),
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

/// A Zyn demo instance PREPARED (host instantiate + patch load + captured
/// post-load state) but not yet committed to the document — Task 10's
/// prepare-outside handoff for [`try_seed_zyn_demo_instruments`]. `state` is
/// already APST-encoded (`plugins::state::encode_state`), the exact bytes
/// `Op::PluginSetState`'s arm expects on the wire.
///
/// Deviation from the plan's literal interface listing: the plan's sketch
/// also carries a `params: Vec<ParamInfo>` field ("as instantiate_and_activate
/// returned"). Dropped here — `Op::PluginAdd`'s own arm always folds in a
/// `HostForward::Instantiate` (its doc: "idempotent by construction... the
/// executor's has_instance check no-ops it and re-syncs params"), which is
/// exactly the plan's own "Consumes" note for this task. Carrying a second,
/// unused copy of the same params the host is about to re-supply would be a
/// field nothing reads — worse than the plan's version, not a faithful copy
/// of it.
struct PreparedZynInstance {
    row: crate::plugins::PluginInstanceInfo,
    state: Option<Vec<u8>>,
}

/// Try to PREPARE the three Zyn demo instances (pad / lead / bass), each
/// loaded with a stock bank patch. Returns `None` when anything on the Zyn
/// path is unavailable (plugin not installed, banks missing, no registered
/// plugin registry) — the caller then keeps the PolySynth fallback, so a
/// machine without plugins is never broken. Partial failures roll back (host
/// `unregister_instance` calls only — Task 10: this function touches NO
/// session state, so there are no rows to retract).
///
/// Task 10 (R-3 closed): this function only PREPARES — instantiate + load
/// patch + capture post-load state, all host I/O, all outside any
/// lock/transaction (prepare-outside, same pattern `plugin_instantiate`'s
/// command path uses). The caller (`seed_demo_project`) applies
/// `Op::PluginAdd` + `Op::PluginSetState` per instance inside the demo's one
/// channel transaction, so the demo's instruments are attributed, undoable
/// in the same step as the rest of the demo, persisted via `PersistEffect`,
/// and cold-replayable from the journal.
fn try_seed_zyn_demo_instruments() -> Option<[PreparedZynInstance; 3]> {
    try_seed_zyn_demo_instruments_with(crate::plugins::state::registered_state_bridge().map(|b| b.as_ref()))
}

/// [`try_seed_zyn_demo_instruments`] with an explicit bridge (test
/// injection point — the `plugins::state` module's own `FormatStateBridge`/
/// fake-bridge pattern, same reason `save_snapshot_into_project_with` and
/// `reactivate_restored_with` take one).
fn try_seed_zyn_demo_instruments_with(
    bridge: Option<&dyn crate::plugins::state::HostStateBridge>,
) -> Option<[PreparedZynInstance; 3]> {
    use crate::plugins::{self, patches, state::encode_state};
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
    let mut prepared: Vec<PreparedZynInstance> = Vec::with_capacity(3);
    for patch in &wanted {
        match plugins::instantiate_and_activate(registry, &uid) {
            // `_params`: the fresh instance's real ranges (`Op::PluginAdd`'s
            // own `HostForward::Instantiate` re-derives and writes these
            // back after commit — see `PreparedZynInstance`'s doc).
            Ok((info, _params)) => {
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
                // Capture the post-patch-load state from the LIVE host —
                // this is what makes the following `Op::PluginSetState`'s
                // computed inverse (a self-inverse, since a fresh instance
                // has no `pending_state` yet — see that op's arm doc) an
                // honest reflection of "nothing to undo to" rather than a
                // stale/absent blob.
                let state = bridge.and_then(|b| match b.save_state(&info.id) {
                    Ok(Some(blob)) => Some(encode_state(&info.uid, &blob)),
                    Ok(None) => None,
                    Err(e) => {
                        log::warn!(
                            "seed demo: capturing state for {} failed ({e}); patch stays \
                             host-only for this instance",
                            info.id
                        );
                        None
                    }
                });
                prepared.push(PreparedZynInstance { row: info, state });
            }
            Err(e) => {
                log::warn!("seed demo: Zyn instantiation failed ({e}); PolySynth fallback");
                // No session state to retract (this function touches none):
                // just tear down whatever host instances already succeeded.
                for p in &prepared {
                    if let Some(host) = plugins::lv2_host::try_global() {
                        host.unregister_instance(&p.row.id);
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
    prepared.try_into().ok()
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
            transpose_semitones: 0,
            velocity_offset: 0,
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
            transpose_semitones: 0,
            velocity_offset: 0,
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

/// Move (and optionally resize) a BATCH of clips in one transaction — thin
/// delegate over [`ControlPlane::move_clips`]. The frontend previews a group
/// drag locally and calls this ONCE at gesture end, inside a
/// `gesture_begin`/`gesture_end` boundary, so the whole drag is one undo
/// step. Additive command.
#[tauri::command]
pub fn move_clips(
    placements: Vec<ClipPlacement>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.move_clips(placements, op::TxMeta::user("move clips"))
}

/// Remove an audio clip from its track — thin delegate over
/// [`ControlPlane::remove_clip`], mirroring `move_clip`'s State/ControlPlane
/// access shape. The frontend removes the clip locally on click/keypress and
/// awaits this to persist it; a failed call leaves the clip in the backend
/// (the caller re-adds it locally on error, same convention as `removeTrack`).
#[tauri::command]
pub fn remove_clip(clip_id: String, control: State<'_, Arc<ControlPlane>>) -> Result<(), String> {
    control.remove_clip(&clip_id, op::TxMeta::user("remove clip"))
}

/// Open an audio clip's source file in the OS default app. Additive,
/// read-only: errors if the clip, project, or source is missing, or if
/// `source_path` is not project-relative.
#[allow(deprecated)]
#[tauri::command]
pub fn open_clip_in_external_editor(
    clip_id: String,
    app: tauri::AppHandle,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    use tauri_plugin_shell::ShellExt;
    let path = control.clip_source_abs_path(&clip_id)?;
    app.shell().open(path.to_string_lossy(), None).map_err(|e| e.to_string())
}

// ---- Plan V — V2: players (Task 9). Additive commands; every frozen
// command name is untouched. ----

/// Every player in the document (Plan V — V1). Read-only.
#[tauri::command]
pub fn players_get(
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Vec<crate::audio::player::Player>, String> {
    Ok(control.players())
}

/// Add a player — thin delegate over [`ControlPlane::add_player`], so it
/// goes through `commit` exactly as `add_track` does (undoable, journaled,
/// persisted). `source` omitted is a knobs-only pad (R5).
#[tauri::command]
pub fn player_add(
    name: Option<String>,
    source: Option<crate::audio::player::PlayerSource>,
    raw: Option<bool>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<crate::audio::player::Player, String> {
    control.add_player(
        name,
        source.unwrap_or_default(),
        raw.unwrap_or(false),
        op::TxMeta::user("add player"),
    )
}

/// Remove a player — thin delegate over [`ControlPlane::remove_player`].
#[tauri::command]
pub fn player_remove(
    player_id: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.remove_player(&player_id, op::TxMeta::user("remove player"))
}

/// Fire a player (a pad press) — thin delegate over
/// [`ControlPlane::player_fire`]. Deliberately NOT a commit: a press is a
/// performance, not a document edit, so it takes no undo entry and never
/// rebuilds the graph.
#[tauri::command]
pub fn player_fire(player_id: String, control: State<'_, Arc<ControlPlane>>) -> Result<(), String> {
    control.player_fire(&player_id)
}

/// Cut a player — thin delegate over [`ControlPlane::player_stop`].
#[tauri::command]
pub fn player_stop(player_id: String, control: State<'_, Arc<ControlPlane>>) -> Result<(), String> {
    control.player_stop(&player_id)
}

/// Set a player's trigger mode — thin delegate over
/// [`ControlPlane::set_trigger_mode`]. Fix round 1 (Task 11): the command
/// surface that makes Gate and Loop reachable by anyone but a unit test.
#[tauri::command]
pub fn player_set_trigger_mode(
    player_id: String,
    mode: crate::audio::player::TriggerMode,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.set_trigger_mode(&player_id, mode, op::TxMeta::user("set trigger mode"))
}

/// Set a pad's quantize division (Plan V — V3) — thin delegate over
/// [`ControlPlane::set_quantize`].
#[tauri::command]
pub fn player_set_quantize(
    player_id: String,
    quantize: crate::audio::player::Quantize,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.set_quantize(&player_id, quantize, op::TxMeta::user("set quantize"))
}

/// Set a pad's choke group, `null` for none (Plan V — V3) — thin delegate
/// over [`ControlPlane::set_choke_group`].
#[tauri::command]
pub fn player_set_choke_group(
    player_id: String,
    group: Option<u8>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.set_choke_group(&player_id, group, op::TxMeta::user("set choke group"))
}

/// Set a pad's velocity-to-gain depth, 0..=1 (Plan V — V3) — thin delegate
/// over [`ControlPlane::set_velocity_to_gain`].
#[tauri::command]
pub fn player_set_velocity_to_gain(
    player_id: String,
    depth: f64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    control.set_velocity_to_gain(&player_id, depth, op::TxMeta::user("set velocity depth"))
}

/// Open a gesture boundary (Plan E Task 14 — round-2 inventory row 31, ADR
/// 0003) — thin delegate over [`ControlPlane::gesture_begin`]. Returns the
/// new gesture's id so a later `gesture_end` can refuse to close a
/// different one. Matching mid-gesture `set_track_mix` calls fold
/// backend-side until that matching end.
#[tauri::command]
pub fn gesture_begin(label: String, control: State<'_, Arc<ControlPlane>>) -> Result<String, String> {
    control.gesture_begin(label)
}

/// Close the open gesture boundary (Plan E Task 14) — thin delegate over
/// [`ControlPlane::gesture_end`] / [`ControlPlane::gesture_end_id`].
/// `id` is additive: omitted (or `null`) closes whatever is open (the
/// old contract); a mismatch is a no-op, never an error.
#[tauri::command]
pub fn gesture_end(id: Option<String>, control: State<'_, Arc<ControlPlane>>) -> Result<(), String> {
    match id.as_deref() {
        Some(id) => control.gesture_end_id(id),
        None => control.gesture_end(),
    }
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

/// What a completed [`ControlPlane::undo_to`] walk did.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoToOutcome {
    /// How many undo steps were applied. 0 is a legal, successful answer:
    /// the target was already the head.
    pub steps: usize,
    /// The label of the LAST step undone — what a toast shows ("back to
    /// <label>" is wrong; this is the step the walk ended on). `None` when
    /// `steps == 0`.
    pub label: Option<String>,
}

/// Read-only product surface over Plan F's retained version chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryVersion {
    pub rev: u64,
    pub materialized: bool,
    pub charged_bytes: usize,
    pub label: String,
    pub actor: String,
    /// True when this revision is on the CURRENT linear undo ancestry, i.e.
    /// when `history_undo_to` will accept it. Retained revisions that are
    /// not: the undo commits themselves, anything already undone (now on the
    /// redo stack), and anything dropped by `UNDO_STACK_LIMIT` while the
    /// version graph still keeps it.
    ///
    /// A HINT, not a guarantee: `history_overview` reads this row from the
    /// `versions` mutex and the undo path it is checked against from a
    /// separate, later acquisition of the `history`/`epoch` mutexes (see
    /// that function's doc). A commit landing between the two can retain a
    /// new row or move the path out from under it, so this bit can be one
    /// refresh stale — set (or unset) for a row that no longer matches by
    /// the time the response reaches the renderer. Safe regardless: the
    /// walk itself re-reads a fresh `undo_path()` under `history_gate` and
    /// refuses a `target_rev` that has fallen off it, so a stale bit can
    /// only mis-enable or mis-disable a button for one refresh — it can
    /// never cause `history_undo_to` to apply the wrong edit.
    pub on_undo_path: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryOverview {
    pub undo_depth: usize,
    pub redo_depth: usize,
    pub retained_bytes: usize,
    pub materialized: usize,
    pub replay_only: usize,
    pub versions: Vec<HistoryVersion>,
    /// The document epoch these versions belong to. Handed back verbatim by
    /// `history_undo_to` so the backend can refuse a request that was
    /// composed against a document that has since been replaced.
    pub epoch: u64,
    /// The revision a plain undo would consume next — the head of the undo
    /// ancestry, `None` when there is nothing to undo. The second half of
    /// the guard pair. NOT the live `Session::rev`: transient commits
    /// (transport play/stop, gesture folds) move that without touching the
    /// undo stack.
    pub head_rev: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryVersionDetail {
    pub rev: u64,
    pub project_name: Option<String>,
    pub track_count: usize,
    pub audio_clip_count: usize,
    pub midi_clip_count: usize,
    pub automation_lane_count: usize,
}

/// TWO SEPARATE LOCK ACQUISITIONS, NOT MERGED: `control.version_overview()`
/// takes `HistoryLog`'s `versions` mutex and releases it; `control.undo_path()`
/// then separately takes `epoch`/`history`. Zipping `on_undo_path` onto the
/// version list therefore pairs two reads from different instants — see
/// [`HistoryVersion::on_undo_path`] for the (benign) consequence. This is
/// deliberate, not an oversight: `control/history.rs`'s module doc makes
/// `history`, `journal` and `versions` a binding invariant that they are
/// NEVER held at the same time (each is a leaf, entered only after the
/// others have been released). Reading `versions` and `undo_path` under one
/// combined hold would mean nesting one inside the other for the first time
/// anywhere in the module, which is the kind of new lock-order edge that
/// wants its own PR and its own argument, not a side effect of adding two
/// display fields to a read-only overview.
#[tauri::command]
pub fn history_overview(control: State<'_, Arc<ControlPlane>>) -> HistoryOverview {
    let (stats, versions) = control.version_overview();
    let (undo_depth, redo_depth) = control.history_depths();
    let path = control.undo_path();
    let on_path: std::collections::HashSet<u64> = path.revs.iter().copied().collect();
    HistoryOverview {
        undo_depth,
        redo_depth,
        retained_bytes: stats.retained_bytes,
        materialized: stats.materialized,
        replay_only: stats.replay_only,
        versions: versions
            .into_iter()
            .map(|v| HistoryVersion {
                on_undo_path: on_path.contains(&v.rev),
                rev: v.rev,
                materialized: v.materialized,
                charged_bytes: v.charged_bytes,
                label: v.label,
                actor: v.actor,
            })
            .collect(),
        epoch: path.epoch,
        head_rev: path.head(),
    }
}

/// Materialization may replay a chain, so keep it off the UI thread.
#[tauri::command]
pub async fn history_version(
    rev: u64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Option<HistoryVersionDetail>, String> {
    let cp = control.inner().clone();
    let detail = tauri::async_runtime::spawn_blocking(move || {
        cp.materialize_version(rev).map(|snapshot| HistoryVersionDetail {
            rev: snapshot.rev,
            project_name: snapshot.project_name.clone(),
            track_count: snapshot.tracks.len(),
            audio_clip_count: snapshot.clips.len(),
            midi_clip_count: snapshot.midi.clips.len(),
            automation_lane_count: snapshot.automation.len(),
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(detail)
}

/// Undo the most recent history step (Plan E Task 17) — thin delegate over
/// [`ControlPlane::undo`]. Additive command.
///
/// I-6 — ASYNC ON PURPOSE, exactly like `seed_demo_project` below and for
/// the same reason: a sync `#[tauri::command]` runs on the MAIN thread, and
/// on Linux the WebKitGTK webview shares the GTK main loop. An undo is a
/// full commit and can be arbitrarily heavy — undoing a `PluginRemove`
/// RE-INSTANTIATES the plugin and reloads its state (the seconds-long Zyn
/// case) — so running it there freezes the window for the whole duration.
/// `spawn_blocking` moves it off both the main thread and the async runtime.
///
/// ASYNC IS INVISIBLE ON THE WIRE: the command name, its (empty) payload and
/// the [`HistoryStep`] it resolves to are byte-identical; the frontend's
/// `invoke` was already promise-based.
#[tauri::command]
pub async fn undo(control: State<'_, Arc<ControlPlane>>) -> Result<HistoryStep, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let label = cp.undo()?;
        let (undo_depth, redo_depth) = cp.history_depths();
        Ok(HistoryStep { label, undo_depth, redo_depth })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Redo the most recently undone step (Plan E Task 17) — thin delegate over
/// [`ControlPlane::redo`]. Additive command, async for [`undo`]'s reason
/// (I-6) and with the same unchanged wire shape.
#[tauri::command]
pub async fn redo(control: State<'_, Arc<ControlPlane>>) -> Result<HistoryStep, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let label = cp.redo()?;
        let (undo_depth, redo_depth) = cp.history_depths();
        Ok(HistoryStep { label, undo_depth, redo_depth })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Walk the undo ancestry back to `target_rev` (Plan F carry-forward (e)) —
/// thin delegate over [`ControlPlane::undo_to`]. Additive command; `undo`
/// and `redo` are untouched.
///
/// `expected_epoch` / `expected_head_rev` are the values the caller read
/// from [`history_overview`]. The backend refuses the walk if either has
/// moved — the renderer never decides that, it only reports what it saw.
///
/// Async for [`undo`]'s reason (I-6), and more so: this is N undos, any one
/// of which can re-instantiate a plugin.
#[tauri::command]
pub async fn history_undo_to(
    target_rev: u64,
    expected_epoch: u64,
    expected_head_rev: Option<u64>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<UndoToOutcome, String> {
    let cp = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || cp.undo_to(target_rev, expected_epoch, expected_head_rev))
        .await
        .map_err(|e| e.to_string())?
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
    use crate::audio::types::AutomationMode;
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

    use crate::audio::player::TriggerMode;
    use crate::audio::types::testutil::{test_clip, test_track};
    use crate::control::op::testutil::set_gain;

    /// A throwaway per-machine routing file path for a test — every test
    /// that exercises `MidiOut` persistence (directly or through
    /// `ControlPlane`'s route/port/clock methods) MUST feed this into
    /// `MidiOut::set_routing_path_for_test` right after construction, or it
    /// silently reads/overwrites the real developer machine's MIDI routing
    /// config.
    fn test_routing_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "aura-midi-routing-test-{label}-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

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
    pub(crate) fn test_plane_with_tracks(
        ids: &[&str],
    ) -> (ControlPlane, crossbeam_channel::Receiver<ControlMsg>, RecordedEvents) {
        let mut store = Store::default();
        for &id in ids {
            store.tracks.push(test_track(id));
        }
        let session = Arc::new(Mutex::new(Session::new(store, MidiStore::default())));
        let shared = Arc::new(SharedRt::default());
        let tables: SharedGraphTables = Arc::new(Mutex::new(GraphTables {
            send_slots: Default::default(),
            generation: 1,
            params: Arc::new(ParamTable::default()),
            clocks: Arc::new(crate::audio::clock::ClockTable::with_slots_and_clocks(64, 2)),
            scene_clocks: Default::default(),
            player_clocks: Default::default(),
            orphan_clock: None,
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
            Arc::new(crate::control::GestureState::new()),
        );
        (cp, engine_rx, events)
    }

    // ---- Plan V — V2: players (Task 9) ----------------------------------

    /// Publish the `GraphTables` `engine::rebuild` would publish for this
    /// plane's CURRENT document — the slot map, the clock table and the
    /// reserved player range `1 ..= players.len()`.
    ///
    /// [M2]'s argument, extended to players: there is no engine thread
    /// behind `EngineHandle::for_tests`, so nothing ever rebuilds, and
    /// without this every `player_fire` would report "no clock yet" and
    /// every assertion below would pass vacuously.
    fn republish_tables(cp: &ControlPlane) {
        use crate::audio::types::{derive_slots_with_players, mixer_slot_count_with_players};
        let session = cp.session.lock();
        let store = &session.store;
        let slots = derive_slots_with_players(&store.tracks, &store.players);
        let n_slots = mixer_slot_count_with_players(&store.tracks, &store.players);
        let params = Arc::new(ParamTable::with_slots_and_sends(n_slots, 0));
        let clocks = Arc::new(crate::audio::clock::ClockTable::with_slots_clocks_and_players(
            n_slots,
            1 + store.players.len(),
            store.players.len(),
        ));
        let player_clocks: std::collections::HashMap<PlayerId, u32> = store
            .players
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id.clone(), 1 + i as u32))
            .collect();
        for (id, &clock) in player_clocks.iter() {
            // Mirrors `engine::rebuild`: the choke group is a document
            // property that has to be IN the table, because a quantized fire
            // chokes from the audio thread (V-20).
            clocks.set_choke_group(
                clock,
                store.players.iter().find(|p| &p.id == id).and_then(|p| p.choke_group),
            );
            if let Some(&slot) = slots.get(&TrackId::from(id.as_str())) {
                clocks.bind_slot(slot, clock);
            }
        }
        let mut tables = cp.tables.lock();
        let generation = tables.generation + 1;
        *tables = GraphTables {
            generation,
            params,
            clocks,
            scene_clocks: Default::default(),
            player_clocks,
            orphan_clock: None,
            slots,
            send_slots: Default::default(),
        };
    }

    /// One audio track carrying one clip, and no player yet. The clip is the
    /// source every player test below fires.
    fn test_control_plane_with_an_audio_clip() -> ControlPlane {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        cp.session.lock().store.clips.push(test_clip("c1", "t-1"));
        republish_tables(&cp);
        cp
    }

    /// Add a player sourced from clip `c1` and republish, i.e. exactly what
    /// the real engine does after `Op::PlayerAdd` sets `effect.rebuild`.
    fn add_audio_player(cp: &ControlPlane, clip_id: &str, raw: bool) -> String {
        let p = cp
            .add_player(
                Some("PAD".into()),
                crate::audio::player::PlayerSource::AudioClip { clip_id: clip_id.into() },
                raw,
                op::TxMeta::user("add player"),
            )
            .unwrap();
        republish_tables(cp);
        p.id.to_string()
    }

    /// Two audio tracks, each carrying one clip. Two players do NOT need
    /// distinct clips to be independent — `two_players_sound_at_once_on_their_own_clocks`
    /// already fires two off the SAME clip — this exists only because the
    /// retrigger test wants two clearly-distinct players to reason about.
    fn test_control_plane_with_two_audio_clips() -> ControlPlane {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1", "t-2"]);
        cp.session.lock().store.clips.push(test_clip("c1", "t-1"));
        cp.session.lock().store.clips.push(test_clip("c2", "t-2"));
        republish_tables(&cp);
        cp
    }

    /// The V2 gate's first line: a pad fires a WAV while the arrangement
    /// plays, and the arrangement's transport does not move.
    #[test]
    fn firing_an_audio_player_sounds_without_touching_the_transport() {
        let cp = test_control_plane_with_an_audio_clip();
        let player_id = add_audio_player(&cp, "c1", true);
        cp.transport(TransportAction::Seek { position_samples: 96_000 }).unwrap();
        cp.transport(TransportAction::Play).unwrap();

        cp.player_fire(&player_id).unwrap();

        let clock = cp.player_clock_for(&player_id).expect("the player has a clock");
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(clock), "the pad is sounding");
        assert_eq!(
            tables.clocks.playhead(tables.slots[&TrackId::from(player_id.as_str())], 96_000, false).pos,
            0,
            "on ITS OWN playhead, at 0 — not at the transport's 96000"
        );
        drop(tables);
        assert_eq!(
            cp.transport_state().position_samples, 96_000,
            "the arrangement kept rolling from where it was"
        );
    }

    /// V-4, the whole reason the overlay was replaced: two pads at once.
    #[test]
    fn two_players_sound_at_once_on_their_own_clocks() {
        let cp = test_control_plane_with_an_audio_clip();
        let a = add_audio_player(&cp, "c1", true);
        let b = add_audio_player(&cp, "c1", false);

        cp.player_fire(&a).unwrap();
        cp.player_fire(&b).unwrap();

        let ca = cp.player_clock_for(&a).unwrap();
        let cb = cp.player_clock_for(&b).unwrap();
        assert_ne!(ca, cb, "each player owns a clock");
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(ca) && tables.clocks.is_on(cb));

        // ...and cutting one leaves the other sounding.
        drop(tables);
        cp.player_stop(&a).unwrap();
        let tables = cp.tables_for_tests();
        assert!(!tables.clocks.is_on(ca));
        assert!(tables.clocks.is_on(cb), "a retrigger/cut touches ONE clock");
    }

    /// The reserved range, from the control side: clock 0 is the transport
    /// and is never a player's, however many pads the document has.
    #[test]
    fn players_own_the_reserved_clock_range_starting_after_the_transport() {
        let cp = test_control_plane_with_an_audio_clip();
        let a = add_audio_player(&cp, "c1", false);
        let b = add_audio_player(&cp, "c1", false);
        assert_eq!(cp.player_clock_for(&a), Some(1));
        assert_eq!(cp.player_clock_for(&b), Some(2));
    }

    #[test]
    fn firing_an_unknown_player_is_an_error() {
        let cp = test_control_plane_with_an_audio_clip();
        assert!(cp.player_fire("ghost").unwrap_err().contains("unknown player"));
        assert!(cp.player_stop("ghost").unwrap_err().contains("unknown player"));
    }

    /// A pad whose source clip was deleted must SAY so rather than fire a
    /// clock over a length nobody can compute.
    #[test]
    fn firing_a_player_whose_clip_is_gone_is_an_error() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        cp.session.lock().store.clips.clear();
        assert!(cp.player_fire(&id).unwrap_err().contains("unknown clip"));
    }

    /// R5: a knobs-only pad has nothing to sound. Pressing it is a no-op,
    /// not an error — a control pad that reports a failure on every press is
    /// worse than one that quietly does what it is.
    #[test]
    fn firing_a_sourceless_player_sounds_nothing_and_is_not_an_error() {
        let cp = test_control_plane_with_an_audio_clip();
        let p = cp
            .add_player(None, Default::default(), false, op::TxMeta::user("add"))
            .unwrap();
        republish_tables(&cp);
        cp.player_fire(p.id.as_str()).unwrap();
        let clock = cp.player_clock_for(p.id.as_str()).unwrap();
        assert!(!cp.tables_for_tests().clocks.is_on(clock), "nothing to play");
    }

    /// Task 10: a MIDI pad's press has a LENGTH, which before it was 0 — so
    /// `player_fire`'s zero-length short-circuit swallowed the press and the
    /// pad never sounded at all. The length is the PLACEMENT's, tick-converted
    /// through the tempo map, exactly as the audio arm uses `length_samples`.
    #[test]
    fn firing_a_midi_player_sounds_for_the_clips_tick_converted_length() {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        cp.shared.sample_rate.store(48_000, Relaxed);
        // Bar 5, one bar long. 960 ticks at 120 bpm / 48 kHz is 24000 samples,
        // and the bar-5 origin must not leak into the length.
        cp.session.lock().midi.clips.push(crate::midi::types::MidiClip {
            id: "mc1".into(),
            track_id: "t-1".into(),
            name: "pad".into(),
            timeline_start_ticks: 15_360,
            length_ticks: 960,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t-1"),
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        });
        let p = cp
            .add_player(
                Some("PAD".into()),
                crate::audio::player::PlayerSource::MidiClip {
                    clip_id: "mc1".into(),
                    instrument_id: Some("plugin:i1".into()),
                },
                false,
                op::TxMeta::user("add"),
            )
            .unwrap();
        republish_tables(&cp);

        cp.player_fire(p.id.as_str()).unwrap();
        let clock = cp.player_clock_for(p.id.as_str()).unwrap();
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(clock), "the pad is sounding");
        tables.clocks.advance(23_999);
        assert!(tables.clocks.is_on(clock), "still inside the bar");
        tables.clocks.advance(1);
        assert!(
            !tables.clocks.is_on(clock),
            "one bar at 120 bpm / 48 kHz is 24000 samples, and the bar-5 \
             origin is not part of it"
        );
    }

    /// The same tolerance the audio arm has, for the same reason: a source
    /// the document no longer has is an error the press reports rather than a
    /// clock spun over a length nobody can compute.
    #[test]
    fn firing_a_midi_player_whose_clip_is_gone_is_an_error() {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        cp.shared.sample_rate.store(48_000, Relaxed);
        let p = cp
            .add_player(
                Some("PAD".into()),
                crate::audio::player::PlayerSource::MidiClip {
                    clip_id: "ghost".into(),
                    instrument_id: Some("plugin:i1".into()),
                },
                false,
                op::TxMeta::user("add"),
            )
            .unwrap();
        republish_tables(&cp);
        assert!(cp.player_fire(p.id.as_str()).unwrap_err().contains("unknown midi clip"));
    }

    /// A pad added since the last rebuild has no lane yet. Say so, rather
    /// than fire whichever clock happens to sit at that index — the same
    /// rule `fire_scene` and `ParamTable`'s setters follow.
    #[test]
    fn firing_a_player_the_graph_has_not_seen_yet_reports_it() {
        let cp = test_control_plane_with_an_audio_clip();
        let p = cp
            .add_player(
                None,
                crate::audio::player::PlayerSource::AudioClip { clip_id: "c1".into() },
                true,
                op::TxMeta::user("add"),
            )
            .unwrap(); // deliberately NOT republished
        let err = cp.player_fire(p.id.as_str()).unwrap_err();
        assert!(err.contains("no clock yet"), "got: {err}");
    }

    /// Fix round 1, finding 3. Nothing could stop a looping pad.
    ///
    /// `TriggerMode::Loop` fires a looping clock; `ClockTable::advance` wraps
    /// such a clock instead of ending it, and `any_running()` keeps the output
    /// callback rendering with the transport stopped. So a looping pad sounded
    /// indefinitely, and the only thing in reach that could cut it was
    /// `player_stop(id)` — which no TypeScript calls yet. `stop_launch_overlay`
    /// is Escape / stop-all, and its own doc already claimed to end everything
    /// that was sounding.
    #[test]
    fn stop_all_cuts_a_looping_pad_and_not_only_scenes() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        cp.commit(op::TxMeta::user("loop"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Player(PlayerId::from(id.as_str())),
                path: PropPath::TriggerMode,
                from: serde_json::Value::Null,
                to: serde_json::json!("loop"),
            })
        })
        .unwrap();
        cp.player_fire(&id).unwrap();
        let clock = cp.player_clock_for(&id).unwrap();

        // It really is unstoppable by time alone: run it far past the clip.
        cp.tables_for_tests().clocks.advance(48_000 * 10);
        assert!(cp.tables_for_tests().clocks.is_on(clock), "a loop never ends itself");

        assert!(cp.stop_launch_overlay(), "stop-all reports it cut something");
        assert!(!cp.tables_for_tests().clocks.is_on(clock), "and the pad is silent");
    }

    /// The other side of the same ruling, and it is a ruling: stopping the
    /// TRANSPORT leaves pads sounding. A scene is a region of the arrangement
    /// that a pad borrowed, so the song ending ends it; a player is not in the
    /// song at all (V-2), and cutting a performance because someone stopped
    /// the transport is the deck going quiet mid-set.
    /// See `docs/backlog/plan-v-players.md`.
    #[test]
    fn stopping_the_transport_leaves_a_sounding_pad_alone() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        cp.transport(TransportAction::Play).unwrap();
        cp.player_fire(&id).unwrap();
        let clock = cp.player_clock_for(&id).unwrap();

        cp.transport(TransportAction::Stop).unwrap();
        assert!(
            cp.tables_for_tests().clocks.is_on(clock),
            "the song stopped; the performance did not"
        );
    }

    /// `player_stop` and `player_fire` must agree about WHY a pad cannot be
    /// reached. Before this, `player_stop` had only the clock map to consult,
    /// so a pad the graph had not seen yet — added and not rebuilt, the state
    /// `firing_a_player_the_graph_has_not_seen_yet_reports_it` pins for the
    /// press — reported "unknown player" and sent the reader looking for a
    /// document bug that was not there.
    #[test]
    fn stopping_a_player_the_graph_has_not_seen_yet_says_so_not_unknown() {
        let cp = test_control_plane_with_an_audio_clip();
        let p = cp
            .add_player(
                None,
                crate::audio::player::PlayerSource::AudioClip { clip_id: "c1".into() },
                true,
                op::TxMeta::user("add"),
            )
            .unwrap(); // deliberately NOT republished
        let err = cp.player_stop(p.id.as_str()).unwrap_err();
        assert!(err.contains("no clock yet"), "got: {err}");
        assert!(
            cp.player_stop("ghost").unwrap_err().contains("unknown player"),
            "and a pad that is not in the document at all still says that"
        );
    }

    // ---- Plan V — V3: polyphony (V-18…V-21) ---------------------------

    fn set_player(cp: &ControlPlane, id: &str, path: PropPath, to: serde_json::Value) {
        cp.commit(TxMeta::user("v3"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Player(PlayerId::from(id)),
                path,
                from: serde_json::Value::Null,
                to,
            })
        })
        .unwrap();
    }

    /// The gate's first line: eight pads sounding at once. Each owns its own
    /// clock and its own slot, so this costs eight atomic writes and no
    /// allocation, and nothing about the eighth press differs from the first.
    #[test]
    fn eight_pads_sound_simultaneously() {
        let cp = test_control_plane_with_an_audio_clip();
        let ids: Vec<String> = (0..8).map(|_| add_audio_player(&cp, "c1", true)).collect();
        for id in &ids {
            cp.player_fire(id).unwrap();
        }
        // Clocks resolved BEFORE the guard: `player_clock_for` takes the
        // same lock, and `parking_lot::Mutex` is not reentrant.
        let clocks: Vec<u32> = ids.iter().map(|id| cp.player_clock_for(id).unwrap()).collect();
        let tables = cp.tables_for_tests();
        for (id, &clock) in ids.iter().zip(&clocks) {
            assert!(tables.clocks.is_on(clock), "pad {id} is silent");
        }
        assert_eq!(tables.clocks.voices_in_use(), 8);
    }

    /// V-19. The 33rd press takes the oldest pad's voice, and says so — the
    /// "visible" half of the owner's answer to design §8 question 1.
    #[test]
    fn the_press_past_the_voice_cap_steals_the_oldest_pad_and_announces_it() {
        let (cp, _rx, events) = test_plane_with_tracks(&["t-1"]);
        cp.session.lock().store.clips.push(test_clip("c1", "t-1"));
        republish_tables(&cp);
        let ids: Vec<String> =
            (0..=VOICE_CAP).map(|_| add_audio_player(&cp, "c1", true)).collect();
        for id in ids.iter().take(VOICE_CAP) {
            cp.player_fire(id).unwrap();
        }
        assert_eq!(cp.tables_for_tests().clocks.voices_in_use(), VOICE_CAP);

        cp.player_fire(&ids[VOICE_CAP]).unwrap();
        let oldest = cp.player_clock_for(&ids[0]).unwrap();
        let newest = cp.player_clock_for(&ids[VOICE_CAP]).unwrap();
        let tables = cp.tables_for_tests();
        assert!(!tables.clocks.is_on(oldest), "the pad pressed first gave up its voice");
        assert!(tables.clocks.is_on(newest));
        assert_eq!(tables.clocks.voices_in_use(), VOICE_CAP, "the cap holds");
        drop(tables);

        let stolen: Vec<_> = events
            .lock()
            .iter()
            .filter(|(name, _)| name == "player://stolen")
            .map(|(_, v)| v["playerId"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(stolen, vec![ids[0].clone()], "and the deck was told which pad went");
    }

    /// A pad that is already sounding is RETRIGGERED, not a second voice —
    /// the clock it would take is the one it already holds. Without that
    /// check a full deck would steal a pad to make room for itself, which at
    /// the cap means every press cutting some other pad for no reason.
    #[test]
    fn retriggering_a_sounding_pad_at_the_cap_steals_nothing() {
        let (cp, _rx, events) = test_plane_with_tracks(&["t-1"]);
        cp.session.lock().store.clips.push(test_clip("c1", "t-1"));
        republish_tables(&cp);
        let ids: Vec<String> = (0..VOICE_CAP).map(|_| add_audio_player(&cp, "c1", true)).collect();
        for id in &ids {
            cp.player_fire(id).unwrap();
        }
        cp.player_fire(&ids[5]).unwrap();

        let clocks: Vec<u32> = ids.iter().map(|id| cp.player_clock_for(id).unwrap()).collect();
        let tables = cp.tables_for_tests();
        for (id, &clock) in ids.iter().zip(&clocks) {
            assert!(tables.clocks.is_on(clock), "pad {id} was cut");
        }
        drop(tables);
        assert!(
            !events.lock().iter().any(|(name, _)| name == "player://stolen"),
            "nothing was stolen, so nothing was announced"
        );
    }

    /// V-20's gate: a choke group of two, and the second press cuts the
    /// first inside one block — `ClockTable::stop`, so the cut pad's
    /// `all_notes_off` rides the path an ending already uses.
    #[test]
    fn a_choke_group_cuts_the_pad_that_was_sounding() {
        let cp = test_control_plane_with_an_audio_clip();
        let open = add_audio_player(&cp, "c1", true);
        let closed = add_audio_player(&cp, "c1", true);
        let other = add_audio_player(&cp, "c1", true);
        set_player(&cp, &open, PropPath::ChokeGroup, serde_json::json!(1));
        set_player(&cp, &closed, PropPath::ChokeGroup, serde_json::json!(1));
        republish_tables(&cp); // the group reaches the table at graph build

        cp.player_fire(&open).unwrap();
        cp.player_fire(&other).unwrap();
        cp.player_fire(&closed).unwrap();

        let (c_open, c_closed, c_other) = (
            cp.player_clock_for(&open).unwrap(),
            cp.player_clock_for(&closed).unwrap(),
            cp.player_clock_for(&other).unwrap(),
        );
        let tables = cp.tables_for_tests();
        assert!(!tables.clocks.is_on(c_open), "cut by its group");
        assert!(tables.clocks.is_on(c_closed));
        assert!(tables.clocks.is_on(c_other), "a pad in no group is untouched");
    }

    /// V-18. The press's velocity reaches the clock every slot bound to it
    /// reads, and a press with no velocity is unity — which is what leaves
    /// every V2 caller, and the V-16 ear-check, exactly where they were.
    #[test]
    fn velocity_reaches_the_clock_and_a_plain_press_is_unity() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        let clock = cp.player_clock_for(&id).unwrap();
        let slot = cp.tables_for_tests().slots[&TrackId::from(id.as_str())];


        cp.player_fire(&id).unwrap();
        assert_eq!(cp.tables_for_tests().clocks.playhead(slot, 0, false).gain, 1.0);

        cp.player_fire_with_velocity(&id, 64).unwrap();
        let g = cp.tables_for_tests().clocks.playhead(slot, 0, false).gain;
        assert!((g - (64.0f32 / 127.0).powi(2)).abs() < 1e-6, "got {g}");
        assert!(cp.tables_for_tests().clocks.is_on(clock));
    }

    /// Depth 0 is "a press is a press": the pad sounds at unity however hard
    /// it was hit. It is the one setting that makes a velocity-sensitive
    /// controller behave like V2's mouse click.
    #[test]
    fn a_zero_depth_pad_ignores_velocity() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        set_player(&cp, &id, PropPath::VelocityToGain, serde_json::json!(0.0));
        let slot = cp.tables_for_tests().slots[&TrackId::from(id.as_str())];
        cp.player_fire_with_velocity(&id, 1).unwrap();
        assert_eq!(cp.tables_for_tests().clocks.playhead(slot, 0, false).gain, 1.0);
    }

    /// V-21. With the arrangement running, a quantized press is ARMED, not
    /// taken: nothing sounds until the transport reaches the boundary.
    #[test]
    fn a_quantized_press_waits_for_the_beat_with_the_arrangement_running() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        set_player(&cp, &id, PropPath::Quantize, serde_json::json!("quarter"));
        cp.transport(TransportAction::Play).unwrap();
        // A quarter at 120 bpm / 48 kHz is 24 000 samples; press a third of
        // the way into the second beat.
        cp.shared.position.store(32_000, Relaxed);
        cp.player_fire(&id).unwrap();

        let clock = cp.player_clock_for(&id).unwrap();
        let tables = cp.tables_for_tests();
        assert!(!tables.clocks.is_on(clock), "the press has not sounded yet");
        assert!(tables.clocks.is_pending(clock));
        assert!(!tables.clocks.arm_pending(32_000, 512), "still short of the beat");
        assert!(
            tables.clocks.arm_pending(47_800, 512),
            "the block containing sample 48 000 is the one that starts it"
        );
        assert!(tables.clocks.is_on(clock), "and it lands on the beat, not on the press");
    }

    /// The other half of V-21, and the one a user meets first: with the
    /// transport STOPPED there is no grid, so a quantized pad sounds now.
    /// Waiting would be a pad that never fires.
    #[test]
    fn a_quantized_press_sounds_now_when_the_transport_is_stopped() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        set_player(&cp, &id, PropPath::Quantize, serde_json::json!("bar"));
        cp.player_fire(&id).unwrap();
        let clock = cp.player_clock_for(&id).unwrap();
        assert!(cp.tables_for_tests().clocks.is_on(clock));
        assert!(!cp.tables_for_tests().clocks.is_pending(clock));
    }

    /// Stopping the song drops a press that is still waiting for a beat: the
    /// grid it was queued against has stopped existing, and the alternative
    /// is a pad that fires on its own whenever the song is next played past
    /// that point. A pad already SOUNDING is left alone — that is V-2, and
    /// `stopping_the_transport_leaves_a_sounding_pad_alone` pins it.
    #[test]
    fn stopping_the_transport_drops_a_press_that_was_waiting_for_a_beat() {
        let cp = test_control_plane_with_an_audio_clip();
        let waiting = add_audio_player(&cp, "c1", true);
        let sounding = add_audio_player(&cp, "c1", true);
        set_player(&cp, &waiting, PropPath::Quantize, serde_json::json!("quarter"));
        cp.transport(TransportAction::Play).unwrap();
        cp.shared.position.store(32_000, Relaxed);
        cp.player_fire(&waiting).unwrap();
        cp.player_fire(&sounding).unwrap();

        cp.transport(TransportAction::Stop).unwrap();
        let (c_waiting, c_sounding) = (
            cp.player_clock_for(&waiting).unwrap(),
            cp.player_clock_for(&sounding).unwrap(),
        );
        let tables = cp.tables_for_tests();
        assert!(!tables.clocks.is_pending(c_waiting));
        assert!(!tables.clocks.arm_pending(0, 10_000_000), "nothing is left queued");
        assert!(tables.clocks.is_on(c_sounding), "the song stopped; the performance did not");
    }

    /// The three V3 properties are document state like any other: undoable,
    /// and byte-identical on the way back. `chokeGroup`'s `null` is a real
    /// value, not a missing one — it is how a pad leaves its group.
    #[test]
    fn the_v3_player_properties_undo_to_what_they_were() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        let before = serde_json::to_value(cp.players()).unwrap();

        cp.set_quantize(&id, crate::audio::player::Quantize::Bar, TxMeta::user("q")).unwrap();
        cp.set_choke_group(&id, Some(7), TxMeta::user("c")).unwrap();
        cp.set_velocity_to_gain(&id, 0.25, TxMeta::user("v")).unwrap();
        let p = cp.players().into_iter().find(|p| p.id.as_str() == id).unwrap();
        assert_eq!(p.trigger.quantize, crate::audio::player::Quantize::Bar);
        assert_eq!(p.choke_group, Some(7));
        assert_eq!(p.velocity_to_gain, 0.25);

        cp.undo().unwrap();
        cp.undo().unwrap();
        cp.undo().unwrap();
        assert_eq!(serde_json::to_value(cp.players()).unwrap(), before);

        // Distinct LABELS, because the history merges same-label edits to
        // one entry inside `COALESCE_WINDOW` (the knob-drag rule), and this
        // test is about the inverse, not about that merge.
        cp.set_choke_group(&id, Some(7), TxMeta::user("join")).unwrap();
        cp.set_choke_group(&id, None, TxMeta::user("leave")).unwrap();
        let p = cp.players().into_iter().find(|p| p.id.as_str() == id).unwrap();
        assert_eq!(p.choke_group, None, "null takes the pad out of its group");
        cp.undo().unwrap();
        let p = cp.players().into_iter().find(|p| p.id.as_str() == id).unwrap();
        assert_eq!(p.choke_group, Some(7), "and undo puts it back");
    }

    /// A depth outside 0..=1 is clamped on WRITE, so the inverse an undo
    /// records observes what the document holds rather than what the caller
    /// asked for — the round-trip rule `Gain`'s clamp established. Undoing
    /// a clamped write must land on 0.25, never on the 4.0 nobody stored.
    #[test]
    fn an_out_of_range_velocity_depth_is_clamped_where_the_inverse_can_see_it() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        let depth = |cp: &ControlPlane| {
            cp.players().into_iter().find(|p| p.id.as_str() == id).unwrap().velocity_to_gain
        };
        cp.set_velocity_to_gain(&id, 0.25, TxMeta::user("quarter")).unwrap();
        cp.set_velocity_to_gain(&id, 4.0, TxMeta::user("over")).unwrap();
        assert_eq!(depth(&cp), 1.0);
        cp.undo().unwrap();
        assert_eq!(depth(&cp), 0.25);

        cp.set_velocity_to_gain(&id, -1.0, TxMeta::user("under")).unwrap();
        assert_eq!(depth(&cp), 0.0);
    }

    /// `TriggerMode::Loop` is what `ClockTable::fire`'s `looping` flag is
    /// for: the pad repeats instead of ending, and the wrap carries its own
    /// discontinuity.
    #[test]
    fn a_loop_mode_player_fires_a_looping_clock() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true);
        cp.commit(op::TxMeta::user("loop"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Player(PlayerId::from(id.as_str())),
                path: PropPath::TriggerMode,
                from: serde_json::Value::Null,
                to: serde_json::json!("loop"),
            })
        })
        .unwrap();
        cp.player_fire(&id).unwrap();

        let clock = cp.player_clock_for(&id).unwrap();
        let tables = cp.tables_for_tests();
        // The clip is 48000 long; run past it and it must still be on.
        tables.clocks.advance(48_000 + 512);
        assert!(tables.clocks.is_on(clock), "a loop does not end at the clip's end");
    }

    /// `TriggerMode::OneShot` is the default, and needs no looping flag: a
    /// non-looping clock already ends itself at its own `end` (`advance`,
    /// pinned by `clock::tests::advance_moves_running_clocks_and_stops_one_at_its_end`).
    /// This pins the BEHAVIOUR through `player_fire` at the exact boundary —
    /// on through the last frame, off past it — not merely "off eventually",
    /// which a clock fired for a fraction of the real length would also
    /// satisfy. Sets Loop first so the OneShot write actually changes
    /// something, through the real command (fix round 1, item 4).
    #[test]
    fn one_shot_plays_to_its_end_and_stops_itself() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", false);
        cp.set_trigger_mode(&id, TriggerMode::Loop, op::TxMeta::user("loop")).unwrap();
        cp.set_trigger_mode(&id, TriggerMode::OneShot, op::TxMeta::user("one-shot")).unwrap();
        cp.player_fire(&id).unwrap();
        let clock = cp.player_clock_for(&id).unwrap();
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(clock));
        tables.clocks.advance(47_999); // the clip is 48000 long (test_clip)
        assert!(tables.clocks.is_on(clock), "still inside the clip");
        tables.clocks.advance(1);
        assert!(!tables.clocks.is_on(clock), "one-shot ends AT the clip's end, not before or after");
    }

    /// `Gate` and `OneShot` are byte-identical in the engine (`player_fire`'s
    /// own comment on its `== Loop` check) — the design's "release cuts it"
    /// is a pointerup calling `player_stop`, which the engine cannot see.
    /// So this pins the ONE thing `set_trigger_mode` (fix round 1's new
    /// command) can be asked to prove: the value it writes reaches the
    /// document and reads back, round-tripped through real `TriggerMode`
    /// serde — not that anything about playback changes, because nothing
    /// does. `player_stop` cutting a sounding pad is already pinned by
    /// `two_players_sound_at_once_on_their_own_clocks`.
    #[test]
    fn set_trigger_mode_reaches_the_document() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", false);
        cp.set_trigger_mode(&id, TriggerMode::Gate, op::TxMeta::user("gate")).unwrap();
        assert_eq!(
            cp.players().iter().find(|p| p.id.as_str() == id).unwrap().trigger.mode,
            TriggerMode::Gate,
            "the write reached the document"
        );
    }

    /// A retrigger rewinds THIS player and nothing else — the property the
    /// single overlay this branch replaced could not have (V-4). Both halves
    /// need a PLAYHEAD, not an on/off flag: a no-op retrigger leaves `a`
    /// running from wherever it was, and a press that rewinds EVERY player —
    /// the overlay behaviour itself — leaves `b` on while silently
    /// restarting it.
    #[test]
    fn retriggering_one_player_leaves_another_sounding() {
        let cp = test_control_plane_with_two_audio_clips();
        let a = add_audio_player(&cp, "c1", false);
        let b = add_audio_player(&cp, "c2", false);
        cp.player_fire(&a).unwrap();
        cp.player_fire(&b).unwrap();
        cp.tables_for_tests().clocks.advance(128);
        cp.player_fire(&a).unwrap();

        let cb = cp.player_clock_for(&b).unwrap();
        let tables = cp.tables_for_tests();
        assert!(tables.clocks.is_on(cb), "b untouched by a's retrigger");
        let slot_a = tables.slots[&TrackId::from(a.as_str())];
        assert_eq!(
            tables.clocks.playhead(slot_a, 0, false).pos,
            0,
            "a's second press rewound ITS OWN playhead back to 0"
        );
        let slot_b = tables.slots[&TrackId::from(b.as_str())];
        assert_eq!(
            tables.clocks.playhead(slot_b, 0, false).pos,
            128,
            "b kept playing from where it was — a's press did not rewind it"
        );
    }

    /// §10, and the reason this moved off `effect.rebuild`: a fader drag on
    /// a pad is an atomic write into the live table, not a graph rebuild.
    /// The Track arm has always worked this way; a player owns a mixer slot
    /// now, so it can too.
    #[test]
    fn a_players_gain_and_pan_are_param_writes_not_rebuilds() {
        use crate::audio::mixer::db_to_linear;
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", false); // NOT raw: the strip applies

        let committed = cp
            .commit(op::TxMeta::user("fader"), |tx| {
                tx.apply(Op::Set {
                    object: ObjectRef::Player(PlayerId::from(id.as_str())),
                    path: PropPath::Gain,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(-6.0),
                })
            })
            .unwrap();
        assert!(!committed.effect.rebuild, "a fader drag must not rebuild the graph");

        let tables = cp.tables_for_tests();
        let slot = tables.slots[&TrackId::from(id.as_str())];
        let got = f32::from_bits(tables.params.gain[slot].load(Relaxed));
        assert!(
            (got - db_to_linear(-6.0)).abs() < 1e-4,
            "the write reached the live table (got {got})"
        );
    }

    /// V-6, at the commit layer. A raw player's strip fields are inert: the
    /// compiled node is unity/centre/unmuted whatever the document holds, so
    /// writing them into the table would make the fader take effect on a
    /// strip the compiler says is at unity — V-16's bit-exact pad, broken
    /// until some unrelated edit rebuilt the graph.
    #[test]
    fn a_raw_players_fader_is_document_only_and_never_reaches_the_table() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", true); // raw

        let committed = cp
            .commit(op::TxMeta::user("fader"), |tx| {
                tx.apply(Op::Set {
                    object: ObjectRef::Player(PlayerId::from(id.as_str())),
                    path: PropPath::Gain,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(-6.0),
                })
            })
            .unwrap();
        assert!(committed.effect.param_writes.is_empty(), "raw: nothing to write");
        assert!(!committed.effect.rebuild);
        assert_eq!(
            cp.session.lock().store.players[0].node.gain_db, -6.0,
            "but the document keeps it, so unticking `raw` restores what the user had"
        );

        let tables = cp.tables_for_tests();
        let slot = tables.slots[&TrackId::from(id.as_str())];
        let got = f32::from_bits(tables.params.gain[slot].load(Relaxed));
        assert!((got - 1.0).abs() < 1e-6, "still unity (got {got})");
    }

    /// `Raw` and `PlayerSource` change what the graph COMPILES — the clips,
    /// the inserts, the sends — so they are the two that still rebuild.
    #[test]
    fn raw_and_source_still_rebuild_because_they_change_what_compiles() {
        let cp = test_control_plane_with_an_audio_clip();
        let id = add_audio_player(&cp, "c1", false);
        for (path, to) in [
            (PropPath::Raw, serde_json::json!(true)),
            (
                PropPath::PlayerSource,
                serde_json::json!({ "kind": "none" }),
            ),
        ] {
            let committed = cp
                .commit(op::TxMeta::user("structural"), |tx| {
                    tx.apply(Op::Set {
                        object: ObjectRef::Player(PlayerId::from(id.as_str())),
                        path,
                        from: serde_json::Value::Null,
                        to: to.clone(),
                    })
                })
                .unwrap();
            assert!(committed.effect.rebuild, "{path:?} changes what the graph compiles");
        }
    }

    /// A pad is a document object (V-1): adding one is undoable and durable,
    /// exactly like adding a track.
    #[test]
    fn add_player_is_undoable_and_persists_the_project() {
        let cp = test_control_plane_with_an_audio_clip();
        let committed = cp
            .commit(op::TxMeta::user("add player"), |tx| {
                tx.apply(Op::PlayerAdd {
                    player: crate::audio::player::Player::new(PlayerId::from("p1"), "PAD"),
                    index: 0,
                })
            })
            .unwrap();
        assert!(committed.effect.persist.project, "a pad lives in project.json");
        assert_eq!(cp.players().len(), 1);
        cp.undo().unwrap();
        assert!(cp.players().is_empty(), "one Ctrl+Z removes the pad");
    }

    // ---- lanes UX: arrange_lanes ----------------------------------------

    fn lane(id: &str, group: Option<&str>) -> LaneArrangement {
        LaneArrangement { track_id: id.into(), group: group.map(str::to_string) }
    }

    fn ids_and_groups(cp: &ControlPlane) -> Vec<(String, Option<String>)> {
        cp.session
            .lock()
            .store
            .tracks
            .iter()
            .map(|t| (t.id.to_string(), t.group.clone()))
            .collect()
    }

    /// The gesture this whole command exists for: drag a lane to a new
    /// position AND into a group. It must be ONE transaction, because it is
    /// one user action — two commits would mean two Ctrl+Z presses to undo
    /// what looked like a single drag.
    #[test]
    fn arrange_lanes_reorders_and_regroups_in_one_undoable_step() {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1", "t-2", "t-3"]);
        let before_rev = cp.session.lock().rev;
        cp.arrange_lanes(
            vec![lane("t-3", Some("Drums")), lane("t-1", Some("Drums")), lane("t-2", None)],
            op::TxMeta::user("arrange"),
        )
        .unwrap();
        assert_eq!(
            ids_and_groups(&cp),
            [
                ("t-3".to_string(), Some("Drums".to_string())),
                ("t-1".to_string(), Some("Drums".to_string())),
                ("t-2".to_string(), None),
            ]
        );
        assert_eq!(cp.session.lock().rev, before_rev + 1, "exactly one commit, so exactly one undo");
    }

    /// A pure reorder journals ONE op, not one per track: `arrange_lanes`
    /// diffs each group against store truth and skips the ones that already
    /// match. Pinned on the committed op list because the cost is per-track
    /// — on a 200-lane project the naive version writes 200 no-op `Set`s
    /// into the journal for every drag.
    #[test]
    fn arrange_lanes_emits_group_writes_only_where_the_group_changed() {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1", "t-2", "t-3"]);
        cp.set_track_group("t-1", Some("Keys".into()), op::TxMeta::user("seed")).unwrap();

        // Re-send t-1's EXISTING group while the order changes, and move
        // t-3 into it. Expect: 1 reorder + 1 Set (for t-3) — not 3 Sets.
        let committed = cp
            .commit(op::TxMeta::user("arrange"), |tx| {
                tx.apply(op::Op::TrackReorder {
                    order: vec!["t-2".into(), "t-1".into(), "t-3".into()],
                })
            })
            .unwrap();
        assert_eq!(committed.ops.len(), 1, "the reorder itself is a single op");

        let before = cp.session.lock().rev;
        cp.arrange_lanes(
            vec![lane("t-3", Some("Keys")), lane("t-1", Some("Keys")), lane("t-2", None)],
            op::TxMeta::user("arrange"),
        )
        .unwrap();
        assert_eq!(
            ids_and_groups(&cp),
            [
                ("t-3".to_string(), Some("Keys".to_string())),
                ("t-1".to_string(), Some("Keys".to_string())),
                ("t-2".to_string(), None),
            ],
            "t-3 joined the group, t-1's survived untouched, t-2 stayed ungrouped"
        );
        assert_eq!(cp.session.lock().rev, before + 1, "still one commit");
    }

    /// A bad arrangement must fail atomically — no partial reorder, and no
    /// group label rewritten. `Op::TrackReorder` is applied FIRST precisely
    /// so this fails before any label is touched.
    #[test]
    fn arrange_lanes_rejects_an_incomplete_arrangement_atomically() {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1", "t-2", "t-3"]);
        let before = ids_and_groups(&cp);
        let r = cp.arrange_lanes(
            vec![lane("t-2", Some("Drums")), lane("t-1", Some("Drums"))],
            op::TxMeta::user("arrange"),
        );
        assert!(r.is_err(), "an arrangement missing t-3 must be rejected");
        assert_eq!(ids_and_groups(&cp), before, "no row moved, no group written");
    }

    #[test]
    fn set_track_name_trims_and_rejects_a_blank_rename() {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        let t = cp.set_track_name("t-1", "  Bass  ".into(), op::TxMeta::user("rename")).unwrap();
        assert_eq!(t.name, "Bass", "the returned row carries the STORED name");
        assert!(cp.set_track_name("t-1", "   ".into(), op::TxMeta::user("rename")).is_err());
        assert_eq!(cp.session.lock().store.tracks[0].name, "Bass", "rejected rename changed nothing");
        assert!(cp.set_track_name("nope", "X".into(), op::TxMeta::user("rename")).is_err());
    }

    #[test]
    fn set_track_group_sets_and_clears() {
        let (cp, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        let t = cp.set_track_group("t-1", Some("Drums".into()), op::TxMeta::user("g")).unwrap();
        assert_eq!(t.group.as_deref(), Some("Drums"));
        let t = cp.set_track_group("t-1", None, op::TxMeta::user("g")).unwrap();
        assert_eq!(t.group, None);
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
            send_slots: Default::default(),
            generation: 1,
            clocks: Arc::new(crate::audio::clock::ClockTable::with_slots_and_clocks(64, 2)),
            scene_clocks: Default::default(),
            player_clocks: Default::default(),
            orphan_clock: None,
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
            Arc::new(crate::control::GestureState::new()),
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
            GraphTables {
                generation: 2,
                params: gen2_params.clone(),
                clocks: Arc::new(crate::audio::clock::ClockTable::with_slots_and_clocks(64, 2)),
                scene_clocks: Default::default(),
                player_clocks: Default::default(),
                orphan_clock: None,
                slots: gen2_slots,
                send_slots: Default::default(),
            };

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
                tx.apply(Op::TrackAdd { track: test_track("t-2"), index: 1, clips: vec![], clip_indices: vec![], automation_clips: vec![], bindings: vec![] })?;
                tx.apply(Op::TrackAdd { track: test_track("t-3"), index: 2, clips: vec![], clip_indices: vec![], automation_clips: vec![], bindings: vec![] })?;
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

    /// The batch's whole reason to exist: audio and MIDI clips move in ONE
    /// transaction (cross-store atomicity, round-2 §4.1) and produce exactly
    /// ONE undo entry — not one per clip.
    #[test]
    fn move_clips_moves_audio_and_midi_in_one_transaction_and_one_history_entry() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd { clip: test_clip("a-1", "t-1"), index: 0 })?;
            tx.apply(Op::ClipAdd { clip: test_clip("a-2", "t-1"), index: 1 })?;
            tx.apply(Op::MidiClipAdd { clip: dummy_midi_clip("t-1"), index: 0 })
        })
        .unwrap();
        let midi_id = cp.session().lock().midi.clips[0].id.to_string();
        let (undo_before, _) = cp.history_depths();

        cp.move_clips(
            vec![
                ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 1_000 },
                ClipPlacement::Audio { clip_id: "a-2".into(), timeline_start_samples: 2_000 },
                ClipPlacement::Midi {
                    clip_id: midi_id.clone(),
                    timeline_start_ticks: 480,
                    length_ticks: None,
                    content_length_ticks: None,
                },
            ],
            TxMeta::user("move clips"),
        )
        .unwrap();

        {
            let s = cp.session().lock();
            assert_eq!(s.store.clips[0].timeline_start_samples, 1_000);
            assert_eq!(s.store.clips[1].timeline_start_samples, 2_000);
            assert_eq!(s.midi.clips[0].timeline_start_ticks, 480);
        }
        let (undo_after, _) = cp.history_depths();
        assert_eq!(undo_after, undo_before + 1, "a group move is ONE undo step");
    }

    /// One Ctrl+Z puts every clip in the batch back — and back to ITS OWN
    /// position. The two clips are seeded at DISTINCT starts on purpose: with
    /// both at 0, an inverse that restored the wrong clip's value would pass,
    /// which is the "length-only assertion" failure mode this track already
    /// shipped once (see the paste-side undo tests, which assert by identity
    /// for the same reason).
    #[test]
    fn move_clips_undo_restores_every_clip_in_the_batch() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a1 = test_clip("a-1", "t-1");
            a1.timeline_start_samples = 1_111;
            let mut a2 = test_clip("a-2", "t-1");
            a2.timeline_start_samples = 2_222;
            tx.apply(Op::ClipAdd { clip: a1, index: 0 })?;
            tx.apply(Op::ClipAdd { clip: a2, index: 1 })
        })
        .unwrap();
        cp.move_clips(
            vec![
                ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 7_000 },
                ClipPlacement::Audio { clip_id: "a-2".into(), timeline_start_samples: 9_000 },
            ],
            TxMeta::user("move clips"),
        )
        .unwrap();
        cp.undo().unwrap();
        let s = cp.session().lock();
        let by_id = |id: &str| {
            s.store.clips.iter().find(|c| c.id == id).expect("clip survived").timeline_start_samples
        };
        assert_eq!(by_id("a-1"), 1_111, "a-1 is back at its OWN start, not a-2's");
        assert_eq!(by_id("a-2"), 2_222, "a-2 is back at its OWN start, not a-1's");
    }

    /// Validate-before-mutate (the `transact`-must-not-panic rule's sibling):
    /// one bad id fails the WHOLE batch, and nothing moved.
    #[test]
    fn move_clips_rejects_an_unknown_id_without_moving_anything() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd { clip: test_clip("a-1", "t-1"), index: 0 })
        })
        .unwrap();
        let (undo_before, _) = cp.history_depths();

        let err = cp
            .move_clips(
                vec![
                    ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 5_000 },
                    ClipPlacement::Audio { clip_id: "nope".into(), timeline_start_samples: 5_000 },
                ],
                TxMeta::user("move clips"),
            )
            .unwrap_err();
        assert!(err.contains("nope"), "the error names the offending id: {err}");
        assert_eq!(cp.session().lock().store.clips[0].timeline_start_samples, 0);
        assert_eq!(cp.history_depths().0, undo_before, "a rejected batch is not a history step");
    }

    /// Inside an open gesture the batch runs TRANSIENT and folds; the ONE
    /// history entry is synthesized by `gesture_end`, and it carries the LAST
    /// position per clip, not the first.
    #[test]
    fn move_clips_folds_into_an_open_gesture_as_one_entry_with_the_last_position() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd { clip: test_clip("a-1", "t-1"), index: 0 })
        })
        .unwrap();
        let (undo_before, _) = cp.history_depths();

        cp.gesture_begin("move clips".into()).unwrap();
        cp.move_clips(
            vec![ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 100 }],
            TxMeta::user("move clips"),
        )
        .unwrap();
        cp.move_clips(
            vec![ClipPlacement::Audio { clip_id: "a-1".into(), timeline_start_samples: 900 }],
            TxMeta::user("move clips"),
        )
        .unwrap();
        assert_eq!(cp.history_depths().0, undo_before, "mid-gesture commits are transient");
        let versions_mid_gesture = cp.version_stats();
        cp.gesture_end().unwrap();

        assert_eq!(cp.history_depths().0, undo_before + 1, "the whole drag is ONE step");
        // ...and ONE version node, charged. The mid-gesture folds are
        // transient, so they leave no node of their own — which is exactly
        // why this synthesized batch's charge may not be zero: it is the
        // only node the whole drag produces (Plan F Task 7).
        let versions = cp.version_stats();
        assert_eq!(
            versions.nodes,
            versions_mid_gesture.nodes + 1,
            "a closed gesture is one version node, and its transient folds were none"
        );
        assert!(
            versions.retained_bytes > versions_mid_gesture.retained_bytes,
            "and it charges the image it retains"
        );
        let batch = cp.take_last_gesture_batch().expect("gesture synthesized a batch");
        assert_eq!(batch.ops.len(), 1, "folded to one net Set: {:?}", batch.ops);
        assert!(
            matches!(&batch.ops[0], Op::Set { to, .. } if to == &serde_json::json!(900u64)),
            "the gesture batch carries the LAST position: {:?}",
            batch.ops[0]
        );
        assert_eq!(cp.session().lock().store.clips[0].timeline_start_samples, 900);
    }

    /// A MIDI entry may carry bounds too (the group loop-length adjust,
    /// Task 7) — and `contentLengthTicks: None` means "unchanged", never
    /// "clear" (scope ruling H).
    #[test]
    fn move_clips_midi_entry_sets_bounds_and_leaves_content_length_alone_when_absent() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = dummy_midi_clip("t-1");
            c.content_length_ticks = Some(1_920);
            tx.apply(Op::MidiClipAdd { clip: c, index: 0 })
        })
        .unwrap();
        let id = cp.session().lock().midi.clips[0].id.to_string();

        cp.move_clips(
            vec![ClipPlacement::Midi {
                clip_id: id,
                timeline_start_ticks: 960,
                length_ticks: Some(7_680),
                content_length_ticks: None,
            }],
            TxMeta::user("resize clips"),
        )
        .unwrap();

        let s = cp.session().lock();
        assert_eq!(s.midi.clips[0].timeline_start_ticks, 960);
        assert_eq!(s.midi.clips[0].length_ticks, 7_680);
        assert_eq!(
            s.midi.clips[0].content_length_ticks,
            Some(1_920),
            "an absent contentLengthTicks means UNCHANGED, not cleared"
        );
    }

    /// An empty batch is a no-op, not an error and not a history step — the
    /// same "nothing to do is not a failure" rule set_mute/set_solo follow.
    #[test]
    fn move_clips_with_no_placements_is_a_no_op() {
        let (cp, _rx, _events) = test_plane_with_tracks(&["t-1"]);
        let (undo_before, _) = cp.history_depths();
        cp.move_clips(vec![], TxMeta::user("move clips")).unwrap();
        assert_eq!(cp.history_depths().0, undo_before);
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

    fn fx_row(id: &str, track: &str) -> crate::plugins::PluginInstanceInfo {
        crate::plugins::PluginInstanceInfo {
            id: id.into(),
            uid: "clap:/x.clap#fx".into(),
            name: "Fx".into(),
            format: "clap".into(),
            status: "stub".into(),
            track_id: Some(track.into()),
        }
    }

    /// Plan G1 Task 3: PluginAdd + InsertAdd in one commit undo as a unit.
    #[test]
    fn insert_add_is_one_transaction_plugin_plus_slot() {
        let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        plane
            .commit(TxMeta::user("insert add"), |tx| {
                tx.apply(Op::PluginAdd { row: fx_row("p-1", "t-1"), index: usize::MAX })?;
                tx.apply(Op::InsertAdd {
                    track_id: "t-1".into(),
                    slot: crate::audio::types::InsertSlot {
                        id: "s-1".into(),
                        instance_id: "p-1".into(),
                        bypassed: false,
                    },
                    index: usize::MAX,
                })
            })
            .unwrap();
        {
            let s = plane.session().lock();
            assert_eq!(
                s.plugins.instances.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
                ["p-1"]
            );
            assert_eq!(s.store.tracks[0].inserts[0].instance_id, "p-1");
        }
        plane.undo().unwrap();
        let s = plane.session().lock();
        assert!(s.plugins.instances.is_empty(), "undo removes the PluginAdd");
        assert!(s.store.tracks[0].inserts.is_empty(), "undo removes the InsertAdd");
    }

    /// Review bug (PR #55): `HostForward::Instantiate` used the instrument
    /// host, so an insert FX never came back live after undo / replay.
    #[test]
    fn plugin_add_of_an_insert_rehosts_as_effect() {
        let fx = crate::plugins::scan_worker::scan_clap_subprocess(
            &crate::plugins::scan::clap_search_paths(),
        )
        .into_iter()
        .find(|d| !d.is_instrument && !d.uid.to_lowercase().contains("cardinal"));
        let Some(fx) = fx else {
            eprintln!("note: no CLAP effect installed; insert re-host skipped");
            return;
        };
        let id = format!("ins-{}", uuid::Uuid::new_v4());
        let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        let mut row = fx_row(&id, "t-1");
        row.uid = fx.uid.clone();
        row.format = "clap".into();
        row.name = fx.name.clone();
        plane
            .commit(TxMeta::user("insert add"), |tx| {
                tx.apply(Op::PluginAdd {
                    row: row.clone(),
                    index: usize::MAX,
                })?;
                tx.apply(Op::InsertAdd {
                    track_id: "t-1".into(),
                    slot: crate::audio::types::InsertSlot {
                        id: "s-1".into(),
                        instance_id: id.clone(),
                        bypassed: false,
                    },
                    index: usize::MAX,
                })
            })
            .unwrap();
        let live = crate::plugins::clap_host::has_instance(&id).unwrap_or(false);
        let _ = crate::plugins::clap_host::remove(&id);
        assert!(
            live,
            "HostForward::Instantiate must host an insert as Effect (uid={})",
            fx.uid
        );
    }

    /// Same hole on project-open: `reactivate_restored` used to call the
    /// instrument instantiate path for every stub clap/lv2 row.
    #[test]
    fn reactivate_restored_hosts_an_insert_as_effect() {
        let fx = crate::plugins::scan_worker::scan_clap_subprocess(
            &crate::plugins::scan::clap_search_paths(),
        )
        .into_iter()
        .find(|d| !d.is_instrument && !d.uid.to_lowercase().contains("cardinal"));
        let Some(fx) = fx else {
            eprintln!("note: no CLAP effect installed; insert reactivate skipped");
            return;
        };
        let id = format!("re-{}", uuid::Uuid::new_v4());
        let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        {
            let mut s = plane.session().lock();
            let mut row = fx_row(&id, "t-1");
            row.uid = fx.uid.clone();
            row.format = "clap".into();
            row.name = fx.name.clone();
            s.plugins.instances.push(row);
            s.store.tracks[0].inserts.push(crate::audio::types::InsertSlot {
                id: "s-1".into(),
                instance_id: id.clone(),
                bypassed: false,
            });
        }
        crate::plugins::state::reactivate_restored(plane.session());
        let live = crate::plugins::clap_host::has_instance(&id).unwrap_or(false);
        let _ = crate::plugins::clap_host::remove(&id);
        assert!(
            live,
            "reactivate_restored must host an insert as Effect (uid={})",
            fx.uid
        );
    }

    /// Plan G1 Task 3 / G-10: remove_track must PluginRemove insert instances.
    #[test]
    fn remove_track_composes_plugin_remove_for_insert_instances() {
        let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        plane
            .commit(TxMeta::user("seed inserts"), |tx| {
                tx.apply(Op::PluginAdd { row: fx_row("p-1", "t-1"), index: usize::MAX })?;
                tx.apply(Op::PluginAdd { row: fx_row("p-2", "t-1"), index: usize::MAX })?;
                tx.apply(Op::InsertAdd {
                    track_id: "t-1".into(),
                    slot: crate::audio::types::InsertSlot {
                        id: "s-1".into(),
                        instance_id: "p-1".into(),
                        bypassed: false,
                    },
                    index: 0,
                })?;
                tx.apply(Op::InsertAdd {
                    track_id: "t-1".into(),
                    slot: crate::audio::types::InsertSlot {
                        id: "s-2".into(),
                        instance_id: "p-2".into(),
                        bypassed: false,
                    },
                    index: 1,
                })
            })
            .unwrap();
        plane.remove_track("t-1", TxMeta::user("remove track")).unwrap();
        assert!(
            plane.session().lock().plugins.instances.is_empty(),
            "G-10: remove_track must PluginRemove insert instances so they do not leak"
        );
        plane.undo().unwrap();
        let s = plane.session().lock();
        assert_eq!(s.store.tracks[0].id.as_str(), "t-1");
        assert_eq!(s.store.tracks[0].inserts.len(), 2, "TrackAdd restores slots");
        assert_eq!(s.plugins.instances.len(), 2, "undo restores the PluginAdds");
    }

    /// Plan G1 Task 3 / R6: effect instances cannot occupy the instrument slot.
    /// Seeds the global scan cache so uid → descriptor.is_instrument is available.
    #[test]
    fn set_track_instrument_rejects_an_effect_instance() {
        // Seed scanned descriptors (same shape as plugins::tests::scanned_registry).
        let seed = vec![
            crate::plugins::PluginDescriptor {
                uid: "lv2:urn:test:synth".into(),
                format: "lv2".into(),
                name: "TestSynth".into(),
                vendor: None,
                version: None,
                path: None,
                is_instrument: true,
                audio_inputs: 0,
                audio_outputs: 2,
                has_note_input: true,
                categories: vec![],
            },
            crate::plugins::PluginDescriptor {
                uid: "clap:/x.clap#fx".into(),
                format: "clap".into(),
                name: "TestFx".into(),
                vendor: None,
                version: None,
                path: Some("/x.clap".into()),
                is_instrument: false,
                audio_inputs: 2,
                audio_outputs: 2,
                has_note_input: false,
                categories: vec![],
            },
        ];
        if let Some(reg) = crate::plugins::registered_registry() {
            reg.lock().scanned = Some(seed);
        } else {
            let reg = Arc::new(parking_lot::Mutex::new(crate::plugins::PluginRegistry {
                scanned: Some(seed),
            }));
            crate::plugins::register_registry(reg);
        }

        let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1"]);
        plane
            .commit(TxMeta::user("seed fx"), |tx| {
                tx.apply(Op::PluginAdd { row: fx_row("p-fx", "t-1"), index: usize::MAX })
            })
            .unwrap();
        let err = plane
            .set_track_instrument("t-1", Some("plugin:p-fx".into()), TxMeta::user("bind fx as inst"))
            .unwrap_err();
        assert!(
            err.contains("effect") || err.contains("insert"),
            "an effect instance cannot occupy the instrument slot, got: {err}"
        );
        assert_eq!(plane.session().lock().store.tracks[0].instrument_id, None);
    }

    /// Controller ruling 2: removing the only soloed track through the
    /// channel must recompute the store-wide `any_solo` RT atomic (old
    /// `remove_track`, audio/mod.rs:378, did this too).
    /// Plan G2: removing a BUS takes every wire pointing at it with it, in
    /// the SAME commit. Left behind, those rows name a destination that no
    /// longer exists: the compiler drops the edge silently, but the send
    /// list on the source track keeps showing it. One undo brings the bus
    /// and its wiring back together.
    #[test]
    fn removing_a_bus_removes_the_sends_that_pointed_at_it() {
        let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1", "b-1"]);
        plane.session().lock().store.tracks[1].kind = "bus".into();
        let send = plane.send_add("t-1", "b-1", TxMeta::user("add send")).unwrap();
        assert_eq!(plane.session().lock().store.tracks[0].sends.len(), 1);

        plane.remove_track("b-1", TxMeta::user("remove bus")).unwrap();
        {
            let session = plane.session().lock();
            assert!(session.store.tracks.iter().all(|t| t.id != "b-1"));
            assert!(
                session.store.tracks[0].sends.is_empty(),
                "the wire goes with the bus, not after it"
            );
        }

        plane.undo().unwrap();
        let session = plane.session().lock();
        assert!(session.store.tracks.iter().any(|t| t.id == "b-1"), "the bus is back");
        assert_eq!(
            session.store.tracks[0].sends.first().map(|s| s.id.clone()),
            Some(send.id),
            "and so is the wire, with its own id"
        );
    }

    /// Removing a bus takes the OUTPUT edges pointing at it too, in the
    /// same commit. Left alone the compiler would fall back to the master
    /// silently, and the document would keep naming a track that is gone.
    #[test]
    fn removing_a_bus_re_routes_tracks_that_output_into_it() {
        let (plane, _rx, _ev) = test_plane_with_tracks(&["t-1", "b-1"]);
        plane.session().lock().store.tracks[1].kind = "bus".into();
        plane.track_set_output("t-1", Some("b-1"), TxMeta::user("route")).unwrap();
        assert_eq!(
            plane.session().lock().store.tracks[0].output.as_ref().map(|o| o.as_str()),
            Some("b-1")
        );

        plane.remove_track("b-1", TxMeta::user("remove bus")).unwrap();
        assert!(
            plane.session().lock().store.tracks[0].output.is_none(),
            "the routed track falls back to the master"
        );

        plane.undo().unwrap();
        let session = plane.session().lock();
        assert!(session.store.tracks.iter().any(|t| t.id == "b-1"), "the bus is back");
        assert_eq!(
            session.store.tracks[0].output.as_ref().map(|o| o.as_str()),
            Some("b-1"),
            "and so is the routing"
        );
    }

    /// A send knob is a MIX change: it must reach the RT table without
    /// scheduling a rebuild (§10 — rebuilding per knob frame at 500 tracks
    /// is what round-2 §2.4 forbids).
    #[test]
    fn send_set_amount_writes_the_param_table_and_queues_no_rebuild() {
        let (plane, engine_rx, _ev) = test_plane_with_tracks(&["t-1", "b-1"]);
        plane.session().lock().store.tracks[1].kind = "bus".into();
        let send = plane.send_add("t-1", "b-1", TxMeta::user("add send")).unwrap();
        // The add IS structural; drain its rebuild so the assertion below is
        // about the knob and nothing else. There is no engine here to
        // republish tables, so publish the send lane by hand — exactly what
        // `engine::rebuild` would have derived for this document.
        while engine_rx.try_recv().is_ok() {}
        {
            let mut tables = plane.tables.lock();
            tables.params = Arc::new(ParamTable::with_slots_and_sends(2, 1));
            tables.send_slots.insert(send.id.clone(), 0);
        }

        plane.send_set_amount("t-1", &send.id, -6.0, TxMeta::user("send amount")).unwrap();
        assert!(
            engine_rx.try_recv().is_err(),
            "NO-REBUILD PIN: a send knob must not queue a graph rebuild"
        );
        let expected = crate::audio::mixer::db_to_linear(-6.0);
        assert!(
            (plane.tables.lock().params.send_amount_linear(0) - expected).abs() < 1e-9,
            "the amount reached the RT table"
        );
    }

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

    /// `remove_clip` runs through the channel — the clip leaves the store
    /// and exactly one `Rebuild` is sent (structural, same as `move_clip`).
    #[test]
    fn remove_clip_goes_through_the_channel_and_rebuilds_once() {
        let (plane, engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        {
            let mut session = plane.session().lock();
            session.store.clips.push(test_clip("c-1", "t-1"));
        }
        plane.remove_clip("c-1", TxMeta::user("remove clip")).unwrap();
        assert!(plane.session().lock().store.clips.iter().all(|c| c.id != "c-1"));
        assert_eq!(engine_rx.try_iter().filter(|m| matches!(m, ControlMsg::Rebuild)).count(), 1);
    }

    #[test]
    fn remove_clip_rejects_unknown_clip_id() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        plane.session().lock().store.clips.push(test_clip("c-1", "t-1"));

        assert!(plane.remove_clip("no-such-clip", TxMeta::user("remove clip")).is_err());
        assert!(plane.session().lock().store.clips.iter().any(|c| c.id == "c-1"), "an unknown-id removal leaves truth untouched");
    }

    /// A throwaway project dir for `clip_source_abs_path` tests — house
    /// idiom (`import.rs`'s own `tmp_parent`): a uuid-tagged dir under the
    /// system temp root, no dev-dep.
    fn tmp_project_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-control-test-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// "Open in external editor" (double-click an audio clip on the
    /// timeline) resolves through `abs_path`: project-relative
    /// `source_path` joined onto the open project's dir.
    #[test]
    fn clip_source_abs_path_resolves_relative_to_the_project_dir() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        let dir = tmp_project_dir("resolve");
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        std::fs::write(dir.join("audio/x.wav"), b"not really audio").unwrap();
        {
            let mut session = plane.session().lock();
            session.store.project_dir = Some(dir.clone());
            session.store.clips.push(test_clip("c-1", "t-1")); // source_path: "audio/x.wav"
        }

        let got = plane.clip_source_abs_path("c-1").unwrap();
        assert_eq!(got, dir.join("audio/x.wav"));
    }

    #[test]
    fn clip_source_abs_path_rejects_unknown_clip_id() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        plane.session().lock().store.clips.push(test_clip("c-1", "t-1"));

        let err = plane.clip_source_abs_path("no-such-clip").unwrap_err();
        assert!(err.contains("no-such-clip"), "the error names the clip: {err}");
    }

    #[test]
    fn clip_source_abs_path_rejects_no_open_project() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        plane.session().lock().store.clips.push(test_clip("c-1", "t-1"));
        // Store::default() carries no project_dir.

        let err = plane.clip_source_abs_path("c-1").unwrap_err();
        assert!(err.contains("no project"), "the error explains why: {err}");
    }

    #[test]
    fn clip_source_abs_path_rejects_a_missing_source_file() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        let dir = tmp_project_dir("missing-source");
        // Deliberately do NOT create `audio/x.wav`.
        {
            let mut session = plane.session().lock();
            session.store.project_dir = Some(dir.clone());
            session.store.clips.push(test_clip("c-1", "t-1"));
        }

        let err = plane.clip_source_abs_path("c-1").unwrap_err();
        assert!(err.contains("missing audio source"), "the error explains why: {err}");
    }

    #[test]
    fn clip_source_abs_path_rejects_an_absolute_source_path() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        let dir = tmp_project_dir("abs-source");
        {
            let mut session = plane.session().lock();
            session.store.project_dir = Some(dir);
            let mut clip = test_clip("c-1", "t-1");
            clip.source_path = "/etc/passwd".into();
            session.store.clips.push(clip);
        }
        let err = plane.clip_source_abs_path("c-1").unwrap_err();
        assert!(err.contains("absolute"), "the error names the escape: {err}");
    }

    #[test]
    fn clip_source_abs_path_rejects_a_dotdot_source_path() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        let dir = tmp_project_dir("dotdot-source");
        {
            let mut session = plane.session().lock();
            session.store.project_dir = Some(dir);
            let mut clip = test_clip("c-1", "t-1");
            clip.source_path = "../secret.wav".into();
            session.store.clips.push(clip);
        }
        let err = plane.clip_source_abs_path("c-1").unwrap_err();
        assert!(err.contains("escapes"), "the error names the escape: {err}");
    }

    /// The inverse (`Op::ClipAdd`) must restore the clip byte-identically —
    /// same precedent as `remove_track_inverse_restores_row_and_clips_
    /// byte_identically` above.
    #[test]
    fn remove_clip_inverse_restores_the_clip_byte_identically() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        let clip = test_clip("c-1", "t-1");
        plane.session().lock().store.clips.push(clip.clone());

        let committed = plane
            .commit(TxMeta::user("remove"), |tx| {
                let c = tx.store().clips.iter().find(|c| c.id == clip.id).cloned().unwrap();
                tx.apply(Op::ClipRemove { clip: c, index: 0 })
            })
            .unwrap();
        assert!(plane.session().lock().store.clips.iter().all(|c| c.id != "c-1"));

        plane
            .commit(TxMeta::user("undo"), |tx| {
                for op in committed.inverses.clone() {
                    tx.apply(op)?;
                }
                Ok(())
            })
            .unwrap();
        let restored = plane.session().lock().store.clips.clone();
        assert_eq!(restored, vec![clip], "clip restored byte-identically");
    }

    fn recording_control_plane() -> (Arc<ControlPlane>, RecordedEvents, EngineHandle) {
        struct NullEvents;
        impl crate::audio::engine::EventSink for NullEvents {
            fn emit(&self, _e: &str, _p: serde_json::Value) {}
        }
        let shared = Arc::new(SharedRt::default());
        let tables = empty_tables();
        let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
        let gesture = Arc::new(crate::control::GestureState::new());
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
            crate::control::testutil::test_committer(&session, &shared, &tables),
            gesture.clone(),
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
            gesture.clone(),
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

    /// Task 2: `set_track_mix` writes `automation_mode` through the new
    /// `PropPath::AutomationMode` arm and returns the as-applied value.
    /// Structural (mirrors `InstrumentId`): changing the mode must trigger a
    /// rebuild, or an already-published `RtGraph::gain_ramps` slot would
    /// keep applying an Off track's lane until the NEXT unrelated rebuild.
    #[test]
    fn set_track_mix_updates_automation_mode_and_triggers_rebuild() {
        let (plane, engine_rx, _events) = test_plane_with_tracks(&["t-1"]);

        let updated = plane
            .set_track_mix(
                vec![TrackMixChange {
                    automation_mode: Some(AutomationMode::Off),
                    ..TrackMixChange::new("t-1")
                }],
                TxMeta::user("set track mix"),
            )
            .unwrap();

        assert_eq!(updated[0].automation_mode, AutomationMode::Off);
        assert!(
            engine_rx.try_iter().any(|m| matches!(m, ControlMsg::Rebuild)),
            "expected a Rebuild message"
        );
    }

    /// Task 2: an `automation_mode` change is a normal undoable `Set` op —
    /// undo restores the prior mode, same as any other track property.
    #[test]
    fn set_track_automation_mode_round_trips_through_undo() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        plane
            .set_track_mix(
                vec![TrackMixChange {
                    automation_mode: Some(AutomationMode::Write),
                    ..TrackMixChange::new("t-1")
                }],
                TxMeta::user("set mode"),
            )
            .unwrap();
        assert_eq!(
            plane.session().lock().store.tracks.iter().find(|t| t.id == "t-1").unwrap().automation_mode,
            AutomationMode::Write
        );
        plane.undo().unwrap();
        assert_eq!(
            plane.session().lock().store.tracks.iter().find(|t| t.id == "t-1").unwrap().automation_mode,
            AutomationMode::Read
        );
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
        // Through the op, not a direct push: that is what production does
        // (the commit lands the stub row and publishes it), and it is what
        // makes the published image EQUAL to the live document on entry —
        // so the only divergence the assertions below can detect is the
        // writeback's own.
        let epoch = {
            let m = cp.session();
            session::Session::transact(m, op::TxMeta::user("add plugin"), |tx| {
                tx.apply(op::Op::PluginAdd { row: row.clone(), index: 0 })
            })
            .expect("plugin add commits");
            let s = m.lock();
            let image = s.published_handle().lock().clone();
            assert_eq!(
                image.plugins.instances[0].status, "stub",
                "precondition: the image starts equal to the live document"
            );
            s.epoch
        };
        let params = vec![crate::plugins::ParamInfo {
            id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
            default: 0.5, value: 0.25, steps: 0, non_automatable: false,
        }];
        cp.committer().apply_instantiate_writeback("inst-1", params, epoch);

        let s = cp.session().lock();
        assert_eq!(s.plugins.instances[0].status, "active");
        assert_eq!(s.plugins.params["inst-1"].len(), 1);
        assert_eq!(s.plugins.params["inst-1"][0].value, 0.25);
        // Plan F Task 5: the writeback is a document write outside
        // `transact`, so it republishes — without this the published slot
        // (which `engine::rebuild` reads) would describe a freshly
        // instantiated plugin as a stub with an empty param mirror until
        // some unrelated commit happened to refresh it.
        let image = s.published_handle().lock().clone();
        assert_eq!(
            image.plugins.instances[0].status, s.plugins.instances[0].status,
            "the instantiate writeback must republish"
        );
        assert_eq!(
            image.plugins.params.get("inst-1"), s.plugins.params.get("inst-1"),
            "including the real param mirror it just installed"
        );
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
            default: 0.5, value: 0.25, steps: 0, non_automatable: false,
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

    #[test]
    fn transport_stop_freezes_playing_before_capturing_the_automation_boundary() {
        let (plane, engine_rx, _events) = test_plane_with_tracks(&["t-1"]);
        plane.shared.position.store(1_234, Relaxed);
        plane.shared.playing.store(true, Relaxed);
        plane.transport(TransportAction::Stop).unwrap();
        assert!(!plane.shared.playing.load(Relaxed));
        assert_eq!(plane.shared.automation_pass.load(Relaxed), 1, "pass rotates before Stop returns");
        let first = engine_rx.try_iter().find_map(|msg| match msg {
            ControlMsg::FinishAutomationStop { at, active_pass, .. } => Some((at, active_pass)),
            _ => None,
        }).expect("active Stop boundary");
        assert_eq!(first, (1_234, true));

        plane.shared.position.store(2_468, Relaxed);
        plane.transport(TransportAction::Stop).unwrap();
        let repeated = engine_rx.try_iter().find_map(|msg| match msg {
            ControlMsg::FinishAutomationStop { at, active_pass, .. } => Some((at, active_pass)),
            _ => None,
        }).expect("repeated Stop boundary");
        assert_eq!(repeated, (2_468, false));
    }

    #[test]
    fn recording_mode_gain_gesture_controls_live_gain_without_moving_the_base_fader() {
        let (plane, engine_rx, events) = test_plane_with_tracks(&["t-1"]);
        {
            let mut session = plane.session().lock();
            let track = &mut session.store.tracks[0];
            track.gain_db = -12.0;
            track.automation_mode = AutomationMode::Touch;
        }
        let slot = *plane.tables.lock().slots.get("t-1").unwrap();
        plane
            .tables
            .lock()
            .params
            .set_gain_linear(slot, crate::audio::mixer::db_to_linear(-12.0));
        plane
            .tables
            .lock()
            .params
            .set_base_gain_linear(slot, crate::audio::mixer::db_to_linear(-12.0));
        plane.shared.playing.store(true, Relaxed);
        plane.shared.position.store(1_234, Relaxed);
        plane.shared.automation_pass.store(7, Relaxed);

        plane.gesture_begin("gain drag".into()).unwrap();
        plane
            .set_track_mix(
                vec![TrackMixChange {
                    gain_db: Some(-6.0),
                    ..TrackMixChange::new("t-1")
                }],
                TxMeta::user("set gain"),
            )
            .unwrap();

        assert_eq!(plane.session().lock().store.tracks[0].gain_db, -12.0);
        assert_eq!(plane.history_depths(), (0, 0));
        assert!(plane.gesture.is_track_gain_touched("t-1"));
        assert!(
            (plane.tables.lock().params.gain_linear(slot)
                - crate::audio::mixer::db_to_linear(-6.0))
                .abs()
                < 1e-6
        );
        assert!(events.lock().iter().all(|(name, _)| name != "project://changed"));

        plane.gesture_end().unwrap();
        let touch_finishes: Vec<_> = engine_rx
            .try_iter()
            .filter_map(|msg| match msg {
                ControlMsg::FinishAutomationTouch(ids) => Some(ids),
                _ => None,
            })
            .collect();
        assert_eq!(touch_finishes.len(), 1);
        assert_eq!(touch_finishes[0][0].track_id, "t-1");
        assert_eq!(touch_finishes[0][0].sample, 1_234);
        assert_eq!(touch_finishes[0][0].pass, 7);
        assert!(
            (touch_finishes[0][0].value - crate::audio::mixer::db_to_linear(6.0)).abs() < 1e-5,
            "-12 dB base to -6 dB live is a ~2x multiplier"
        );
        assert!(plane.take_last_gesture_batch().is_none());
    }

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

    /// Task 7 (Off/Read/Write/Touch/Latch): the read-only "is the user
    /// dragging THIS track's fader right now" query the automation recorder
    /// asks from the engine control thread. It must tell apart: no gesture
    /// open; a gesture open that has folded nothing yet; a gesture that folded
    /// a gain write for THIS track; a gesture that touched something else (a
    /// different track, or a different property of the same track); and a
    /// gesture that has since closed.
    #[test]
    fn is_track_gain_touched_reflects_the_open_gesture() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1", "t-2"]);
        assert!(!plane.gesture.is_track_gain_touched("t-1"), "no gesture open");

        plane.gesture_begin("gain drag".into()).unwrap();
        assert!(
            !plane.gesture.is_track_gain_touched("t-1"),
            "a gesture that has folded nothing yet touches no track"
        );

        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-3.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set gain"),
            )
            .unwrap();
        assert!(plane.gesture.is_track_gain_touched("t-1"));
        assert!(
            !plane.gesture.is_track_gain_touched("t-2"),
            "a different track must not read as touched"
        );

        // Same gesture, a PAN write on t-2: the key's target matches that
        // track but its path does not, so t-2's GAIN is still untouched.
        plane
            .set_track_mix(
                vec![TrackMixChange { pan: Some(0.5), ..TrackMixChange::new("t-2") }],
                TxMeta::user("set pan"),
            )
            .unwrap();
        assert!(
            !plane.gesture.is_track_gain_touched("t-2"),
            "a pan drag is not a gain drag — the PropPath half of the key matters"
        );

        plane.gesture_end().unwrap();
        assert!(
            !plane.gesture.is_track_gain_touched("t-1"),
            "the gesture is closed — nothing is touched any more"
        );
    }

    /// I-8 (Plan E whole-branch review): folding a knob drag into a gesture is
    /// only half the fix — a TRANSIENT commit still executes its full
    /// `EngineEffect`, persist included, so `project.json` was still rewritten
    /// once per rAF batch. Deferring the persist onto the open gesture is what
    /// makes "one drag = one write" true.
    #[test]
    fn a_gesture_defers_its_folded_commits_persist_and_executes_it_once_at_close() {
        let (cp, _events, _engine) = recording_control_plane();
        let dir = std::env::temp_dir().join(format!(
            "aura-gesture-persist-{}-{}", std::process::id(), uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (_p, dir) = crate::audio::project::create(&dir, "Song", 48_000, 120.0).unwrap();
        {
            let mut s = cp.session().lock();
            s.store.project_dir = Some(dir.clone());
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
                track_id: None,
            });
            s.plugins.params.insert("inst-1".into(), vec![crate::plugins::ParamInfo {
                id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
                default: 0.0, value: 0.0, steps: 0, non_automatable: false,
            }]);
        }
        let stored_value = |dir: &std::path::Path| -> Option<f64> {
            let v: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join("project.json")).ok()?).ok()?;
            v.get("plugins")?.as_array()?.iter()
                .find(|r| r["id"] == "inst-1")?
                .get("params")?.as_array()?.iter()
                .find(|p| p["id"] == 7)?
                .get("value")?.as_f64()
        };

        cp.gesture_begin("plugin param drag".into()).unwrap();
        // `commit_transient_for_gesture` is only sound INSIDE
        // `GestureState::commit_transient_and_fold` (its own doc) — that is
        // what folds the persist effect into the open gesture's accumulator
        // (`fold_committed`) and marks `IN_GESTURE_FOLD` for the M-3
        // sanctioned-transient check. `set_track_mix` wraps it the same
        // way; this test drives the same path directly rather than through
        // a mix change.
        for v in [0.25f64, 0.5, 0.75] {
            cp.gesture
                .commit_transient_and_fold(&op::Actor::User, || {
                    cp.commit_transient_for_gesture(op::TxMeta::user("plugin set param"), |tx| {
                        tx.apply(op::Op::Set {
                            object: op::ObjectRef::Plugin("inst-1".into()),
                            path: op::PropPath::Param { index: 7 },
                            from: serde_json::Value::Null,
                            to: serde_json::json!(v),
                        })
                    })
                })
                .unwrap()
                .unwrap();
        }
        assert!(
            stored_value(&dir).is_none() || stored_value(&dir) == Some(0.0),
            "no project.json write may land while the gesture is open"
        );

        cp.gesture_end().unwrap();
        assert_eq!(stored_value(&dir), Some(0.75), "one write, at close, with the LAST value");
        let (undo_depth, _redo) = cp.history_depths();
        assert_eq!(undo_depth, 1, "three folded commits, one undo entry");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// Fix round 1, Important-1: `close_gesture`'s early return (nothing in
    /// `last` to synthesize a history batch from) must still execute a
    /// deferred persist. Constructed honestly through the public API: ONE
    /// folded commit whose two `Set`s on the same (object, path) key net to
    /// NO change (`session.rs`'s `fold_ops` elides a same-key Set pair
    /// whose `from == to` — "wiggle and back" within a single transact) —
    /// its `Committed.ops` end up empty, so nothing lands in `last`/
    /// `baselines`, but `apply_raw` set `effect.persist.plugins = true` on
    /// BOTH `tx.apply` calls unconditionally, before `fold_ops` ever runs,
    /// so `fold_committed` still merges it into the gesture's accumulator.
    /// Observed via a signal the "no ops, no ONE thing changed" path can't
    /// fake: a fresh project's `project.json` carries no `plugins` key at
    /// all until a plugin snapshot is written.
    #[test]
    fn close_gesture_executes_a_deferred_persist_even_when_the_gesture_nets_to_no_ops() {
        let (cp, _events, _engine) = recording_control_plane();
        let dir = std::env::temp_dir().join(format!(
            "aura-gesture-persist-empty-batch-{}-{}", std::process::id(), uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let (_p, dir) = crate::audio::project::create(&dir, "Song", 48_000, 120.0).unwrap();
        {
            let mut s = cp.session().lock();
            s.store.project_dir = Some(dir.clone());
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
                track_id: None,
            });
            s.plugins.params.insert("inst-1".into(), vec![crate::plugins::ParamInfo {
                id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
                default: 0.0, value: 0.0, steps: 0, non_automatable: false,
            }]);
        }
        let has_plugins_key = |dir: &std::path::Path| -> bool {
            let Ok(bytes) = std::fs::read(dir.join("project.json")) else { return false };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { return false };
            v.get("plugins").is_some()
        };
        assert!(!has_plugins_key(&dir), "a fresh project carries no plugins key yet");

        cp.gesture_begin("plugin param wiggle-and-back".into()).unwrap();
        cp.gesture
            .commit_transient_and_fold(&op::Actor::User, || {
                cp.commit_transient_for_gesture(op::TxMeta::user("plugin wiggle"), |tx| {
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::Plugin("inst-1".into()),
                        path: op::PropPath::Param { index: 7 },
                        from: serde_json::Value::Null,
                        to: serde_json::json!(0.5),
                    })?;
                    tx.apply(op::Op::Set {
                        object: op::ObjectRef::Plugin("inst-1".into()),
                        path: op::PropPath::Param { index: 7 },
                        from: serde_json::Value::Null,
                        to: serde_json::json!(0.0), // back to the starting value
                    })
                })
            })
            .unwrap()
            .unwrap();

        cp.gesture_end().unwrap();
        assert!(
            has_plugins_key(&dir),
            "the accumulated persist must run even though this gesture's synthesized batch is empty"
        );
        let (undo_depth, _redo) = cp.history_depths();
        assert_eq!(undo_depth, 0, "a net no-op gesture creates no history entry");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// I-8: a knob drag inside a gesture is ONE undo entry — the per-(instance,
    /// param) `CoalesceKey` already exists for `Op::Set{Plugin, Param}`; what
    /// was missing is that `plugin_set_param` never consulted the gesture at
    /// all (`commit_transient_and_fold` was wired only into `set_track_mix`).
    #[test]
    fn plugin_param_writes_fold_into_an_open_gesture() {
        let (cp, _events, _engine) = recording_control_plane();
        {
            let mut s = cp.session().lock();
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
                track_id: None,
            });
            s.plugins.params.insert("inst-1".into(), vec![crate::plugins::ParamInfo {
                id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
                default: 0.0, value: 0.0, steps: 0, non_automatable: false,
            }]);
        }
        cp.gesture_begin("plugin param drag".into()).unwrap();
        for v in [0.25f64, 0.5, 0.75] {
            cp.set_plugin_params(
                "inst-1",
                &[crate::plugins::ParamChange { id: 7, value: v }],
                op::TxMeta::user("plugin set param"),
            )
            .unwrap();
        }
        assert_eq!(cp.history_depths().0, 0, "nothing reaches history while the gesture is open");
        cp.gesture_end().unwrap();
        assert_eq!(cp.history_depths().0, 1, "the whole drag is one undo entry");

        let batch = cp.take_last_gesture_batch().expect("gesture_end must produce a batch");
        assert_eq!(batch.ops.len(), 1, "coalesced to the LAST write per (instance, param)");
        assert!(matches!(
            &batch.ops[0],
            op::Op::Set { object: op::ObjectRef::Plugin(id), path: op::PropPath::Param { index: 7 }, to, .. }
                if id == "inst-1" && to.as_f64() == Some(0.75)
        ), "{:?}", batch.ops[0]);
        // and the baseline is the value BEFORE the drag, not the previous move
        assert!(matches!(
            &batch.inverses[0],
            op::Op::Set { to, .. } if to.as_f64() == Some(0.0)
        ), "{:?}", batch.inverses[0]);
    }

    /// Outside a gesture, nothing changes: one invoke, one history entry.
    #[test]
    fn plugin_param_writes_outside_a_gesture_stay_one_entry_each() {
        let (cp, _events, _engine) = recording_control_plane();
        {
            let mut s = cp.session().lock();
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
                track_id: None,
            });
            s.plugins.params.insert("inst-1".into(), vec![crate::plugins::ParamInfo {
                id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
                default: 0.0, value: 0.0, steps: 0, non_automatable: false,
            }]);
        }
        cp.set_plugin_params(
            "inst-1",
            &[crate::plugins::ParamChange { id: 7, value: 0.4 }],
            op::TxMeta::user("plugin set param"),
        )
        .unwrap();
        assert_eq!(cp.history_depths().0, 1);
    }

    /// A lane drag is ONE undo entry: successive whole-lane replaces of the
    /// same lane fold by lane id inside an open gesture (the §4.4
    /// value-replacement wrapper is coalescable by construction — what was
    /// missing is that `CoalesceKey::for_op` only ever keyed `Op::Set`).
    #[test]
    fn automation_lane_edits_fold_into_an_open_gesture_by_lane_id() {
        use crate::plugins::automation::{AutomationLane, AutomationPoint};
        let (cp, _events, _engine) = recording_control_plane();
        let mk = |id: &str, v: f32| AutomationLane {
            id: id.into(),
            target_node: "track:t-1".into(),
            param_id: 0,
            points: vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 3840, value: v },
            ],
        };
        // seed the lane outside the gesture so the gesture's baseline is a real
        // previous lane, not "absent"
        cp.set_automation_lane(mk("lane-a", 0.9), op::TxMeta::user("edit automation")).unwrap();
        assert_eq!(cp.history_depths().0, 1);

        cp.gesture_begin("automation drag".into()).unwrap();
        for v in [0.6f32, 0.3, 0.0] {
            cp.set_automation_lane(mk("lane-a", v), op::TxMeta::user("edit automation")).unwrap();
        }
        assert_eq!(cp.history_depths().0, 1, "nothing new reaches history while open");
        cp.gesture_end().unwrap();
        assert_eq!(cp.history_depths().0, 2, "the whole drag adds exactly one entry");

        let batch = cp.take_last_gesture_batch().expect("a batch");
        assert_eq!(batch.ops.len(), 1, "coalesced by lane id: {:?}", batch.ops);
        match &batch.ops[0] {
            op::Op::AutomationSetLane { key, lane: Some(l) } => {
                assert_eq!(key, "lane-a");
                assert_eq!(l.points.last().unwrap().value, 0.0, "last write wins");
            }
            other => panic!("{other:?}"),
        }
        match &batch.inverses[0] {
            op::Op::AutomationSetLane { lane: Some(l), .. } => {
                assert_eq!(l.points.last().unwrap().value, 0.9, "baseline is pre-gesture truth");
            }
            other => panic!("{other:?}"),
        }
    }

    /// Two DIFFERENT lanes edited inside one gesture stay two ops — the key is
    /// the lane id, not "automation".
    #[test]
    fn automation_lane_edits_do_not_coalesce_across_lanes() {
        use crate::plugins::automation::{AutomationLane, AutomationPoint};
        let (cp, _events, _engine) = recording_control_plane();
        let mk = |id: &str, v: f32| AutomationLane {
            id: id.into(),
            target_node: "track:t-1".into(),
            param_id: 0,
            points: vec![AutomationPoint { tick: 0, value: v }],
        };
        cp.gesture_begin("automation multi".into()).unwrap();
        cp.set_automation_lane(mk("lane-a", 0.1), op::TxMeta::user("edit automation")).unwrap();
        cp.set_automation_lane(mk("lane-b", 0.2), op::TxMeta::user("edit automation")).unwrap();
        cp.gesture_end().unwrap();
        let batch = cp.take_last_gesture_batch().expect("a batch");
        assert_eq!(batch.ops.len(), 2, "{:?}", batch.ops);
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

    /// Track D leftover: a late `gesture_end` from a promise continuation
    /// must not close a different gesture that began while it was awaiting.
    /// `gesture_begin` returns the open gesture's id; `gesture_end_id`
    /// no-ops on mismatch and leaves the live drag's batch intact.
    #[test]
    fn gesture_end_with_a_stale_id_does_not_close_the_open_gesture() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&["t-1"]);

        let first = plane.gesture_begin("plugin param drag".into()).unwrap();
        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-3.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set gain"),
            )
            .unwrap();

        // Fader starts while the knob's end is still in flight: begin
        // auto-closes the plugin gesture (its edits are already folded).
        let second = plane.gesture_begin("gain drag".into()).unwrap();
        let closed = plane.take_last_gesture_batch().expect("auto-close commits the stale gesture");
        assert_eq!(closed.meta.label, "plugin param drag");
        assert_ne!(first, second, "each begin mints its own id");

        plane
            .set_track_mix(
                vec![TrackMixChange { gain_db: Some(-6.0), ..TrackMixChange::new("t-1") }],
                TxMeta::user("set gain"),
            )
            .unwrap();

        // The knob's late end must not close the fader.
        plane.gesture_end_id(&first).unwrap();
        assert!(
            plane.take_last_gesture_batch().is_none(),
            "a stale gesture_end must not synthesize a batch"
        );

        plane.gesture_end_id(&second).unwrap();
        let batch = plane
            .take_last_gesture_batch()
            .expect("matching end closes the open gesture");
        assert_eq!(batch.meta.label, "gain drag");
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

    /// After count-in (#38), `transport_snapshot` prefers the RT `playing`
    /// atomic over the store label. The recording guard's empty commit is
    /// not enough on its own: Play must also leave that atomic alone, or
    /// the returned snapshot says `"playing"` while the document still
    /// says `"recording"`.
    #[test]
    fn transport_play_does_not_arm_playing_atomic_while_recording() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&[]);
        plane.session().lock().store.transport.state = "recording".into();
        assert!(!plane.shared.playing.load(Relaxed), "harness starts stopped");
        assert!(!plane.shared.recording.load(Relaxed), "this fixture is store-only");

        let snap = plane.transport(TransportAction::Play).unwrap();

        assert_eq!(
            plane.session().lock().store.transport.state,
            "recording",
            "document guard still holds"
        );
        assert!(
            !plane.shared.playing.load(Relaxed),
            "Play must not arm the RT playing flag while recording owns the label"
        );
        assert!(!plane.shared.recording.load(Relaxed));
        assert_eq!(snap.state, "recording");
    }

    /// A live take already has both RT flags set (`start_recording` writes
    /// them together). Play is a no-op for the document and must not
    /// *clear* `playing` — the take is still rolling.
    #[test]
    fn transport_play_does_not_disarm_an_already_rolling_take() {
        let (plane, _engine_rx, _events) = test_plane_with_tracks(&[]);
        plane.session().lock().store.transport.state = "recording".into();
        plane.shared.recording.store(true, Relaxed);
        plane.shared.playing.store(true, Relaxed);

        let snap = plane.transport(TransportAction::Play).unwrap();

        assert_eq!(snap.state, "recording");
        assert!(plane.shared.recording.load(Relaxed));
        assert!(
            plane.shared.playing.load(Relaxed),
            "Play must leave an already-rolling take's playing flag set"
        );
        assert_eq!(plane.session().lock().store.transport.state, "recording");
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

    /// MIDI-in selection is a config carve-out (ruling 1): attributed, logged,
    /// and it must never produce an op, a rev bump or a journal line — the
    /// exact contract `select_input_and_output_device_send_the_existing_control_msg`
    /// pins for audio devices.
    ///
    /// HAZARD: this test mutates the process-global `crate::audio::midi_in::
    /// hub()` while `cargo test` runs threads in parallel — it is one of
    /// only three tests allowed to touch `hub()` (every other test
    /// constructs `MidiInHub::new()`) and clears the target again before
    /// returning so the process-global does not leak into other tests.
    #[test]
    fn midi_input_track_selection_has_no_op() {
        let (cp, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let t = cp.add_track(Some("Keys".into()), Some("midi".into()), TxMeta::user("add")).unwrap();
        let rev_before = cp.session().lock().rev;
        cp.select_midi_input_track(Some(t.id.to_string()), TxMeta::user("select midi input track")).unwrap();
        assert_eq!(cp.session().lock().rev, rev_before, "config selection is not a document edit");
        assert_eq!(crate::audio::midi_in::hub().target_track().as_deref(), Some(t.id.as_str()));
        // Clear again so this process-global does not leak into other tests.
        cp.select_midi_input_track(None, TxMeta::user("clear")).unwrap();
        assert_eq!(cp.session().lock().rev, rev_before);
    }

    /// A rejected selection (unknown track, or a non-"midi" track) changes
    /// nothing — same HAZARD as above (process-global `hub()`), asserted at
    /// the end instead of cleared, since a rejection never sets it.
    #[test]
    fn midi_input_track_selection_rejects_non_midi_and_unknown_tracks() {
        let (cp, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let audio = cp.add_track(Some("Drums".into()), Some("audio".into()), TxMeta::user("add")).unwrap();
        let err = cp.select_midi_input_track(Some(audio.id.to_string()), TxMeta::user("x")).unwrap_err();
        assert!(err.contains("midi track"), "got {err}");
        let err = cp.select_midi_input_track(Some("ghost".into()), TxMeta::user("x")).unwrap_err();
        assert!(err.contains("unknown track"), "got {err}");
        assert!(crate::audio::midi_in::hub().target_track().is_none(), "a rejected selection changes nothing");
    }

    /// `select_midi_input_port` errors instead of panicking when
    /// `attach_midi_input` was never called — every unit test's
    /// `ControlPlane` (built via `test_plane_with_tracks`, not `lib.rs::run`)
    /// is in exactly this state.
    #[test]
    fn midi_input_port_selection_without_an_attached_manager_errors() {
        let (cp, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let err = cp.select_midi_input_port(Some("nope#0".into()), true, TxMeta::user("x")).unwrap_err();
        assert!(err.contains("midi input"), "got {err}");
    }

    /// MIDI-out selection is a config carve-out (ruling 1, same shape as
    /// MIDI-in): unattached (every unit test's `ControlPlane`), both seams
    /// must return an honest error, never panic and never write to the
    /// document.
    #[test]
    fn midi_output_selection_has_no_op() {
        let (cp, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let rev_before = cp.session().lock().rev;
        // Unattached: an honest error, never a panic and never a document write.
        assert!(cp.open_midi_output_port("x#0".into(), TxMeta::user("x")).is_err());
        assert!(cp.set_midi_output_clock_enabled("x#0".into(), true, TxMeta::user("x")).is_err());
        assert_eq!(cp.session().lock().rev, rev_before);
    }

    /// Deleting the routed track must not leave the MIDI-in target pointing
    /// at an id the document no longer has. HAZARD: process-global `hub()`
    /// (same as the two selection tests above). Every observation is taken
    /// FIRST and the global restored BEFORE the first assertion, so a
    /// failing assertion cannot leak a routing target into sibling tests.
    #[test]
    fn removing_the_routed_track_clears_the_midi_input_target() {
        let (cp, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let keys = cp.add_track(Some("Keys".into()), Some("midi".into()), TxMeta::user("add")).unwrap();
        let other = cp.add_track(Some("Pads".into()), Some("midi".into()), TxMeta::user("add")).unwrap();
        cp.select_midi_input_track(Some(other.id.to_string()), TxMeta::user("route")).unwrap();

        cp.remove_track(keys.id.as_str(), TxMeta::user("delete")).unwrap();
        let after_unrelated = crate::audio::midi_in::hub().target_track();
        cp.remove_track(other.id.as_str(), TxMeta::user("delete")).unwrap();
        let after_routed = crate::audio::midi_in::hub().target_track();
        crate::audio::midi_in::hub().set_target_track(None);

        assert_eq!(
            after_unrelated.as_deref(),
            Some(other.id.as_str()),
            "an unrelated delete must not clear the routing"
        );
        assert_eq!(after_routed, None, "deleting the routed track leaves a dangling target");
    }

    /// Same for note-out (ruling 10's app-config routing): the deleted
    /// track's id must not survive in `midi_output_status`. Needs a real
    /// attached `MidiOut` — no port is opened, so no thread starts.
    #[test]
    fn removing_the_routed_track_clears_the_note_out_target() {
        let (cp, _engine_rx, _events) = test_plane_with_tracks(&[]);
        let out = Arc::new(crate::midi_out::MidiOut::default());
        out.set_routing_path_for_test(test_routing_path("note-out-target"));
        cp.attach_midi_out(Arc::clone(&out));
        let keys = cp.add_track(Some("Keys".into()), Some("midi".into()), TxMeta::user("add")).unwrap();
        let other = cp.add_track(Some("Pads".into()), Some("midi".into()), TxMeta::user("add")).unwrap();
        cp.set_midi_track_route(other.id.to_string(), Some("x#0".into()), None, TxMeta::user("route")).unwrap();

        cp.remove_track(keys.id.as_str(), TxMeta::user("delete")).unwrap();
        assert!(
            out.routes().contains_key(&crate::midi_out::RouteScope::Track(other.id.to_string())),
            "an unrelated delete must not clear the routing"
        );

        cp.remove_track(other.id.as_str(), TxMeta::user("delete")).unwrap();
        assert!(
            !out.routes().contains_key(&crate::midi_out::RouteScope::Track(other.id.to_string())),
            "deleting the routed track leaves a dangling note-out target"
        );
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
        let gesture = Arc::new(crate::control::GestureState::new());
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(Recorder(Arc::clone(&events))),
            crate::control::testutil::test_committer(&session, &shared, &tables),
            gesture.clone(),
        );
        let cp = ControlPlane::new(
            session,
            shared.clone(),
            tables,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
            Box::new(|_, _| {}),
            std::sync::Arc::new(crate::control::HistoryLog::new()),
            gesture.clone(),
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
        let gesture = Arc::new(crate::control::GestureState::new());
        let engine = crate::audio::engine::start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
            crate::control::testutil::test_committer(&session, &shared, &tables),
            gesture.clone(),
        );
        let cp = ControlPlane::new(
            session.clone(),
            shared.clone(),
            tables,
            engine.clone(),
            Arc::new(crate::sidecars::jobs::JobManager::default()),
            Box::new(|_, _| {}),
            std::sync::Arc::new(crate::control::HistoryLog::new()),
            gesture.clone(),
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
                sends: Vec::new(),
                output: None,
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
                inserts: Vec::new(),
                group: None,
                automation_mode: AutomationMode::Read,
            });
        }
        let slots = derive_slots(&store.tracks);
        let (pad, lead, groove) = demo_seed_clips_v2("pad", "lead", "bass", 960);
        assert!(!pad.notes.is_empty() && !lead.notes.is_empty() && !groove.notes.is_empty());
        for n in pad.notes.iter().chain(lead.notes.iter()).chain(groove.notes.iter()) {
            n.validate().unwrap();
        }
        let midi = crate::midi::MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![crate::midi::TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![crate::midi::MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![pad, lead, groove],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        crate::midi::playback::append_from(&crate::control::snapshot::MidiSnapshot::from_store(&midi), &store.tracks, &store.clips, &crate::control::session::PluginDoc::default(), &slots, 48_000, None, &crate::midi_out::RoutedOut::default(), &mut nodes, &mut out);
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
    /// seeder's PREPARE step (Task 10: `try_seed_zyn_demo_instruments`
    /// touches no session state) yields three ACTIVE patched Zyn instances,
    /// and the demo arrangement bound to them renders non-silent audio
    /// through the real graph path. Machines without Zyn skip (the
    /// PolySynth fallback is covered by the test above).
    ///
    /// This test builds its OWN local session from the prepared rows
    /// (mirroring what `Op::PluginAdd`'s arm would do to the document) —
    /// it exercises the prepare half only, not the commit/op path, which is
    /// `the_seed_demo_transaction_journals_its_plugin_rows`'s job.
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
        let Some(prepared) = try_seed_zyn_demo_instruments() else {
            eprintln!("skipping: ZynAddSubFX or its banks not installed");
            return;
        };
        let session = Arc::new(Mutex::new(Session::new(
            crate::audio::types::Store::default(),
            crate::midi::MidiStore::default(),
        )));
        {
            let mut s = session.lock();
            for p in &prepared {
                s.plugins.instances.push(p.row.clone());
                s.plugins.params.entry(p.row.id.clone()).or_default();
            }
        }
        let ids: Vec<String> = prepared.iter().map(|p| p.row.id.clone()).collect();
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
                sends: Vec::new(),
                output: None,
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
                inserts: Vec::new(),
                group: None,
                automation_mode: AutomationMode::Read,
            });
        }
        let slots = derive_slots(&store.tracks);
        let (pad, lead, groove) = demo_seed_clips_v2("pad", "lead", "bass", 960);
        let midi = crate::midi::MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![crate::midi::TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![crate::midi::MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![pad, lead, groove],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        let doc = session.lock().plugin_snapshot();
        crate::midi::playback::append_from(&crate::control::snapshot::MidiSnapshot::from_store(&midi), &store.tracks, &store.clips, &doc, &slots, 48_000, None, &crate::midi_out::RoutedOut::default(), &mut nodes, &mut out);
        assert_eq!(out.len(), 3);
        for (track, inst) in [("pad", &ids[0]), ("lead", &ids[1]), ("bass", &ids[2])] {
            assert_eq!(
                nodes.key_of(track),
                Some(format!("plugin:{inst}@48000#0!active").as_str()),
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

    fn fake_prepared_zyn(tag: &str) -> PreparedZynInstance {
        let row = crate::plugins::PluginInstanceInfo {
            id: format!("fake-zyn-{tag}"),
            uid: "test:fake-zyn".into(),
            name: format!("Fake Zyn {tag}"),
            // Deliberately NOT "lv2"/"clap": `HostForward::Instantiate`'s
            // executor `continue`s for any other format ("non-hosted
            // format: stays 'stub', nothing to sync"), so this fixture
            // never touches a real plugin host.
            format: "test".into(),
            status: "stub".into(),
            track_id: None,
        };
        let blob = crate::plugins::state::StateBlob {
            kind: crate::plugins::state::KIND_OPAQUE,
            data: vec![1, 2, 3, 4],
        };
        let state = Some(crate::plugins::state::encode_state(&row.uid, &blob));
        PreparedZynInstance { row, state }
    }

    /// Task 10 (R-3 closed) — Step 1: the demo seed's Zyn bootstrap commits
    /// through the channel as ops, not a direct session write. Drives
    /// `seed_demo_project_commit` with a hand-built fixture (see
    /// `fake_prepared_zyn`) instead of a real Zyn host — this test is about
    /// the COMMIT half, not the PREPARE half (that's
    /// `seeded_demo_zyn_instruments_bind_and_render`, gated on real Zyn).
    #[test]
    fn the_seed_demo_transaction_journals_its_plugin_rows() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("seed-zyn-journal");
        cp.create_project(parent.to_str().unwrap(), "SeedZynJournal").unwrap();
        let dir = cp.session().lock().store.project_dir.clone().unwrap();

        let zyn = [fake_prepared_zyn("pad"), fake_prepared_zyn("lead"), fake_prepared_zyn("bass")];
        let ids: Vec<String> = zyn.iter().map(|p| p.row.id.clone()).collect();
        cp.seed_demo_project_commit(Some(zyn)).expect("seed commits");

        // (a) the rows landed in the document, and via the op path: three
        // `pluginAdd` + three `pluginSetState` lines in the journal's seed
        // batch.
        {
            let s = cp.session().lock();
            assert_eq!(s.plugins.instances.len(), 3, "all three Zyn rows registered");
            for id in &ids {
                assert!(
                    s.plugins.instances.iter().any(|r| &r.id == id),
                    "instance {id} present"
                );
            }
            let bound = s
                .store
                .tracks
                .iter()
                .filter(|t| t.instrument_id.as_deref().is_some_and(|iid| ids.iter().any(|id| iid == format!("plugin:{id}"))))
                .count();
            assert_eq!(bound, 3, "all three demo tracks bound to their Zyn instance");
        }
        let text = std::fs::read_to_string(dir.join("journal.ndjson")).expect("journal exists");
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad journal line {l:?}: {e}")))
            .collect();
        let seed_batch = lines
            .iter()
            .find(|l| {
                l.get("ops")
                    .and_then(|o| o.as_array())
                    .is_some_and(|ops| ops.iter().any(|op| op["kind"] == "pluginAdd"))
            })
            .expect("a batch carrying pluginAdd ops exists");
        let ops = seed_batch["ops"].as_array().unwrap();
        let plugin_adds = ops.iter().filter(|op| op["kind"] == "pluginAdd").count();
        let plugin_set_states = ops.iter().filter(|op| op["kind"] == "pluginSetState").count();
        assert_eq!(plugin_adds, 3, "three pluginAdd ops in the seed batch: {ops:#?}");
        assert_eq!(plugin_set_states, 3, "three pluginSetState ops in the seed batch: {ops:#?}");

        // (b) ONE undo removes tracks, clips AND plugin rows — the demo is
        // one step.
        let label = cp.undo().unwrap();
        assert!(label.is_some(), "the seed is undoable");
        let s = cp.session().lock();
        assert!(s.store.tracks.is_empty(), "undo removed the demo tracks");
        assert!(s.midi.clips.is_empty(), "undo removed the demo clips");
        // (c) no direct-write remains: plugins is empty after the undo (the
        // grep-level "no direct write" assertion is Task 13's).
        assert!(s.plugins.instances.is_empty(), "undo removed the plugin rows too");

        drop(s);
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// The plain (no Zyn) demo path stays green: when preparation returns
    /// `None`, `seed_demo_project` still seeds the PolySynth demo exactly
    /// as today, with no plugin rows at all. Drives `seed_demo_project_
    /// commit(None)` directly rather than the public `seed_demo_project()`
    /// — `try_seed_zyn_demo_instruments` reads the PROCESS-GLOBAL plugin
    /// registry (`registered_registry`), which another `#[test]` in this
    /// same binary (`seeded_demo_zyn_instruments_bind_and_render`) may have
    /// already registered; on a machine that genuinely has zynaddsubfx-lv2
    /// installed, calling the real `seed_demo_project()` here would then
    /// seed real Zyn rows too — an environment-dependent flake this test
    /// must not have.
    #[test]
    fn seed_demo_project_without_zyn_still_seeds_the_polysynth_demo() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("seed-no-zyn");
        cp.create_project(parent.to_str().unwrap(), "SeedNoZyn").unwrap();

        let snapshot = cp.seed_demo_project_commit(None).expect("seed commits without Zyn");
        assert_eq!(snapshot.tracks.len(), 3, "three demo tracks seeded");
        let s = cp.session().lock();
        assert!(s.plugins.instances.is_empty(), "no plugin rows when preparation is None");
        assert!(
            s.store.tracks.iter().all(|t| t.instrument_id.is_none()),
            "tracks stay unbound (PolySynth fallback) without Zyn"
        );
        drop(s);
        let _ = std::fs::remove_dir_all(&parent);
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

    pub(crate) fn dummy_midi_clip(track_id: &str) -> crate::midi::MidiClip {
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
            transpose_semitones: 0,
            velocity_offset: 0,
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

    /// C-1 RESIDUAL, the dangerous half — the sibling of the persist skip
    /// above, at the undo door. `HistoryLog::record_commit`'s guard drops a
    /// stale journal line and a stale history entry, but it runs AFTER the
    /// effect phase: by then a stale undo's inverses have already been
    /// applied to the document. Re-opening the same project is routine and
    /// keeps every id, so those inverses apply CLEANLY against the wrong
    /// revision instead of failing loudly.
    ///
    /// Staged exactly like the persist test: bump `session.epoch` directly,
    /// standing in for an epoch function's swap block — which really does
    /// bump it under the session lock BEFORE calling
    /// `HistoryLog::epoch_boundary`, so this IS the window in which the pop
    /// reports the old epoch while `Tx` already runs under the new one.
    /// Evidence the guard fired: `commit_replay` returns Err AND the gain
    /// never moved — "nothing applied", not merely "nothing recorded".
    #[test]
    fn an_undo_whose_document_swapped_after_the_pop_applies_nothing() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("undo-epoch-skip");
        cp.create_project(parent.to_str().unwrap(), "UndoEpochSkip").unwrap();

        let id =
            cp.add_track(Some("Audio".into()), None, TxMeta::user("add track")).unwrap().id.to_string();
        cp.set_track_mix(
            vec![TrackMixChange { gain_db: Some(-6.0), ..TrackMixChange::new(id.clone()) }],
            TxMeta::user("set track gain"),
        )
        .unwrap();
        let gain = || cp.session().lock().store.tracks.iter().find(|t| t.id == id).unwrap().gain_db;
        assert_eq!(gain(), -6.0);
        let depths_before = cp.history_depths();
        assert_eq!(depths_before, (2, 0), "the add and the gain are two undoable steps");

        // The document swaps out from under the undo, after it popped.
        cp.session().lock().epoch += 1;

        let err = cp.undo().expect_err("an undo against a swapped document must fail, not apply");
        assert!(err.contains("document changed"), "the failure must name the reason, got {err:?}");
        assert_eq!(gain(), -6.0, "NOTHING was applied to the swapped-in document");
        assert_eq!(
            cp.history_depths(),
            depths_before,
            "a rejected undo consumes no history step — the entry went back untouched"
        );

        // The guard is a staleness check, not a mute button: once the
        // epochs agree again the very same entry undoes normally.
        cp.session().lock().epoch -= 1;
        assert_eq!(cp.undo().unwrap().as_deref(), Some("set track gain"));
        assert_eq!(gain(), 0.0);
        assert_eq!(cp.history_depths(), (1, 1));

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
        assert!(committed.effect.persist.modulation, "facade also persists the modulation document");
        assert!(committed.effect.rebuild, "Track D: a lane edit rebuilds (see session.rs's arm doc)");

        // By the time `commit` returned above, the write already happened
        // (persist runs synchronously inside `commit`, before the event
        // emit) — read it back right away, no waiting.
        // Task 7: modulation save is authoritative (drops automation[]).
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(after["schemaVersion"], 4);
        assert!(after.get("automation").is_none(), "modulation save drops automation[]");
        let bindings = after["modulation"]["bindings"].as_array().unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0]["id"], "a-1");
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

    /// I-1: before this fix, `save_project_as_epoch` wrote ONLY
    /// project.json + the midi snapshot — plugin state blobs and automation
    /// lanes were silently left behind. Combined with Task 1's I-7
    /// adopt-clear fix, the very next COLD OPEN of the Save-As'd project
    /// would see no `plugins`/`automation` fields on disk and actively
    /// CLEAR whatever the session had — a Save-As that destroys plugin
    /// state and automation. Ruling F-6: both are snapshotted under the
    /// same short lock the midi snapshot already uses, and written after
    /// the lock drops via the same helpers `execute_persist` calls.
    #[test]
    fn save_as_carries_plugin_rows_state_blobs_and_automation_into_the_new_dir() {
        let (cp, _events, _engine) = recording_control_plane();
        {
            let mut s = cp.session().lock();
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(),
                uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(),
                format: "lv2".into(),
                status: "stub".into(),
                track_id: None,
            });
            s.plugins.params.insert("inst-1".into(), vec![]);
            s.plugins.pending_state.insert(
                "inst-1".into(),
                crate::plugins::state::encode_state(
                    "lv2:urn:test:synth",
                    &crate::plugins::state::StateBlob {
                        kind: crate::plugins::state::KIND_OPAQUE,
                        data: vec![7u8; 16],
                    },
                ),
            );
            s.plugins.dirty_state.insert("inst-1".into());
            s.automation.lanes.push(crate::plugins::automation::AutomationLane {
                id: "track:t-1:gain".into(),
                target_node: "track:t-1".into(),
                param_id: 0,
                points: vec![crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 }],
            });
        }

        let parent = cp_tmp_parent("saveas-plugins-automation");
        cp.save_project_as(parent.to_str().unwrap(), "PluginsAuto").unwrap();
        let dir = parent.join("PluginsAuto.aura");

        let pj: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("project.json")).unwrap(),
        )
        .unwrap();
        let rows = pj
            .get("plugins")
            .and_then(|v| v.as_array())
            .expect("plugins[] must be written by Save-As (I-1)");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "inst-1");
        assert!(dir.join("plugins").join("inst-1.state").exists(), "state blob must land");

        // Track F: Save-As upgrades to v4 `modulation{}` (and drops
        // `automation[]`). The I-1 contract is that the lane data lands,
        // not that the retired key survives.
        let loaded = crate::modulation::persist::load_from_project(&dir)
            .expect("modulation{} must be written by Save-As (I-1)");
        assert_eq!(loaded.bindings.len(), 1, "gain lane migrated into a binding");
        assert_eq!(loaded.bindings[0].id, "track:t-1:gain");
        assert_eq!(loaded.curves.len(), 1);
        assert!(!loaded.curves[0].points.is_empty(), "curve points must land");

        assert!(
            !cp.session().lock().plugins.dirty_state.contains("inst-1"),
            "dirty_state cleared for the id whose pending bytes just landed on disk"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// I-1 end-to-end: a Save-As'd project's plugins + automation survive a
    /// COLD OPEN. `ControlPlane::open_project_epoch`'s own adopt step
    /// (`plugins::state`/`plugins::automation::adopt_open_project`) reaches
    /// its session through a process-global, first-registration-wins
    /// `OnceLock` shared by the WHOLE test binary — `state.rs`'s and
    /// `automation.rs`'s own registered-session tests already document that
    /// as a "pre-existing, accepted risk" for tests WITHIN their own module
    /// (each serializes against its own siblings via a private
    /// `TEST_SESSION_LOCK`, but nothing stops a DIFFERENT module's
    /// registered-session test from running concurrently against that same
    /// global — confirmed here: an earlier version of this test that also
    /// registered against the global flaked under the full parallel suite,
    /// clobbered mid-test by one of those other modules' tests). So this
    /// test instead drives the exact same UNDERLYING restore primitives
    /// `adopt_open_project` calls — `plugins::state::restore_into_session`
    /// (disk -> a session it's handed directly, no global) and
    /// `plugins::automation::load_lanes` — against `cp.session()` itself,
    /// with no process-global involved at all: a deterministic, race-free
    /// exercise of the SAME disk-round-trip contract, without the
    /// mid-air-shared-global hazard the plain `open_project_epoch` route
    /// would reintroduce.
    #[test]
    fn save_as_then_cold_open_round_trips_plugins_and_automation() {
        let (cp, _events, _engine) = recording_control_plane();

        {
            let mut s = cp.session().lock();
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(),
                uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(),
                format: "lv2".into(),
                status: "stub".into(),
                track_id: None,
            });
            s.plugins.params.insert("inst-1".into(), vec![]);
            s.plugins.pending_state.insert(
                "inst-1".into(),
                crate::plugins::state::encode_state(
                    "lv2:urn:test:synth",
                    &crate::plugins::state::StateBlob {
                        kind: crate::plugins::state::KIND_OPAQUE,
                        data: vec![7u8; 16],
                    },
                ),
            );
            s.plugins.dirty_state.insert("inst-1".into());
            s.automation.lanes.push(crate::plugins::automation::AutomationLane {
                id: "track:t-1:gain".into(),
                target_node: "track:t-1".into(),
                param_id: 0,
                points: vec![crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 }],
            });
        }

        let parent = cp_tmp_parent("saveas-cold-open-roundtrip");
        cp.save_project_as(parent.to_str().unwrap(), "RoundTrip").unwrap();
        let saved_dir = parent.join("RoundTrip.aura");

        // "Cold open" — blank the in-memory session the way a fresh app
        // process (nothing adopted yet) would look, WITHOUT going through
        // `open_project_epoch`'s process-global-dependent adopt step (see
        // this test's doc comment for why).
        {
            let mut s = cp.session().lock();
            s.plugins = session::PluginDoc::default();
            s.automation.lanes.clear();
        }
        assert!(cp.session().lock().plugins.instances.is_empty(), "sanity: session blanked");
        assert!(cp.session().lock().automation.lanes.is_empty(), "sanity: session blanked");

        // The actual restore under test: the SAME primitives
        // `open_project_epoch`'s adopt step calls
        // (`plugins::state::read_restored_rows`/`install_restored_rows` via
        // `restore_into_session`, and `automation::load_lanes`), driven
        // directly against `cp.session()` — reads back exactly what
        // `save_project_as_epoch`'s I-1 fix just wrote.
        {
            let mut s = cp.session().lock();
            crate::plugins::state::restore_into_session(&saved_dir, &mut s).unwrap();
        }
        let doc = crate::modulation::persist::load_from_project(&saved_dir)
            .expect("modulation{} present after Save-As (I-1)");
        let lanes = crate::modulation::compat::lanes_from_doc(&doc);
        {
            let mut s = cp.session().lock();
            s.modulation = doc;
            s.automation.lanes = lanes;
        }

        let s = cp.session().lock();
        assert_eq!(
            s.plugins.instances.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["inst-1"],
            "plugin instance round-tripped through Save-As + cold open"
        );
        let pending = s.plugins.pending_state.get("inst-1").expect("pending_state present");
        let (uid, blob) = crate::plugins::state::decode_state(pending).unwrap();
        assert_eq!(uid, "lv2:urn:test:synth");
        assert_eq!(blob.data, vec![7u8; 16], "state blob bytes round-tripped");
        assert_eq!(s.automation.lanes.len(), 1, "automation lane round-tripped");
        assert_eq!(s.automation.lanes[0].id, "track:t-1:gain");
        assert_eq!(s.automation.lanes[0].points, vec![crate::plugins::automation::AutomationPoint {
            tick: 0,
            value: 1.0
        }]);
        drop(s);

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// M-2 (Task 3, whole-branch review): before this fix, `save_project_
    /// mark` (Ctrl+S) wrote ONLY `project.json` — a midi edit left dirty by
    /// a prior FAILED auto-persist (mirrors M-5's own scenario: `midi.dirty`
    /// stuck `true` with the edit never reaching disk) survived the save
    /// untouched, so the journal's mark record then claimed durability the
    /// snapshot didn't have. The edit here is added directly to
    /// `session.midi.clips` (bypassing `commit`, same direct-drive sanction
    /// `persist_effect_writes_midi_after_the_lock_and_before_the_emit` uses)
    /// so nothing auto-persists it first; `midi.dirty` is forced `true` to
    /// stand in for the failed auto-persist. `save_project_mark` must flush
    /// it and clear the flag.
    #[test]
    fn save_project_mark_flushes_a_failed_midi_autopersist() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("save-mark-midi");
        cp.create_project(parent.to_str().unwrap(), "SaveMarkMidi").unwrap();
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
            // Stands in for a prior FAILED auto-persist (M-5's own
            // scenario) — the edit is in memory, but nothing on disk
            // reflects it yet.
            session.midi.dirty = true;
        }

        cp.save_project_mark().unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(
            raw["content"].as_array().unwrap().len(),
            1,
            "M-2: Ctrl+S flushes the midi edit a failed auto-persist left behind"
        );
        let ev_ref = raw["content"][0]["eventsRef"]
            .as_str()
            .expect("events chunk ref written for a clip with notes");
        assert!(dir.join(ev_ref).exists(), "AMEV chunk file exists on disk after save_project_mark");
        assert!(!cp.session().lock().midi.dirty, "M-2: save_project_mark clears the recovered dirty flag");

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// M-2 (Task 3): same recovery, for plugin state — a `dirty_state` id
    /// left over from a failed auto-persist must flush on Ctrl+S, mirroring
    /// `save_as_carries_plugin_rows_state_blobs_and_automation_into_the_new_dir`'s
    /// setup but against an already-open project (`save_project_mark`, not
    /// Save-As).
    #[test]
    fn save_project_mark_flushes_dirty_plugin_state() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("save-mark-plugins");
        cp.create_project(parent.to_str().unwrap(), "SaveMarkPlugins").unwrap();
        let dir = cp.session().lock().store.project_dir.clone().unwrap();

        {
            let mut s = cp.session().lock();
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(),
                uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(),
                format: "lv2".into(),
                status: "stub".into(),
                track_id: None,
            });
            s.plugins.params.insert("inst-1".into(), vec![]);
            s.plugins.pending_state.insert(
                "inst-1".into(),
                crate::plugins::state::encode_state(
                    "lv2:urn:test:synth",
                    &crate::plugins::state::StateBlob {
                        kind: crate::plugins::state::KIND_OPAQUE,
                        data: vec![7u8; 16],
                    },
                ),
            );
            // Stands in for a failed auto-persist: bytes are pending, the
            // flag is set, nothing has landed on disk yet.
            s.plugins.dirty_state.insert("inst-1".into());
        }

        cp.save_project_mark().unwrap();

        assert!(
            dir.join("plugins").join("inst-1.state").exists(),
            "M-2: Ctrl+S flushes the pending plugin state blob"
        );
        assert!(
            cp.session().lock().plugins.dirty_state.is_empty(),
            "M-2: save_project_mark clears dirty_state once the bytes landed on disk"
        );

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// M-1 (Task 3, whole-branch review): a `PluginSetState` landing between
    /// the snapshot `execute_persist`/`save_project_mark`/`save_project_as_
    /// epoch` take and their post-write re-lock must NOT have its dirty flag
    /// cleared just because SOME earlier bytes for that id were written —
    /// `clear_dirty_state_matching` must compare live pending bytes against
    /// the snapshot's, not just check the id off a list.
    #[test]
    fn a_concurrent_set_state_between_snapshot_and_clear_stays_dirty() {
        let (cp, _events, _engine) = recording_control_plane();
        let snapshot = {
            let mut s = cp.session().lock();
            s.plugins.pending_state.insert("inst-1".into(), vec![1, 2, 3]);
            s.plugins.dirty_state.insert("inst-1".into());
            s.plugin_snapshot()
        };
        // The concurrent SetState: live pending bytes move on to something
        // the snapshot above never saw — standing in for a `PluginSetState`
        // landing in the window between the snapshot and this call.
        cp.session().lock().plugins.pending_state.insert("inst-1".into(), vec![9, 9, 9]);

        cp.committer().clear_dirty_state_matching(&["inst-1".to_string()], &snapshot);

        assert!(
            cp.session().lock().plugins.dirty_state.contains("inst-1"),
            "M-1: a concurrent SetState after the snapshot keeps the id dirty"
        );
    }

    /// M-1's complementary case: when nothing raced the snapshot, live
    /// pending bytes still match what was written, and the helper clears
    /// the flag exactly like the pre-M-1 unconditional `remove` did.
    #[test]
    fn matching_pending_bytes_clear_the_dirty_flag() {
        let (cp, _events, _engine) = recording_control_plane();
        let snapshot = {
            let mut s = cp.session().lock();
            s.plugins.pending_state.insert("inst-1".into(), vec![1, 2, 3]);
            s.plugins.dirty_state.insert("inst-1".into());
            s.plugin_snapshot()
        };

        cp.committer().clear_dirty_state_matching(&["inst-1".to_string()], &snapshot);

        assert!(
            !cp.session().lock().plugins.dirty_state.contains("inst-1"),
            "M-1: matching pending bytes clear the dirty flag"
        );
    }

    /// Review #23 follow-up: a stale persist can overwrite newer bytes and
    /// leave dirty already-false (the newer writer cleared it). Mismatch
    /// must re-dirty so the next flush rewrites the live document.
    #[test]
    fn clear_midi_dirty_re_dirties_when_live_moved_and_flag_was_clear() {
        let (cp, _events, _engine) = recording_control_plane();
        let written = {
            let mut s = cp.session().lock();
            s.midi.clips.push(dummy_midi_clip("t-1"));
            s.midi.dirty = false;
            s.midi_snapshot()
        };
        {
            let mut s = cp.session().lock();
            s.midi.clips[0].name = "moved-on".into();
            s.midi.dirty = false;
        }

        let matched = cp.committer().clear_midi_dirty_if_unchanged(&written);
        assert!(!matched, "live bytes moved on");
        assert!(
            cp.session().lock().midi.dirty,
            "stale write must re-dirty so the live document flushes again"
        );
    }

    /// Same hole on plugin blobs: dirty already cleared by a later persist,
    /// then a stale write's compare must put the id back.
    #[test]
    fn clear_dirty_state_re_dirties_when_live_moved_and_flag_was_clear() {
        let (cp, _events, _engine) = recording_control_plane();
        let snapshot = {
            let mut s = cp.session().lock();
            s.plugins.pending_state.insert("inst-1".into(), vec![1, 2, 3]);
            s.plugins.dirty_state.clear();
            s.plugin_snapshot()
        };
        cp.session().lock().plugins.pending_state.insert("inst-1".into(), vec![9, 9, 9]);

        let matched = cp.committer().clear_dirty_state_matching(&["inst-1".to_string()], &snapshot);
        assert!(!matched, "live pending bytes moved on");
        assert!(
            cp.session().lock().plugins.dirty_state.contains("inst-1"),
            "stale plugin write must re-dirty the id"
        );
    }

    /// Persist I/O is serialized: a holder of `persist_gate` blocks
    /// `execute_persist` until it drops. Without the gate, two writers can
    /// snapshot under the session lock and interleave their disk writes.
    #[test]
    fn persist_gate_blocks_execute_persist_until_released() {
        let (cp, _events, _engine) = recording_control_plane();
        let parent = cp_tmp_parent("persist-gate");
        cp.create_project(parent.to_str().unwrap(), "PersistGate").unwrap();
        let epoch = cp.session().lock().epoch;
        let gate = cp.session().lock().persist_gate.clone();
        let held = gate.lock();
        let committer = cp.committer().clone();
        let handle = std::thread::spawn(move || {
            committer.execute_persist(
                &session::PersistEffect { midi: true, ..session::PersistEffect::default() },
                epoch,
            );
        });
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(!handle.is_finished(), "execute_persist must wait on persist_gate");
        drop(held);
        handle.join().expect("persist thread");
        let _ = std::fs::remove_dir_all(&parent);
    }

    /// Track D leftover: the automation driver's CLAP writes went through
    /// `forward_params_to_host`'s "clap" arm, which called the blocking
    /// `clap_host::set_params` (`plugin_main().run(...)`) — a ~1000/s
    /// round-trip cost under an active ramp. Wedge the plugin-main thread
    /// behind a closure that only releases when WE say so; if the clap arm
    /// still blocks on it, the call won't return within the budget.
    ///
    /// Wedges the same process-wide `plugin_main()` singleton
    /// `clap_host`'s own tests share — safe under this project's CI
    /// convention (`--test-threads=1`); a parallel local run can stall
    /// another concurrently running CLAP test that's mid-`run()`.
    #[test]
    fn forward_params_to_host_does_not_block_the_clap_arm() {
        use crate::plugins::host::plugin_main;
        let (release_tx, release_rx) = crossbeam_channel::bounded::<()>(0);
        plugin_main().post(move |_| {
            let _ = release_rx.recv();
        });

        let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(1);
        std::thread::spawn(move || {
            forward_params_to_host("no-such-instance", "clap", &[(0, 0.5)]);
            let _ = done_tx.send(());
        });

        let returned_promptly =
            done_rx.recv_timeout(std::time::Duration::from_millis(200)).is_ok();
        let _ = release_tx.send(());
        assert!(
            returned_promptly,
            "forward_params_to_host's clap arm must not block on the plugin-main queue"
        );
    }
}
