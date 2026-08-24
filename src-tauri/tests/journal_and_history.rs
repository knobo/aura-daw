//! Gate E deliverable 3 + §7 test 5: THE OP LOG IS ON.
//!
//! `src-tauri/src/control/history.rs` carries the unit tests for `History`'s
//! merge/bounds/clear algebra and `JournalWriter`'s line format in isolation.
//! THIS file drives the real thing: a headless `ControlPlane` wired exactly
//! as `audio::init` + lib.rs's setup wire it (ONE `HistoryLog` shared with
//! the engine's own `Committer`), through the real `undo`/`redo` entry
//! points, reading `journal.ndjson` back off disk.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;

use aura_lib::audio::engine::{self, ControlMsg, EventSink};
use aura_lib::audio::rt::{GraphTables, SharedRt};
use aura_lib::audio::types::Store;
use aura_lib::control::op::{ObjectRef, Op, PropPath, TxMeta};
use aura_lib::control::{
    ops, Committed, Committer, ControlPlane, EventEmitter, HistoryLog, HistoryMode, Session,
    TrackMixChange, TransportAction,
};
use aura_lib::midi::MidiStore;
use aura_lib::sidecars::jobs::JobManager;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct NullEvents;
impl EventSink for NullEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

struct Fix {
    cp: Arc<ControlPlane>,
    eng: engine::EngineHandle,
    /// A `Committer` over the SAME session/log the `ControlPlane` uses —
    /// this is the shape the engine control thread holds (Task 13), and how
    /// this file produces genuine `Actor::Engine` commits without a real
    /// audio device.
    engine_committer: Committer,
    log: Arc<HistoryLog>,
}

fn fixture() -> Fix {
    fixture_with_emitter(Box::new(|_e, _p| {}))
}

/// [`fixture`], but with a say in what the `ControlPlane`'s event emitter
/// does.
///
/// WHY THIS EXISTS: `project://changed` is emitted SYNCHRONOUSLY on the
/// committing thread at the end of every non-transient commit, with no lock
/// held (`commit_with_rebuild`). That makes it the one place a
/// single-threaded test can act BETWEEN two steps of an `undo_to` walk —
/// which is where the concurrency C-1 is about actually lands, and which
/// would otherwise need a second thread plus a scheduler willing to
/// cooperate. Every other test in this file passes the no-op emitter
/// [`fixture`] supplies.
fn fixture_with_emitter(emit: EventEmitter) -> Fix {
    let shared = Arc::new(SharedRt::default());
    let tables = GraphTables::empty();
    let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
    // ONE log, shared — production wiring (audio::init creates it, lib.rs
    // hands it to `ControlPlane::new`).
    let log = Arc::new(HistoryLog::new());
    let committer = Committer::new(
        session.clone(),
        shared.clone(),
        tables.clone(),
        Arc::new(Box::new(|_: &str, _: serde_json::Value| {}) as EventEmitter),
        log.clone(),
    );
    let gesture = Arc::new(aura_lib::control::GestureState::new());
    let eng = engine::start(
        shared.clone(),
        tables.clone(),
        session.clone(),
        Box::new(NullEvents),
        committer.clone(),
        gesture.clone(),
    );
    let cp = Arc::new(ControlPlane::new(
        session,
        shared,
        tables,
        eng.clone(),
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        emit,
        log.clone(),
        gesture,
    ));
    Fix { cp, eng, engine_committer: committer, log }
}

fn tmp_parent(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aura-journal-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn set_gain(track: &str, to: f64) -> Op {
    Op::Set {
        object: ObjectRef::Track(track.into()),
        path: PropPath::Gain,
        from: serde_json::Value::Null,
        to: serde_json::json!(to),
    }
}

fn add_track(cp: &ControlPlane, name: &str) -> String {
    let mut id = String::new();
    cp.commit(TxMeta::user(format!("add {name}")), |tx| {
        let t = ops::add_track_tx(tx, Some(name.into()), Some("audio".into()))?;
        id = t.id.to_string();
        Ok(())
    })
    .expect("add track");
    id
}

fn gain_of(cp: &ControlPlane, id: &str) -> f64 {
    cp.project_state().tracks.iter().find(|t| t.id == id).expect("track").gain_db
}

fn clip_start(cp: &ControlPlane, id: &str) -> u64 {
    cp.project_state().clips.iter().find(|c| c.id == id).expect("clip").timeline_start_samples
}

fn test_clip(id: &str, track_id: &str) -> aura_lib::audio::types::Clip {
    aura_lib::audio::types::Clip {
        id: id.into(),
        track_id: track_id.into(),
        name: format!("Clip {id}"),
        source_path: "audio/x.wav".into(),
        source_id: aura_lib::ids::SourceId::default(),
        source_channels: 2,
        source_sample_rate: 48_000,
        source_length_samples: 48_000,
        timeline_start_samples: 24_000,
        offset_samples: 0,
        length_samples: 48_000,
        gain_db: 0.0,
        fade_in_samples: 0,
        fade_out_samples: 0,
        content_id: aura_lib::ids::ContentId::mint(),
        lane_id: aura_lib::ids::LaneId::default_for_track(track_id),
    }
}

/// Every line of `<dir>/journal.ndjson`, parsed. Fails loudly if any line is
/// not valid JSON — "every line parses" is part of the contract.
fn journal_lines(dir: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join("journal.ndjson")).expect("journal exists");
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("unparseable journal line {l:?}: {e}")))
        .collect()
}

fn batches(lines: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    lines.iter().filter(|l| l.get("ops").is_some()).collect()
}

// ---------------------------------------------------------------------------
// undo / redo
// ---------------------------------------------------------------------------

#[test]
fn undo_and_redo_round_trip_through_the_commands() {
    let f = fixture();
    let parent = tmp_parent("roundtrip");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");

    // Two edits with DIFFERENT keys, so the 350 ms fallback cannot merge
    // them into one step (that is its own test, below).
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    f.cp.commit(TxMeta::user("pan"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::Track(t.clone().into()),
            path: PropPath::Pan,
            from: serde_json::Value::Null,
            to: serde_json::json!(0.5),
        })
    })
    .unwrap();
    assert_eq!(f.log.depths(), (3, 0), "add track + gain + pan");
    assert_eq!(gain_of(&f.cp, &t), -6.0);

    // Undo the pan, then the gain.
    assert_eq!(f.cp.undo().unwrap().as_deref(), Some("pan"));
    assert_eq!(f.cp.undo().unwrap().as_deref(), Some("gain"));
    assert_eq!(gain_of(&f.cp, &t), 0.0, "gain is back to its baseline");
    assert_eq!(f.log.depths(), (1, 2));

    // Redo both.
    assert_eq!(f.cp.redo().unwrap().as_deref(), Some("gain"));
    assert_eq!(f.cp.redo().unwrap().as_deref(), Some("pan"));
    assert_eq!(gain_of(&f.cp, &t), -6.0, "redo restores the edit");
    assert_eq!(f.log.depths(), (3, 0));

    // Undo past the structural op too — the track goes away and comes back.
    f.cp.undo().unwrap();
    f.cp.undo().unwrap();
    f.cp.undo().unwrap();
    assert!(f.cp.project_state().tracks.is_empty(), "undoing the TrackAdd removes the track");
    assert_eq!(f.log.depths(), (0, 3));
    assert!(f.cp.undo().unwrap().is_none(), "an empty history is not an error");

    f.cp.redo().unwrap();
    assert_eq!(f.cp.project_state().tracks.len(), 1);

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn a_new_edit_clears_the_redo_stack() {
    let f = fixture();
    let parent = tmp_parent("redoclear");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();

    f.cp.undo().unwrap();
    assert_eq!(f.log.depths().1, 1, "there is a redo future");

    // A genuinely new edit invalidates it.
    f.cp.commit(TxMeta::user("pan"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::Track(t.clone().into()),
            path: PropPath::Pan,
            from: serde_json::Value::Null,
            to: serde_json::json!(-0.5),
        })
    })
    .unwrap();
    assert_eq!(f.log.depths().1, 0, "a new edit clears the redo stack");
    assert!(f.cp.redo().unwrap().is_none());

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn undo_and_redo_commits_are_journaled_but_create_no_new_history_entry() {
    let f = fixture();
    let parent = tmp_parent("replay-journal");
    let project = f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let dir = std::path::PathBuf::from(project.path.unwrap());
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();

    let before = batches(&journal_lines(&dir)).len();
    f.cp.undo().unwrap();
    f.cp.redo().unwrap();
    let lines = journal_lines(&dir);
    let after = batches(&lines).len();
    assert_eq!(after, before + 2, "an undo and a redo are mutations — replay must see them");
    assert_eq!(f.log.depths(), (2, 0), "...but neither creates a new history entry");

    // The labels say which is which, and the run id is PRESERVED so a
    // journal reader can correlate an edit with its own undo.
    let bs = batches(&lines);
    let undo_line = bs[bs.len() - 2];
    let redo_line = bs[bs.len() - 1];
    assert_eq!(undo_line["label"], "undo: gain");
    assert_eq!(redo_line["label"], "redo: gain");
    let edit_run = bs[bs.len() - 3]["run"].clone();
    assert_eq!(undo_line["run"], edit_run, "the undo belongs to the run it reverses");
    assert_eq!(redo_line["run"], edit_run);

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// undo to here (Plan F carry-forward (e), ordered next step)
// ---------------------------------------------------------------------------

/// Four edits, then a walk back to the second one: the document is the one
/// that revision describes, every step in between is on the redo stack in
/// the right order, and redoing twice puts the document back.
#[test]
fn undo_to_walks_back_to_a_revision_and_leaves_a_redo_chain() {
    let f = fixture();
    let parent = tmp_parent("undoto-roundtrip");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");

    // Distinct labels so the 350 ms same-key merge cannot fold them.
    for (label, gain) in [("g1", -3.0), ("g2", -6.0), ("g3", -9.0)] {
        f.cp.commit(TxMeta::user(label), |tx| tx.apply(set_gain(&t, gain))).unwrap();
    }
    let path = f.log.undo_path();
    assert_eq!(path.revs.len(), 4, "add track + three gain edits");
    let target = path.revs[1]; // the state after g1

    let out = f.cp.undo_to(target, path.epoch, path.head()).expect("undo to here");
    assert_eq!(out.steps, 2, "g3 and g2 are undone; g1 stays applied");
    assert_eq!(gain_of(&f.cp, &t), -3.0, "the document is the one revision g1 describes");
    assert_eq!(f.log.depths(), (2, 2), "two steps left to undo, two to redo");
    assert_eq!(f.log.undo_path().head(), Some(target), "the head is now the target");

    // The redo chain is ordered, not merely present.
    assert_eq!(f.cp.redo().unwrap().as_deref(), Some("g2"));
    assert_eq!(f.cp.redo().unwrap().as_deref(), Some("g3"));
    assert_eq!(gain_of(&f.cp, &t), -9.0, "redo restores the walked-back edits");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// Selecting the head is legal and does nothing — the document is already
/// the one that revision describes.
#[test]
fn undo_to_the_head_revision_is_a_no_op() {
    let f = fixture();
    let parent = tmp_parent("undoto-head");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();

    let path = f.log.undo_path();
    let out = f.cp.undo_to(path.head().unwrap(), path.epoch, path.head()).unwrap();
    assert_eq!(out.steps, 0);
    assert_eq!(out.label, None);
    assert_eq!(gain_of(&f.cp, &t), -6.0);
    assert_eq!(f.log.depths(), (2, 0), "nothing moved between the stacks");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// STALE TARGET: the ancestry moved between the read and the request (a new
/// edit landed). The request carries the head it saw, so it aborts whole —
/// no partial walk, no consumed step.
#[test]
fn undo_to_aborts_when_the_undo_head_moved_under_it() {
    let f = fixture();
    let parent = tmp_parent("undoto-stale");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    let path = f.log.undo_path();
    let target = path.revs[1];

    // Something else commits before the request arrives.
    f.cp.commit(TxMeta::user("g2"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();

    let err = f.cp.undo_to(target, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("changed"), "the message must say the history moved: {err}");
    assert_eq!(gain_of(&f.cp, &t), -6.0, "nothing was applied");
    assert_eq!(f.log.depths(), (3, 0), "no step was consumed");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// EVICTION / off-path target: a revision that is not on the undo stack is
/// refused. An already-undone step is the reachable case (it sits on the
/// redo stack); a bottom-evicted one behaves identically because both are
/// simply absent from `undo_revs`.
#[test]
fn undo_to_refuses_a_revision_that_is_not_on_the_undo_path() {
    let f = fixture();
    let parent = tmp_parent("undoto-offpath");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    f.cp.commit(TxMeta::user("g2"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    let gone = f.log.undo_path().head().unwrap(); // g2's rev

    f.cp.undo().unwrap(); // g2 moves to the redo stack
    let path = f.log.undo_path();
    assert!(!path.revs.contains(&gone));

    let err = f.cp.undo_to(gone, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("not on the undo path"), "unexpected message: {err}");
    assert_eq!(gain_of(&f.cp, &t), -3.0, "nothing was applied");
    assert_eq!(f.log.depths(), (2, 1), "the stacks are untouched");

    // A revision that never existed is refused the same way.
    let err = f.cp.undo_to(9_999, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("not on the undo path"), "unexpected message: {err}");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// EPOCH SWAP: the document was replaced between the read and the request.
/// The epoch guard refuses before any op is applied — undoing across a
/// document swap is corruption, not undo (`History::clear`'s doc).
#[test]
fn undo_to_aborts_when_the_document_was_swapped() {
    let f = fixture();
    let parent = tmp_parent("undoto-epoch");
    f.cp.create_project(parent.to_str().unwrap(), "A").unwrap();
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    let path = f.log.undo_path();
    let target = path.revs[1];

    // A second project = a document swap = an epoch boundary.
    f.cp.create_project(parent.to_str().unwrap(), "B").unwrap();
    assert_eq!(f.log.depths(), (0, 0), "the swap cleared history");

    let err = f.cp.undo_to(target, path.epoch, path.head()).unwrap_err();
    // Load-bearing: proves the EPOCH guard fired (not the head guard, which
    // would also reject this call since B's empty stack has no head to match).
    assert!(err.contains("epoch"), "the message must name the swap: {err}");
    // Not load-bearing: the swap already emptied the stacks (asserted above,
    // before this call), so this holds whether or not the guard did anything —
    // it only reconfirms undo_to did not somehow push onto B's history.
    assert_eq!(f.log.depths(), (0, 0), "nothing was pushed onto the new document's stacks");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// Every step is an ORDINARY undo commit: journaled, `HistoryMode::Replay`,
/// no new history entry. Two steps = two journal batches, not one.
#[test]
fn undo_to_journals_one_batch_per_step_and_creates_no_history_entry() {
    let f = fixture();
    let parent = tmp_parent("undoto-journal");
    let project = f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let dir = std::path::PathBuf::from(project.path.unwrap());
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("g1"), |tx| tx.apply(set_gain(&t, -3.0))).unwrap();
    f.cp.commit(TxMeta::user("g2"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    let before = batches(&journal_lines(&dir)).len();

    let path = f.log.undo_path();
    let target = path.revs[0];
    let out = f.cp.undo_to(target, path.epoch, path.head()).unwrap();
    assert_eq!(out.steps, 2);
    assert_eq!(out.label.as_deref(), Some("g1"), "the label of the LAST step undone");

    let lines = journal_lines(&dir);
    let after = batches(&lines).len();
    assert_eq!(after - before, 2, "one journal batch per undo step");
    assert_eq!(f.log.depths(), (1, 2), "two entries migrated, none created");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// A MID-WALK ABORT KEEPS ITS APPLIED PREFIX (C-1, whole-branch review).
///
/// `history_gate` serialises a walk against other `undo`/`redo` commands and
/// against NOTHING ELSE — ordinary commits land without it, from the engine
/// control thread, an MCP agent tool, or an automation write pass on
/// release. This drives exactly that: one commit is recorded onto the undo
/// stack from inside the `project://changed` emit of the walk's FIRST step,
/// which is precisely "between two steps".
///
/// WHAT THE FIX BUYS. A walk that trusted a precomputed step count would pop
/// whatever the back holds, so step 2 would undo the RACING commit — an edit
/// nobody asked to undo — leave `gb` applied, and still return
/// `Ok(UndoToOutcome { steps: 2 })`. With the conditional pop, step 2
/// consumes the revision it planned to or nothing, so the walk stops and
/// reports honestly. What it does NOT do is roll back: step 1 was a real
/// committed transaction and stays applied, which is the documented
/// contract this test pins alongside the stacks.
#[test]
fn undo_to_stops_when_a_commit_lands_between_two_steps_of_the_walk() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    /// The racing commit, plus the sink to record it into. Filled in only
    /// after the fixture and its setup edits exist — the emitter closure has
    /// to be built before either of them does.
    struct Racer {
        log: Arc<HistoryLog>,
        batch: Committed,
    }
    let racer: Arc<OnceLock<Racer>> = Arc::new(OnceLock::new());
    let armed = Arc::new(AtomicBool::new(false));

    let (slot, arm) = (racer.clone(), armed.clone());
    let f = fixture_with_emitter(Box::new(move |event: &str, _p| {
        // One shot, on the first commit the walk makes.
        if event != "project://changed" || !arm.swap(false, Ordering::SeqCst) {
            return;
        }
        let r = slot.get().expect("armed only once the racer exists");
        r.log.record_commit(&r.batch, HistoryMode::Record);
    }));

    let parent = tmp_parent("undoto-raced");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let a = add_track(&f.cp, "A");
    let b = add_track(&f.cp, "B");
    // Distinct labels AND distinct coalesce keys, so nothing merges.
    f.cp.commit(TxMeta::user("ga1"), |tx| tx.apply(set_gain(&a, -1.0))).unwrap();
    f.cp.commit(TxMeta::user("gb"), |tx| tx.apply(set_gain(&b, -6.0))).unwrap();
    let last = f.cp.commit(TxMeta::user("ga2"), |tx| tx.apply(set_gain(&a, -3.0))).unwrap();

    // A genuine batch, re-labelled and given a rev above everything on the
    // stack — what any committer that is not holding `history_gate` puts
    // there. It goes in through the production sink, so it inserts by rev
    // and clears redo exactly as a real commit would.
    let _ = racer.set(Racer {
        log: f.log.clone(),
        batch: Committed { rev: last.rev + 1_000, meta: TxMeta::user("raced the walk"), ..last },
    });

    let path = f.log.undo_path();
    assert_eq!(path.revs.len(), 5, "two adds + three gain edits");
    let target = path.revs[2]; // the state after ga1: undo ga2, then gb
    let gb_rev = path.revs[3];

    armed.store(true, Ordering::SeqCst);
    let err = f.cp.undo_to(target, path.epoch, path.head()).unwrap_err();
    assert!(err.contains("stopped after 1 of 2 steps"), "the error must say how far it got: {err}");
    assert!(
        err.contains(&format!("no longer offers revision {gb_rev}")),
        "…and which revision the stack stopped offering: {err}"
    );
    assert!(err.contains("stay applied"), "…and that the prefix is still applied: {err}");

    // THE APPLIED PREFIX STAYS APPLIED: step 1 (undo ga2) really happened.
    // -1.0 is ga1's value, not a default, so this cannot pass by accident.
    assert_eq!(gain_of(&f.cp, &a), -1.0, "the step before the abort is not rolled back");
    // AND THE RACING COMMIT WAS NOT UNDONE — the bug C-1 describes.
    assert_eq!(gain_of(&f.cp, &b), -6.0, "gb was never consumed by a step aimed elsewhere");

    // NOTHING LOST, NOTHING DOUBLE-CONSUMED: five entries on the undo stack
    // (the four the walk did not reach, plus the racing commit) and the one
    // step the walk did apply, on the redo stack.
    assert_eq!(f.log.depths(), (5, 1));
    let revs = f.log.undo_path().revs;
    assert!(revs.contains(&gb_rev), "the refused step is still there to undo: {revs:?}");
    assert_eq!(revs.last().copied(), Some(last.rev + 1_000), "the racer is the new head");
    assert_eq!(f.cp.redo().unwrap().as_deref(), Some("ga2"), "the applied step is redoable");
    assert_eq!(gain_of(&f.cp, &a), -3.0, "and redo puts the walked-back edit back");
    assert_eq!(f.log.depths(), (6, 0), "every entry is accounted for");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// the 350 ms fallback, through the real commit path
// ---------------------------------------------------------------------------

#[test]
fn consecutive_same_key_edits_collapse_into_one_undo_step() {
    let f = fixture();
    let parent = tmp_parent("merge");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    let depth_before = f.log.depths().0;

    // A fader drag with NO gesture boundary — the boundary-less caller the
    // fallback exists for. These land microseconds apart, so they merge.
    for db in [-1.0, -2.0, -3.0, -4.0] {
        f.cp.set_track_mix(
            vec![TrackMixChange { track_id: t.clone(), gain_db: Some(db), ..TrackMixChange::new(t.clone()) }],
            TxMeta::user("set track gain"),
        )
        .unwrap();
    }
    assert_eq!(
        f.log.depths().0,
        depth_before + 1,
        "four boundary-less same-key edits are ONE undo step"
    );

    // And undoing that one step goes back to the value BEFORE the run, not
    // to the previous intermediate value (the first inverse is kept).
    f.cp.undo().unwrap();
    assert_eq!(gain_of(&f.cp, &t), 0.0, "undo returns to where the run started");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn a_gesture_batch_is_one_undo_step_and_is_never_merged_away() {
    let f = fixture();
    let parent = tmp_parent("gesture");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    let depth_before = f.log.depths().0;

    for label in ["drag 1", "drag 2"] {
        f.cp.gesture_begin(label.into()).unwrap();
        for db in [-2.0, -4.0, -8.0] {
            f.cp.set_track_mix(
                vec![TrackMixChange { track_id: t.clone(), gain_db: Some(db), ..TrackMixChange::new(t.clone()) }],
                TxMeta::user("set track gain"),
            )
            .unwrap();
        }
        f.cp.gesture_end().unwrap();
    }
    assert_eq!(
        f.log.depths().0,
        depth_before + 2,
        "two gestures are TWO undo steps — a pre-folded batch is never 350 ms-merged"
    );
    // Task 17's direct sink: history has the batch the moment the gesture
    // closes, not when something later drains the park.
    assert_eq!(gain_of(&f.cp, &t), -8.0);
    f.cp.undo().unwrap();
    assert_eq!(gain_of(&f.cp, &t), -8.0, "undoing drag 2 restores drag 1's end value");
    f.cp.undo().unwrap();
    assert_eq!(gain_of(&f.cp, &t), 0.0, "undoing drag 1 restores the pre-gesture baseline");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// transient
// ---------------------------------------------------------------------------

#[test]
fn transient_commits_reach_neither_history_nor_the_journal() {
    let f = fixture();
    let parent = tmp_parent("transient");
    let project = f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let dir = std::path::PathBuf::from(project.path.unwrap());
    let t = add_track(&f.cp, "Audio");

    let depth_before = f.log.depths().0;
    let lines_before = journal_lines(&dir).len();

    // Transport ops (scope ruling 2) — real document writes through the
    // channel, deliberately invisible to history and journal alike.
    f.cp.transport(TransportAction::SetLoop { enabled: true, start_samples: 0, end_samples: 4800 })
        .unwrap();
    f.cp.transport(TransportAction::SetStopAtEnd { enabled: true }).unwrap();
    // ...and a transient edit on a normal (non-transport) document path:
    // a MID-GESTURE fold, which is the only sanctioned shape for one (M-3,
    // now enforced by `debug_assert_transient_invariant` — a bare transient
    // `Set` against a track outside a gesture is the redo-corruption bug
    // that assertion exists to catch, so it cannot be used as a stand-in
    // here). Asserted while the gesture is still OPEN: the fold is
    // committed, document-visible, and in neither stream.
    f.cp.gesture_begin("mid-drag".into()).unwrap();
    f.cp.set_track_mix(
        vec![TrackMixChange { track_id: t.clone(), gain_db: Some(-12.0), ..TrackMixChange::new(t.clone()) }],
        TxMeta::user("set track gain"),
    )
    .unwrap();

    assert_eq!(f.log.depths().0, depth_before, "no transient batch becomes an undo step");
    assert_eq!(journal_lines(&dir).len(), lines_before, "no transient batch reaches the journal");
    // The write itself DID happen — transient means "not logged", not "not applied".
    assert_eq!(gain_of(&f.cp, &t), -12.0);
    f.cp.gesture_end().unwrap();
    assert!(f.cp.transport_state().stop_at_end);

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// net-no-op batches (fix round 1, I-1)
// ---------------------------------------------------------------------------

#[test]
fn a_net_noop_batch_produces_no_history_entry_and_no_journal_line() {
    let f = fixture();
    let parent = tmp_parent("noop");
    let project = f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let dir = std::path::PathBuf::from(project.path.unwrap());
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("add clip"), |tx| {
        tx.apply(Op::ClipAdd { clip: test_clip("c-1", &t), index: 0 })
    })
    .unwrap();

    let depth_before = f.log.depths().0;
    let lines_before = batches(&journal_lines(&dir)).len();
    let start_before = clip_start(&f.cp, "c-1");

    // (a) A move to the sample the clip ALREADY sits on. `fold_ops` elides
    // the net-zero `Set` group, so the commit succeeds with zero ops —
    // an ordinary outcome, not an error.
    f.cp.move_clip("c-1", start_before, TxMeta::user("move clip")).unwrap();
    // ...and a move away and straight back, folded inside ONE transaction.
    f.cp.commit(TxMeta::user("wiggle"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::Clip("c-1".into()),
            path: PropPath::TimelineStartSamples,
            from: serde_json::Value::Null,
            to: serde_json::json!(48_000u64),
        })?;
        tx.apply(Op::Set {
            object: ObjectRef::Clip("c-1".into()),
            path: PropPath::TimelineStartSamples,
            from: serde_json::Value::Null,
            to: serde_json::json!(start_before),
        })
    })
    .unwrap();
    // ...and a gain write to the value the track already has. NB: the
    // current value is read BEFORE the closure — `gain_of` takes the
    // session lock, and the closure body runs while `Session::transact`
    // already holds it (`parking_lot::Mutex` is not reentrant).
    let current_gain = gain_of(&f.cp, &t);
    f.cp.commit(TxMeta::user("same gain"), |tx| tx.apply(set_gain(&t, current_gain))).unwrap();

    assert_eq!(clip_start(&f.cp, "c-1"), start_before, "nothing actually moved");
    assert_eq!(
        f.log.depths().0,
        depth_before,
        "a net-no-op batch must not become a phantom undo step"
    );
    assert_eq!(
        batches(&journal_lines(&dir)).len(),
        lines_before,
        "a net-no-op batch must not write an empty ops[] journal line"
    );
    // Ctrl+Z therefore still undoes the last REAL edit — the clip add.
    assert_eq!(f.cp.undo().unwrap().as_deref(), Some("add clip"));
    assert!(f.cp.project_state().clips.is_empty(), "undo hit the real edit, not a phantom");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn a_real_edit_after_a_net_noop_records_normally() {
    let f = fixture();
    let parent = tmp_parent("noop-then-real");
    let project = f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let dir = std::path::PathBuf::from(project.path.unwrap());
    let t = add_track(&f.cp, "Audio");

    let depth_before = f.log.depths().0;
    let lines_before = batches(&journal_lines(&dir)).len();

    // A no-op...
    f.cp.commit(TxMeta::user("same gain"), |tx| tx.apply(set_gain(&t, 0.0))).unwrap();
    assert_eq!(f.log.depths().0, depth_before, "the no-op recorded nothing");

    // ...then a genuine edit, which must record and journal as usual — the
    // guard skips empty batches, it does not wedge the log.
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    assert_eq!(f.log.depths().0, depth_before + 1, "the real edit still records");
    assert_eq!(batches(&journal_lines(&dir)).len(), lines_before + 1);
    assert_eq!(gain_of(&f.cp, &t), -6.0);

    // And it undoes to the value the no-op left untouched.
    assert_eq!(f.cp.undo().unwrap().as_deref(), Some("gain"));
    assert_eq!(gain_of(&f.cp, &t), 0.0);

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// epochs
// ---------------------------------------------------------------------------

#[test]
fn an_epoch_boundary_clears_history_and_rotates_the_journal() {
    let f = fixture();
    let parent = tmp_parent("epoch");
    let a = f.cp.create_project(parent.to_str().unwrap(), "A").unwrap();
    let dir_a = std::path::PathBuf::from(a.path.unwrap());
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    f.cp.undo().unwrap();
    assert_eq!(f.log.depths(), (1, 1), "one undoable step and one redoable one");
    let a_lines = journal_lines(&dir_a).len();

    // A second project = a document swap = an epoch boundary.
    let b = f.cp.create_project(parent.to_str().unwrap(), "B").unwrap();
    let dir_b = std::path::PathBuf::from(b.path.unwrap());
    assert_eq!(f.log.depths(), (0, 0), "history and redo are cleared at a document swap");
    assert_eq!(journal_lines(&dir_a).len(), a_lines, "project A's journal is left exactly as it was");

    let b_lines = journal_lines(&dir_b);
    assert_eq!(b_lines.len(), 1, "the new journal starts with its boundary record");
    assert_eq!(b_lines[0]["epochEvent"], "create");
    assert_eq!(b_lines[0]["v"], aura_lib::control::op::OP_FORMAT_VERSION);
    assert!(b_lines[0]["epoch"].as_u64().unwrap() >= 1);
    assert_eq!(
        f.log.journal_path().unwrap(),
        dir_b.join("journal.ndjson"),
        "the writer rotated onto the new project"
    );

    // Re-OPENING project A appends to A's journal; it never truncates it.
    f.cp.open_project_epoch(&dir_a).unwrap();
    let reopened = journal_lines(&dir_a);
    assert_eq!(reopened.len(), a_lines + 1, "the open boundary is appended to A's existing log");
    assert_eq!(reopened.last().unwrap()["epochEvent"], "open");

    // A save MARK is not an epoch: history survives it, the journal is not
    // rotated, and only a mark line is written.
    let t2 = add_track(&f.cp, "Another");
    assert_eq!(f.log.depths().0, 1);
    let before_save = journal_lines(&dir_a).len();
    f.cp.save_project_mark().unwrap();
    let after_save = journal_lines(&dir_a);
    assert_eq!(after_save.len(), before_save + 1);
    assert_eq!(after_save.last().unwrap()["epochEvent"], "save");
    assert_eq!(f.log.depths().0, 1, "a snapshot mark does NOT clear history");
    assert!(!t2.is_empty());

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// C-1 (whole-branch review): the epoch guard used to protect
/// `execute_persist` ONLY. `record_commit`/`record_gesture` ran in the same
/// post-lock window — after `execute_host_forward`'s blocking plugin
/// round-trips and `execute_persist`'s disk I/O — with no epoch notion at
/// all, so an epoch function landing in that window produced a journal line
/// in the NEW project's file and a poppable undo entry describing a document
/// that was no longer open.
///
/// The interleaving is staged the way `execute_persist`'s own guard test
/// (`control/mod.rs`) stages its: run the commit, let the epoch function
/// complete, THEN deliver the sink call the in-flight effect phase would
/// have made, carrying the epoch it captured under `Session::transact`'s
/// lock. RE-OPENING THE SAME PROJECT is the reachable case the review names
/// — identical ids, so a stale inverse would apply cleanly against the
/// wrong revision instead of failing loudly.
#[test]
fn a_commit_whose_sink_call_lands_after_a_document_swap_reaches_neither_stream() {
    let f = fixture();
    let parent = tmp_parent("stale-epoch");
    let p = f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let dir = std::path::PathBuf::from(p.path.unwrap());
    let t = add_track(&f.cp, "Audio");

    // The commit that is about to be overtaken. It records normally here;
    // what matters is the (rev, epoch, ops) it captured under the lock.
    let committed =
        f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    let stale_epoch = committed.epoch;

    // The epoch function lands in the effect window: the SAME project is
    // re-opened, so history is cleared and the journal rotates (onto the
    // very same file, appending).
    f.cp.open_project_epoch(&dir).unwrap();
    assert_eq!(f.log.depths(), (0, 0), "the swap cleared history");
    let after_swap = journal_lines(&dir).len();

    // ...and only now does the overtaken commit reach the sink.
    let versions_after_swap = f.log.version_stats();
    assert_eq!(versions_after_swap.nodes, 0, "the swap drained the version graph too");
    f.log.record_commit(&committed, aura_lib::control::HistoryMode::Record);
    assert_eq!(
        f.log.depths(),
        (0, 0),
        "a stale-epoch commit must not push an undo entry onto the NEW document's stack"
    );
    assert_eq!(
        journal_lines(&dir).len(),
        after_swap,
        "a stale-epoch commit must not write a line into the NEW document's journal"
    );

    assert_eq!(
        f.log.version_stats(),
        versions_after_swap,
        "...nor a version node onto the NEW document's chain"
    );

    // The gesture sink is the same sink and gets the same guard.
    f.log.record_gesture(&committed);
    assert_eq!(f.log.depths(), (0, 0), "same for a gesture batch");
    assert_eq!(journal_lines(&dir).len(), after_swap, "same for a gesture batch");
    assert_eq!(f.log.version_stats(), versions_after_swap, "same for a gesture batch");

    // The guard is a stale-epoch guard, not a mute button: a commit at the
    // LIVE epoch still records, in both streams.
    // (`t` itself did not survive the reload — `Op::TrackAdd` sets no
    // `persist.project`, so the re-opened document is the one on disk.)
    let live = f
        .cp
        .commit(TxMeta::user("add after the swap"), |tx| {
            ops::add_track_tx(tx, Some("After".into()), Some("audio".into())).map(|_| ())
        })
        .unwrap();
    assert!(live.epoch > stale_epoch, "the swap really did move the epoch");
    assert_eq!(f.log.depths(), (1, 0), "the live document still records normally");
    assert_eq!(journal_lines(&dir).len(), after_swap + 1);

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// M-4: Ctrl+Z with the pointer still down. `undo` used to go straight to
/// the history stacks, so the open gesture's folded (transient, un-recorded)
/// writes were invisible to it: the undo skipped past the whole drag to the
/// step BEFORE it, and the drag's own value then landed as a separate step
/// whenever the pointer finally came up. `undo` now closes the gesture
/// first (F-7's auto-close, the same one `gesture_begin` uses), so the drag
/// is a finished, undoable step by the time the pop happens.
#[test]
fn undo_mid_gesture_closes_the_gesture_first_and_undoes_its_fold() {
    let f = fixture();
    let parent = tmp_parent("undo-mid-gesture");
    f.cp.create_project(parent.to_str().unwrap(), "P").unwrap();
    let t = add_track(&f.cp, "Audio");
    let (undo_before, _) = f.log.depths();

    f.cp.gesture_begin("fader".into()).unwrap();
    for db in [-2.0, -4.0, -8.0] {
        f.cp.set_track_mix(
            vec![TrackMixChange { track_id: t.clone(), gain_db: Some(db), ..TrackMixChange::new(t.clone()) }],
            TxMeta::user("set track gain"),
        )
        .unwrap();
    }
    assert_eq!(gain_of(&f.cp, &t), -8.0);
    assert_eq!(
        f.log.depths().0,
        undo_before,
        "mid-gesture folds are transient — the drag is not yet a history step"
    );

    // Ctrl+Z, pointer still down.
    let label = f.cp.undo().unwrap();
    assert_eq!(label.as_deref(), Some("fader"), "the undo consumed the GESTURE's own step");
    assert_eq!(gain_of(&f.cp, &t), 0.0, "the gain is back at its pre-gesture baseline");
    assert_eq!(
        f.log.depths(),
        (undo_before, 1),
        "exactly ONE entry was created by the close and consumed by the undo"
    );

    // The gesture really is closed: the next mix commit is its OWN history
    // step, not another fold into a still-open gesture.
    f.cp.set_track_mix(
        vec![TrackMixChange { track_id: t.clone(), gain_db: Some(-1.0), ..TrackMixChange::new(t.clone()) }],
        TxMeta::user("set track gain"),
    )
    .unwrap();
    assert_eq!(
        f.log.depths(),
        (undo_before + 1, 0),
        "a fresh, non-transient edit — so the gesture was closed, not left open"
    );

    // A late pointerup is still the no-op it always was.
    f.cp.gesture_end().unwrap();
    assert_eq!(f.log.depths(), (undo_before + 1, 0), "the trailing gesture_end changes nothing");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn an_unsaved_session_journals_nothing_but_undo_still_works() {
    let f = fixture();
    assert!(f.log.journal_path().is_none(), "no project dir, no journal file");

    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    assert_eq!(f.log.depths(), (2, 0), "history is in memory and works without a project");
    assert!(f.log.journal_path().is_none(), "still nothing on disk");

    f.cp.undo().unwrap();
    assert_eq!(gain_of(&f.cp, &t), 0.0, "undo works in an unsaved session");
    f.cp.redo().unwrap();
    assert_eq!(gain_of(&f.cp, &t), -6.0);

    // `save_project_as` mints the dir — the journal starts there.
    let parent = tmp_parent("unsaved");
    let saved = f.cp.save_project_as(parent.to_str().unwrap(), "Named").unwrap();
    let dir = std::path::PathBuf::from(saved.path.unwrap());
    let lines = journal_lines(&dir);
    assert_eq!(lines[0]["epochEvent"], "saveAs");
    assert_eq!(f.log.depths(), (0, 0), "save-as is a document-identity epoch: history resets");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// §7 test 5 — ATTRIBUTION, from the first enabled day
// ---------------------------------------------------------------------------

#[test]
fn every_journal_line_carries_a_resolvable_actor_and_run() {
    let f = fixture();
    let parent = tmp_parent("attribution");
    let project = f.cp.create_project(parent.to_str().unwrap(), "Attr").unwrap();
    let dir = std::path::PathBuf::from(project.path.unwrap());

    // USER — a human through the IPC surface.
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("set gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();

    // AGENT — an MCP tool call, named.
    f.cp.commit(TxMeta::agent("set_track_mix", "agent gain"), |tx| tx.apply(set_gain(&t, -3.0)))
        .unwrap();

    // SYSTEM — an automated process (a sidecar's post-job hook shape).
    f.cp.commit(TxMeta::system("stem import"), |tx| {
        ops::add_track_tx(tx, Some("Stem".into()), Some("audio".into())).map(|_| ())
    })
    .unwrap();

    // ENGINE — a finalize-shaped commit through the engine's own
    // `Committer` (the same object the control thread holds), non-transient
    // exactly like `stop recording`'s `ClipAdd` batch.
    f.engine_committer
        .commit_with_rebuild(
            TxMeta::engine("stop recording"),
            |tx| {
                ops::add_track_tx(tx, Some("Take 1".into()), Some("audio".into())).map(|_| ())
            },
            true,
            || {},
        )
        .unwrap();

    let lines = journal_lines(&dir);
    let mut seen: Vec<String> = Vec::new();
    for line in &lines {
        assert_eq!(
            line["v"],
            aura_lib::control::op::OP_FORMAT_VERSION,
            "every line carries the op-format version"
        );
        if line.get("epochEvent").is_some() {
            // An epoch record: no actor by design (ruling 4 — epochs are
            // not ops), but it must carry its epoch.
            assert!(line["epoch"].is_u64(), "epoch record without an epoch: {line}");
            continue;
        }
        // A batch line: actor and run must both be present and resolvable.
        let actor = &line["actor"];
        let run = line["run"].as_str().unwrap_or_else(|| panic!("no run id on {line}"));
        assert!(!run.is_empty(), "empty run id on {line}");
        assert!(line["label"].is_string(), "no label on {line}");
        assert!(line["rev"].is_u64(), "no rev on {line}");
        assert!(line["ops"].is_array(), "no ops on {line}");
        let name = match actor {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Object(o) => {
                // `Actor::Agent { tool }` — the tool name must be there.
                let tool = o["agent"]["tool"].as_str().expect("agent actor names its tool");
                assert!(!tool.is_empty());
                format!("agent:{tool}")
            }
            other => panic!("unresolvable actor {other} on {line}"),
        };
        seen.push(name);
    }

    assert!(seen.iter().any(|a| a == "user"), "user actor missing: {seen:?}");
    assert!(seen.iter().any(|a| a == "agent:set_track_mix"), "agent actor missing: {seen:?}");
    assert!(seen.iter().any(|a| a == "system"), "system actor missing: {seen:?}");
    assert!(seen.iter().any(|a| a == "engine"), "engine actor missing: {seen:?}");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// the corrected envelope schema
// ---------------------------------------------------------------------------

/// Match the ONE pattern shape `op-envelope.schema.json` uses for `kind`
/// (`^[a-z][a-zA-Z0-9]*$`) without pulling in a regex or jsonschema crate
/// (neither is a dependency, and adding one to check a six-token pattern
/// would be absurd). The test below asserts the schema's pattern STRING is
/// exactly this one first, so this hand-rolled matcher can never silently
/// drift from what the schema actually says.
const KIND_PATTERN: &str = "^[a-z][a-zA-Z0-9]*$";

fn matches_kind_pattern(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric())
}

#[test]
fn journal_op_kinds_match_the_corrected_envelope_schema_pattern() {
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string("../docs/ipc-schemas/op-envelope.schema.json")
            .expect("the envelope schema is in the repo"),
    )
    .expect("the envelope schema is valid JSON");
    let kind = &schema["$defs"]["op"]["properties"]["kind"];
    assert_eq!(
        kind["pattern"].as_str(),
        Some(KIND_PATTERN),
        "the schema's kind pattern changed — update KIND_PATTERN and matches_kind_pattern together"
    );
    assert!(
        kind["$comment"].as_str().unwrap_or_default().contains("ADR 0007"),
        "the correction must stay MARKED (scope ruling 1), not silent"
    );
    // The dotted DRAFT form must be gone — that is the correction.
    assert!(!matches_kind_pattern("clip.move"));

    // A sample of REAL kinds (the ones the plan names) must match.
    for k in ["set", "trackAdd", "tempoSet", "automationSetLane", "pluginAdd"] {
        assert!(matches_kind_pattern(k), "landed op kind {k} must match the corrected pattern");
    }

    // ...and so must every kind a real mixed session actually writes.
    let f = fixture();
    let parent = tmp_parent("schema");
    let project = f.cp.create_project(parent.to_str().unwrap(), "S").unwrap();
    let dir = std::path::PathBuf::from(project.path.unwrap());
    let t = add_track(&f.cp, "Audio");
    f.cp.commit(TxMeta::user("mixed"), |tx| {
        tx.apply(set_gain(&t, -6.0))?;
        tx.apply(Op::TempoSet {
            ppq: 960,
            events: vec![aura_lib::midi::TempoEvent { tick: 0, bpm: 100.0 }],
            meter: vec![aura_lib::midi::MeterEvent { tick: 0, num: 4, den: 4 }],
        })?;
        tx.apply(Op::AutomationSetLane {
            key: "lane".into(),
            lane: Some(aura_lib::plugins::automation::AutomationLane {
                id: "lane".into(),
                target_node: format!("track:{t}"),
                param_id: 0,
                points: vec![],
            }),
        })?;
        tx.apply(Op::PluginAdd {
            row: aura_lib::plugins::PluginInstanceInfo {
                id: "p-1".into(),
                uid: "stub:p-1".into(),
                name: "Stub".into(),
                format: "stub".into(),
                status: "stub".into(),
                track_id: Some(t.clone()),
            },
            index: 0,
        })?;
        // I-5: a state-carrying op, so the base64 assertion below runs
        // against a line a real session actually wrote.
        tx.apply(Op::PluginSetState { instance: "p-1".into(), state: b"hello".to_vec() })
    })
    .unwrap();
    f.cp.undo().unwrap();

    let lines = journal_lines(&dir);
    let mut kinds = 0usize;
    let mut state_blobs = 0usize;
    for line in &lines {
        assert_eq!(
            line["v"],
            aura_lib::control::op::OP_FORMAT_VERSION,
            "every line declares the format its ops are encoded in"
        );
        let Some(ops) = line["ops"].as_array() else { continue };
        for op in ops {
            let k = op["kind"].as_str().unwrap_or_else(|| panic!("op without a kind: {op}"));
            assert!(matches_kind_pattern(k), "journaled op kind {k:?} violates the schema pattern");
            kinds += 1;
            // I-5: a state blob is a base64 STRING on the wire. A JSON
            // number array here is the ~4x blowup the format-2 bump exists
            // to remove — and `additionalProperties: true` means the
            // envelope schema would happily validate either, so the shape
            // has to be asserted here or nowhere.
            if let Some(state) = op.get("state") {
                if !state.is_null() {
                    assert!(
                        state.is_string(),
                        "journaled state blob must be base64, not a number array: {op}"
                    );
                    state_blobs += 1;
                }
            }
        }
    }
    assert!(kinds >= 8, "the session should have journaled a good spread of kinds, got {kinds}");
    assert!(state_blobs >= 1, "the session should have journaled at least one state blob");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}
