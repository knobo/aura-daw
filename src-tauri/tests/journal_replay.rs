//! Plan F Task 9, the headline: **a cold tail replay reproduces the crashed
//! session's document.**
//!
//! `src-tauri/src/control/replay.rs` carries the reader's own unit tests —
//! the sort, the malformed-line rules, the tail extraction. THIS file drives
//! the real thing end to end: a real headless `ControlPlane` writes a real
//! `journal.ndjson` through real commits across every op family; the plane
//! is then dropped ("the crash"); and a fresh `Session`, built by the same
//! disk loaders `open_project_epoch` uses, is brought back up to the live
//! document by replaying the journal's unsaved tail.
//!
//! WHAT IS COMPARED, and what is not:
//! * COMPARED, byte-identically: tracks, audio clips, ppq, tempo and meter
//!   maps, every MIDI clip INCLUDING its note ids and its `next_note_id`
//!   watermark, automation lanes, project name/created-at. NO note-id
//!   masking and NO watermark normalization — that is the point (ruling
//!   F-9). The watermark is persisted with the snapshot, `apply_raw` mints
//!   from it and from nothing else, so a `noteId: 0` sentinel replayed cold
//!   lands on the same id it got live. This test asserting that IS the proof
//!   that inventory L-2 is benign.
//! * NOT compared: `rev`, `epoch`, `store.transport`. Transient batches bump
//!   `rev` without ever reaching the journal, so a replay counts journaled
//!   batches instead of revisions, and the transport a replay ends with is
//!   the BASE's (Task 7's I-2, and Task 11's standing acceptance criterion
//!   that a restore must keep live transport). To stop that exclusion from
//!   hiding a real defect, `transport.tempo_bpm` — the one transport field a
//!   journaled op (`TempoSet`) does write — is asserted SEPARATELY below.
//! * NOT exercised: plugins. A plugin op needs a host round-trip to be
//!   honest, and `plugins::state`'s adoption seam is process-global; the op
//!   families here are the ones a project can carry with no host installed.
//!
//! WHY THE BASE IS LOADED AT THE MARK RATHER THAN AFTER THE CRASH. AURA
//! auto-persists: nearly every op carries a `PersistEffect` and
//! `execute_persist` writes it out immediately after the commit, so re-
//! reading the project directory after the crash would hand back a document
//! that ALREADY contains the tail. The state a replay must start from is the
//! one the last save mark describes, so this test runs the loaders at that
//! moment and keeps their result. That is also why the open-time detection
//! only ever warns and never applies (ruling F-8) — see
//! `replay::detect_unsaved_tail`'s doc for what a non-zero tail does and does
//! not prove.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;

use aura_lib::audio::engine::{self, ControlMsg, EventSink};
use aura_lib::audio::rt::{GraphTables, SharedRt};
use aura_lib::audio::types::{Clip, Store};
use aura_lib::control::op::{ObjectRef, Op, PropPath, TxMeta};
use aura_lib::control::replay::{read_journal, replay_tail, unsaved_tail};
use aura_lib::control::{ops, Committer, ControlPlane, EventEmitter, HistoryLog, Session};
use aura_lib::ids::{ClipId, ContentId, LaneId, NoteId, SourceId, TrackId};
use aura_lib::midi::{MeterEvent, MidiClip, MidiNote, MidiStore, TempoEvent};
use aura_lib::plugins::automation::{AutomationLane, AutomationPoint};
use aura_lib::sidecars::jobs::JobManager;

// ---------------------------------------------------------------------------
// Fixture — the production wiring, headless (mirrors journal_and_history.rs)
// ---------------------------------------------------------------------------

struct NullEvents;
impl EventSink for NullEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

fn fixture() -> (Arc<ControlPlane>, Arc<Mutex<Session>>, engine::EngineHandle) {
    let shared = Arc::new(SharedRt::default());
    let tables = GraphTables::empty();
    let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
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
        committer,
        gesture.clone(),
    );
    let cp = Arc::new(ControlPlane::new(
        session.clone(),
        shared,
        tables,
        eng.clone(),
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        Box::new(|_e, _p| {}),
        log,
        gesture,
    ));
    (cp, session, eng)
}

fn tmp_parent(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aura-jreplay-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------------------
// The cold loader — the same three readers `open_project_epoch` runs
// ---------------------------------------------------------------------------

/// Build a scratch `Session` from what is on disk RIGHT NOW, using the same
/// loaders the real open path uses: `project::load` for the store,
/// `midi::persist::load_from_project` for the MIDI document (what
/// `adopt_midi_from_dir` wraps — that wrapper is `pub(crate)`), and
/// `modulation::persist::load_from_project` + the lane facade (what
/// `adopt_modulation_from_dir` wraps after Track F).
fn load_cold(dir: &Path) -> Session {
    let (project, dir) = aura_lib::audio::project::load(dir).expect("project loads");
    let mut store = Store::default();
    store.tracks = project.tracks.clone();
    store.clips = project.clips.clone();
    store.project_dir = Some(dir.clone());
    store.project_name = Some(project.name.clone());
    store.created_at = project.created_at.clone();
    if let Some(t) = &project.transport {
        store.transport.tempo_bpm = t.tempo_bpm;
        store.transport.state = "stopped".into();
        store.transport.loop_enabled = t.loop_enabled;
        store.transport.loop_start_samples = t.loop_start_samples;
        store.transport.loop_end_samples = t.loop_end_samples;
    }
    let mut midi = MidiStore::default();
    if let Some(v3) = aura_lib::midi::persist::load_from_project(&dir).expect("midi loads") {
        midi.ppq = v3.ppq;
        midi.tempo_events = v3.tempo_events;
        midi.meter_events = v3.meter_events;
        midi.clips = v3.clips;
    }
    let mut s = Session::new(store, midi);
    let doc = aura_lib::modulation::persist::load_from_project(&dir).expect("modulation loads");
    s.automation.lanes = aura_lib::modulation::compat::lanes_from_doc(&doc);
    s.modulation = doc;
    s
}

// ---------------------------------------------------------------------------
// The canonical oracle
// ---------------------------------------------------------------------------

/// Everything a journal replay is expected to reproduce, in one canonical
/// JSON string. `rev`/`epoch`/`transport` are deliberately absent — see this
/// file's header.
fn content_json(s: &Session) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "projectName": s.store.project_name,
        "createdAt": s.store.created_at,
        "tracks": s.store.tracks,
        "clips": s.store.clips,
        "ppq": s.midi.ppq,
        "tempoEvents": s.midi.tempo_events,
        "meterEvents": s.midi.meter_events,
        "midiClips": s.midi.clips,
        "automation": s.automation.lanes,
    }))
    .unwrap()
}

// ---------------------------------------------------------------------------
// Op builders
// ---------------------------------------------------------------------------

fn set_gain(track: &str, to: f64) -> Op {
    Op::Set {
        object: ObjectRef::Track(track.into()),
        path: PropPath::Gain,
        from: serde_json::Value::Null,
        to: serde_json::json!(to),
    }
}

fn audio_clip(id: &str, track_id: &str) -> Clip {
    Clip {
        id: id.into(),
        track_id: track_id.into(),
        name: format!("Clip {id}"),
        source_path: "audio/x.wav".into(),
        // NOT `SourceId::default()`: an EMPTY source id is a legacy shape
        // that `project::load` migrates by minting a deterministic id from
        // the path (`assign_source_ids`). The cold base would then carry an
        // id the live document never had — a loader migration showing up as
        // a replay difference, which would make this test lie about what it
        // measures.
        source_id: SourceId::from("src-x-wav"),
        source_channels: 2,
        source_sample_rate: 48_000,
        source_length_samples: 48_000,
        timeline_start_samples: 24_000,
        offset_samples: 0,
        length_samples: 48_000,
        gain_db: 0.0,
        fade_in_samples: 0,
        fade_out_samples: 0,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(track_id),
    }
}

fn midi_clip(id: &str, track_id: &str) -> MidiClip {
    MidiClip {
        id: ClipId::from(id),
        track_id: TrackId::from(track_id),
        name: format!("M {id}"),
        timeline_start_ticks: 0,
        length_ticks: 1920,
        notes: vec![],
        next_note_id: 1,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(track_id),
        content_length_ticks: None,
        transpose_semitones: 0,
        velocity_offset: 0,
    }
}

/// A note carrying the `noteId: 0` MINT SENTINEL — the exact shape ruling
/// F-9 is about, and the reason this test refuses to mask note ids.
fn sentinel_note(tick: u32, key: u8) -> MidiNote {
    MidiNote { tick, length_ticks: 240, key, velocity: 100, channel: 0, note_id: NoteId(0) }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

/// FIX ROUND 1's CRITICAL, through public API only: the detector must still
/// detect after the project has been re-opened.
///
/// `open_project_epoch` appends its own `{"epochEvent":"open","epoch":N}` to
/// the journal it is opening, with N above every epoch already in the file.
/// A max-epoch tail rule then picks that batch-less epoch and reports
/// "nothing unsaved" — on EVERY re-open within a process run, which is the
/// ordinary File▸Open A / edit / File▸Open B / File▸Open A path. The unit
/// fixtures could not see it because they hand-wrote journals ending in
/// batches, an order production never produces.
#[test]
fn unsaved_work_is_still_detected_after_the_project_is_reopened() {
    let (cp, _session, eng) = fixture();
    let parent = tmp_parent("reopen");
    let a = std::path::PathBuf::from(
        cp.create_project(parent.to_str().unwrap(), "A").unwrap().path.unwrap(),
    );
    let b = std::path::PathBuf::from(
        cp.create_project(parent.to_str().unwrap(), "B").unwrap().path.unwrap(),
    );
    cp.open_project_epoch(&a).unwrap();

    // Two real edits, never saved.
    let mut t = String::new();
    cp.commit(TxMeta::user("add track"), |tx| {
        t = ops::add_track_tx(tx, Some("Audio".into()), Some("audio".into()))?.id.to_string();
        Ok(())
    })
    .unwrap();
    cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t, -6.0))).unwrap();
    assert_eq!(
        aura_lib::control::replay::detect_unsaved_tail(&a),
        2,
        "two unsaved batches, before anything re-opens the project"
    );

    // The user goes to another project and comes back — the ordinary path.
    cp.open_project_epoch(&b).unwrap();
    cp.open_project_epoch(&a).unwrap();

    assert_eq!(
        aura_lib::control::replay::detect_unsaved_tail(&a),
        2,
        "the re-open's own boundary record must not swallow the tail"
    );

    // Anti-vacuity: the trap is really in the file. Its newest epoch is
    // above the batches' epoch AND carries no batches of its own — which is
    // exactly what made a max-epoch rule report zero here.
    let report = read_journal(&a.join("journal.ndjson")).unwrap();
    let newest = report.records.iter().map(|r| r.epoch()).max().unwrap();
    let batch_epoch = report
        .records
        .iter()
        .filter(|r| matches!(r, aura_lib::control::replay::JournalRecord::Batch { .. }))
        .map(|r| r.epoch())
        .max()
        .unwrap();
    assert!(newest > batch_epoch, "the re-open wrote a strictly newer epoch: {newest} > {batch_epoch}");

    eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn a_cold_tail_replay_reproduces_the_crashed_sessions_document_byte_identically() {
    let (cp, session_handle, eng) = fixture();
    let parent = tmp_parent("fidelity");
    let project = cp.create_project(parent.to_str().unwrap(), "Crashed").unwrap();
    let dir = std::path::PathBuf::from(project.path.clone().unwrap());

    // ---- before the mark: one edit from each family that can live without
    // a plugin host --------------------------------------------------------
    let mut t1 = String::new();
    cp.commit(TxMeta::user("add track"), |tx| {
        let t = ops::add_track_tx(tx, Some("Audio".into()), Some("audio".into()))?;
        t1 = t.id.to_string();
        Ok(())
    })
    .unwrap();
    cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_gain(&t1, -4.5))).unwrap();
    cp.commit(TxMeta::user("add clip"), |tx| {
        tx.apply(Op::ClipAdd { clip: audio_clip("c-1", &t1), index: 0 })
    })
    .unwrap();
    cp.commit(TxMeta::user("move clip"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::Clip("c-1".into()),
            path: PropPath::TimelineStartSamples,
            from: serde_json::Value::Null,
            to: serde_json::json!(96_000u64),
        })
    })
    .unwrap();
    cp.commit(TxMeta::user("add midi clip"), |tx| {
        tx.apply(Op::MidiClipAdd { clip: midi_clip("mc-1", &t1), index: 0 })
    })
    .unwrap();
    cp.commit(TxMeta::user("notes"), |tx| {
        tx.apply(Op::MidiSetNotes {
            clip: ClipId::from("mc-1"),
            notes: vec![sentinel_note(0, 60), sentinel_note(480, 64)],
        })
    })
    .unwrap();
    cp.commit(TxMeta::user("tempo"), |tx| {
        tx.apply(Op::TempoSet {
            ppq: 960,
            events: vec![TempoEvent { tick: 0, bpm: 128.0 }],
            meter: vec![MeterEvent { tick: 0, num: 3, den: 4 }],
        })
    })
    .unwrap();
    cp.commit(TxMeta::user("automation"), |tx| {
        tx.apply(Op::AutomationSetLane {
            key: "lane-1".into(),
            lane: Some(AutomationLane {
                id: "lane-1".into(),
                target_node: format!("track:{t1}"),
                param_id: 0,
                points: vec![
                    AutomationPoint { tick: 0, value: 0.1 },
                    AutomationPoint { tick: 960, value: 0.9 },
                ],
            }),
        })
    })
    .unwrap();

    // ---- the save mark ---------------------------------------------------
    cp.save_project_mark().unwrap();
    // The document a crash would come back to: read from disk, at the mark.
    let base = load_cold(&dir);
    let base_json = content_json(&base);

    // ---- after the mark: the tail ----------------------------------------
    let mut t2 = String::new();
    cp.commit(TxMeta::user("add track 2"), |tx| {
        let t = ops::add_track_tx(tx, Some("Second".into()), Some("audio".into()))?;
        t2 = t.id.to_string();
        Ok(())
    })
    .unwrap();
    cp.commit(TxMeta::user("gain 2"), |tx| tx.apply(set_gain(&t2, -9.0))).unwrap();
    cp.commit(TxMeta::user("add midi clip 2"), |tx| {
        tx.apply(Op::MidiClipAdd { clip: midi_clip("mc-2", &t2), index: 1 })
    })
    .unwrap();
    // More sentinels, on a clip whose watermark is already ABOVE 1 — the
    // minting has to come off the persisted watermark, not off a count.
    cp.commit(TxMeta::user("more notes"), |tx| {
        tx.apply(Op::MidiSetNotes {
            clip: ClipId::from("mc-1"),
            notes: vec![sentinel_note(0, 60), sentinel_note(480, 64), sentinel_note(960, 67)],
        })
    })
    .unwrap();
    cp.commit(TxMeta::user("notes on the new clip"), |tx| {
        tx.apply(Op::MidiSetNotes {
            clip: ClipId::from("mc-2"),
            notes: vec![sentinel_note(120, 48)],
        })
    })
    .unwrap();
    cp.commit(TxMeta::user("tempo 2"), |tx| {
        tx.apply(Op::TempoSet {
            ppq: 960,
            events: vec![TempoEvent { tick: 0, bpm: 128.0 }, TempoEvent { tick: 3840, bpm: 96.0 }],
            meter: vec![MeterEvent { tick: 0, num: 3, den: 4 }],
        })
    })
    .unwrap();
    cp.commit(TxMeta::user("automation 2"), |tx| {
        tx.apply(Op::AutomationSetLane {
            key: "lane-2".into(),
            lane: Some(AutomationLane {
                id: "lane-2".into(),
                target_node: format!("track:{t2}"),
                param_id: 0,
                points: vec![AutomationPoint { tick: 480, value: 0.42 }],
            }),
        })
    })
    .unwrap();

    // ---- the crash -------------------------------------------------------
    let live_json = content_json(&session_handle.lock());
    let live_tempo_bpm = session_handle.lock().store.transport.tempo_bpm;
    assert_ne!(live_json, base_json, "the tail really moved the document");
    eng.send(ControlMsg::Shutdown);
    drop(cp);

    // ---- the cold path ---------------------------------------------------
    let report = read_journal(&dir.join("journal.ndjson")).expect("the journal is readable");
    assert_eq!(report.skipped(), 0, "a journal this process just wrote has no unusable lines");
    assert!(!report.torn_tail, "and no torn tail — nothing interrupted the writer");
    let tail = unsaved_tail(&report);
    assert_eq!(tail.len(), 7, "exactly the seven batches committed after the mark");

    let session = Mutex::new(base);
    let applied = replay_tail(&session, &tail).expect("the tail replays cleanly");
    assert_eq!(applied, 7);

    // ---- the whole point -------------------------------------------------
    let replayed = session.lock();
    assert_eq!(
        content_json(&replayed),
        live_json,
        "cold replay reproduces the crashed document — note ids and watermarks included, unmasked"
    );
    // The one transport field a journaled op writes, asserted separately so
    // the transport exclusion above cannot hide a real regression.
    assert_eq!(replayed.store.transport.tempo_bpm, live_tempo_bpm, "TempoSet's bpm mirror replayed");
    // Anti-vacuity: the comparison must be over a document that actually has
    // the interesting things in it.
    let mc1 = replayed.midi.clips.iter().find(|c| c.id.as_str() == "mc-1").expect("mc-1");
    assert_eq!(mc1.notes.len(), 3, "the post-mark note write really replayed");
    assert!(mc1.notes.iter().all(|n| n.note_id.0 >= 1), "every sentinel got a real id");
    assert_eq!(mc1.next_note_id, 6, "watermark: 1,2 before the mark, then 3,4,5 after");
    assert_eq!(replayed.store.tracks.len(), 2);
    assert_eq!(replayed.automation.lanes.len(), 2);

    drop(replayed);
    let _ = std::fs::remove_dir_all(&parent);
}
