//! The op log turns on (Plan E Task 17, Gate E deliverable 3).
//!
//! Two things live here, deliberately behind ONE handle ([`HistoryLog`]):
//!
//! * [`History`] — the in-memory undo/redo stacks. Bounded, cleared at every
//!   document-swap epoch boundary, with the 350 ms same-key merge round-2
//!   §4.4 specifies as the FALLBACK for callers that emit no explicit
//!   gesture boundary (the mechanism is `gesture_begin`/`gesture_end`,
//!   Task 14 — this is the safety net under it, not a replacement).
//! * [`JournalWriter`] — the append-only `<project dir>/journal.ndjson`.
//!   One line per NON-transient committed batch, plus a record at every
//!   epoch boundary. This is where `OP_FORMAT_VERSION` stops being
//!   decorative (see [`JournalWriter::append_batch`]).
//!
//! WHY ONE HANDLE: both are driven from exactly the same place —
//! `Committer::commit_with_rebuild`, after the effects, when
//! `!meta.transient` — and BOTH `Committer` instances (the `ControlPlane`'s
//! own and the engine control thread's, see `Committer`'s doc) must share
//! them, or the engine's non-transient recording finalize
//! (`TxMeta::engine("stop recording")`, audio/engine.rs) would land in a
//! second, invisible history. One `Arc<HistoryLog>`, created in
//! `audio::init` and handed to both.
//!
//! LOCK ORDER (binding, and it is the LEAF of the whole system):
//! `gesture` -> `session` -> `history` / `journal`. Both mutexes here are
//! acquired ONLY inside this module's own methods, are never held across
//! any other lock acquisition, and no code path takes the session or
//! gesture lock while holding one of them. Concretely: [`HistoryLog`]'s
//! methods are called from `commit_with_rebuild` AFTER the session lock is
//! released, and from the epoch functions after their swap block ends. The
//! journal's disk write happens under the journal mutex — which is legal
//! precisely because it is the leaf: no other lock is held at that moment
//! (this is the same "no I/O under the session lock" rule round-2 §4
//! imposes, satisfied by construction rather than by review).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::op::{Actor, Op, TxMeta, OP_FORMAT_VERSION};
use super::CoalesceKey;

// ---------------------------------------------------------------------------
// Tunables (documented constants, not magic numbers)
// ---------------------------------------------------------------------------

/// The boundary-less coalescing window (round-2 §4.4). Two CONSECUTIVE
/// single-key history entries with the same key, the same actor and the
/// same label, landing within this window, fold into one undo step.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(350);

/// Maximum number of entries kept on the undo stack. When the stack is
/// full the OLDEST entry is dropped (a DAW's undo history is a recency
/// window, not an archive — the journal keeps the full record on disk).
///
/// 200 is chosen to be generous for a session's worth of interactive
/// editing while keeping the worst-case memory bounded: an entry holds its
/// ops and inverses, and the heaviest of those (a `MidiSetNotes` over a
/// dense clip, a `PluginRemove` carrying a state blob) is measured in tens
/// of kilobytes, so the ceiling is single-digit megabytes. Plan F owns real
/// retention policy (disk-backed history, per-epoch segments); this is the
/// honest interim bound, not a design.
pub const UNDO_STACK_LIMIT: usize = 200;

// ---------------------------------------------------------------------------
// HistoryEntry
// ---------------------------------------------------------------------------

/// One undoable step.
///
/// An entry is IMMUTABLE once recorded and MIGRATES between the two stacks:
/// undoing pops it off `undo` and pushes the very same value onto `redo`;
/// redoing moves it back. Nothing is re-derived on the way, so an entry
/// cannot drift from the edit it describes — undo applies [`Self::inverses`],
/// redo applies [`Self::ops`], and both are the exact op lists
/// `Session::transact` computed (or, for a gesture, the exact lists
/// `close_gesture` folded).
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub label: String,
    pub actor: Actor,
    pub run: String,
    /// Applied to REDO this step (forward order).
    pub ops: Vec<Op>,
    /// Applied to UNDO this step. Already in apply order —
    /// `Session::transact` reverses them before handing them out.
    pub inverses: Vec<Op>,
    pub at: Instant,
    /// `Some(key)` when EVERY op in the batch shares one coalesce key —
    /// the 350 ms boundary-less fallback merges consecutive same-key
    /// entries (round-2 §4.4: fallback, not mechanism). `None` disables
    /// merging for this entry in both directions, which is what a
    /// structural batch, a mixed batch, and a gesture batch all want.
    /// `pub(crate)`, not `pub`: `CoalesceKey` is itself `pub(crate)` (see
    /// its doc in control/mod.rs — it is an internal coalescing detail, not
    /// a wire type), and re-exporting it just to widen this one field would
    /// make an implementation detail part of the crate's public surface.
    pub(crate) coalesce: Option<CoalesceKey>,
}

impl HistoryEntry {
    /// Build an entry from a committed batch. `coalesce` is derived from the
    /// ops: `Some(key)` only when the batch is non-empty and every op maps
    /// to the SAME key. A batch containing any op with no key at all (every
    /// structural op: `TrackAdd`, `ClipRemove`, `PluginAdd`, …) yields
    /// `None`, which is exactly the "a structural op breaks the merge" rule.
    pub fn from_committed(meta: &TxMeta, ops: &[Op], inverses: &[Op]) -> Self {
        Self {
            label: meta.label.clone(),
            actor: meta.actor.clone(),
            run: meta.run.clone(),
            ops: ops.to_vec(),
            inverses: inverses.to_vec(),
            at: Instant::now(),
            coalesce: batch_coalesce_key(ops),
        }
    }

    /// Same, but with merging force-disabled — a GESTURE batch. It arrives
    /// PRE-FOLDED (`close_gesture` already reduced the whole drag to one
    /// net op per key), so it is the finished undo step by construction;
    /// letting a 350 ms window merge it with a neighbour would silently
    /// join two deliberate gestures into one, defeating the very boundary
    /// the user's `pointerdown`/`pointerup` drew.
    pub fn from_gesture(meta: &TxMeta, ops: &[Op], inverses: &[Op]) -> Self {
        Self { coalesce: None, ..Self::from_committed(meta, ops, inverses) }
    }
}

/// The batch-wide coalesce key: `Some(k)` iff `ops` is non-empty and every
/// op yields `Some(k)` for the same `k`.
fn batch_coalesce_key(ops: &[Op]) -> Option<CoalesceKey> {
    let mut it = ops.iter();
    let first = CoalesceKey::for_history_op(it.next()?)?;
    for op in it {
        if CoalesceKey::for_history_op(op)? != first {
            return None;
        }
    }
    Some(first)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct History {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a fresh edit. Clears the redo stack (a new edit invalidates
    /// any redo future), applies the 350 ms same-key merge, and enforces
    /// [`UNDO_STACK_LIMIT`].
    ///
    /// THE MERGE (round-2 §4.4's boundary-less fallback): if the top of the
    /// undo stack and `e` are both single-key entries with the SAME key,
    /// the same actor and the same label, and less than
    /// [`COALESCE_WINDOW`] elapsed between them, they fold into one entry
    /// that keeps the OLDER entry's `inverses` (undo must go back to where
    /// the run of edits STARTED) and takes the NEWER entry's `ops` (redo
    /// must land where it ENDED) — the same first-inverse/last-op rule
    /// `GestureState::fold_committed` uses inside a gesture.
    ///
    /// `at` advances to the new entry's timestamp so a continuous stream of
    /// edits keeps merging (the window is "since the last edit", not "since
    /// the run began"); a pause longer than the window starts a new step.
    pub fn record(&mut self, e: HistoryEntry) {
        self.redo.clear();
        if let Some(top) = self.undo.last_mut() {
            if merges(top, &e) {
                top.ops = e.ops;
                top.at = e.at;
                return;
            }
        }
        self.undo.push(e);
        if self.undo.len() > UNDO_STACK_LIMIT {
            self.undo.remove(0);
        }
    }

    /// Pop the next step to undo. The caller applies its `inverses` through
    /// a normal commit and then calls [`Self::push_redo`] with the SAME
    /// entry on success (or [`Self::push_undo_unchanged`] to put it back on
    /// failure).
    pub fn pop_undo(&mut self) -> Option<HistoryEntry> {
        self.undo.pop()
    }

    /// Pop the next step to redo. The caller applies its `ops`.
    pub fn pop_redo(&mut self) -> Option<HistoryEntry> {
        self.redo.pop()
    }

    /// Put an entry on the redo stack (after a successful undo). Does NOT
    /// clear anything — that is [`Self::record`]'s job, and a redo future
    /// must survive its own undo.
    pub fn push_redo(&mut self, e: HistoryEntry) {
        self.redo.push(e);
    }

    /// Put an entry back on the undo stack — after a successful redo, or to
    /// restore the stack when an undo commit failed. Never merges (the
    /// entry is already a finished step) and never clears the redo stack.
    pub fn push_undo_unchanged(&mut self, e: HistoryEntry) {
        self.undo.push(e);
        if self.undo.len() > UNDO_STACK_LIMIT {
            self.undo.remove(0);
        }
    }

    /// Epoch boundary (ruling 4): the document was swapped, so every entry
    /// describes a document that is no longer open. Undoing across a
    /// document swap is not "undo", it is corruption.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }
}

/// The 350 ms same-key merge predicate. Both entries must carry a key
/// (`None` — structural, mixed, or gesture — never merges in either
/// direction), the keys/actors/labels must match, and the gap must be
/// under the window.
fn merges(top: &HistoryEntry, next: &HistoryEntry) -> bool {
    let (Some(a), Some(b)) = (&top.coalesce, &next.coalesce) else { return false };
    a == b
        && top.actor == next.actor
        && top.label == next.label
        && next.at.saturating_duration_since(top.at) < COALESCE_WINDOW
}

// ---------------------------------------------------------------------------
// JournalWriter
// ---------------------------------------------------------------------------

/// Which epoch boundary a journal record describes (ruling 4). `Save` is
/// the snapshot mark — `save_project_mark` writes it WITHOUT rotating or
/// clearing history, because it is not a document swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochEvent {
    Open,
    Create,
    SaveAs,
    Ensure,
    Save,
}

impl EpochEvent {
    fn as_str(self) -> &'static str {
        match self {
            EpochEvent::Open => "open",
            EpochEvent::Create => "create",
            EpochEvent::SaveAs => "saveAs",
            EpochEvent::Ensure => "ensure",
            EpochEvent::Save => "save",
        }
    }
}

/// The journal file name inside a project directory.
pub const JOURNAL_FILE: &str = "journal.ndjson";

/// Append-only NDJSON writer over `<project dir>/journal.ndjson`.
///
/// Closed (no file) until an epoch boundary hands it a project directory —
/// an UNSAVED session has nowhere to write, and that is not an error: the
/// in-memory history still works, the journal simply starts at the first
/// `save_project_as`/`create_project`.
///
/// DURABILITY: every line is flushed as it is written, and there is NO
/// `fsync`. The journal is a LOG; the project file is the snapshot. A
/// power cut can therefore lose the tail of the journal, which is the
/// accepted trade (an fsync per knob-drag batch would be absurd). Plan F
/// owns durability policy — group commit, fsync cadence, torn-tail
/// recovery. Recorded here rather than left to be discovered.
#[derive(Default)]
pub struct JournalWriter {
    path: Option<PathBuf>,
    file: Option<std::fs::File>,
}

impl JournalWriter {
    pub fn closed() -> Self {
        Self::default()
    }

    /// Open (or re-open) the journal in `dir`, APPENDING — a project's
    /// journal spans sessions; re-opening the same project must not erase
    /// what it already recorded. Rotation at a document swap is exactly
    /// this: drop the previous file, open the new project's.
    ///
    /// A failure to open is logged and leaves the writer closed: no edit
    /// may ever fail because the log could not be written.
    pub fn open_in(&mut self, dir: &Path) {
        let path = dir.join(JOURNAL_FILE);
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                self.file = Some(f);
                self.path = Some(path);
            }
            Err(e) => {
                log::warn!("journal: cannot open {}: {e}", path.display());
                self.close();
            }
        }
    }

    pub fn close(&mut self) {
        self.file = None;
        self.path = None;
    }

    pub fn is_open(&self) -> bool {
        self.file.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// One line per NON-transient committed batch.
    ///
    /// `OP_FORMAT_VERSION` BECOMES LOAD-BEARING HERE. Until this call site
    /// existed, the constant described a format nothing had ever written,
    /// so any op could be reshaped freely. From the first line this writes,
    /// the ops in a journal are DATA ON DISK that a future AURA must still
    /// be able to read: a breaking change to any `Op` variant's wire form
    /// now requires bumping `OP_FORMAT_VERSION` and teaching the reader
    /// both shapes. Additive fields with `#[serde(default)]` stay
    /// non-breaking (that is why `clip_indices`, `transient` and
    /// `PluginRemove::params` were added that way) and keep the version at
    /// 1.
    ///
    /// Ops serialize through their own `serde` impls — the SAME wire form
    /// the IPC surface uses, single-camelCase-token `kind`s per ruling 1.
    pub fn append_batch(&mut self, rev: u64, epoch: u64, meta: &TxMeta, ops: &[Op]) {
        let line = serde_json::json!({
            "v": OP_FORMAT_VERSION,
            "rev": rev,
            "epoch": epoch,
            "actor": meta.actor,
            "run": meta.run,
            "label": meta.label,
            "ops": ops,
        });
        self.write_line(&line);
    }

    /// A boundary record: `{"v":1,"epochEvent":"open","epoch":N}`.
    pub fn append_epoch(&mut self, event: EpochEvent, epoch: u64) {
        let line = serde_json::json!({
            "v": OP_FORMAT_VERSION,
            "epochEvent": event.as_str(),
            "epoch": epoch,
        });
        self.write_line(&line);
    }

    /// Line-buffered: one `write_all` of `<json>\n` plus a `flush`, no
    /// `fsync` (see this struct's doc). A write error is logged once and
    /// the writer stays open — a full disk must not take the DAW down.
    fn write_line(&mut self, value: &serde_json::Value) {
        let Some(f) = self.file.as_mut() else { return };
        let mut line = value.to_string();
        line.push('\n');
        if let Err(e) = f.write_all(line.as_bytes()).and_then(|()| f.flush()) {
            log::warn!("journal: write failed: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// HistoryLog — the one handle both Committers share
// ---------------------------------------------------------------------------

/// How a commit should be reflected in the undo/redo stacks. Passed down
/// the CALL PATH (`Committer::commit_with_rebuild_mode`), deliberately not
/// a thread-local: an undo runs on whatever thread invoked it, and a
/// thread-local would silently mis-classify a commit that a wrapper made on
/// its behalf.
///
/// THE INVARIANT REDO-SOUNDNESS RESTS ON (fix round 1, M-3): transient
/// writes must never touch document fields an entry's `ops` can address, or
/// a pending redo silently lands on a different state. Redo replays an
/// entry's stored `ops` against whatever the document is NOW, so anything
/// that mutates those same fields WITHOUT leaving a history entry — which
/// is precisely what `TxMeta::transient` means — moves the ground under a
/// redo without invalidating it. Today the transient writers stay clear by
/// construction: transport `Op::Set`s address `ObjectRef::Transport` only,
/// and mid-gesture folds are themselves superseded by the gesture batch
/// that closes over them. A future transient writer touching a track,
/// clip, midi or plugin field would break this silently — it is a rule
/// about what may be marked transient, not a property the code checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMode {
    /// A fresh edit: record it (clearing redo, applying the 350 ms merge).
    Record,
    /// This commit IS an undo or a redo. It is still journaled — replay
    /// must see it, it is a real mutation — but it must not create a new
    /// history entry: `ControlPlane::undo`/`redo` migrate the ORIGINAL
    /// entry between the stacks instead.
    Replay,
}

/// History + journal behind one handle, shared by every `Committer`.
pub struct HistoryLog {
    history: Mutex<History>,
    journal: Mutex<JournalWriter>,
}

impl Default for HistoryLog {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryLog {
    pub fn new() -> Self {
        Self { history: Mutex::new(History::new()), journal: Mutex::new(JournalWriter::closed()) }
    }

    /// The one call `Committer::commit_with_rebuild` makes, after the
    /// effects, for every NON-transient batch. Transient batches
    /// (transport, mid-gesture folds, engine state mirrors) never get here
    /// — the caller checks `meta.transient` — which is scope ruling 2's
    /// "through the channel, never journaled, never undoable", enforced at
    /// exactly one place.
    ///
    /// AN EMPTY BATCH IS RECORDED NOWHERE (fix round 1, I-1). `fold_ops`
    /// DROPS a `Set` group whose net `from == to` (session.rs), so an empty
    /// `Committed` is a live, ordinary outcome — not a defensive
    /// impossibility: `move_clip` back to the sample it already sits on, or
    /// a `set_track_mix` writing a track's current gain, both produce one.
    /// Such a batch must leave no trace in EITHER stream:
    /// * no history entry — otherwise Ctrl+Z consumes a step, bumps `rev`,
    ///   emits `project://changed` and toasts a label, while changing
    ///   nothing the user can see. A phantom undo step is worse than a
    ///   missing one: it makes the whole stack feel untrustworthy.
    /// * no journal line — `"ops":[]` documents nothing; a replay applying
    ///   it is a guaranteed no-op, so the line is pure noise in a log whose
    ///   entire value is that every line means something.
    ///
    /// `close_gesture` has always had the equivalent guard (its `last`
    /// emptiness check); this is the ordinary path catching up with it, and
    /// it lives HERE rather than at the call site so no future caller can
    /// route around it.
    pub fn record_commit(&self, rev: u64, epoch: u64, meta: &TxMeta, ops: &[Op], inverses: &[Op], mode: HistoryMode) {
        if ops.is_empty() {
            return;
        }
        // Journal FIRST, unconditionally: an undo/redo is a mutation and
        // replay must see it, even though it creates no new history entry.
        self.journal.lock().append_batch(rev, epoch, meta, ops);
        if mode == HistoryMode::Record {
            self.history.lock().record(HistoryEntry::from_committed(meta, ops, inverses));
        }
    }

    /// A closed GESTURE's synthesized batch (Task 14). Journaled like any
    /// other non-transient batch — its `rev`/`epoch` are the session's
    /// current values, read by `close_gesture` — and recorded PRE-FOLDED
    /// (never 350 ms-merged, see [`HistoryEntry::from_gesture`]).
    ///
    /// The same empty-batch guard as [`Self::record_commit`], for the same
    /// reasons. `close_gesture` cannot currently reach here with an empty
    /// `ops` (it returns early when the gesture folded nothing), so this is
    /// belt-and-braces — but the rule "an empty batch is recorded nowhere"
    /// belongs to the sink, not to one caller's discipline.
    pub fn record_gesture(&self, rev: u64, epoch: u64, meta: &TxMeta, ops: &[Op], inverses: &[Op]) {
        if ops.is_empty() {
            return;
        }
        self.journal.lock().append_batch(rev, epoch, meta, ops);
        self.history.lock().record(HistoryEntry::from_gesture(meta, ops, inverses));
    }

    /// A DOCUMENT-SWAP epoch boundary (ruling 4): clear history and redo,
    /// rotate the journal onto the new project dir, write the boundary
    /// record. Order matters — the record must land in the NEW journal,
    /// since it describes the document that journal belongs to.
    pub fn epoch_boundary(&self, dir: &Path, event: EpochEvent, epoch: u64) {
        self.history.lock().clear();
        let mut j = self.journal.lock();
        j.open_in(dir);
        j.append_epoch(event, epoch);
    }

    /// The SNAPSHOT MARK (`save_project_mark`): same document, same
    /// content, so history is NOT cleared and the journal is NOT rotated —
    /// only a mark is written, so a replay can tell where the on-disk
    /// snapshot caught up with the log.
    pub fn snapshot_mark(&self, epoch: u64) {
        self.journal.lock().append_epoch(EpochEvent::Save, epoch);
    }

    pub fn pop_undo(&self) -> Option<HistoryEntry> {
        self.history.lock().pop_undo()
    }

    pub fn pop_redo(&self) -> Option<HistoryEntry> {
        self.history.lock().pop_redo()
    }

    pub fn push_redo(&self, e: HistoryEntry) {
        self.history.lock().push_redo(e);
    }

    pub fn push_undo_unchanged(&self, e: HistoryEntry) {
        self.history.lock().push_undo_unchanged(e);
    }

    /// `(undo_len, redo_len)` — what the `undo`/`redo` commands report back
    /// so the UI can enable/disable its menu items without a second call.
    pub fn depths(&self) -> (usize, usize) {
        let h = self.history.lock();
        (h.undo_len(), h.redo_len())
    }

    pub fn journal_path(&self) -> Option<PathBuf> {
        self.journal.lock().path().map(|p| p.to_path_buf())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::op::{ObjectRef, PropPath};

    fn set_gain(track: &str, to: f64) -> Op {
        Op::Set {
            object: ObjectRef::Track(track.into()),
            path: PropPath::Gain,
            from: serde_json::Value::Null,
            to: serde_json::json!(to),
        }
    }

    fn set_pan(track: &str, to: f64) -> Op {
        Op::Set {
            object: ObjectRef::Track(track.into()),
            path: PropPath::Pan,
            from: serde_json::Value::Null,
            to: serde_json::json!(to),
        }
    }

    fn track_add(id: &str) -> Op {
        Op::TrackAdd {
            track: crate::audio::types::TrackState {
                id: id.into(),
                name: id.into(),
                kind: "audio".into(),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                armed: false,
                color: "#7c9cff".into(),
                instrument_id: None,
            },
            index: 0,
            clips: vec![],
            clip_indices: vec![],
        }
    }

    fn entry(label: &str, ops: Vec<Op>, at: Instant) -> HistoryEntry {
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: label.into(), transient: false };
        let inverses = ops.clone();
        HistoryEntry { at, ..HistoryEntry::from_committed(&meta, &ops, &inverses) }
    }

    #[test]
    fn same_key_entries_inside_the_window_merge_keeping_first_inverses_and_last_ops() {
        let mut h = History::new();
        let t0 = Instant::now();
        let mut first = entry("set gain", vec![set_gain("t-1", -3.0)], t0);
        first.inverses = vec![set_gain("t-1", 0.0)]; // the baseline
        h.record(first);
        h.record(entry("set gain", vec![set_gain("t-1", -6.0)], t0 + Duration::from_millis(100)));
        h.record(entry("set gain", vec![set_gain("t-1", -9.0)], t0 + Duration::from_millis(200)));

        assert_eq!(h.undo_len(), 1, "three same-key edits inside the window are ONE undo step");
        let e = h.pop_undo().unwrap();
        assert_eq!(e.ops, vec![set_gain("t-1", -9.0)], "the LAST forward op wins");
        assert_eq!(e.inverses, vec![set_gain("t-1", 0.0)], "the FIRST baseline is what undo restores");
    }

    #[test]
    fn a_different_key_does_not_merge() {
        let mut h = History::new();
        let t0 = Instant::now();
        h.record(entry("mix", vec![set_gain("t-1", -3.0)], t0));
        h.record(entry("mix", vec![set_pan("t-1", 0.5)], t0 + Duration::from_millis(10)));
        assert_eq!(h.undo_len(), 2, "gain and pan are different keys");

        // Same path, different OBJECT is also a different key.
        h.record(entry("mix", vec![set_gain("t-2", -3.0)], t0 + Duration::from_millis(20)));
        assert_eq!(h.undo_len(), 3);
    }

    #[test]
    fn beyond_the_window_does_not_merge() {
        let mut h = History::new();
        let t0 = Instant::now();
        h.record(entry("set gain", vec![set_gain("t-1", -3.0)], t0));
        h.record(entry("set gain", vec![set_gain("t-1", -6.0)], t0 + COALESCE_WINDOW));
        assert_eq!(h.undo_len(), 2, "exactly at the window is already too late");
    }

    #[test]
    fn a_structural_op_breaks_the_merge() {
        let mut h = History::new();
        let t0 = Instant::now();
        h.record(entry("set gain", vec![set_gain("t-1", -3.0)], t0));
        // A structural batch has no key at all.
        h.record(entry("add track", vec![track_add("t-9")], t0 + Duration::from_millis(10)));
        h.record(entry("set gain", vec![set_gain("t-1", -6.0)], t0 + Duration::from_millis(20)));
        assert_eq!(h.undo_len(), 3, "a structural batch never merges, in either direction");

        // A MIXED batch (a Set plus a structural op) has no key either.
        let mut h2 = History::new();
        h2.record(entry("set gain", vec![set_gain("t-1", -3.0)], t0));
        h2.record(entry("set gain", vec![set_gain("t-1", -6.0), track_add("t-9")], t0 + Duration::from_millis(10)));
        assert_eq!(h2.undo_len(), 2);
    }

    #[test]
    fn a_gesture_batch_is_never_merged() {
        let mut h = History::new();
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "fader".into(), transient: false };
        let ops = vec![set_gain("t-1", -3.0)];
        h.record(HistoryEntry::from_gesture(&meta, &ops, &ops));
        h.record(HistoryEntry::from_gesture(&meta, &ops, &ops));
        assert_eq!(h.undo_len(), 2, "two deliberate gestures stay two undo steps");
        // ...and a plain entry does not merge INTO a gesture batch either.
        h.record(entry("fader", vec![set_gain("t-1", -6.0)], Instant::now()));
        assert_eq!(h.undo_len(), 3);
    }

    #[test]
    fn midi_set_notes_merges_by_clip_the_boundary_less_fallback() {
        // `midi_set_notes_core` keeps a FIXED label ("set midi notes")
        // precisely so this fallback can coalesce a run of note edits on
        // one clip into one undo step (midi/mod.rs's own doc says so).
        let notes = |k: u8| Op::MidiSetNotes {
            clip: "mc-1".into(),
            notes: vec![crate::midi::MidiNote {
                tick: 0,
                length_ticks: 480,
                key: k,
                velocity: 100,
                channel: 0,
                note_id: crate::ids::NoteId(0),
            }],
        };
        let mut h = History::new();
        let t0 = Instant::now();
        h.record(entry("set midi notes", vec![notes(60)], t0));
        h.record(entry("set midi notes", vec![notes(62)], t0 + Duration::from_millis(50)));
        assert_eq!(h.undo_len(), 1);
        // A different clip is a different key.
        let other = Op::MidiSetNotes { clip: "mc-2".into(), notes: vec![] };
        h.record(entry("set midi notes", vec![other], t0 + Duration::from_millis(60)));
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn a_new_edit_clears_the_redo_stack() {
        let mut h = History::new();
        h.record(entry("a", vec![set_gain("t-1", -1.0)], Instant::now()));
        let e = h.pop_undo().unwrap();
        h.push_redo(e);
        assert_eq!(h.redo_len(), 1);
        h.record(entry("b", vec![set_pan("t-1", 0.2)], Instant::now()));
        assert_eq!(h.redo_len(), 0, "a fresh edit invalidates the redo future");
    }

    #[test]
    fn the_undo_stack_is_bounded_and_drops_the_oldest() {
        let mut h = History::new();
        let t0 = Instant::now();
        for i in 0..(UNDO_STACK_LIMIT + 10) {
            // Distinct labels so nothing merges.
            h.record(entry(&format!("edit {i}"), vec![set_gain("t-1", i as f64)], t0));
        }
        assert_eq!(h.undo_len(), UNDO_STACK_LIMIT);
        let newest = h.pop_undo().unwrap();
        assert_eq!(newest.label, format!("edit {}", UNDO_STACK_LIMIT + 9), "the NEWEST survives");
    }

    #[test]
    fn clear_empties_both_stacks() {
        let mut h = History::new();
        h.record(entry("a", vec![set_gain("t-1", -1.0)], Instant::now()));
        let e = h.pop_undo().unwrap();
        h.push_redo(e);
        h.record(entry("b", vec![set_pan("t-1", 0.2)], Instant::now()));
        h.clear();
        assert_eq!((h.undo_len(), h.redo_len()), (0, 0));
    }

    #[test]
    fn journal_writes_ndjson_and_is_a_no_op_while_closed() {
        let mut j = JournalWriter::closed();
        let meta = TxMeta { actor: Actor::User, run: "r-1".into(), label: "mix".into(), transient: false };
        // Closed: silently does nothing (an unsaved session has nowhere to
        // write; edits must still succeed).
        j.append_batch(1, 0, &meta, &[set_gain("t-1", -3.0)]);
        assert!(!j.is_open());

        let dir = std::env::temp_dir().join(format!("aura-journal-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        j.open_in(&dir);
        j.append_epoch(EpochEvent::Create, 1);
        j.append_batch(1, 1, &meta, &[set_gain("t-1", -3.0)]);
        j.append_batch(2, 1, &meta, &[track_add("t-9")]);

        let text = std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "one line per record, nothing buffered");
        let epoch: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(epoch["v"], 1);
        assert_eq!(epoch["epochEvent"], "create");
        assert_eq!(epoch["epoch"], 1);
        let batch: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(batch["v"], 1);
        assert_eq!(batch["rev"], 1);
        assert_eq!(batch["actor"], "user");
        assert_eq!(batch["run"], "r-1");
        assert_eq!(batch["label"], "mix");
        assert_eq!(batch["ops"][0]["kind"], "set");
        let structural: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(structural["ops"][0]["kind"], "trackAdd");

        // Re-opening the SAME dir appends; it never truncates.
        j.open_in(&dir);
        j.append_epoch(EpochEvent::Open, 2);
        let text = std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap();
        assert_eq!(text.lines().count(), 4, "re-open appends, the record survives");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_log_replay_mode_journals_but_records_no_entry() {
        let log = HistoryLog::new();
        let dir = std::env::temp_dir().join(format!("aura-journal-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        log.epoch_boundary(&dir, EpochEvent::Create, 1);

        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "mix".into(), transient: false };
        let ops = [set_gain("t-1", -3.0)];
        log.record_commit(1, 1, &meta, &ops, &ops, HistoryMode::Record);
        assert_eq!(log.depths(), (1, 0));
        log.record_commit(2, 1, &meta, &ops, &ops, HistoryMode::Replay);
        assert_eq!(log.depths(), (1, 0), "a replay commit creates no new entry");

        let text = std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap();
        assert_eq!(text.lines().count(), 3, "epoch record + BOTH commits, replay included");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
