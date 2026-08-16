//! The journal's FIRST READER (Plan F Task 9) — parse, `(epoch, rev)` sort,
//! save-mark tail extraction, and replay onto a scratch session.
//!
//! Until this module existed, `journal.ndjson` was write-only: nothing in
//! the process ever read a line back, so `OP_FORMAT_VERSION` could be bumped
//! without a dual-shape reader (that is exactly what the v1 -> v2 base64
//! bump did). **That freedom ends here.** From this module on, a breaking
//! change to any `Op` variant's wire form costs a version bump AND a reader
//! that understands both shapes — see [`JournalWriter::append_batch`]'s doc
//! and ruling F-5.
//!
//! THE SORT (inventory L-4). File order is NOT rev order: the journal mutex
//! is the leaf, so two committers can serialize their `rev` bumps under the
//! session lock in one order and reach the journal in the other. A reader
//! that trusted file order would replay a later revision's ops before an
//! earlier one's. So every record carries its epoch and (for batches) its
//! rev, and [`read_journal`] sorts by `(epoch, rev)` before returning.
//!
//! WHAT THIS MODULE REFUSES TO DO. It never applies anything by itself, and
//! it never rewrites the journal. A journal is the last thing standing when
//! everything else has failed, so the rules are:
//!
//! * A malformed line is SKIPPED and COUNTED, never fatal and never silent —
//!   [`ReadReport`] carries one counter per reason and every skip logs.
//! * A diverged replay STOPS at the first failing batch and returns `Err`
//!   rather than leaving a partially-applied document behind.
//! * Nothing here auto-applies (ruling F-8). `open_project_epoch` uses the
//!   reader for DETECTION only, and warns.
//!
//! TWO CARRY-FORWARDS THIS MODULE NAMES BUT DOES NOT OWN. Both are about the
//! FILE rather than the parse, both were found by reading this module back,
//! and they want ONE owner because one change closes both:
//!
//! 1. THE JOURNAL GROWS WITHOUT BOUND. It is append-only across every
//!    session of a project, has no rotation and no cap, and auto-persist
//!    means one line per non-transient commit forever. [`read_journal`] does
//!    `read_to_string` of the whole thing, on the command thread, on every
//!    project open.
//! 2. `(epoch, rev)` IS NOT UNIQUE IN THE FILE. `Session::new` starts every
//!    process at `epoch: 0`/`rev: 0`, so the coordinate space is
//!    process-local while the file is not — see [`read_journal`]'s duplicate
//!    note for what that does to the sort.
//!
//! WHY (2) IS NOT FIXED HERE, argued rather than asserted. The obvious patch
//! — seed `session.epoch` above the journal's max when a project opens — is
//! a change to the EPOCH PATH, not to the reader: it would put file I/O in
//! front of a value the session lock's critical section depends on, and the
//! epoch is consumed by C-1's sink guard, `execute_persist`'s skip check,
//! `snapshot_mark`'s check and `VersionGraph::clear`. It is also only half a
//! fix, because `rev` restarts too, and seeding THAT reaches into `Committed`
//! and the version graph. Per-session journal SEGMENTS fix both this and (1),
//! cost the reader only a directory listing, and leave the epoch path alone —
//! which is why the recommendation is to give (1) and (2) the same owner and
//! not to spend the epoch path's invariants on a reader's convenience.

use std::path::Path;

use parking_lot::Mutex;

use super::op::{Actor, Op, TxMeta, OP_FORMAT_VERSION};
use super::session::Session;

/// One parsed journal line.
///
/// Unknown fields are tolerated (D-06): a line written by a newer AURA that
/// added a `#[serde(default)]` field still reads here, which is the whole
/// point of the additive-fields rule.
#[derive(Debug, Clone, PartialEq)]
pub enum JournalRecord {
    /// A committed, non-transient batch: `{"v":2,"rev":N,"epoch":E,...}`.
    Batch {
        v: u16,
        rev: u64,
        epoch: u64,
        actor: Actor,
        run: String,
        label: String,
        ops: Vec<Op>,
    },
    /// A DOCUMENT-SWAP boundary: `{"epochEvent":"open"|"create"|"saveAs"|
    /// "ensure","epoch":E}`. The `event` stays a `String` rather than
    /// becoming `EpochEvent`, deliberately: a reader must not fail on an
    /// event name a future writer invents.
    Epoch { event: String, epoch: u64 },
    /// The snapshot mark: `{"epochEvent":"save","epoch":E}`. "The on-disk
    /// snapshot had caught up with the log HERE."
    SaveMark { epoch: u64 },
}

impl JournalRecord {
    pub fn epoch(&self) -> u64 {
        match self {
            JournalRecord::Batch { epoch, .. } => *epoch,
            JournalRecord::Epoch { epoch, .. } => *epoch,
            JournalRecord::SaveMark { epoch } => *epoch,
        }
    }
}

/// The outcome of reading one journal file: the records IN `(epoch, rev)`
/// ORDER, plus one counter per reason a line was dropped.
///
/// The counters are not decoration. Silently dropping entries is the worst
/// thing a recovery path can do, so every skip is both counted here and
/// logged at warn level; a caller that wants to refuse to trust a journal
/// has the numbers to decide with.
///
/// DEVIATION from the plan's sketch (which listed only `skipped_v1` and
/// `torn_tail`): a v1 line, a line from a FUTURE format version, and a line
/// that is well-formed JSON but not a well-formed record are three different
/// facts about a journal, and folding them into one counter would leave the
/// second two with no defined answer at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReadReport {
    /// In `(epoch, rev)` order — see [`read_journal`].
    pub records: Vec<JournalRecord>,
    /// Batch lines whose `v` is BELOW [`OP_FORMAT_VERSION`] (ruling F-5:
    /// the v1 -> v2 bump shipped without a dual-shape reader, so v1 ops are
    /// unreadable by construction, not by omission).
    pub skipped_v1: usize,
    /// Batch lines whose `v` is ABOVE [`OP_FORMAT_VERSION`] — written by a
    /// NEWER AURA. Skipped for the same reason and counted separately,
    /// because "this journal is from the future" is a different diagnosis
    /// from "this journal is from the past".
    pub skipped_future: usize,
    /// Lines that were not a usable record for any other reason: not JSON,
    /// not an object, no recognizable shape, or a shape whose fields failed
    /// to deserialize (a bad `rev`, an op variant this build does not know).
    /// Excludes the torn tail below, which has its own, benign explanation.
    pub skipped_malformed: usize,
    /// The file's LAST line was unusable AND the file did not end in a
    /// newline — i.e. the process died mid-write. Every line is written with
    /// one `write_all` of `<json>\n` followed by a flush, so a missing final
    /// newline is the signature of a truncated write and nothing else. The
    /// line is skipped and this is NOT counted as corruption.
    pub torn_tail: bool,
}

impl ReadReport {
    /// Total lines dropped for a reason that is NOT the torn tail.
    pub fn skipped(&self) -> usize {
        self.skipped_v1 + self.skipped_future + self.skipped_malformed
    }
}

/// Which "class" a record sorts into WITHIN one `(epoch, anchor rev)`.
/// Ordering is the point: an epoch record heads its epoch, a save mark
/// trails the batch it anchors to.
///
/// DEFENCE IN DEPTH, and measured as such: neither distinction is
/// independently observable today. A mark's anchor is the rev of the batch
/// that precedes it IN FILE ORDER, so a stable sort already places it after
/// that batch even if it shared the batch's class (sabotage measured: no
/// test turns red); and an epoch record heads its epoch already because
/// `rev` starts at 1, so no batch ever ties with its anchor of 0. The class
/// makes the order TOTAL and intentional instead of resting on those two
/// coincidences — a rev-0 batch or a differently-anchored mark would break
/// them silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    Epoch = 0,
    Batch = 1,
    SaveMark = 2,
}

/// Read, parse and SORT one journal file.
///
/// THE SORT KEY is `(epoch, anchor rev, class, file position)`:
///
/// * `epoch` first — a document swap is a hard boundary, and revisions from
///   two epochs are revisions of two different documents.
/// * an `Epoch` record anchors at rev 0 with the lowest class, so it heads
///   its own epoch regardless of where in the file it landed.
/// * a `Batch` anchors at its own `rev`.
/// * a `SaveMark` CARRIES NO REV. Its meaning is positional — "the snapshot
///   caught up HERE" — so it anchors to the HIGHEST rev among the batches
///   that precede it in file order within its epoch, with the highest class.
///   That places it after every batch already written and before the next
///   one, which is exactly what the mark asserts. A mark before any batch
///   in its epoch anchors at 0, i.e. right after the epoch record.
///
///   HIGHEST-SO-FAR, NOT LAST-SEEN, and the difference is the whole reason
///   this module exists. If the file holds rev 5 then rev 3 then a mark, the
///   snapshot the mark describes contains BOTH (the mark was written after
///   both commits landed); anchoring on the last line seen would put the
///   mark at 3 and hand rev 5 back as unsaved work to be re-applied. Pinned
///   by `a_mark_anchors_past_a_higher_rev_that_arrived_out_of_file_order` —
///   the literal last-seen rule leaves every other test in this module
///   green.
/// * file position last, via a STABLE sort: two records with an identical
///   key keep the order the file gave them.
///
/// DUPLICATE `(epoch, rev)` PAIRS ARE NOT AN ERROR and are not deduplicated.
/// A reader that silently dropped one of a pair would be choosing which of
/// two mutations to forget, which is not a choice a recovery path may make
/// on its own. They come back adjacent, in file order — the stable sort is
/// the CONTRACT here, not an implementation detail.
///
/// THEY ARE ALSO ROUTINE, not hypothetical. A rev is minted once per
/// process, under the session lock — but `journal.ndjson` is APPEND-ONLY
/// ACROSS PROCESSES, while `Session::new` starts every process at `epoch: 0`
/// and `rev: 0`. So the epoch namespace is process-local and the file is
/// not: run AURA twice against the same project and the file carries two
/// `(1, 1)`s, two `(1, 2)`s, and so on, belonging to different sessions.
/// Sorted by `(epoch, rev)` they interleave. See [`unsaved_tail`] for what
/// that costs and who owns the fix.
///
/// GAPS IN `rev` ARE NORMAL, not damage: transient batches bump `rev` and
/// are deliberately never journaled (round-2 §4.4), so a healthy journal is
/// sparse. Nothing here requires contiguity.
///
/// `Err` only for I/O — an unreadable or absent file. Every content problem
/// is a counter on [`ReadReport`], never an error: a journal with one bad
/// line is still a journal.
pub fn read_journal(path: &Path) -> Result<ReadReport, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(parse_journal(&text))
}

/// [`read_journal`]'s pure half — the whole file's text in, the report out.
/// Split out so the parse rules can be tested without a temp directory, and
/// so a future streaming reader can reuse the line rules.
pub fn parse_journal(text: &str) -> ReadReport {
    let mut report = ReadReport::default();
    let ends_with_newline = text.is_empty() || text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    // Anchor rev per epoch, advanced as batches are seen IN FILE ORDER —
    // this is what gives a save mark its position (see the fn doc).
    let mut anchor: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut keyed: Vec<((u64, u64, Class), JournalRecord)> = Vec::with_capacity(lines.len());

    for (i, line) in lines.iter().enumerate() {
        let is_last = i + 1 == lines.len();
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Ok(rec) => {
                let epoch = rec.epoch();
                let key = match &rec {
                    JournalRecord::Epoch { .. } => (epoch, 0, Class::Epoch),
                    JournalRecord::Batch { rev, .. } => {
                        // HIGHEST so far, not last seen — see the fn doc:
                        // a mark written after an out-of-order pair must not
                        // anchor below the higher of the two.
                        let a = anchor.entry(epoch).or_insert(0);
                        *a = (*a).max(*rev);
                        (epoch, *rev, Class::Batch)
                    }
                    JournalRecord::SaveMark { .. } => {
                        (epoch, *anchor.get(&epoch).unwrap_or(&0), Class::SaveMark)
                    }
                };
                keyed.push((key, rec));
            }
            Err(reason) => {
                if is_last && !ends_with_newline {
                    log::warn!("journal: torn final line ({reason}) — the writer was interrupted");
                    report.torn_tail = true;
                } else {
                    log::warn!("journal: skipping unusable line {}: {reason}", i + 1);
                    match reason {
                        SkipReason::OldVersion(_) => report.skipped_v1 += 1,
                        SkipReason::FutureVersion(_) => report.skipped_future += 1,
                        SkipReason::Malformed(_) => report.skipped_malformed += 1,
                    }
                }
            }
        }
    }

    // STABLE — equal keys (a duplicate `(epoch, rev)` pair) keep file order.
    keyed.sort_by_key(|(k, _)| *k);
    report.records = keyed.into_iter().map(|(_, r)| r).collect();
    report
}

enum SkipReason {
    OldVersion(u16),
    FutureVersion(u16),
    Malformed(String),
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::OldVersion(v) => {
                write!(f, "batch written at op format v{v}, this build reads v{OP_FORMAT_VERSION}")
            }
            SkipReason::FutureVersion(v) => {
                write!(f, "batch written at op format v{v} by a NEWER build (this one reads v{OP_FORMAT_VERSION})")
            }
            SkipReason::Malformed(e) => write!(f, "{e}"),
        }
    }
}

/// The wire shape of a batch line. `#[serde(default)]` nowhere: a batch line
/// missing `rev`, `epoch`, `actor`, `run`, `label` or `ops` is not a batch
/// this reader will guess at.
#[derive(serde::Deserialize)]
struct BatchLine {
    v: u16,
    rev: u64,
    epoch: u64,
    actor: Actor,
    run: String,
    label: String,
    ops: Vec<Op>,
}

#[derive(serde::Deserialize)]
struct EpochLine {
    #[serde(rename = "epochEvent")]
    epoch_event: String,
    epoch: u64,
}

fn parse_line(line: &str) -> Result<JournalRecord, SkipReason> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| SkipReason::Malformed(format!("not JSON: {e}")))?;
    if !value.is_object() {
        return Err(SkipReason::Malformed("not a JSON object".into()));
    }
    if value.get("epochEvent").is_some() {
        let e: EpochLine = serde_json::from_value(value)
            .map_err(|e| SkipReason::Malformed(format!("bad epoch record: {e}")))?;
        // The version is deliberately NOT gated on a boundary record: its
        // shape ({epochEvent, epoch}) has never changed, and refusing to
        // read a v1 save mark would move a v2 tail's start backwards, which
        // is the one direction that costs re-applied work.
        return Ok(if e.epoch_event == "save" {
            JournalRecord::SaveMark { epoch: e.epoch }
        } else {
            JournalRecord::Epoch { event: e.epoch_event, epoch: e.epoch }
        });
    }
    if value.get("ops").is_some() {
        // Version FIRST: a v1 line's ops cannot deserialize into this
        // build's `Op` (base64 vs number arrays, I-5), and reporting that as
        // "malformed" would blame the file for a decision this project made
        // knowingly.
        match value.get("v").and_then(|v| v.as_u64()) {
            Some(v) if v < OP_FORMAT_VERSION as u64 => {
                return Err(SkipReason::OldVersion(v as u16))
            }
            Some(v) if v > OP_FORMAT_VERSION as u64 => {
                return Err(SkipReason::FutureVersion(v as u16))
            }
            Some(_) => {}
            None => return Err(SkipReason::Malformed("batch line has no `v`".into())),
        }
        let b: BatchLine = serde_json::from_value(value)
            .map_err(|e| SkipReason::Malformed(format!("bad batch record: {e}")))?;
        return Ok(JournalRecord::Batch {
            v: b.v,
            rev: b.rev,
            epoch: b.epoch,
            actor: b.actor,
            run: b.run,
            label: b.label,
            ops: b.ops,
        });
    }
    Err(SkipReason::Malformed("neither a batch (`ops`) nor a boundary (`epochEvent`)".into()))
}

/// THE UNSAVED TAIL: the batches of the report's LAST EPOCH THAT CONTAINS A
/// BATCH, that come after that epoch's last save mark — or ALL of that
/// epoch's batches when it has no mark at all. Empty means the on-disk
/// snapshot is current with the log.
///
/// Only one epoch, because every earlier epoch describes a document that has
/// since been swapped out; replaying its ops onto today's document would
/// apply one project's edits to another (ruling 4, the same reason a swap
/// clears the undo stacks).
///
/// WHY "LAST EPOCH THAT CONTAINS A BATCH" AND NOT "MAX EPOCH". Every epoch
/// boundary APPENDS ITS OWN RECORD to the journal it is opening, so a file
/// routinely ends in an epoch that has no batches yet. Under a plain
/// max-epoch rule that empty epoch wins and the tail is empty — the detector
/// reports "nothing unsaved" precisely when a document was just re-opened
/// with unsaved work in its log, which is the one moment the answer matters.
/// Fix round 1 shipped this predicate AND moved the only production caller
/// ahead of its `epoch_boundary` call; either alone would do, and defence in
/// depth is what a recovery path is for. Pinned by
/// `the_tail_survives_the_epoch_record_an_open_appends_before_reading`.
///
/// A MARK AT THE VERY END gives an empty tail — the normal, healthy case.
/// A MARK IS NEVER "BEYOND THE END": it has no index into anything, only a
/// position among records, so there is no out-of-range case to handle.
///
/// HOW EXACT THE TAIL IS — stated at the width the proof actually supports,
/// because the two earlier versions of this paragraph were both absolute and
/// both wrong in the direction that flatters the design (ADR 0007; fix round
/// 1 claimed the tail could re-offer preceding batches, fix round 2 claimed
/// saved work is never re-offered at all).
///
/// WHAT IS PROVED, and it is narrow: **no batch that precedes the mark in
/// file order can enter the tail.** A batch enters only if its rev is
/// STRICTLY above the mark's anchor (equal revs sort before the mark, since
/// `Class::Batch < Class::SaveMark`), and the anchor is the maximum rev over
/// exactly those preceding batches. Pinned by
/// `a_batch_written_before_the_mark_never_enters_the_tail_however_torn_the_file`.
///
/// WHAT IS NOT TRUE: that saved work is never re-offered. A batch can FOLLOW
/// the mark in file order and still be saved — revs are minted under the
/// session lock, so a lower rev is an earlier mutation and a late LINE is not
/// a late mutation (which is the whole reason this reader sorts by rev
/// instead of trusting the file). Such a batch is excluded only while its rev
/// stays under the anchor, and an unreadable line at the pre-mark high-water
/// rev drops the anchor beneath it:
///
/// ```text
/// b rev1   UNREADABLE rev9   b rev4   [MARK]   b rev7   b rev14
/// anchor = max(1, 4) = 4  ->  tail = [rev7, rev14]
/// ```
///
/// rev 7 was committed before rev 9 and the snapshot had reached rev 9, so it
/// is saved — and it is offered back. Pinned by
/// `an_unreadable_line_at_the_high_water_rev_re_offers_saved_work`.
///
/// THE TRUE GENERAL PROPERTY, and the reason the design is still right: the
/// mark CARRIES NO REV, so the anchor is only a LOWER BOUND on the revision
/// the snapshot actually reached. The tail is therefore a SUPERSET of the
/// genuinely-unsaved batches — over-wide, never under-wide — and unreadable
/// lines only widen it further. Over-wide is the safe direction for a
/// detector (it over-reports work at risk, never under-reports it) and the
/// unsafe direction for an applier, which is why re-application is
/// human-gated (ruling F-8) and why a future "restore unsaved work"
/// affordance MUST NOT read this as "replay cannot duplicate saved work".
/// Giving the mark the rev it was written at would make the tail exact; that
/// is a journal FORMAT change, and it belongs with the segmentation work this
/// module's header hands forward.
///
/// TWO RESIDUALS IN THE OTHER DIRECTION — work goes missing — each
/// survivable only because nothing auto-applies:
///
/// * AN UNREADABLE BATCH LINE IS LOST, full stop: it is counted in
///   `skipped_malformed` and can never be replayed, whether it was saved or
///   not. The counter is the whole defence.
/// * `ControlPlane::save_project_mark` releases the session lock, writes
///   `project.json`, and only then calls `HistoryLog::snapshot_mark`. A
///   commit that journals inside that window lands BEFORE the mark in file
///   order and is classified as saved, even though the snapshot on disk
///   predates it. This is also the one thing that can push the anchor ABOVE
///   the snapshot's revision, so it is the exception to the superset property
///   above — with that window open, the tail can be under-wide. Closing it
///   means moving the mark under the same critical section as the snapshot —
///   a locking decision on the save path (and one round-2 §4's
///   no-disk-I/O-under-the-session-lock rule constrains), not a reader
///   change. Named here so the next person on that path knows the reader
///   depends on it.
pub fn unsaved_tail(report: &ReadReport) -> Vec<&JournalRecord> {
    let Some(last_epoch) = report
        .records
        .iter()
        .filter(|r| matches!(r, JournalRecord::Batch { .. }))
        .map(|r| r.epoch())
        .max()
    else {
        return vec![];
    };
    let in_epoch: Vec<&JournalRecord> =
        report.records.iter().filter(|r| r.epoch() == last_epoch).collect();
    let after = in_epoch
        .iter()
        .rposition(|r| matches!(r, JournalRecord::SaveMark { .. }))
        .map(|i| i + 1)
        .unwrap_or(0);
    in_epoch[after..]
        .iter()
        .copied()
        .filter(|r| matches!(r, JournalRecord::Batch { .. }))
        .collect()
}

/// OPEN-TIME DETECTION (ruling F-8): read the project's journal, warn if it
/// records batches after its last save mark, and return how many. Detection
/// ONLY — nothing is applied, no event is emitted, no UI is involved.
///
/// Best effort in both directions: a project with no journal (or an
/// unreadable one) is normal, not an error, so an I/O failure returns 0 after
/// a debug line.
///
/// **CALL IT BEFORE THE EPOCH BOUNDARY, NOT AFTER.**
/// `HistoryLog::epoch_boundary` appends `{"epochEvent":"open","epoch":N}` to
/// the very file this reads, and N is higher than anything already in it. Fix
/// round 1's Critical: called after the boundary, this returned 0 on every
/// re-open within a process run — the detector could not detect. (Plan defect
/// #17: the plan's own step text said "after the adopt steps".) Reading needs
/// neither the boundary nor the adopts. [`unsaved_tail`]'s
/// last-epoch-with-a-batch rule now covers the same hole from the other side;
/// keep BOTH.
///
/// WHAT A NON-ZERO COUNT ACTUALLY MEANS, stated precisely because the
/// obvious reading is wrong for today's AURA: it means "the log records edits
/// the user never explicitly saved", NOT "the files on disk are stale".
/// Nearly every op carries a `PersistEffect` and `execute_persist` writes it
/// out right after the commit, so the disk usually IS current and a non-zero
/// tail is expected on any project edited since its last Ctrl+S. The signal
/// becomes a real staleness signal exactly when a persist FAILED (each one
/// only warns) or when the process died between a commit and its persist.
///
/// AND IT COUNTS ACROSS PROCESS RUNS. The journal is append-only across
/// sessions while `epoch`/`rev` restart at 0 in each one (see
/// [`read_journal`]'s duplicate note), so on a project opened for the Nth
/// time this count can span several sessions' batches sharing one epoch
/// number. That is fine for a warning — "there is unsaved work in the log"
/// stays true — and is exactly why the number must not be fed to
/// [`replay_tail`] without a human in the loop.
///
/// COST AND THREAD: runs on the calling command thread, holding NO lock, and
/// parses the whole journal — which is append-only ACROSS SESSIONS and has no
/// rotation policy yet. That is O(journal) work on every project open. It is
/// after the session lock is released so it blocks no other thread, but it
/// does sit on the open path; a size cap or a reverse scan is the fix if
/// journals ever grow past a few tens of megabytes.
pub fn detect_unsaved_tail(dir: &Path) -> usize {
    let report = match read_journal(&dir.join(super::history::JOURNAL_FILE)) {
        Ok(r) => r,
        Err(e) => {
            log::debug!("journal: nothing to read in {}: {e}", dir.display());
            return 0;
        }
    };
    if report.skipped() > 0 || report.torn_tail {
        log::warn!(
            "journal: {} unusable line(s) in {} (v1: {}, newer format: {}, malformed: {}, torn \
             tail: {}) — they were skipped, not applied",
            report.skipped() + usize::from(report.torn_tail),
            dir.display(),
            report.skipped_v1,
            report.skipped_future,
            report.skipped_malformed,
            report.torn_tail,
        );
    }
    let n = unsaved_tail(&report).len();
    if n > 0 {
        log::warn!(
            "journal: {n} batch(es) recorded after the last save mark — the on-disk snapshot may \
             be behind the log"
        );
    }
    n
}

/// Apply a tail to a session, one `transact` per journaled batch, in the
/// order [`unsaved_tail`] returned them. Returns how many batches applied.
///
/// STOPS AND RETURNS `Err` ON THE FIRST FAILING BATCH, leaving the batches
/// before it applied. A journal that has diverged from the document it is
/// replayed onto must be loud: continuing past a failure would apply a
/// SUFFIX of an edit history onto a state its prefix never reached, which is
/// a plausible-looking document that never existed. The caller decides what
/// to do; nothing in this plan calls this automatically.
///
/// ONE `transact` PER BATCH, not one per op: a batch was atomic when it was
/// committed and must be atomic when it is replayed, so a batch whose third
/// op fails rolls its first two back (`transact`'s own Err arm) instead of
/// leaving a half-batch.
///
/// FOR A SCRATCH SESSION. `transact` publishes and bumps `rev`, but it does
/// NOT journal — journaling lives in `Committer::commit_with_rebuild`. So
/// replaying onto the LIVE session would mutate the document without leaving
/// a record, which is precisely the divergence this whole module exists to
/// detect. Build the base with [`Session::from_snapshot`] or from the disk
/// loaders and replay onto that.
///
/// WHY THE REPLAYED IDS MATCH (inventory L-2, ruling F-9). `apply_raw` is
/// deterministic: it mints nothing, reads no clock and no RNG — every id a
/// replay produces comes either from the op payload itself (`TrackAdd`,
/// `MidiClipAdd` carry whole rows) or from a watermark that is part of the
/// document being replayed onto (`MidiSetNotes`' `noteId: 0` sentinels are
/// minted from the clip's persisted `next_note_id`, ADR 0001). Both inputs
/// are on disk, so replay is reproducible rather than merely plausible —
/// which is what `tests/journal_replay.rs` asserts byte-identically instead
/// of masking note ids.
///
/// `rev` IS NOT REPRODUCED, and cannot be: transient batches bump `rev`
/// without leaving a line, so the replayed session's revision counter counts
/// journaled batches, not original revisions. The same hole gives the
/// replayed session the BASE's transport (Task 7's I-2) — content is
/// reproduced, bookkeeping is not.
pub fn replay_tail(session: &Mutex<Session>, tail: &[&JournalRecord]) -> Result<usize, String> {
    let mut applied = 0usize;
    for rec in tail {
        let JournalRecord::Batch { rev, actor, run, label, ops, .. } = rec else {
            return Err("replay_tail: not a batch record (unsaved_tail returns batches only)".into());
        };
        let meta = TxMeta {
            actor: actor.clone(),
            run: run.clone(),
            label: label.clone(),
            // A journaled batch is by definition non-transient — transient
            // ones never reach the journal at all.
            transient: false,
        };
        Session::transact(session, meta, |tx| {
            for op in ops {
                tx.apply(op.clone())?;
            }
            Ok(())
        })
        .map_err(|e| format!("replay stopped at rev {rev} after {applied} batch(es): {e}"))?;
        applied += 1;
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::history::{EpochEvent, JournalWriter, JOURNAL_FILE};
    use crate::control::op::{ObjectRef, PropPath};

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-replay-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A batch line, built as JSON text — `JournalWriter` has no
    /// out-of-order mode, and out-of-order is exactly what the sort exists
    /// for.
    fn batch_line(rev: u64, epoch: u64, label: &str) -> String {
        serde_json::json!({
            "v": OP_FORMAT_VERSION,
            "rev": rev,
            "epoch": epoch,
            "actor": "user",
            "run": "r-1",
            "label": label,
            "ops": [set_gain_json()],
        })
        .to_string()
    }

    fn set_gain_json() -> serde_json::Value {
        serde_json::to_value(Op::Set {
            object: ObjectRef::Track("t-1".into()),
            path: PropPath::Gain,
            from: serde_json::Value::Null,
            to: serde_json::json!(-3.0),
        })
        .unwrap()
    }

    fn epoch_line(event: &str, epoch: u64) -> String {
        serde_json::json!({ "v": OP_FORMAT_VERSION, "epochEvent": event, "epoch": epoch })
            .to_string()
    }

    fn labels(records: &[JournalRecord]) -> Vec<String> {
        records
            .iter()
            .map(|r| match r {
                JournalRecord::Batch { label, .. } => label.clone(),
                JournalRecord::Epoch { event, epoch } => format!("epoch:{event}:{epoch}"),
                JournalRecord::SaveMark { epoch } => format!("save:{epoch}"),
            })
            .collect()
    }

    #[test]
    fn records_are_sorted_by_epoch_then_rev_regardless_of_file_order() {
        // Written 3, 1, 2 within epoch 1, and the epoch's own boundary
        // record written LAST — file order is nothing like rev order.
        let text = [
            batch_line(3, 1, "third"),
            batch_line(1, 1, "first"),
            batch_line(2, 1, "second"),
            epoch_line("create", 1),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(
            labels(&r.records),
            vec!["epoch:create:1", "first", "second", "third"],
            "(epoch, rev) order, with the boundary record at its epoch's head"
        );
        assert_eq!(r.skipped(), 0);
        assert!(!r.torn_tail);
    }

    #[test]
    fn an_earlier_epochs_high_rev_still_sorts_before_a_later_epochs_low_rev() {
        // The epoch is the OUTER key: a swap resets nothing about `rev`
        // ordering by itself, so a reader keyed on rev alone would replay
        // the old document's rev 9 after the new document's rev 2.
        let text = [
            batch_line(2, 2, "new-doc-rev-2"),
            batch_line(9, 1, "old-doc-rev-9"),
            epoch_line("open", 2),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(
            labels(&r.records),
            vec!["old-doc-rev-9", "epoch:open:2", "new-doc-rev-2"],
        );
    }

    #[test]
    fn duplicate_epoch_rev_pairs_are_kept_in_file_order_not_dropped() {
        // Two lines claiming the same revision should be impossible (a rev
        // is minted once, under the session lock). If it happens anyway, a
        // reader that dropped one would be choosing which mutation to
        // forget — so both survive, adjacent, in the order the file gave.
        // Duplicates are ROUTINE, not hypothetical: `epoch`/`rev` restart at
        // 0 in every process while the file is append-only across them, so
        // any project opened twice has them (see `read_journal`'s doc). File
        // order for equal keys is therefore the CONTRACT — it is what keeps
        // one session's revisions in their own sequence instead of shuffled
        // against another's.
        //
        // 30 collisions, not 3 — and MEASURED: swapping the implementation
        // to `sort_unstable_by_key` does NOT turn this red at 30, 200 or
        // 2000 identical keys (pdqsort leaves an all-equal run alone). The
        // contract is pinned by outcome; the specific sort cannot be pinned
        // cheaply, which is why `sort_by_key` (stable BY GUARANTEE, not by
        // observed behaviour) is the one to keep.
        let mut lines: Vec<String> = (0..30).map(|i| batch_line(1, 1, &format!("dup-{i:02}"))).collect();
        lines.push(batch_line(2, 1, "later"));
        let r = parse_journal(&(lines.join("\n") + "\n"));
        let mut expected: Vec<String> = (0..30).map(|i| format!("dup-{i:02}")).collect();
        expected.push("later".into());
        assert_eq!(labels(&r.records), expected);
        assert_eq!(r.skipped(), 0, "a duplicate is not a skip");
    }

    #[test]
    fn gaps_in_rev_are_normal_and_an_empty_journal_is_not_an_error() {
        // Transient batches bump `rev` without journaling, so 1, 4, 9 is a
        // HEALTHY journal, not a damaged one.
        let text = [batch_line(9, 1, "c"), batch_line(1, 1, "a"), batch_line(4, 1, "b")]
            .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(labels(&r.records), vec!["a", "b", "c"]);
        assert_eq!(r.skipped(), 0);

        let empty = parse_journal("");
        assert!(empty.records.is_empty());
        assert!(!empty.torn_tail, "an empty file is not a torn one");
        assert_eq!(empty.skipped(), 0);
        assert!(unsaved_tail(&empty).is_empty(), "nothing recorded, nothing unsaved");
    }

    #[test]
    fn v1_batch_lines_are_skipped_and_counted_and_a_torn_tail_is_tolerated() {
        let v1 = serde_json::json!({
            "v": 1, "rev": 1, "epoch": 1, "actor": "user", "run": "r", "label": "old",
            "ops": [set_gain_json()],
        })
        .to_string();
        let future = serde_json::json!({
            "v": 99, "rev": 2, "epoch": 1, "actor": "user", "run": "r", "label": "future",
            "ops": [set_gain_json()],
        })
        .to_string();
        // A final line cut off mid-write: no closing brace, NO trailing
        // newline. That combination is the writer's own signature for
        // "interrupted", since every complete line ends in `\n`.
        let torn = r#"{"v":2,"rev":4,"epoch":1,"actor":"user","run":"r","label":"tor"#;
        let text = format!(
            "{}\n{}\n{}\n{}\n{}",
            epoch_line("create", 1),
            v1,
            batch_line(3, 1, "good"),
            future,
            torn
        );
        let r = parse_journal(&text);
        assert_eq!(labels(&r.records), vec!["epoch:create:1", "good"], "only the readable v2 batch");
        assert_eq!(r.skipped_v1, 1, "the v1 line is counted, not silently gone");
        assert_eq!(r.skipped_future, 1, "and a future-version line is a DIFFERENT diagnosis");
        assert_eq!(r.skipped_malformed, 0, "a version skip is not corruption");
        assert!(r.torn_tail, "the interrupted final line is tolerated and flagged");
    }

    #[test]
    fn a_valid_json_line_that_is_not_a_valid_record_is_counted_as_malformed() {
        // Each of these is well-formed JSON and none is a usable record.
        let text = [
            epoch_line("create", 1).as_str(),
            r#"{"hello":"world"}"#,                        // neither shape
            r#"[1,2,3]"#,                                  // not an object
            r#"{"v":2,"rev":"one","epoch":1,"actor":"user","run":"r","label":"l","ops":[]}"#, // rev is a string
            r#"{"v":2,"epoch":1,"actor":"user","run":"r","label":"l","ops":[]}"#, // no rev at all
            r#"{"v":2,"rev":5,"epoch":1,"actor":"user","run":"r","label":"l","ops":[{"kind":"noSuchOp"}]}"#,
            r#"{"epochEvent":"save"}"#,                    // a mark with no epoch
            &batch_line(7, 1, "survivor"),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(labels(&r.records), vec!["epoch:create:1", "survivor"]);
        assert_eq!(r.skipped_malformed, 6, "every unusable line counted, one by one");
        assert_eq!(r.skipped_v1 + r.skipped_future, 0);
        assert!(!r.torn_tail, "a bad line that is NOT the truncated tail is corruption, not tearing");
    }

    #[test]
    fn a_bad_final_line_with_a_trailing_newline_is_corruption_not_a_torn_tail() {
        // The discriminator is the newline: the writer emits `<json>\n` in
        // one `write_all` + flush, so a complete final newline means the
        // write finished and the damage came from somewhere else.
        let text = format!("{}\n{{\"nope\":1}}\n", batch_line(1, 1, "a"));
        let r = parse_journal(&text);
        assert!(!r.torn_tail);
        assert_eq!(r.skipped_malformed, 1);
    }

    #[test]
    fn unsaved_tail_is_the_batches_after_the_last_save_mark_of_the_last_epoch() {
        // epoch 1: batch, MARK, batch, batch -> that epoch's tail is 2...
        let mut text = [
            epoch_line("create", 1),
            batch_line(1, 1, "e1-a"),
            epoch_line("save", 1),
            batch_line(2, 1, "e1-b"),
            batch_line(3, 1, "e1-c"),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()), vec!["e1-b", "e1-c"]);

        // ...and a SECOND save in the same epoch moves the tail forward. A
        // reader that took the FIRST mark instead of the last would replay
        // e1-b, which the second save already wrote to disk.
        text.push_str(&format!("{}\n{}\n", epoch_line("save", 1), batch_line(4, 1, "e1-d")));
        let r = parse_journal(&text);
        assert_eq!(labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()), vec!["e1-d"]);

        // ...until the document swaps. Epoch 2 has no mark, so ITS tail is
        // all of its own batches and NONE of epoch 1's — those describe a
        // document that is no longer open.
        text.push_str(&format!("{}\n{}\n", epoch_line("open", 2), batch_line(1, 2, "e2-a")));
        let r = parse_journal(&text);
        assert_eq!(labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()), vec!["e2-a"]);

        // A mark as the LAST record: the snapshot is current, tail empty.
        text.push_str(&format!("{}\n", epoch_line("save", 2)));
        let r = parse_journal(&text);
        assert!(unsaved_tail(&r).is_empty(), "a trailing mark means the disk caught up");

        // And a mark that arrives out of file order still anchors where the
        // FILE put it: written after e2-a, so e2-a stays saved.
        text.push_str(&format!("{}\n", batch_line(2, 2, "e2-b")));
        let r = parse_journal(&text);
        assert_eq!(labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()), vec!["e2-b"]);
    }

    /// FIX ROUND 1's CRITICAL, at the unit level. Every epoch boundary
    /// appends its own record to the journal it opens, so a real file
    /// routinely ENDS in an epoch that has no batches yet. A max-epoch rule
    /// picks that empty epoch and reports "nothing unsaved" at exactly the
    /// moment a document was re-opened with unsaved work in its log.
    ///
    /// The previous fixture hand-wrote journals ENDING IN BATCHES — an order
    /// production never produces — which is why nothing caught it.
    #[test]
    fn the_tail_survives_the_epoch_record_an_open_appends_before_reading() {
        let text = [
            epoch_line("create", 1),
            batch_line(1, 1, "e1-a"),
            epoch_line("save", 1),
            batch_line(2, 1, "unsaved-1"),
            batch_line(3, 1, "unsaved-2"),
            // ...and now the user re-opens the project. `epoch_boundary`
            // appends THIS, to the file about to be read, with an epoch
            // above everything already in it and no batches of its own.
            epoch_line("open", 3),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(
            labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()),
            vec!["unsaved-1", "unsaved-2"],
            "the re-open's own boundary record must not swallow the tail"
        );
        // Anti-vacuity: the trap really is in the fixture — the newest epoch
        // in the file is 3, it is above the batches' epoch, and it is empty.
        assert_eq!(r.records.iter().map(|r| r.epoch()).max(), Some(3));
        assert!(
            !r.records.iter().any(|x| x.epoch() == 3 && matches!(x, JournalRecord::Batch { .. })),
            "and epoch 3 carries no batches, which is what made the max-epoch rule wrong"
        );
    }

    /// The anchor is the HIGHEST rev seen so far in the epoch, not the last
    /// one written. The two rules differ exactly in the out-of-order case
    /// this module exists for — and the doc used to describe the wrong one.
    #[test]
    fn a_mark_anchors_past_a_higher_rev_that_arrived_out_of_file_order() {
        // rev 5 reaches the file BEFORE rev 3 (two committers, journal mutex
        // is the leaf). The mark is written after both landed, so both are
        // saved; only rev 6 is unsaved.
        let text = [
            batch_line(1, 1, "a"),
            batch_line(5, 1, "arrived-early"),
            batch_line(3, 1, "arrived-late"),
            epoch_line("save", 1),
            batch_line(6, 1, "genuinely-unsaved"),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(
            labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()),
            vec!["genuinely-unsaved"],
            "a last-seen anchor would sit at rev 3 and hand rev 5 back as unsaved work"
        );
    }

    /// The invariant that makes the tail safe in the re-apply direction: a
    /// batch whose rev the snapshot had already reached can never enter the
    /// tail, because the anchor is the maximum rev over every batch
    /// preceding the mark. Unreadable lines can only LOWER the anchor, and a
    /// lower anchor admits only HIGHER revs — mutations the document had not
    /// reached when the snapshot was taken.
    #[test]
    fn a_batch_written_before_the_mark_never_enters_the_tail_however_torn_the_file() {
        let text = [
            batch_line(1, 1, "saved-1").as_str(),
            batch_line(9, 1, "saved-high").as_str(),
            // The highest pre-mark rev is UNREADABLE — the worst case for
            // the anchor.
            r#"{"v":2,"rev":12,"epoch":1,"actor":"user","run":"r","label":"lost","ops":[{"kind":"noSuchOp"}]}"#,
            batch_line(4, 1, "saved-low").as_str(),
            epoch_line("save", 1).as_str(),
            // Written after the mark but carrying a LOWER rev than one
            // already in the file. Revs are minted under the session lock,
            // so rev 7's mutation was in the document before rev 9's was —
            // and the snapshot the mark describes was taken from a document
            // that already had rev 9. A late-arriving LINE is not a late
            // mutation: this one is SAVED, and classifying it by rev rather
            // than by file position is exactly what L-4's sort is for.
            batch_line(7, 1, "saved-but-journaled-late").as_str(),
            batch_line(14, 1, "genuinely-after").as_str(),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        let tail = labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>());
        assert_eq!(tail, vec!["genuinely-after"], "only what the document had not reached");
        assert!(
            !tail.iter().any(|l| l.starts_with("saved")),
            "no batch the snapshot already contains is offered for re-application"
        );
        // The real residual, asserted rather than merely described: the
        // unreadable batch is LOST — not saved, not in the tail, just
        // counted. The counter is the whole defence.
        assert_eq!(r.skipped_malformed, 1);
        assert!(!labels(&r.records).iter().any(|l| l == "lost"));
    }

    /// THE LIMIT OF THE PROPERTY ABOVE, as a test rather than as a caveat.
    /// The sibling test's unreadable line sits ABOVE every surviving pre-mark
    /// rev, so the anchor stays high and nothing saved is re-offered. Move it
    /// DOWN to the high-water rev — one number — and the anchor drops beneath
    /// a batch the snapshot already contained, which is then handed back as
    /// unsaved work.
    ///
    /// Asserted, not lamented: the tail is a SUPERSET of the genuinely
    /// unsaved batches, and this is what makes it a strict superset. That is
    /// the safe direction for a detector and the unsafe one for an applier,
    /// which is exactly why nothing auto-applies (ruling F-8).
    ///
    /// THIS IS A CHARACTERIZATION TEST OF A KNOWN IMPRECISION, not a
    /// requirement: it holds because a save mark carries no rev. Give the
    /// mark the rev it was written at (a journal FORMAT change, handed
    /// forward with the segmentation work) and the tail becomes exact and
    /// this test SHOULD go red — delete it then, do not weaken it.
    /// Measured to discriminate: making the rev-9 line readable, and
    /// changing nothing else, collapses the tail to `["genuinely-after"]`.
    #[test]
    fn an_unreadable_line_at_the_high_water_rev_re_offers_saved_work() {
        let text = [
            batch_line(1, 1, "saved-1").as_str(),
            // The pre-mark HIGH-WATER rev is the unreadable one. Nothing
            // surviving records that the document ever reached rev 9.
            r#"{"v":2,"rev":9,"epoch":1,"actor":"user","run":"r","label":"lost","ops":[{"kind":"noSuchOp"}]}"#,
            batch_line(4, 1, "saved-low").as_str(),
            epoch_line("save", 1).as_str(),
            // Committed BEFORE rev 9, journaled after the mark. Saved — the
            // snapshot was taken from a document that had reached rev 9.
            batch_line(7, 1, "saved-but-journaled-late").as_str(),
            batch_line(14, 1, "genuinely-after").as_str(),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(
            labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()),
            vec!["saved-but-journaled-late", "genuinely-after"],
            "anchor fell to 4 with rev 9 unreadable, so a SAVED batch is re-offered"
        );
        // Anti-vacuity: this differs from the sibling test in exactly one
        // number. Both parts must hold or the fixture proves nothing — the
        // high-water line really is gone, and the re-offered batch really did
        // follow the mark in the file.
        assert_eq!(r.skipped_malformed, 1, "the rev-9 line really is unreadable");
        assert!(!labels(&r.records).iter().any(|l| l == "lost"));
        assert!(
            text.find("\"label\":\"saved-but-journaled-late\"").unwrap()
                > text.find("\"epochEvent\":\"save\"").unwrap(),
            "and rev 7 really was journaled after the mark"
        );
    }

    #[test]
    fn a_boundary_record_is_read_whatever_version_wrote_it() {
        // The version gate is BATCH-ONLY on purpose: a boundary record's
        // shape ({epochEvent, epoch}) has never changed, and refusing a v1
        // save mark would move the tail's start BACKWARDS — the one
        // direction that costs re-applied work.
        let text = [
            r#"{"v":1,"epochEvent":"save","epoch":1}"#.to_string(),
            batch_line(2, 1, "after"),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(labels(&r.records), vec!["save:1", "after"], "the v1 mark is a mark");
        assert_eq!(r.skipped(), 0, "and is not counted as skipped");
        assert_eq!(
            labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()),
            vec!["after"]
        );
    }

    #[test]
    fn a_mark_before_any_batch_of_its_epoch_leaves_the_whole_epoch_unsaved() {
        let text = [
            epoch_line("open", 4),
            epoch_line("save", 4),
            batch_line(1, 4, "after"),
        ]
        .join("\n")
            + "\n";
        let r = parse_journal(&text);
        assert_eq!(labels(&unsaved_tail(&r).into_iter().cloned().collect::<Vec<_>>()), vec!["after"]);
    }

    #[test]
    fn read_journal_reads_what_the_writer_wrote_and_errs_only_on_io() {
        let dir = tmp_dir("roundtrip");
        let mut j = JournalWriter::closed();
        j.open_in(&dir);
        j.append_epoch(EpochEvent::Create, 1);
        let meta = TxMeta { actor: Actor::User, run: "r-1".into(), label: "mix".into(), transient: false };
        let op = Op::Set {
            object: ObjectRef::Track("t-1".into()),
            path: PropPath::Gain,
            from: serde_json::Value::Null,
            to: serde_json::json!(-3.0),
        };
        j.append_batch(7, 1, &meta, std::slice::from_ref(&op));
        j.append_epoch(EpochEvent::Save, 1);

        let r = read_journal(&dir.join(JOURNAL_FILE)).expect("readable");
        assert_eq!(labels(&r.records), vec!["epoch:create:1", "mix", "save:1"]);
        // The OPS round-trip through the real writer, not just the shape.
        match &r.records[1] {
            JournalRecord::Batch { rev, epoch, actor, run, ops, .. } => {
                assert_eq!((*rev, *epoch), (7, 1));
                assert_eq!(*actor, Actor::User);
                assert_eq!(run, "r-1");
                assert_eq!(ops, &vec![op]);
            }
            other => panic!("expected a batch, got {other:?}"),
        }
        assert!(unsaved_tail(&r).is_empty(), "the mark is last: nothing unsaved");

        assert!(
            read_journal(&dir.join("no-such-file.ndjson")).is_err(),
            "an absent journal is an I/O Err, which callers treat as 'no journal'"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What `open_project_epoch` actually calls. The call site itself is one
    /// unbranched line (there is no log capture in this crate to assert the
    /// warn text against), so what is pinned here is the whole decision it
    /// delegates: absent journal, current journal, journal with a tail.
    #[test]
    fn open_time_detection_counts_the_tail_and_treats_an_absent_journal_as_nothing_to_say() {
        let dir = tmp_dir("detect");
        assert_eq!(detect_unsaved_tail(&dir), 0, "no journal at all is not an error");

        let path = dir.join(JOURNAL_FILE);
        std::fs::write(
            &path,
            [epoch_line("open", 1), batch_line(1, 1, "a"), epoch_line("save", 1)].join("\n") + "\n",
        )
        .unwrap();
        assert_eq!(detect_unsaved_tail(&dir), 0, "saved up to the end: nothing to report");

        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(&format!("{}\n{}\n", batch_line(2, 1, "b"), batch_line(3, 1, "c")));
        std::fs::write(&path, &text).unwrap();
        assert_eq!(detect_unsaved_tail(&dir), 2, "two batches after the last mark");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_applies_each_batch_atomically_and_stops_loudly_on_the_first_failure() {
        use crate::audio::types::Store;
        use crate::midi::MidiStore;

        let session = Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let add = |id: &str, index: usize| Op::TrackAdd {
            track: crate::audio::types::TrackState {
                id: id.into(),
                name: "T".into(),
                kind: "audio".into(),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                armed: false,
                color: "#888888".into(),
                instrument_id: None,
            },
            index,
            clips: vec![],
            clip_indices: vec![],
            automation_clips: vec![],
            bindings: vec![],
        };
        let batch = |rev: u64, label: &str, ops: Vec<Op>| JournalRecord::Batch {
            v: OP_FORMAT_VERSION,
            rev,
            epoch: 1,
            actor: Actor::User,
            run: "r".into(),
            label: label.into(),
            ops,
        };
        let good = batch(1, "add track", vec![add("t-1", 0)]);
        let good2 = batch(2, "add another", vec![add("t-2", 1)]);
        // Second batch: a valid gain Set followed by one addressing a track
        // that does not exist. `apply_raw` rejects the second, and the
        // BATCH must roll back as a unit — a batch was atomic when it was
        // committed and must be atomic when it is replayed.
        let bad = batch(
            3,
            "half bad",
            vec![
                Op::Set {
                    object: ObjectRef::Track("t-1".into()),
                    path: PropPath::Gain,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(-9.0),
                },
                Op::Set {
                    object: ObjectRef::Track("gone".into()),
                    path: PropPath::Gain,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(-9.0),
                },
            ],
        );

        let n = replay_tail(&session, &[&good]).expect("the first batch applies");
        assert_eq!(n, 1);
        assert_eq!(session.lock().store.tracks.len(), 1, "the track really landed");

        let err = replay_tail(&session, &[&good2, &bad]).expect_err("a diverged log must be loud");
        assert!(err.contains("rev 3"), "the error names WHERE it stopped: {err}");
        assert!(err.contains("after 1 batch(es)"), "and how far it got: {err}");
        let s = session.lock();
        assert_eq!(s.store.tracks.len(), 2, "the batches before the failure stayed applied");
        assert_eq!(
            s.store.tracks[0].gain_db, 0.0,
            "the FAILING batch rolled back whole — its first op did not survive"
        );
    }

    #[test]
    fn apply_raw_is_deterministic_so_replay_mints_the_same_ids_twice() {
        // Ruling F-9 / inventory L-2, asserted rather than assumed: two
        // independent replays of the SAME journal batch onto the SAME base
        // must produce byte-identical documents, note ids included. If
        // anything under `apply_raw` reached for `mint()`, `Uuid::new_v4`,
        // a clock or an RNG, the two runs would differ here.
        use crate::audio::types::Store;
        use crate::ids::{ClipId, ContentId, LaneId};
        use crate::midi::{MidiClip, MidiNote, MidiStore};

        let clip_id = ClipId::from("mc-1");
        let base_clip = MidiClip {
            id: clip_id.clone(),
            track_id: "t-1".into(),
            name: "C".into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: vec![],
            next_note_id: 5, // a watermark that came off disk
            content_id: ContentId::default(),
            lane_id: LaneId::default_for_track("t-1"),
            content_length_ticks: None,
        };
        // noteId 0 = the mint sentinel — the exact case ruling F-9 is about.
        let sentinel = MidiNote { tick: 0, length_ticks: 120, key: 60, velocity: 100, channel: 0, note_id: crate::ids::NoteId(0) };
        let rec = JournalRecord::Batch {
            v: OP_FORMAT_VERSION,
            rev: 1,
            epoch: 1,
            actor: Actor::User,
            run: "r".into(),
            label: "notes".into(),
            ops: vec![Op::MidiSetNotes { clip: clip_id.clone(), notes: vec![sentinel.clone(), MidiNote { tick: 240, ..sentinel }] }],
        };

        let render = || {
            let mut midi = MidiStore::default();
            midi.clips = vec![base_clip.clone()];
            let session = Mutex::new(Session::new(Store::default(), midi));
            replay_tail(&session, &[&rec]).expect("replays");
            let s = session.lock();
            serde_json::to_string(&s.midi.clips).unwrap()
        };
        let first = render();
        assert_eq!(first, render(), "replay is reproducible, ids and watermark included");
        assert!(first.contains("\"noteId\":5"), "minted from the watermark, not from a counter: {first}");
        assert!(first.contains("\"nextNoteId\":7"), "and the watermark advanced past both: {first}");
    }
}
