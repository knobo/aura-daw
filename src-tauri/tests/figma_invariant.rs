//! Gate E, §7 test 4 — THE FIGMA INVARIANT.
//!
//! "Every mutation reaches the document through the one channel, and every
//! declared read path is pure." The test that proves it, end to end, on the
//! code exactly as Tasks 1-16 left it:
//!
//! 1. build a real (headless) `ControlPlane`, open a real project on disk;
//! 2. apply a SCRIPTED MIXED SESSION touching every family this plan routed
//!    (mix `Set`s, track add/remove, audio clip add + move, midi clip add +
//!    notes + bounds, `TempoSet`, `AutomationSetLane`, the four plugin ops,
//!    an instrument binding, a real audio import, and a TRANSIENT transport
//!    op);
//! 3. snapshot the canonical document (store + midi + plugins + automation);
//! 4. UNDO everything — inverses replayed through NEW commits, in reverse
//!    order, never by executing a parked `effect` (the placeholder rule);
//! 5. run EVERY declared read path, each bracketed by snapshots that must be
//!    byte-equal (the pure-reader sweep — a read that mutates fails here);
//! 6. REDO everything — the batches' own forward ops, replayed in the
//!    original order (sound because every op in the vocabulary is
//!    absolute-valued, never a delta; this is the same algebra
//!    `ControlPlane::redo` uses);
//! 7. assert the post-redo canonical snapshot equals the pre-undo one, with
//!    per-clip `next_note_id` MASKED per binding scope ruling 3 (ADR 0001:
//!    the watermark is monotonic and never rewinds) and separately asserted
//!    never to decrease.
//!
//! The fixture and helper style deliberately mirror `tests/pure_readers.rs`
//! (the real headless `ControlPlane`) and `tests/channel_properties.rs` (the
//! snapshot + `mask_watermarks` oracle) — this directory's established
//! convention of local, independent fixtures rather than the crate's private
//! `#[cfg(test)]` ones.
//!
//! READ PATHS THAT ARE NOT REACHABLE FROM HERE, and why (recorded so the
//! "every declared read path" claim stays auditable):
//! * `midi_get_clips` (midi/mod.rs) is a `#[tauri::command]` over
//!   `State<'_, MidiState>`; `MidiState` can only be wired through the Tauri
//!   app setup. Its document half is `session.midi.clips`, which
//!   `ControlPlane::project_state().midi_clips` serves from the same lock and
//!   IS swept below; the specific "a midi read must not resync/adopt"
//!   RED/GREEN proof lives in-crate (`src/midi/mod.rs`'s
//!   `read_midi_never_resyncs_even_when_project_dir_moved_underneath_it`),
//!   exactly as `tests/pure_readers.rs` already documents.
//! * `midi_export_file` is a read + an export ARTIFACT write (carve-out: it
//!   writes a file OUTSIDE the document, never the document itself) — same
//!   `State<'_, MidiState>` reachability limit.
//! * meters (`read_meters`) need a running device to produce a frame; the
//!   read itself is swept (it returns `None` headless, stably).

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

use aura_lib::audio::engine::{self, ControlMsg, EventSink};
use aura_lib::audio::rt::{GraphTables, SharedRt};
use aura_lib::audio::types::{Clip, Store};
use aura_lib::control::op::{Actor, ObjectRef, Op, PropPath, TxMeta};
use aura_lib::control::{
    Committed, Committer, ControlPlane, EventEmitter, ImportClipRequest, Session, TransportAction,
};
use aura_lib::ids::{ContentId, LaneId, NoteId, SourceId};
use aura_lib::midi::{MeterEvent, MidiClip, MidiNote, MidiStore, TempoEvent};
use aura_lib::plugins::automation::{AutomationLane, AutomationPoint};
use aura_lib::plugins::PluginInstanceInfo;
use aura_lib::sidecars::jobs::JobManager;

// ---------------------------------------------------------------------------
// Fixture (local + independent, per this directory's convention)
// ---------------------------------------------------------------------------

struct NullEvents;
impl EventSink for NullEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

/// A real, headless `ControlPlane` (real engine control thread, no audio
/// device required — `engine::start`'s own doc: "without a device the engine
/// still runs... so the UI and tests stay functional").
fn fixture() -> (Arc<ControlPlane>, engine::EngineHandle) {
    let shared = Arc::new(SharedRt::default());
    let tables = GraphTables::empty();
    let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
    // ONE `HistoryLog`, shared by the engine's `Committer` and the
    // `ControlPlane` — the same wiring `audio::init` + lib.rs's setup do in
    // production (Plan E Task 17). A per-instance log would hide the
    // engine's own non-transient commits from the history under test.
    let log = Arc::new(aura_lib::control::HistoryLog::new());
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
        committer,
    );
    let cp = Arc::new(ControlPlane::new(
        session,
        shared,
        tables,
        eng.clone(),
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        Box::new(|_e, _p| {}),
        log,
    ));
    (cp, eng)
}

fn tmp_parent(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aura-figma-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------------------
// The canonical snapshot oracle
// ---------------------------------------------------------------------------

/// Deep, canonical snapshot of the WHOLE document: `Store` + `MidiStore` +
/// the plugin document half + the automation lanes. Wider than
/// `channel_properties.rs`'s `snapshot` (which predates Tasks 9/10) —
/// Gate E's claim is about the whole document, so the plugin registry's
/// document half and the automation store are part of the oracle here.
///
/// `Session.rev`/`Session.epoch` are deliberately absent (both are
/// `pub(crate)`, and both legitimately advance across undo/redo — every
/// undo is itself a transaction).
///
/// THE TRANSPORT SPLIT (binding scope ruling 2, "position is engine state,
/// not document state"): `ops::transport_snapshot` composes the store's
/// transport mirror with LIVE RT ATOMICS, and three of those atomics are
/// written by the ENGINE on its own turn, with no document write involved
/// at all — `position` (the RT callback, per buffer, engine.rs:330),
/// `sample_rate` (`open_output`'s device probe, engine.rs:705) and
/// `song_end` (recomputed from the graph on every rebuild,
/// engine.rs:864). They are engine state that happens to be visible through
/// a document-shaped read, so a DOCUMENT oracle must not include them:
/// doing so would make this gate assert on when the engine control thread
/// got around to its next turn, not on whether the document survived a
/// round trip. The transport fields that ARE document state (`state`,
/// `tempo_bpm`, the loop window, `stop_at_end` — all persisted in
/// project.json, all written only through `Op::Set{Transport,…}`) stay in.
fn snapshot(cp: &ControlPlane) -> String {
    let state = cp.project_state();
    let t = &state.transport;
    let transport = serde_json::json!({
        "state": t.state,
        "tempoBpm": t.tempo_bpm,
        "loopEnabled": t.loop_enabled,
        "loopStartSamples": t.loop_start_samples,
        "loopEndSamples": t.loop_end_samples,
        "stopAtEnd": t.stop_at_end,
    });
    let plugin_rows = cp.plugin_rows();
    let params: std::collections::BTreeMap<String, Vec<aura_lib::plugins::ParamInfo>> = plugin_rows
        .iter()
        .map(|r| (r.id.clone(), cp.plugin_params(&r.id).unwrap_or_default()))
        .collect();
    serde_json::json!({
        "projectName": state.project_name,
        "transport": transport,
        "tracks": state.tracks,
        "clips": state.clips,
        "midiClips": state.midi_clips,
        "ppq": state.ppq,
        "tempoEvents": state.tempo_events,
        "meterMap": state.meter_map,
        "plugins": plugin_rows,
        "pluginParams": params,
        "automation": cp.automation_lanes(),
    })
    .to_string()
}

/// Map of `midi clip id -> next_note_id`, read off store truth (scope ruling
/// 3's "assert monotonicity separately" half).
fn watermarks(cp: &ControlPlane) -> HashMap<String, u32> {
    cp.project_state().midi_clips.iter().map(|c| (c.id.to_string(), c.next_note_id)).collect()
}

/// BINDING SCOPE RULING 3 (copied, deliberately, from
/// `tests/channel_properties.rs::mask_watermarks` — same oracle, same
/// rationale, one plan): zero every midi clip's `nextNoteId` before
/// comparing. `MidiSetNotes`'s inverse never rewinds the watermark (ADR
/// 0001: ids are never reused, so an undo cannot "give back" an id it
/// already handed out). Every OTHER field — notes and their individual
/// `noteId`s included — stays under strict byte-identical scrutiny; the
/// asymmetry this masks is asserted separately via `watermarks`.
fn mask_watermarks(snapshot_json: &str) -> String {
    let mut v: serde_json::Value =
        serde_json::from_str(snapshot_json).expect("snapshot() always emits valid JSON");
    if let Some(clips) = v.pointer_mut("/midiClips").and_then(|c| c.as_array_mut()) {
        for clip in clips {
            if let Some(obj) = clip.as_object_mut() {
                obj.insert("nextNoteId".to_string(), serde_json::json!(0));
            }
        }
    }
    serde_json::to_string(&v).expect("re-serializing a parsed Value never fails")
}

// ---------------------------------------------------------------------------
// Op payload builders
// ---------------------------------------------------------------------------

fn audio_clip(id: &str, track_id: &str, path: &str) -> Clip {
    Clip {
        id: id.into(),
        track_id: track_id.into(),
        name: format!("Clip {id}"),
        source_path: path.into(),
        source_id: SourceId::default(),
        source_channels: 2,
        source_sample_rate: 48_000,
        source_length_samples: 48_000,
        timeline_start_samples: 0,
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
        id: id.into(),
        track_id: track_id.into(),
        name: format!("MIDI {id}"),
        timeline_start_ticks: 0,
        length_ticks: 3840,
        notes: vec![],
        next_note_id: 1,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(track_id),
        content_length_ticks: None,
        transpose_semitones: 0,
        velocity_offset: 0,
    }
}

fn note(tick: u32, key: u8) -> MidiNote {
    // `noteId: 0` = MINT (the keep-rule sentinel, ADR 0001 / Task 15).
    MidiNote { tick, length_ticks: 480, key, velocity: 100, channel: 0, note_id: NoteId(0) }
}

fn plugin_row(id: &str, track_id: &str) -> PluginInstanceInfo {
    // `format: "stub"` is the test's plugin HOST STUB: `execute_host_forward`
    // (control/mod.rs) dispatches `Instantiate`/`Destroy`/`LoadState`/
    // `ParamWrite` by `row.format`, and every arm is a documented no-op for a
    // format that is neither "clap" nor "lv2". The DOCUMENT half — the row,
    // the param mirror, the state blob — travels through the ops exactly as
    // it does for a real host, which is what this gate is about.
    PluginInstanceInfo {
        id: id.into(),
        uid: format!("stub:{id}"),
        name: format!("Stub {id}"),
        format: "stub".into(),
        status: "stub".into(),
        track_id: Some(track_id.into()),
    }
}

fn set_track(track_id: &str, path: PropPath, to: serde_json::Value) -> Op {
    Op::Set { object: ObjectRef::Track(track_id.into()), path, from: serde_json::Value::Null, to }
}

/// A 0.25 s stereo 48 kHz sine as a real `.wav` on disk, so
/// `ControlPlane::import_audio_clip` has genuine audio to decode AND builds
/// a genuine waveform pyramid — which is what makes the waveform-tile read
/// path below a REAL read instead of a miss.
fn write_test_wav(path: &std::path::Path) {
    let frames = 12_000usize;
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = ((i as f32) * 0.02).sin() * 0.5;
        samples.push(v);
        samples.push(v);
    }
    aura_lib::control::export::write_wav(path, &samples, 48_000, 16).expect("write test wav");
}

// ---------------------------------------------------------------------------
// The scripted mixed session
// ---------------------------------------------------------------------------

struct Scripted {
    /// Every NON-transient batch, in commit order — the test's own history.
    log: Vec<Committed>,
    audio_track: String,
    midi_track: String,
    imported_clip_id: String,
}

/// Applies the scripted mixed session, capturing each batch's `Committed`
/// (the test's own stand-in for the history stack Deliverable 3 builds —
/// this test must pass on the code as it stands, BEFORE the log turns on).
fn scripted_session(cp: &Arc<ControlPlane>, dir: &std::path::Path) -> Scripted {
    let mut log: Vec<Committed> = Vec::new();

    // ---- structural: two tracks (the tx-tier `add_track` path) ------------
    let mut audio_track = String::new();
    let mut midi_track = String::new();
    log.push(
        cp.commit(TxMeta::user("add tracks"), |tx| {
            let a = aura_lib::control::ops::add_track_tx(tx, Some("Audio".into()), Some("audio".into()))?;
            let m = aura_lib::control::ops::add_track_tx(tx, Some("Keys".into()), Some("midi".into()))?;
            audio_track = a.id.to_string();
            midi_track = m.id.to_string();
            Ok(())
        })
        .expect("add tracks"),
    );

    // ---- mix Sets (row 1) -------------------------------------------------
    log.push(
        cp.commit(TxMeta::user("mix"), |tx| {
            tx.apply(set_track(&audio_track, PropPath::Gain, serde_json::json!(-6.0)))?;
            tx.apply(set_track(&audio_track, PropPath::Pan, serde_json::json!(-0.25)))?;
            tx.apply(set_track(&midi_track, PropPath::Muted, serde_json::json!(true)))?;
            tx.apply(set_track(&midi_track, PropPath::Soloed, serde_json::json!(true)))?;
            Ok(())
        })
        .expect("mix"),
    );

    // ---- audio clip add + move (rows 3, 20) -------------------------------
    log.push(
        cp.commit(TxMeta::user("add clip"), |tx| {
            tx.apply(Op::ClipAdd { clip: audio_clip("c-fig", &audio_track, "audio/c-fig.wav"), index: 0 })
        })
        .expect("clip add"),
    );
    log.push(cp.move_clip("c-fig", 96_000, TxMeta::user("move clip")).expect("move clip"));
    log.push(
        cp.commit(TxMeta::user("trim clip"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Clip("c-fig".into()),
                path: PropPath::LengthSamples,
                from: serde_json::Value::Null,
                to: serde_json::json!(24_000u64),
            })?;
            tx.apply(Op::Set {
                object: ObjectRef::Clip("c-fig".into()),
                path: PropPath::OffsetSamples,
                from: serde_json::Value::Null,
                to: serde_json::json!(1_000u64),
            })
        })
        .expect("trim clip"),
    );

    // ---- midi clip add + notes + bounds (rows 5, 6, 7) --------------------
    log.push(
        cp.commit(TxMeta::user("add midi clip"), |tx| {
            tx.apply(Op::MidiClipAdd { clip: midi_clip("mc-fig", &midi_track), index: 0 })
        })
        .expect("midi clip add"),
    );
    log.push(
        cp.commit(TxMeta::user("set midi notes"), |tx| {
            tx.apply(Op::MidiSetNotes {
                clip: "mc-fig".into(),
                notes: vec![note(0, 60), note(480, 64), note(960, 67)],
            })
        })
        .expect("midi set notes"),
    );
    log.push(
        cp.commit(TxMeta::user("midi clip bounds"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::MidiClip("mc-fig".into()),
                path: PropPath::TimelineStartTicks,
                from: serde_json::Value::Null,
                to: serde_json::json!(1920u64),
            })?;
            tx.apply(Op::Set {
                object: ObjectRef::MidiClip("mc-fig".into()),
                path: PropPath::LengthTicks,
                from: serde_json::Value::Null,
                to: serde_json::json!(7680u64),
            })?;
            tx.apply(Op::Set {
                object: ObjectRef::MidiClip("mc-fig".into()),
                path: PropPath::ContentLengthTicks,
                from: serde_json::Value::Null,
                to: serde_json::json!(1920u64),
            })
        })
        .expect("midi clip bounds"),
    );

    // ---- tempo map (row 4) ------------------------------------------------
    log.push(
        cp.commit(TxMeta::agent("set_tempo_map", "tempo"), |tx| {
            tx.apply(Op::TempoSet {
                ppq: 960,
                events: vec![TempoEvent { tick: 0, bpm: 128.0 }, TempoEvent { tick: 3840, bpm: 96.0 }],
                meter: vec![MeterEvent { tick: 0, num: 3, den: 4 }],
            })
        })
        .expect("tempo set"),
    );

    // ---- automation lane (row 11) ----------------------------------------
    log.push(
        cp.commit(TxMeta::user("automation"), |tx| {
            tx.apply(Op::AutomationSetLane {
                key: "lane-fig".into(),
                lane: Some(AutomationLane {
                    id: "lane-fig".into(),
                    target_node: format!("track:{audio_track}"),
                    param_id: 0,
                    points: vec![
                        AutomationPoint { tick: 0, value: 0.0 },
                        AutomationPoint { tick: 1920, value: 1.0 },
                    ],
                }),
            })
        })
        .expect("automation set"),
    );

    // ---- plugin family (rows 12, 13, 14) ---------------------------------
    log.push(
        cp.commit(TxMeta::user("add plugin"), |tx| {
            tx.apply(Op::PluginAdd { row: plugin_row("p-fig", &midi_track), index: 0 })
        })
        .expect("plugin add"),
    );
    log.push(
        cp.commit(TxMeta::user("plugin param"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Plugin("p-fig".into()),
                path: PropPath::Param { index: 3 },
                from: serde_json::Value::Null,
                to: serde_json::json!(0.75),
            })
        })
        .expect("plugin param"),
    );
    log.push(
        cp.commit(TxMeta::user("plugin patch"), |tx| {
            tx.apply(Op::PluginSetState { instance: "p-fig".into(), state: vec![7u8, 7, 7, 7] })
        })
        .expect("plugin set state"),
    );
    // Instrument binding (row 2) — the plugin exists by now, so undo's
    // reverse order unbinds BEFORE the plugin row goes away.
    log.push(
        cp.commit(TxMeta::user("bind instrument"), |tx| {
            tx.apply(set_track(&midi_track, PropPath::InstrumentId, serde_json::json!("plugin:p-fig")))
        })
        .expect("bind instrument"),
    );
    // A second instance, added then REMOVED — the `PluginRemove` family.
    log.push(
        cp.commit(TxMeta::user("add plugin 2"), |tx| {
            tx.apply(Op::PluginAdd { row: plugin_row("p-fig2", &audio_track), index: 1 })
        })
        .expect("plugin add 2"),
    );
    log.push(
        cp.commit(TxMeta::user("remove plugin 2"), |tx| {
            tx.apply(Op::PluginRemove {
                row: plugin_row("p-fig2", &audio_track),
                index: 1,
                state: Some(vec![9u8, 9]),
                params: vec![],
            })
        })
        .expect("plugin remove 2"),
    );

    // ---- track add + remove (row 1's structural half) --------------------
    let mut doomed = String::new();
    log.push(
        cp.commit(TxMeta::system("add scratch track"), |tx| {
            let t = aura_lib::control::ops::add_track_tx(tx, Some("Scratch".into()), Some("audio".into()))?;
            doomed = t.id.to_string();
            Ok(())
        })
        .expect("add scratch track"),
    );
    let doomed_row = cp
        .project_state()
        .tracks
        .into_iter()
        .find(|t| t.id == doomed)
        .expect("scratch track exists");
    log.push(
        cp.commit(TxMeta::system("remove scratch track"), |tx| {
            tx.apply(Op::TrackRemove { track: doomed_row.clone(), index: 0, clips: vec![], clip_indices: vec![] })
        })
        .expect("remove scratch track"),
    );

    // ---- a REAL import (rows 16-17's sink shape, and the source of a real
    // waveform pyramid for the tile read below).
    //
    // Deliberately onto its OWN auto-created track (`track_id: None`), and
    // deliberately NOT in `log`: it is the test's "content that must survive
    // the whole round trip untouched" baseline. Putting it on a track that
    // the logged batches created would be a HISTORY HOLE — undoing the
    // batch that created that track removes the import's clip with it (a
    // `TrackRemove` takes its clips), while the redo replays the ORIGINAL
    // `Op::TrackAdd { clips: [] }` and the clip never comes back. That is
    // not a defect in the channel; it is the exact reason `history.record`
    // (Deliverable 3) must record EVERY non-transient commit: replay is
    // faithful only against a COMPLETE log. Recorded here because the first
    // draft of this test hit it.
    let wav = dir.join("figma-source.wav");
    write_test_wav(&wav);
    let imported = cp
        .import_audio_clip(ImportClipRequest {
            path: wav.to_string_lossy().into_owned(),
            track_id: None,
            at_samples: Some(0),
        })
        .expect("import audio clip");

    // ---- TRANSIENT transport ops (rows 28-29, scope ruling 2) ------------
    // Through the channel (attributed, atomic, single-writer) but NEVER in
    // history and never journaled: they are deliberately absent from `log`.
    // No `Play` here on purpose — a playing transport lets the engine's own
    // auto-stop commit land mid-test, which would make the snapshot depend
    // on wall-clock timing rather than on the invariant under test.
    cp.transport(TransportAction::SetLoop { enabled: true, start_samples: 0, end_samples: 48_000 })
        .expect("set loop");
    cp.transport(TransportAction::SetStopAtEnd { enabled: true }).expect("set stop at end");

    Scripted { log, audio_track, midi_track, imported_clip_id: imported.id.to_string() }
}

// ---------------------------------------------------------------------------
// The read sweep
// ---------------------------------------------------------------------------

/// Runs EVERY declared read path reachable from `ControlPlane`'s public
/// surface, each bracketed by a full canonical snapshot that must be
/// byte-equal across the call. Returns the number of read paths swept.
fn pure_reader_sweep(cp: &Arc<ControlPlane>, project_dir: &std::path::Path, clip_id: &str) -> usize {
    let mut swept = 0usize;
    macro_rules! sweep {
        ($name:literal, $body:expr) => {{
            let before = snapshot(cp);
            let _ = $body;
            let after = snapshot(cp);
            assert_eq!(before, after, "READ PATH MUTATED THE DOCUMENT: {}", $name);
            swept += 1;
        }};
    }

    // The `get_project_state` core (and, through it, the midi read half and
    // the tempo/section-table derivation).
    sweep!("project_state", cp.project_state());
    // Transport snapshot (store mirror + RT atomics).
    sweep!("transport_state", cp.transport_state());
    // Automation lanes (Task 10's pure `automation_get` body).
    sweep!("automation_lanes", cp.automation_lanes());
    // Plugin document reads (Task 9): list, one row, params, existence.
    sweep!("plugin_rows", cp.plugin_rows());
    sweep!("plugin_row", cp.plugin_row("p-fig"));
    sweep!("plugin_params", cp.plugin_params("p-fig"));
    sweep!("plugin_exists", cp.plugin_exists("p-fig"));
    // Meters: headless this is `None`, but the read path itself is swept.
    sweep!("read_meters", cp.read_meters());
    // Export capabilities + the export job registry read.
    sweep!("export_capabilities", cp.export_capabilities());
    sweep!("export_job_status", cp.export_job_status("no-such-job").is_err());
    // Sidecar job registry reads.
    sweep!("list_jobs", cp.list_jobs());
    // The waveform-tile read — reachable headless because `import_audio_clip`
    // built a REAL pyramid from a real wav above (no audio device involved:
    // decoding and pyramid building are pure file work). This is
    // `get_waveform_tile`'s whole body below the command wrapper.
    let cache = project_dir.join("cache/waveforms").join(clip_id);
    sweep!("waveform read_tile", {
        let tile = aura_lib::audio::waveform::read_tile(&cache, 0, 0).expect("tile read");
        assert!(tile.is_some(), "the imported clip must have a real pyramid to read");
        tile
    });
    swept
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn undo_then_read_sweep_then_redo_is_byte_identical() {
    let (cp, eng) = fixture();
    let parent = tmp_parent("gate");
    let project = cp.create_project(parent.to_str().unwrap(), "Figma").expect("create project");
    let dir = std::path::PathBuf::from(project.path.clone().expect("created project has a path"));

    let scripted = scripted_session(&cp, &parent);
    assert!(scripted.log.len() >= 15, "the scripted session must cover every routed family");

    let before_undo = snapshot(&cp);
    let watermarks_before = watermarks(&cp);
    // Sanity: the scripted session really did touch every family (a snapshot
    // equality test passes trivially against an empty document).
    let state = cp.project_state();
    assert_eq!(
        state.tracks.len(),
        3,
        "Audio + Keys + the import's own auto-created track survive the scratch add/remove"
    );
    assert_eq!(state.clips.len(), 2, "the op-added clip + the imported one");
    assert_eq!(state.midi_clips.len(), 1);
    assert_eq!(state.midi_clips[0].notes.len(), 3);
    assert_eq!(state.ppq, 960);
    assert_eq!(cp.automation_lanes().len(), 1);
    assert_eq!(cp.plugin_rows().len(), 1, "p-fig survives, p-fig2 was removed");

    // ---- UNDO: inverses replayed through NEW commits, reverse order ------
    // NEVER by executing a parked `effect` (`close_gesture`'s placeholder
    // rule) — each undo is a first-class transaction that computes its own
    // fresh effect and its own fresh inverses.
    let mut undos: Vec<Committed> = Vec::new();
    for (i, entry) in scripted.log.iter().enumerate().rev() {
        let ops = entry.inverses.clone();
        let label = format!("undo: {}", entry.meta.label);
        let meta = TxMeta {
            actor: Actor::User,
            run: entry.meta.run.clone(), // run id preserved across undo
            label,
            transient: false,
        };
        let committed = cp
            .commit(meta, |tx| {
                for op in ops {
                    tx.apply(op)?;
                }
                Ok(())
            })
            .unwrap_or_else(|e| panic!("undo of batch {i} ({}) failed: {e}", entry.meta.label));
        undos.push(committed);
    }

    // The undo really rolled the document back (not a no-op that would make
    // the redo comparison vacuous).
    let after_undo = cp.project_state();
    assert!(after_undo.midi_clips.is_empty(), "undo removed the midi clip");
    assert!(cp.automation_lanes().is_empty(), "undo removed the automation lane");
    assert!(cp.plugin_rows().is_empty(), "undo removed both plugin rows");

    // ---- the pure-reader sweep, at the undone state ----------------------
    let swept = pure_reader_sweep(&cp, &dir, &scripted.imported_clip_id);
    assert!(swept >= 12, "every declared read path must be swept, got {swept}");

    // ---- REDO ------------------------------------------------------------
    // The batches' own forward `ops`, replayed in the ORIGINAL order — the
    // history stack Deliverable 3 builds does exactly this. Sound because
    // EVERY op in the vocabulary is absolute-valued, never a delta: `Set`
    // carries `to` (its `from` is advisory — `apply_raw` re-reads truth),
    // `TempoSet`/`MidiSetNotes`/`AutomationSetLane`/`PluginSetState` are
    // whole-value replacements, and the structural adds/removes carry their
    // full row. Replaying them against a document that the inverses just
    // rolled back therefore reproduces the pre-undo document exactly.
    //
    // NOTE (recorded, not worked around — see docs/SIDE-CHANNEL-INVENTORY.md
    // "recorded replay limitations"): `Op::MidiSetNotes` is absolute in its
    // note VALUES but its `noteId: 0` entries are MINT SENTINELS, so a
    // replay re-mints. That is byte-identical here because the redo of
    // `Op::MidiClipAdd` restores the clip row — watermark included — before
    // the notes op replays. A redo that does NOT re-create the clip (only
    // the notes edit was undone) re-mints from the CURRENT watermark and the
    // re-added notes get fresh ids. That is ADR 0001 behaving as designed
    // (ids are never reused and never resurrect a deleted note's identity),
    // the same asymmetry scope ruling 3 already masks for the watermark
    // itself — recorded so nobody "fixes" it into id reuse.
    for (i, entry) in scripted.log.iter().enumerate() {
        let ops = entry.ops.clone();
        let meta = TxMeta {
            actor: Actor::User,
            run: entry.meta.run.clone(), // run id preserved across redo
            label: format!("redo: {}", entry.meta.label),
            transient: false,
        };
        cp.commit(meta, |tx| {
            for op in ops {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("redo of batch {i} ({}) failed: {e}", entry.meta.label));
    }
    // `undos` exists to prove the undo direction produced real, first-class
    // transactions (each with its own computed inverses), not to drive the
    // redo — see the note above.
    assert_eq!(undos.len(), scripted.log.len(), "every batch was undone exactly once");

    // ---- THE ASSERTION ---------------------------------------------------
    let after_redo = snapshot(&cp);
    assert_eq!(
        mask_watermarks(&after_redo),
        mask_watermarks(&before_undo),
        "the document after undo-everything + read-everything + redo-everything must be \
         byte-identical to the document before the undo"
    );

    // Scope ruling 3's other half: the watermark is monotonic, never rewound.
    let watermarks_after = watermarks(&cp);
    for (id, before) in &watermarks_before {
        let after = watermarks_after.get(id).copied().unwrap_or(0);
        assert!(
            after >= *before,
            "next_note_id must never decrease for clip {id}: {before} -> {after}"
        );
    }

    eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// The pure-reader sweep on its own, against a FULLY POPULATED document (the
/// gate test above sweeps at the undone state, where several stores are
/// empty — an empty store cannot prove a reader doesn't mutate what it
/// reads). Same bracketing, richer subject.
#[test]
fn every_read_path_is_pure_against_a_populated_document() {
    let (cp, eng) = fixture();
    let parent = tmp_parent("sweep");
    let project = cp.create_project(parent.to_str().unwrap(), "Sweep").expect("create project");
    let dir = std::path::PathBuf::from(project.path.clone().expect("created project has a path"));

    let scripted = scripted_session(&cp, &parent);
    assert!(!scripted.audio_track.is_empty() && !scripted.midi_track.is_empty());

    let swept = pure_reader_sweep(&cp, &dir, &scripted.imported_clip_id);
    assert!(swept >= 12, "every declared read path must be swept, got {swept}");

    // And the whole sweep, run twice back to back, is stable (the
    // `midi_get_clips`-style lazy-resync bug would only show on a SECOND
    // call after a state change — see tests/pure_readers.rs).
    let before = snapshot(&cp);
    pure_reader_sweep(&cp, &dir, &scripted.imported_clip_id);
    pure_reader_sweep(&cp, &dir, &scripted.imported_clip_id);
    assert_eq!(before, snapshot(&cp), "repeated full sweeps must not drift the document");

    eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}
