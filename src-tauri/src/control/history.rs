//! The op log turns on (Plan E Task 17, Gate E deliverable 3).
//!
//! Two things live here, deliberately behind ONE handle ([`HistoryLog`]):
//!
//! * [`History`] — the in-memory undo/redo stacks. Bounded, cleared at every
//!   document-swap epoch boundary, with the 350 ms same-key merge round-2
//!   §4.4 specifies as the FALLBACK for callers that emit no explicit
//!   gesture boundary (the mechanism is `gesture_begin`/`gesture_end`,
//!   Task 14 — this is the safety net under it, not a replacement). Ordered
//!   by the commits' `rev`, not by arrival at the sink (Plan F Task 11,
//!   L-4's stack half — see [`History`]).
//! * [`JournalWriter`] — the append-only `<project dir>/journal.ndjson`.
//!   One line per NON-transient committed batch, plus a record at every
//!   epoch boundary. This is where `OP_FORMAT_VERSION` stops being
//!   decorative (see [`JournalWriter::append_batch`]). Its READER lives in
//!   [`crate::control::replay`] (Plan F Task 9) — the writer here and the
//!   parse rules there are a matched pair.
//! * [`VersionGraph`] — the retention substrate (Plan F Task 7): one node
//!   per non-transient batch, materialized or replay-only, bounded by a
//!   bytes ceiling and a steps floor, with evicted loads dropped on the
//!   janitor thread. Fed from the same sink as the other two.
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
//! `gesture` -> `session` -> `epoch` -> `history` / `journal` / `versions`.
//! All four mutexes here are acquired ONLY inside this module's own methods,
//! are never held across any other lock acquisition, and no code path takes
//! the session or gesture lock while holding one of them. Within the module,
//! [`HistoryLog::epoch`] is the outermost of the four (C-1's guard needs
//! the check and the write to be one atomic step) and `history`/`journal`/
//! `versions` are never held at the same time. Concretely: [`HistoryLog`]'s
//! methods are called from `commit_with_rebuild` AFTER the session lock is
//! released, and from the epoch functions after their swap block ends. The
//! journal's disk write happens under the journal mutex — which is legal
//! precisely because it is the leaf: no other lock is held at that moment
//! (this is the same "no I/O under the session lock" rule round-2 §4
//! imposes, satisfied by construction rather than by review).
//!
//! Plan F Task 5 added one more leaf OUTSIDE this module:
//! `Session::published`, the published-snapshot slot. It is taken from
//! under `session` (that is the point — capture and publish happen in the
//! same critical section as the `rev` bump) and, like the three here, never
//! held across another acquisition or across I/O. It neither precedes nor
//! follows the three above, because no path ever holds one of them and the
//! published slot at the same time.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::op::{Actor, Op, TxMeta, OP_FORMAT_VERSION};
use super::vergraph::{Janitor, VersionGraph, VersionItem, VersionStats};
use super::CoalesceKey;
use crate::audio::types::AutomationMode;

// ---------------------------------------------------------------------------
// Tunables (documented constants, not magic numbers)
// ---------------------------------------------------------------------------

/// The boundary-less coalescing window (round-2 §4.4). Two CONSECUTIVE
/// single-key history entries with the same key, the same actor and the
/// same label, landing within this window, fold into one undo step.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(350);

/// Maximum number of entries kept on the undo stack. When the stack is full
/// the LOWEST-REV entry is dropped — the oldest COMMIT, which since Task 11
/// is not necessarily the oldest arrival (a DAW's undo history is a recency
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
    /// The `rev` of the batch this entry describes — the session-lock-minted
    /// sequence number, which is the ONLY total order on commits that
    /// exists. Arrival at the sink is not that order (L-4: `record_commit`
    /// runs after the session lock is released, so two committers can reach
    /// it inverted), and [`History::record`] uses this field to put the
    /// entry where the commit happened rather than where it arrived.
    pub rev: u64,
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
    pub fn from_committed(meta: &TxMeta, ops: &[Op], inverses: &[Op], rev: u64) -> Self {
        Self {
            label: meta.label.clone(),
            actor: meta.actor.clone(),
            run: meta.run.clone(),
            ops: ops.to_vec(),
            inverses: inverses.to_vec(),
            at: Instant::now(),
            rev,
            coalesce: batch_coalesce_key(ops),
        }
    }

    /// Same, but with merging force-disabled — a GESTURE batch. It arrives
    /// PRE-FOLDED (`close_gesture` already reduced the whole drag to one
    /// net op per key), so it is the finished undo step by construction;
    /// letting a 350 ms window merge it with a neighbour would silently
    /// join two deliberate gestures into one, defeating the very boundary
    /// the user's `pointerdown`/`pointerup` drew.
    pub fn from_gesture(meta: &TxMeta, ops: &[Op], inverses: &[Op], rev: u64) -> Self {
        Self { coalesce: None, ..Self::from_committed(meta, ops, inverses, rev) }
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

/// The undo/redo stacks, ordered by [`HistoryEntry::rev`] rather than by
/// arrival (Plan F Task 11, L-4's stack half; ruling F-4 chose ordered
/// consumption over a commit-sequence lock).
///
/// `undo` is kept ASCENDING in `rev`, so [`Self::pop_undo`] takes the
/// highest-rev entry and eviction drops the lowest — both ends of the same
/// invariant. `VecDeque`, not `Vec`, so eviction at the cap is a pop instead
/// of an O(n) memmove on every commit once the stack is full (M-7).
///
/// SCOPE OF THE ORDERING CLAIM, since it is easy to overstate: this orders
/// the STACK, it does not serialize the commits. Two batches that raced
/// still applied to the document in whatever order the session lock granted
/// — `rev` IS that order — and this only guarantees that the stack replays
/// their inverses in the reverse of it. It says nothing about batches that
/// never reach the sink (a stale-epoch drop) or about `redo`, which is
/// ordered by construction: `redo` is populated ONLY by [`Self::push_redo`]
/// after an undo pop, and undo pops are rev-descending, so the redo stack is
/// descending from bottom to top and its `pop_redo` therefore returns the
/// lowest-rev entry — the step to redo first. [`Self::record`] clears it, so
/// no fresh edit can ever interleave a rev into it.
#[derive(Default)]
pub struct History {
    undo: std::collections::VecDeque<HistoryEntry>,
    redo: std::collections::VecDeque<HistoryEntry>,
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
    ///
    /// THE INSERTION POINT (Task 11): the entry goes where its `rev` belongs,
    /// found by scanning from the BACK — which is one comparison for the
    /// ordinary in-order arrival and only walks as far as an inversion
    /// actually reaches. A late-arriving older batch therefore lands BELOW
    /// the newer entries instead of on top of them.
    ///
    /// AN OUT-OF-ORDER ARRIVAL NEVER MERGES. The merge only ever considers
    /// the entry immediately below the insertion point, and only when that
    /// point is the top of the stack (see [`merges`]'s `rev` clause). Merging
    /// an inverted pair would be wrong twice over: the folded entry would
    /// take the OLDER batch's ops as "where redo must land", and it would
    /// have to claim one of two revs it does not span.
    pub fn record(&mut self, e: HistoryEntry) {
        self.redo.clear();
        let at = self.undo.iter().rposition(|x| x.rev <= e.rev).map_or(0, |i| i + 1);
        if at == self.undo.len() {
            if let Some(top) = self.undo.back_mut() {
                if merges(top, &e) {
                    top.ops = e.ops;
                    top.at = e.at;
                    // The folded step now describes the document as of the
                    // NEWER batch, so it must sort as that batch — otherwise
                    // a third entry between the two revs would insert above
                    // a step that already contains it.
                    top.rev = e.rev;
                    return;
                }
            }
        }
        self.undo.insert(at, e);
        if self.undo.len() > UNDO_STACK_LIMIT {
            self.undo.pop_front();
        }
    }

    /// Pop the next step to undo — the back, which [`Self::record`]'s
    /// ordering makes the highest-rev entry rather than merely the
    /// last-arrived one. The caller applies its `inverses` through a normal
    /// commit and then calls [`Self::push_redo`] with the SAME entry on
    /// success (or [`Self::push_undo_unchanged`] to put it back on failure).
    pub fn pop_undo(&mut self) -> Option<HistoryEntry> {
        self.undo.pop_back()
    }

    /// Pop the next step to redo. The caller applies its `ops`.
    pub fn pop_redo(&mut self) -> Option<HistoryEntry> {
        self.redo.pop_back()
    }

    /// Put an entry on the redo stack (after a successful undo). Does NOT
    /// clear anything — that is [`Self::record`]'s job, and a redo future
    /// must survive its own undo.
    pub fn push_redo(&mut self, e: HistoryEntry) {
        self.redo.push_back(e);
    }

    /// Put an entry back on the undo stack — after a successful redo, or to
    /// restore the stack when an undo commit failed. Never merges (the
    /// entry is already a finished step) and never clears the redo stack.
    ///
    /// Pushes onto the BACK without consulting `rev`, and that is correct
    /// rather than an omission: this only ever returns an entry that
    /// [`Self::pop_undo`] just took off the back, and nothing can have been
    /// recorded in between — [`Self::record`] clears `redo`, so a redo
    /// future only exists while no fresh edit has landed, and the failed-undo
    /// path restores the entry inside the same command. Ordering it would
    /// compute the same position at O(n).
    pub fn push_undo_unchanged(&mut self, e: HistoryEntry) {
        self.undo.push_back(e);
        if self.undo.len() > UNDO_STACK_LIMIT {
            self.undo.pop_front();
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
/// direction), the keys/actors/labels must match, the gap must be under the
/// window, and `next` must be the LATER commit.
///
/// The `rev` clause is ordering, not adjacency: `next.rev` only has to
/// EXCEED `top.rev`, never to be `top.rev + 1`. Transient batches consume
/// revs without leaving a history entry, so consecutive undo steps are
/// routinely several revs apart — requiring literal adjacency would silently
/// switch the coalescing fallback off for exactly the knob-drag case round-2
/// §4.4 wrote it for.
///
/// AND IT IS NOT INDEPENDENTLY OBSERVABLE TODAY — measured, not assumed:
/// deleting this one clause leaves the whole suite green. [`History::record`]
/// only consults this predicate when the insertion point is the top of the
/// stack, which already implies `next.rev >= top.rev`, so the clause bites
/// exactly on the residue: two DISTINCT batches carrying the SAME rev. That
/// is not impossible, only unreachable from the tests — `close_gesture`
/// READS `session.rev` rather than minting one, so a gesture entry can share
/// a rev with a concurrent committer's batch that minted it. Merging those
/// two would fold a gesture into an unrelated edit; the clause refuses, and
/// the `coalesce: None` a gesture entry carries refuses first. It stays as
/// depth, on Task 9's `Class` ruling (unobservable defence belongs in a
/// recovery path), and is written down as unobservable — not as impossible —
/// so the next reader does not mistake it for tested behaviour.
fn merges(top: &HistoryEntry, next: &HistoryEntry) -> bool {
    let (Some(a), Some(b)) = (&top.coalesce, &next.coalesce) else { return false };
    a == b
        && top.actor == next.actor
        && top.label == next.label
        && next.rev > top.rev
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
    /// `PluginRemove::params` were added that way) and need no bump.
    ///
    /// The version is 2 as of the Plan E follow-up (I-5): plugin state
    /// blobs are base64 on the wire, not JSON number arrays. That WAS a
    /// breaking change and deliberately shipped without a dual-shape
    /// reader, because the journal was then still write-only — see
    /// `OP_FORMAT_VERSION`'s own doc for why that window was the moment.
    ///
    /// CORRECTION (ADR 0007, Plan F Task 9): the journal is NO LONGER
    /// write-only. [`crate::control::replay`] reads these lines back and
    /// gates each one on `v`, so the freedom that justified the v2 bump is
    /// spent: the next breaking change costs a migration.
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

    /// A boundary record: `{"v":<OP_FORMAT_VERSION>,"epochEvent":"open","epoch":N}`.
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
    /// The document epoch BOTH streams below currently describe (C-1,
    /// whole-branch review). `execute_persist` has always re-checked the
    /// committing epoch against the live one before writing a snapshot;
    /// this is the same discipline at the other sink — see
    /// [`Self::record_commit`].
    ///
    /// LOCK ORDER inside this module: `epoch` -> `history` / `journal`.
    /// `epoch` is the OUTERMOST of the three and is acquired first by every
    /// method that takes more than one of them; `history` and `journal` are
    /// never held simultaneously, so no order between those two exists to
    /// violate. All three remain leaf-most globally (the module doc's
    /// `gesture -> session -> history/journal` is unchanged): nothing takes
    /// the session or gesture lock while holding any of them.
    ///
    /// WHY A MUTEX AND NOT AN `AtomicU64`: the check and the write must be
    /// ATOMIC with respect to [`Self::epoch_boundary`]. A plain atomic read
    /// would leave exactly the window C-1 describes — read the (still old)
    /// epoch, get overtaken by the whole boundary, then append into the NEW
    /// journal and push onto the freshly CLEARED stack.
    epoch: Mutex<u64>,
    history: Mutex<History>,
    journal: Mutex<JournalWriter>,
    /// The retention substrate (Plan F Task 7). A THIRD stream fed from the
    /// same sink, under the same two guards (empty batch, stale epoch) as
    /// the other two — the guards now cover three streams instead of two,
    /// which is the point of there being one sink.
    ///
    /// LOCK ORDER: `epoch` first, as for the others; `versions` is never
    /// held at the same time as `history` or `journal`, and — like them —
    /// never across another acquisition. Evicted loads leave this mutex
    /// before they are disposed of, so no drop ever runs under it.
    versions: Mutex<VersionGraph>,
    /// Where evicted loads go to die: another thread, always. Never behind
    /// a mutex — `dispose` is called after every lock above is released.
    janitor: Janitor,
}

impl Default for HistoryLog {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryLog {
    pub fn new() -> Self {
        Self {
            // `Session::epoch` also starts at 0, so a commit made before any
            // epoch boundary (an unsaved session) matches and records — the
            // guard is a staleness check, not a "needs a project" check.
            epoch: Mutex::new(0),
            history: Mutex::new(History::new()),
            journal: Mutex::new(JournalWriter::closed()),
            versions: Mutex::new(VersionGraph::new()),
            janitor: Janitor::spawn(),
        }
    }

    /// The epoch both streams currently describe. Test/diagnostic accessor;
    /// the guard itself never reads through this (it holds the lock across
    /// the check AND the write — see the field's doc).
    pub fn current_epoch(&self) -> u64 {
        *self.epoch.lock()
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
    /// A STALE-EPOCH BATCH IS RECORDED NOWHERE (C-1, whole-branch review).
    /// `epoch` is the `Committed.epoch` the batch captured under
    /// `Session::transact`'s lock. The window between that lock being
    /// released and this call is not microscopic — it contains
    /// `execute_host_forward` (blocking plugin main-thread round-trips; a
    /// CLAP instantiate/`load_state` runs for hundreds of ms) and
    /// `execute_persist` (disk I/O). An epoch function landing inside it
    /// swaps the document out from under this batch, and then:
    /// * the journal line would go into the NEW project's `journal.ndjson`
    ///   (already rotated by [`Self::epoch_boundary`]) carrying the OLD
    ///   epoch number, describing ops `execute_persist` had already refused
    ///   to write anywhere;
    /// * the history entry would land on the undo stack AFTER
    ///   [`History::clear`] ran — a live, poppable step describing a
    ///   document that is no longer open. `History::clear`'s own doc calls
    ///   that corruption, and re-opening the SAME project (routine) makes it
    ///   reachable rather than theoretical: identical ids mean Ctrl+Z
    ///   applies the inverse SUCCESSFULLY against the wrong revision instead
    ///   of failing cleanly.
    ///
    /// So the batch is dropped, with a warn, in BOTH streams — the same
    /// shape (and the same justification) `execute_persist`'s guard already
    /// uses at the persist sink. The epoch mutex is held across the check
    /// AND both writes, which is what makes the drop atomic with respect to
    /// a concurrent boundary rather than merely likely.
    ///
    /// TAKES THE WHOLE `Committed` (Task 7): the version graph classifies on
    /// `snapshot_charge` and retains `snapshot`, so the six-arg form could
    /// no longer describe the batch. Same values, one argument.
    pub fn record_commit(&self, committed: &super::Committed, mode: HistoryMode) {
        if committed.ops.is_empty() {
            return;
        }
        let (rev, epoch) = (committed.rev, committed.epoch);
        let evicted = {
            let current = self.epoch.lock();
            if epoch != *current {
                log::warn!(
                    "history/journal: dropping batch rev {rev} — epoch changed between commit and \
                     record ({epoch} -> {current}); it describes a document that is no longer open"
                );
                return;
            }
            // Journal FIRST, unconditionally: an undo/redo is a mutation and
            // replay must see it, even though it creates no new history entry.
            self.journal.lock().append_batch(rev, epoch, &committed.meta, &committed.ops);
            if mode == HistoryMode::Record {
                self.history.lock().record(HistoryEntry::from_committed(
                    &committed.meta,
                    &committed.ops,
                    &committed.inverses,
                    rev,
                ));
            }
            self.versions.lock().record(committed)
        };
        // Every mutex above is released here — the drop runs on the janitor,
        // never on the thread that just committed.
        self.janitor.dispose(evicted);
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
    /// The same stale-epoch guard as [`Self::record_commit`], for the same
    /// reasons (C-1). A gesture's window is if anything wider: `close_gesture`
    /// reads `rev`/`epoch` under its own short session lock and then commits
    /// the synthesized batch through the ordinary effect phase before
    /// reaching here.
    ///
    /// Takes a `Committed` too (Task 7), but a SYNTHESIZED one:
    /// `close_gesture` builds it from the folded ops plus the image the
    /// gesture's last transient commit published (which IS the folded
    /// state), because there is no real `Committed` for the net batch.
    pub fn record_gesture(&self, committed: &super::Committed) {
        if committed.ops.is_empty() {
            return;
        }
        let (rev, epoch) = (committed.rev, committed.epoch);
        let evicted = {
            let current = self.epoch.lock();
            if epoch != *current {
                log::warn!(
                    "history/journal: dropping gesture batch rev {rev} — epoch changed between \
                     commit and record ({epoch} -> {current})"
                );
                return;
            }
            self.journal.lock().append_batch(rev, epoch, &committed.meta, &committed.ops);
            self.history.lock().record(HistoryEntry::from_gesture(
                &committed.meta,
                &committed.ops,
                &committed.inverses,
                rev,
            ));
            self.versions.lock().record(committed)
        };
        self.janitor.dispose(evicted);
    }

    /// A DOCUMENT-SWAP epoch boundary (ruling 4): clear history and redo,
    /// rotate the journal onto the new project dir, write the boundary
    /// record. Order matters — the record must land in the NEW journal,
    /// since it describes the document that journal belongs to.
    ///
    /// C-1: this is also where the sink's own notion of "which document am
    /// I logging?" advances. The epoch mutex is taken FIRST and held across
    /// the clear and the rotation, so a concurrent `record_commit` either
    /// completes entirely before the swap (its line lands in the OLD
    /// journal, its entry on the stack this call is about to clear — both
    /// correct) or observes the new epoch and drops itself. There is no
    /// interleaving in which it appends to the new journal.
    pub fn epoch_boundary(&self, dir: &Path, event: EpochEvent, epoch: u64) {
        let drained = {
            let mut current = self.epoch.lock();
            *current = epoch;
            self.history.lock().clear();
            {
                let mut j = self.journal.lock();
                j.open_in(dir);
                j.append_epoch(event, epoch);
            }
            // Task 7: the graph is cleared for `History::clear`'s reason and
            // then some — its nodes hold IMAGES of a document nobody can
            // reach any more, which is the largest thing in the process that
            // a swap makes garbage.
            self.versions.lock().clear(epoch)
        };
        self.janitor.dispose(drained);
    }

    /// The SNAPSHOT MARK (`save_project_mark`): same document, same
    /// content, so history is NOT cleared and the journal is NOT rotated —
    /// only a mark is written, so a replay can tell where the on-disk
    /// snapshot caught up with the log.
    ///
    /// The epoch it is handed is NOT advanced (there was no swap) but it IS
    /// checked, for C-1's reason one line down from the others: a mark whose
    /// epoch has moved would claim, in the NEW project's journal, that a
    /// DIFFERENT document's snapshot caught up with this log.
    pub fn snapshot_mark(&self, epoch: u64) {
        let current = self.epoch.lock();
        if epoch != *current {
            log::warn!(
                "journal: dropping snapshot mark — epoch changed between snapshot and mark \
                 ({epoch} -> {current})"
            );
            return;
        }
        self.journal.lock().append_epoch(EpochEvent::Save, epoch);
    }

    /// Pop the next step to undo, TOGETHER with the epoch it was popped
    /// under (C-1 residual). The caller applies its `inverses` through a
    /// normal commit and hands that epoch back to [`Self::push_redo`] (on
    /// success) or [`Self::push_undo_unchanged`] (on failure), which migrate
    /// the entry only if the document is still the same one.
    ///
    /// LOCK ORDER: `epoch` then `history` — the SAME internal order
    /// [`Self::record_commit`] already uses, so this introduces no new one.
    /// The epoch is read under its own mutex rather than through
    /// [`Self::current_epoch`] so the pop and the read are one step with
    /// respect to a concurrent [`Self::epoch_boundary`]: an interleaved
    /// boundary either clears the stack before this pop (nothing to pop) or
    /// lands after it (and the push-back is then dropped).
    ///
    /// The session lock is NEVER held around this call — `ControlPlane::undo`
    /// pops before `commit_replay` and pushes after it returns.
    pub fn pop_undo(&self) -> Option<(HistoryEntry, u64)> {
        let epoch = self.epoch.lock();
        let entry = self.history.lock().pop_undo()?;
        Some((entry, *epoch))
    }

    /// [`Self::pop_undo`]'s mirror for the redo stack; the caller applies
    /// the entry's `ops`.
    pub fn pop_redo(&self) -> Option<(HistoryEntry, u64)> {
        let epoch = self.epoch.lock();
        let entry = self.history.lock().pop_redo()?;
        Some((entry, *epoch))
    }

    /// Migrate an entry onto the redo stack after a successful undo — ONLY
    /// if the document is still the one it was popped under.
    ///
    /// C-1 RESIDUAL. [`Self::record_commit`]'s guard covers the fresh-edit
    /// sink; undo/redo reach the stacks through this other door, and the
    /// window between the pop and the push is the widest in the system — it
    /// contains a whole commit (plugin main-thread round-trips, disk I/O).
    /// An [`Self::epoch_boundary`] landing inside it clears stacks that no
    /// longer hold this entry, and an unguarded push would then put it back:
    /// a live, poppable step describing a document that is no longer open,
    /// which [`History::clear`]'s own doc calls corruption. So it is dropped
    /// with a warn instead — the entry belongs to a document nobody can
    /// reach any more.
    ///
    /// Same lock order as [`Self::pop_undo`] (`epoch` -> `history`), with
    /// the check and the push under the epoch mutex so the drop is atomic
    /// with respect to a concurrent boundary rather than merely likely.
    pub fn push_redo(&self, e: HistoryEntry, popped_epoch: u64) {
        let current = self.epoch.lock();
        if popped_epoch != *current {
            log::warn!(
                "history: dropping entry {:?} on its way to the redo stack — epoch changed \
                 between pop and push ({popped_epoch} -> {current}); it describes a document \
                 that is no longer open",
                e.label
            );
            return;
        }
        self.history.lock().push_redo(e);
    }

    /// The same migration back onto the undo stack — after a successful
    /// redo, or to restore the stack when an undo's commit failed — with
    /// [`Self::push_redo`]'s guard, for the same reason.
    pub fn push_undo_unchanged(&self, e: HistoryEntry, popped_epoch: u64) {
        let current = self.epoch.lock();
        if popped_epoch != *current {
            log::warn!(
                "history: dropping entry {:?} on its way back to the undo stack — epoch changed \
                 between pop and push ({popped_epoch} -> {current})",
                e.label
            );
            return;
        }
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

    /// What the version graph currently retains. A plain method, not
    /// `#[cfg(test)]`: the integration suite (`tests/snapshot_store.rs`) is
    /// a separate crate and cannot see cfg(test) items, and Task 11 wants it
    /// for the browsing UI's depth report anyway.
    pub fn version_stats(&self) -> VersionStats {
        self.versions.lock().stats()
    }

    /// Read-only descriptors for the version-history product surface.
    pub fn version_overview(&self) -> (VersionStats, Vec<VersionItem>) {
        let versions = self.versions.lock();
        (versions.stats(), versions.items())
    }

    /// Rebuild the document at `rev`, or `None` if that revision was never
    /// recorded or has been evicted.
    ///
    /// TWO PHASES ON PURPOSE: the plan is cloned out under the mutex, the
    /// REPLAY runs after it is released. The replay is CPU-bound (a scratch
    /// session plus an `apply_raw` loop) and holding the leaf-most mutex of
    /// the system across it would stall every committing thread.
    ///
    /// THE RESULT'S `transport` IS NOT THE REV'S — it is the replay base's,
    /// because transport batches commit transiently and leave no node
    /// (`vergraph::ReplayPlan::run` carries the full argument). A caller
    /// restoring this image into the live document must keep the live
    /// transport and take only the content, or loop points, stop-at-end and
    /// recording state rewind silently.
    ///
    /// A document swap landing between the two phases does NOT invalidate
    /// the result — it invalidates its RELEVANCE. The returned image carries
    /// the epoch it belongs to, and a caller that would apply it to the live
    /// document must compare that against the live epoch, exactly as every
    /// other cross-window consumer here does.
    pub fn materialize_version(&self, rev: u64) -> Option<crate::control::SessionSnapshot> {
        let plan = self.versions.lock().replay_plan(rev)?;
        plan.run()
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
                inserts: Vec::new(),
                group: None,
                automation_mode: AutomationMode::Read,
            },
            index: 0,
            clips: vec![],
            clip_indices: vec![],
            automation_clips: vec![],
            bindings: vec![],
        }
    }

    /// A `Committed` for the sink's tests — the batch plus the snapshot and
    /// charge Task 7's version graph needs. `rev`/`epoch` are what the two
    /// guards read; the image is an empty one, because these tests are about
    /// the guards and the stacks, not about content.
    fn committed_for_test(rev: u64, epoch: u64, meta: &TxMeta, ops: &[Op], inverses: &[Op]) -> super::super::Committed {
        super::super::Committed {
            rev,
            epoch,
            ops: ops.to_vec(),
            inverses: inverses.to_vec(),
            effect: crate::control::session::EngineEffect::default(),
            meta: meta.clone(),
            snapshot: std::sync::Arc::new(crate::control::SessionSnapshot::empty()),
            snapshot_charge: 64,
        }
    }

    /// Entries for the merge/bound tests, whose `rev`s only have to be
    /// ASCENDING (the ordering tests below set `rev` explicitly instead).
    /// A monotonic counter is what a same-thread run of commits produces, so
    /// this helper reproduces the ordinary case and never accidentally
    /// exercises the out-of-order one.
    fn entry(label: &str, ops: Vec<Op>, at: Instant) -> HistoryEntry {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_REV: AtomicU64 = AtomicU64::new(1);
        entry_rev(label, ops, at, NEXT_REV.fetch_add(1, Ordering::Relaxed))
    }

    fn entry_rev(label: &str, ops: Vec<Op>, at: Instant, rev: u64) -> HistoryEntry {
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: label.into(), transient: false };
        let inverses = ops.clone();
        HistoryEntry { at, ..HistoryEntry::from_committed(&meta, &ops, &inverses, rev) }
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
        h.record(HistoryEntry::from_gesture(&meta, &ops, &ops, 1));
        h.record(HistoryEntry::from_gesture(&meta, &ops, &ops, 2));
        assert_eq!(h.undo_len(), 2, "two deliberate gestures stay two undo steps");
        // ...and a plain entry does not merge INTO a gesture batch either.
        h.record(entry_rev("fader", vec![set_gain("t-1", -6.0)], Instant::now(), 3));
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

    /// L-4's stack half. `record_commit` runs AFTER the session lock is
    /// released, so two concurrent committers can reach the sink inverted:
    /// the batch that committed FIRST arrives LAST. Under an arrival-ordered
    /// stack the top would then be the OLDER batch, and Ctrl+Z would apply
    /// an older inverse over a newer write while leaving the newer one
    /// undoable — the document ends up in a state no edit ever produced.
    #[test]
    fn a_late_arriving_older_batch_inserts_below_the_newer_one() {
        let mut h = History::new();
        let t0 = Instant::now();
        // Distinct labels throughout so nothing can merge and hide the order.
        h.record(entry_rev("a", vec![set_gain("t-1", -1.0)], t0, 5));
        h.record(entry_rev("c", vec![set_gain("t-1", -3.0)], t0, 7));
        // ...and rev 6 finally arrives, having committed BEFORE rev 7.
        h.record(entry_rev("b", vec![set_gain("t-1", -2.0)], t0, 6));

        assert_eq!(h.undo_len(), 3, "an out-of-order arrival is kept, not dropped");
        let order: Vec<(u64, String)> =
            std::iter::from_fn(|| h.pop_undo()).map(|e| (e.rev, e.label)).collect();
        assert_eq!(
            order,
            vec![(7, "c".to_string()), (6, "b".to_string()), (5, "a".to_string())],
            "undo pops in DESCENDING rev — commit order, not arrival order"
        );

        // A MERGED step must sort as the batch it now ends at, or the very
        // next late arrival sorts above a step that already contains it.
        let mut h2 = History::new();
        h2.record(entry_rev("set gain", vec![set_gain("t-1", -1.0)], t0, 5));
        h2.record(entry_rev("set gain", vec![set_gain("t-1", -2.0)], t0 + Duration::from_millis(100), 8));
        assert_eq!(h2.undo_len(), 1, "the in-order same-key pair still merges");
        h2.record(entry_rev("pan", vec![set_pan("t-1", 0.5)], t0, 6));
        let order: Vec<u64> = std::iter::from_fn(|| h2.pop_undo()).map(|e| e.rev).collect();
        assert_eq!(order, vec![8, 6], "the folded step carries the NEWER rev");
    }

    /// The same inversion at the sink itself, which is the only place that
    /// can get the `rev` wrong: `History::record` cannot order by a number
    /// `record_commit` never handed it. A hard-coded `rev` would leave both
    /// entries equal, restoring arrival order while the unit test above
    /// still passed.
    #[test]
    fn the_sink_carries_the_commits_rev_onto_the_entry() {
        let log = HistoryLog::new();
        let dir = std::env::temp_dir().join(format!("aura-journal-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        log.epoch_boundary(&dir, EpochEvent::Create, 1);

        let meta = |label: &str| TxMeta {
            actor: Actor::User,
            run: "r".into(),
            label: label.into(),
            transient: false,
        };
        let ops = [set_gain("t-1", -3.0)];
        log.record_commit(&committed_for_test(7, 1, &meta("newer"), &ops, &ops), HistoryMode::Record);
        log.record_commit(&committed_for_test(6, 1, &meta("older"), &ops, &ops), HistoryMode::Record);
        assert_eq!(log.depths(), (2, 0));

        let (top, _) = log.pop_undo().expect("a step to undo");
        assert_eq!(top.rev, 7, "the entry learned its batch's rev");
        assert_eq!(top.label, "newer", "the NEWER batch is undone first");
        let (next, _) = log.pop_undo().expect("a second step");
        assert_eq!((next.rev, next.label.as_str()), (6, "older"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Eviction is the OTHER end of the same order. It must drop the lowest
    /// rev, which under an arrival-ordered stack is not the front.
    #[test]
    fn eviction_at_the_cap_drops_the_lowest_rev_entry() {
        let mut h = History::new();
        let t0 = Instant::now();
        // Ascending revs, distinct labels: the ordinary case, cap exceeded.
        for i in 1..=(UNDO_STACK_LIMIT + 10) {
            h.record(entry_rev(&format!("edit {i}"), vec![set_gain("t-1", i as f64)], t0, i as u64));
        }
        assert_eq!(h.undo_len(), UNDO_STACK_LIMIT);
        let newest = h.pop_undo().unwrap();
        assert_eq!(newest.rev, (UNDO_STACK_LIMIT + 10) as u64, "the NEWEST survives");
        h.push_undo_unchanged(newest);

        // Now a batch from BELOW the surviving window arrives late. It is the
        // lowest rev on the stack, so it is the one eviction must drop — and
        // it must never become poppable ahead of anything.
        h.record(entry_rev("straggler", vec![set_gain("t-1", 0.0)], t0, 2));
        assert_eq!(h.undo_len(), UNDO_STACK_LIMIT, "still capped");
        let top = h.pop_undo().unwrap();
        assert_eq!(
            top.rev,
            (UNDO_STACK_LIMIT + 10) as u64,
            "the straggler did not displace the newest entry"
        );
        h.push_undo_unchanged(top);
        assert!(
            std::iter::from_fn(|| h.pop_undo()).all(|e| e.label != "straggler"),
            "the lowest rev is what the cap drops, wherever it arrived"
        );
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
        assert_eq!(epoch["v"], OP_FORMAT_VERSION);
        assert_eq!(epoch["epochEvent"], "create");
        assert_eq!(epoch["epoch"], 1);
        let batch: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(batch["v"], OP_FORMAT_VERSION);
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
        log.record_commit(&committed_for_test(1, 1, &meta, &ops, &ops), HistoryMode::Record);
        assert_eq!(log.depths(), (1, 0));
        log.record_commit(&committed_for_test(2, 1, &meta, &ops, &ops), HistoryMode::Replay);
        assert_eq!(log.depths(), (1, 0), "a replay commit creates no new entry");

        let text = std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap();
        assert_eq!(text.lines().count(), 3, "epoch record + BOTH commits, replay included");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// C-1 at the sink itself: a batch carrying an epoch the sink has moved
    /// past is dropped from BOTH streams. The integration-level version of
    /// this (a real `ControlPlane`, a real re-open of the same project) is
    /// `tests/journal_and_history.rs::
    /// a_commit_whose_sink_call_lands_after_a_document_swap_reaches_neither_stream`.
    #[test]
    fn a_stale_epoch_batch_reaches_neither_the_journal_nor_history() {
        let log = HistoryLog::new();
        let dir = std::env::temp_dir().join(format!("aura-journal-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "mix".into(), transient: false };
        let ops = [set_gain("t-1", -3.0)];

        log.epoch_boundary(&dir, EpochEvent::Create, 1);
        assert_eq!(log.current_epoch(), 1);
        log.record_commit(&committed_for_test(1, 1, &meta, &ops, &ops), HistoryMode::Record);
        assert_eq!(log.depths(), (1, 0));

        // The document is swapped (re-opening the SAME dir, the reachable
        // case): history clears, the journal rotates onto the same file.
        log.epoch_boundary(&dir, EpochEvent::Open, 2);
        assert_eq!(log.depths(), (0, 0));
        let lines_after_swap = std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap().lines().count();

        // The overtaken commit's sink call finally lands, carrying epoch 1.
        log.record_commit(&committed_for_test(2, 1, &meta, &ops, &ops), HistoryMode::Record);
        log.record_gesture(&committed_for_test(2, 1, &meta, &ops, &ops));
        log.snapshot_mark(1);
        assert_eq!(log.depths(), (0, 0), "no stale undo entry survives the swap");
        assert_eq!(
            std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap().lines().count(),
            lines_after_swap,
            "no stale line lands in the new document's journal"
        );

        // ...and the guard is not a mute button: the LIVE epoch still records.
        log.record_commit(&committed_for_test(3, 2, &meta, &ops, &ops), HistoryMode::Record);
        log.snapshot_mark(2);
        assert_eq!(log.depths(), (1, 0));
        assert_eq!(
            std::fs::read_to_string(dir.join(JOURNAL_FILE)).unwrap().lines().count(),
            lines_after_swap + 2
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The C-1 RESIDUAL: `record_commit`'s guard covers the fresh-edit sink,
    /// but undo/redo reach the stacks through a DIFFERENT door — pop the
    /// entry, commit it, push it back. A document swap landing between the
    /// pop and the push used to resurrect the entry onto the NEW document's
    /// stack: `epoch_boundary` cleared stacks that no longer held it, and
    /// the push then put it back, poppable, describing a document that is
    /// no longer open.
    #[test]
    fn a_push_back_after_an_epoch_boundary_drops_the_entry_instead_of_resurrecting_it() {
        let log = HistoryLog::new();
        let dir = std::env::temp_dir().join(format!("aura-journal-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "mix".into(), transient: false };
        let ops = [set_gain("t-1", -3.0)];

        log.epoch_boundary(&dir, EpochEvent::Create, 1);
        log.record_commit(&committed_for_test(1, 1, &meta, &ops, &ops), HistoryMode::Record);
        assert_eq!(log.depths(), (1, 0));

        // The undo pops — and learns WHICH document it popped under.
        let (entry, popped) = log.pop_undo().expect("one step to undo");
        assert_eq!(popped, 1, "the pop reports the epoch it was popped under");
        assert_eq!(log.depths(), (0, 0));

        // ...and the document is swapped before the push-back lands.
        log.epoch_boundary(&dir, EpochEvent::Open, 2);
        log.push_redo(entry, popped);
        assert_eq!(log.depths(), (0, 0), "the migrated entry must not survive the swap");

        // Same door, same guard, the other direction.
        log.record_commit(&committed_for_test(2, 2, &meta, &ops, &ops), HistoryMode::Record);
        let (entry, popped) = log.pop_undo().unwrap();
        assert_eq!(popped, 2);
        log.epoch_boundary(&dir, EpochEvent::Open, 3);
        log.push_undo_unchanged(entry, popped);
        assert_eq!(log.depths(), (0, 0), "a failed undo's restore is dropped across a swap too");

        // The guard is a staleness check, not a mute button: a push at the
        // LIVE epoch migrates normally.
        log.record_commit(&committed_for_test(3, 3, &meta, &ops, &ops), HistoryMode::Record);
        let (entry, popped) = log.pop_undo().unwrap();
        log.push_redo(entry, popped);
        assert_eq!(log.depths(), (0, 1), "same document — the entry migrates onto the redo stack");
        let (entry, popped) = log.pop_redo().expect("one step to redo");
        assert_eq!(popped, 3);
        log.push_undo_unchanged(entry, popped);
        assert_eq!(log.depths(), (1, 0), "and back again");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
