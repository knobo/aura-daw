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

    /// `Some(rev)` for a batch; `None` for the two boundary records, which
    /// carry no revision of their own (see [`read_journal`]'s sort rules).
    pub fn rev(&self) -> Option<u64> {
        match self {
            JournalRecord::Batch { rev, .. } => Some(*rev),
            _ => None,
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
    /// Every batch record, in order.
    pub fn batches(&self) -> impl Iterator<Item = &JournalRecord> {
        self.records.iter().filter(|r| matches!(r, JournalRecord::Batch { .. }))
    }

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
///   caught up HERE" — so it anchors to the rev of the last batch that
///   preceded it IN FILE ORDER within its epoch, with the highest class.
///   That places it strictly after that batch and strictly before the next
///   one, which is exactly what the mark asserts. A mark before any batch
///   in its epoch anchors at 0, i.e. right after the epoch record.
/// * file position last, via a STABLE sort: two records with an identical
///   key keep the order the file gave them.
///
/// DUPLICATE `(epoch, rev)` PAIRS ARE NOT AN ERROR and are not deduplicated.
/// They should not occur (a rev is minted once, under the session lock), but
/// a reader that silently dropped one of a pair would be choosing which of
/// two mutations to forget, which is not a choice a recovery path may make
/// on its own. They come back adjacent, in file order.
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

/// THE UNSAVED TAIL: the batches of the report's LAST epoch that come after
/// that epoch's LAST save mark — or ALL of that epoch's batches when it has
/// no mark at all. Empty means the on-disk snapshot is current with the log.
///
/// Only the last epoch, because every earlier epoch describes a document
/// that has since been swapped out; replaying its ops onto today's document
/// would apply one project's edits to another (ruling 4, the same reason a
/// swap clears the undo stacks).
///
/// A MARK AT THE VERY END gives an empty tail — the normal, healthy case.
/// A MARK IS NEVER "BEYOND THE END": it has no index into anything, only a
/// position among records, so there is no out-of-range case to handle.
///
/// KNOWN CONSEQUENCE of a mark whose neighbouring batch line is unreadable:
/// the mark anchors to the last batch that DID parse, so the tail can begin
/// one or more batches earlier than the truth and a replay would re-apply
/// work already on disk. That is why nothing auto-applies (ruling F-8) —
/// the tail is a detection signal, and a human decides.
pub fn unsaved_tail(report: &ReadReport) -> Vec<&JournalRecord> {
    let Some(last_epoch) = report.records.iter().map(|r| r.epoch()).max() else {
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
/// WHAT A NON-ZERO COUNT ACTUALLY MEANS, stated precisely because the
/// obvious reading is wrong for today's AURA: it means "the log records edits
/// the user never explicitly saved", NOT "the files on disk are stale".
/// Nearly every op carries a `PersistEffect` and `execute_persist` writes it
/// out right after the commit, so the disk usually IS current and a non-zero
/// tail is expected on any project edited since its last Ctrl+S. The signal
/// becomes a real staleness signal exactly when a persist FAILED (each one
/// only warns) or when the process died between a commit and its persist.
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
        // 30 collisions, not 3 — and MEASURED, not assumed: swapping the
        // implementation to `sort_unstable_by_key` does NOT turn this red at
        // 30, 200 or 2000 identical keys (pdqsort leaves an all-equal run
        // alone). So this test pins the OUTCOME "no duplicate is dropped or
        // reordered", which is the contract; it does not, and cannot
        // cheaply, pin the choice of a stable sort as such.
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
