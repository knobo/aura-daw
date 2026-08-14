//! Gate E test-4 precursor (Task 6): declared read paths mutate nothing.
//!
//! Builds a real (headless) `ControlPlane` fixture, drives it through a
//! sanctioned epoch function to get a saved project on disk, snapshots the
//! full document (`ControlPlane::project_state` — the same shape
//! `get_project_state` serves), invokes the read path twice, and asserts
//! byte-identical (canonical JSON) snapshots.
//!
//! The specific historical bug this task closes — `midi_get_clips`/
//! `midi_export_file` lazily resyncing from disk on every call, cascading
//! into a live plugin/automation adopt as a side effect of what looked like
//! a pure read (round-2 inventory row 9) — lived in `midi::mod`'s
//! `with_synced_store`/`sync_midi_store`, both crate-private (and
//! `AudioState::control_parts` too, the seam needed to wire a `MidiState`
//! to a chosen session). Reaching them requires in-crate access, the same
//! constraint `control::mod`'s own `old_graph_never_sees_the_new_tracks_params`
//! test documents for `EngineHandle::for_tests` — so that specific RED/GREEN
//! proof lives in `src/midi/mod.rs`'s own `#[cfg(test)]` module
//! (`read_midi_never_resyncs_even_when_project_dir_moved_underneath_it` and
//! `with_synced_store_mutating_path_does_not_resync_before_running_f`).
//! This file covers the same claim at the `ControlPlane` — the crate's one
//! true front door (control/mod.rs's own doc: "Anything only reachable from
//! a `#[tauri::command]` body is a bug") — abstraction level, using only
//! `ControlPlane`'s public surface, exactly as every other integration test
//! in this directory is written (see `channel_properties.rs`'s note on why
//! fixtures here are local, not the crate's private `#[cfg(test)]` ones).

use std::sync::Arc;

use parking_lot::Mutex;

use aura_lib::audio::engine::{self, ControlMsg, EventSink};
use aura_lib::audio::rt::{GraphTables, SharedRt};
use aura_lib::audio::types::Store;
use aura_lib::control::{op::TxMeta, Committer, ControlPlane, EventEmitter, Session};
use aura_lib::midi::MidiStore;
use aura_lib::sidecars::jobs::JobManager;

struct NullEvents;
impl EventSink for NullEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

/// A real, headless `ControlPlane` (real engine control thread, no audio
/// device required — `engine::start`'s own doc: "without a device the
/// engine still runs... so the UI and tests stay functional"). Local,
/// independent fixture (this file's own — not the crate's private
/// `#[cfg(test)]` ones, per this directory's established convention).
fn fixture() -> (ControlPlane, engine::EngineHandle) {
    let shared = Arc::new(SharedRt::default());
    let tables = GraphTables::empty();
    let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
    let committer = Committer::new(
        session.clone(),
        shared.clone(),
        tables.clone(),
        Arc::new(Box::new(|_: &str, _: serde_json::Value| {}) as EventEmitter),
    );
    let eng = engine::start(
        shared.clone(),
        tables.clone(),
        session.clone(),
        Box::new(NullEvents),
        committer,
    );
    let cp = ControlPlane::new(
        session,
        shared,
        tables,
        eng.clone(),
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        Box::new(|_e, _p| {}),
    );
    (cp, eng)
}

fn tmp_parent(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aura-pure-readers-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `ControlPlane::project_state` (the `get_project_state` payload shape),
/// called twice back to back, must be byte-identical — no read path may
/// mutate the document it's reading.
#[test]
fn project_state_read_twice_is_byte_identical() {
    let (cp, eng) = fixture();
    let parent = tmp_parent("basic");
    let project = cp.create_project(parent.to_str().unwrap(), "Basic").unwrap();
    assert_eq!(project.name, "Basic");

    cp.add_track(Some("Keys".into()), Some("midi".into()), TxMeta::user("add track")).unwrap();

    let first = serde_json::to_value(cp.project_state()).unwrap();
    let second = serde_json::to_value(cp.project_state()).unwrap();
    assert_eq!(first, second, "two reads of the same document must be identical");

    eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

/// The historical trigger, reproduced as far as `ControlPlane`'s public
/// surface reaches: open a SECOND project (an epoch boundary — the
/// sanctioned document swap) after the first, then read twice. Before
/// Task 6, `midi_get_clips`/`midi_export_file`'s lazy resync was keyed off
/// exactly this kind of project-dir change; epoch functions now adopt
/// eagerly, so by the time `open_project_epoch` returns, `project_state`'s
/// reads already reflect project B and a repeat read changes nothing
/// further — proving the read itself has no adopt/resync side effect left
/// to trigger.
#[test]
fn project_state_after_an_epoch_boundary_is_stable_across_repeated_reads() {
    let (cp, eng) = fixture();
    let parent = tmp_parent("epoch-boundary");

    let dir_a = std::path::Path::new(&parent).join("A.aura");
    let dir_b = std::path::Path::new(&parent).join("B.aura");
    let proj_a = aura_lib::audio::project::create(&parent, "A", 48_000, 120.0).unwrap().0;
    let proj_b = aura_lib::audio::project::create(&parent, "B", 48_000, 120.0).unwrap().0;
    assert_eq!(proj_a.name, "A");
    assert_eq!(proj_b.name, "B");

    cp.open_project_epoch(&dir_a).unwrap();
    let after_open_a = serde_json::to_value(cp.project_state()).unwrap();
    assert_eq!(
        after_open_a["projectDir"],
        serde_json::json!(dir_a.to_string_lossy()),
    );

    // The old lazy-resync trigger: a project-dir change (here: a second,
    // sanctioned epoch — opening B).
    cp.open_project_epoch(&dir_b).unwrap();

    let first = serde_json::to_value(cp.project_state()).unwrap();
    assert_eq!(
        first["projectDir"],
        serde_json::json!(dir_b.to_string_lossy()),
        "the epoch function itself adopts B eagerly"
    );
    let second = serde_json::to_value(cp.project_state()).unwrap();
    assert_eq!(first, second, "a read after the epoch boundary must not further mutate anything");

    eng.send(ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}
