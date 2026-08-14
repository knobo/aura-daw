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
    ops, Committer, ControlPlane, EventEmitter, HistoryLog, Session, TrackMixChange,
    TransportAction,
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
    let eng = engine::start(
        shared.clone(),
        tables.clone(),
        session.clone(),
        Box::new(NullEvents),
        committer.clone(),
    );
    let cp = Arc::new(ControlPlane::new(
        session,
        shared,
        tables,
        eng.clone(),
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        Box::new(|_e, _p| {}),
        log.clone(),
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
    // ...and an explicitly transient edit on a normal document path.
    f.cp.commit(TxMeta::user("mid-drag").transient(), |tx| tx.apply(set_gain(&t, -12.0))).unwrap();

    assert_eq!(f.log.depths().0, depth_before, "no transient batch becomes an undo step");
    assert_eq!(journal_lines(&dir).len(), lines_before, "no transient batch reaches the journal");
    // The write itself DID happen — transient means "not logged", not "not applied".
    assert_eq!(gain_of(&f.cp, &t), -12.0);
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
    assert_eq!(b_lines[0]["v"], 1);
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
        assert_eq!(line["v"], 1, "every line carries the op-format version");
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
        })
    })
    .unwrap();
    f.cp.undo().unwrap();

    let lines = journal_lines(&dir);
    let mut kinds = 0usize;
    for line in &lines {
        let Some(ops) = line["ops"].as_array() else { continue };
        for op in ops {
            let k = op["kind"].as_str().unwrap_or_else(|| panic!("op without a kind: {op}"));
            assert!(matches_kind_pattern(k), "journaled op kind {k:?} violates the schema pattern");
            kinds += 1;
        }
    }
    assert!(kinds >= 8, "the session should have journaled a good spread of kinds, got {kinds}");

    f.eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}
