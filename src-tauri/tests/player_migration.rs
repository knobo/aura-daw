//! The V2 migration gate (Task 12): a project saved with launch bindings
//! opens with players, and the same pads fire the same material.
//!
//! This is the test that says the overlay's retirement cost nobody their
//! work. Written as an integration test against `ControlPlane`'s public
//! surface, like every other file in this directory (see `pure_readers.rs`'s
//! header for why fixtures here are local, not the crate's private
//! `#[cfg(test)]` ones).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use aura_lib::audio::engine::{self, EventSink};
use aura_lib::audio::player::{Player, PlayerSource};
use aura_lib::audio::project;
use aura_lib::audio::rt::{GraphTables, SharedRt};
use aura_lib::audio::types::{Store, TrackState};
use aura_lib::control::{Committer, ControlPlane, EventEmitter, Session};
use aura_lib::ids::{ClipId, ContentId, LaneId, PlayerId};
use aura_lib::midi::launch::{LaunchBinding, LaunchMap, LaunchTarget};
use aura_lib::midi::types::MidiClip;
use aura_lib::midi::MidiStore;
use aura_lib::sidecars::jobs::JobManager;

struct NullEvents;
impl EventSink for NullEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

fn fixture() -> (ControlPlane, engine::EngineHandle) {
    let shared = Arc::new(SharedRt::default());
    let tables = GraphTables::empty();
    let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
    let log = Arc::new(aura_lib::control::HistoryLog::new());
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
    let cp = ControlPlane::new(
        session,
        shared,
        tables,
        eng.clone(),
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        Box::new(|_e, _p| {}),
        log,
        gesture,
    );
    (cp, eng)
}

fn tmp_parent(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aura-player-migration-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn test_track(id: &str, instrument_id: Option<&str>) -> TrackState {
    TrackState {
        sends: Vec::new(),
        output: None,
        id: id.into(),
        name: id.into(),
        kind: "midi".into(),
        gain_db: 0.0,
        pan: 0.0,
        muted: false,
        soloed: false,
        armed: false,
        color: "#7c9cff".into(),
        instrument_id: instrument_id.map(String::from),
        inserts: Vec::new(),
        group: None,
        automation_mode: aura_lib::audio::types::AutomationMode::Read,
    }
}

fn test_midi_clip(id: &str, track_id: &str) -> MidiClip {
    MidiClip {
        id: id.into(),
        track_id: track_id.into(),
        name: id.into(),
        timeline_start_ticks: 0,
        length_ticks: 960,
        notes: Vec::new(),
        next_note_id: 1,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(track_id),
        content_length_ticks: None,
        transpose_semitones: 0,
        velocity_offset: 0,
    }
}

fn clip_binding(id: &str, note: u8, clip_id: &str) -> LaunchBinding {
    LaunchBinding {
        id: id.into(),
        name: id.into(),
        note,
        channel: None,
        target: LaunchTarget::Clip { clip_id: clip_id.into() },
    }
}

fn region_binding(id: &str, note: u8, track_id: &str) -> LaunchBinding {
    LaunchBinding {
        id: id.into(),
        name: id.into(),
        note,
        channel: None,
        target: LaunchTarget::Region {
            start_ticks: 0,
            length_ticks: 960,
            track_ids: vec![track_id.into()],
        },
    }
}

/// Write a project on disk with one MIDI track (bound to an instrument),
/// one MIDI clip on it, and a launch map with the given bindings — the
/// on-disk shape a pre-Task-12 save left behind.
fn write_project(
    dir_name: &str,
    tracks: Vec<TrackState>,
    clips: Vec<MidiClip>,
    map: LaunchMap,
) -> PathBuf {
    let parent = tmp_parent(dir_name);
    let (mut proj, dir) = project::create(&parent, "P", 48_000, 120.0).unwrap();
    proj.tracks = tracks;
    project::save(&dir, &proj).unwrap();

    let midi = MidiStore {
        clips,
        launch_maps: vec![map],
        ..MidiStore::default()
    };
    aura_lib::midi::persist::save_into_project(&dir, &midi).unwrap();
    dir
}

fn control_plane_with_a_saved_project_containing_a_clip_binding() -> (ControlPlane, PathBuf) {
    let (cp, _eng) = fixture();
    let dir = write_project(
        "clip-binding",
        vec![test_track("t1", Some("plugin:i1"))],
        vec![test_midi_clip("mc1", "t1")],
        LaunchMap {
            bindings: vec![clip_binding("b1", 36, "mc1")],
            ..LaunchMap::default_map()
        },
    );
    (cp, dir)
}

fn control_plane_with_a_saved_project_containing_a_region_binding() -> (ControlPlane, PathBuf) {
    let (cp, _eng) = fixture();
    let dir = write_project(
        "region-binding",
        vec![test_track("t1", None)],
        vec![],
        LaunchMap {
            bindings: vec![region_binding("b1", 60, "t1")],
            ..LaunchMap::default_map()
        },
    );
    (cp, dir)
}

fn control_plane_with_two_bindings_on_one_clip() -> (ControlPlane, PathBuf) {
    let (cp, _eng) = fixture();
    let dir = write_project(
        "two-bindings-one-clip",
        vec![test_track("t1", Some("plugin:i1"))],
        vec![test_midi_clip("mc1", "t1")],
        LaunchMap {
            bindings: vec![clip_binding("b1", 36, "mc1"), clip_binding("b2", 37, "mc1")],
            ..LaunchMap::default_map()
        },
    );
    (cp, dir)
}

fn control_plane_with_two_bindings_on_two_distinct_clips() -> (ControlPlane, PathBuf) {
    let (cp, _eng) = fixture();
    let dir = write_project(
        "two-bindings-two-clips",
        vec![test_track("t1", Some("plugin:i1")), test_track("t2", Some("plugin:i2"))],
        vec![test_midi_clip("mc1", "t1"), test_midi_clip("mc2", "t2")],
        LaunchMap {
            bindings: vec![clip_binding("b1", 36, "mc1"), clip_binding("b2", 37, "mc2")],
            ..LaunchMap::default_map()
        },
    );
    (cp, dir)
}

fn control_plane_with_a_binding_pointing_at_a_deleted_clip() -> (ControlPlane, PathBuf) {
    let (cp, _eng) = fixture();
    let dir = write_project(
        "dangling-clip",
        vec![test_track("t1", Some("plugin:i1"))],
        vec![], // the clip the binding names does not exist
        LaunchMap {
            bindings: vec![clip_binding("b1", 36, "does-not-exist")],
            ..LaunchMap::default_map()
        },
    );
    (cp, dir)
}

#[test]
fn a_project_with_clip_bindings_opens_with_players_that_fire_the_same_clip() {
    let (cp, dir) = control_plane_with_a_saved_project_containing_a_clip_binding();
    cp.open_project_epoch(Path::new(&dir)).unwrap();

    let players = cp.players();
    assert_eq!(players.len(), 1, "the binding became one player");
    assert_eq!(
        players[0].source,
        PlayerSource::MidiClip {
            clip_id: ClipId::from("mc1"),
            instrument_id: Some("plugin:i1".into()),
        },
        "the player plays what the binding played, through the clip's own instrument"
    );

    let binding = cp.launch_snapshot().maps[0].bindings[0].clone();
    assert_eq!(
        binding.target,
        LaunchTarget::Player { player_id: players[0].id.clone() },
        "and the same pad now points at it"
    );
    assert_eq!(binding.note, 36, "the note it was learned on is untouched");
}

#[test]
fn region_bindings_are_left_alone_by_the_migration() {
    let (cp, dir) = control_plane_with_a_saved_project_containing_a_region_binding();
    cp.open_project_epoch(Path::new(&dir)).unwrap();
    assert!(cp.players().is_empty(), "a scene is not a player");
    assert!(matches!(
        cp.launch_snapshot().maps[0].bindings[0].target,
        LaunchTarget::Region { .. }
    ));
}

#[test]
fn two_bindings_on_the_same_clip_share_one_player() {
    let (cp, dir) = control_plane_with_two_bindings_on_one_clip();
    cp.open_project_epoch(Path::new(&dir)).unwrap();
    let players = cp.players();
    assert_eq!(players.len(), 1, "one clip, one player");
    let maps = cp.launch_snapshot().maps;
    assert_eq!(maps[0].bindings[0].target, maps[0].bindings[1].target);
}

/// Fix round 1, Important 4: the reviewer replaced the target assignment
/// with `players[0].id.clone()` and every existing test here still
/// passed — no fixture had two distinct clips, so `players[0]` was
/// trivially right everywhere. This is the test that can only pass if
/// each binding resolves to ITS OWN clip's player.
#[test]
fn two_bindings_on_distinct_clips_get_distinct_players_matching_their_own_clip() {
    let (cp, dir) = control_plane_with_two_bindings_on_two_distinct_clips();
    cp.open_project_epoch(Path::new(&dir)).unwrap();

    let players = cp.players();
    assert_eq!(players.len(), 2, "two distinct clips, two distinct players");

    let maps = cp.launch_snapshot().maps;
    let player_for = |target: &LaunchTarget| {
        let LaunchTarget::Player { player_id } = target else {
            panic!("expected a Player target, got {target:?}");
        };
        players
            .iter()
            .find(|p| &p.id == player_id)
            .expect("the referenced player exists")
    };

    let p1 = player_for(&maps[0].bindings[0].target);
    let p2 = player_for(&maps[0].bindings[1].target);
    assert_eq!(
        p1.source,
        PlayerSource::MidiClip {
            clip_id: ClipId::from("mc1"),
            instrument_id: Some("plugin:i1".into()),
        },
        "b1 must resolve to mc1's player, not merely SOME player"
    );
    assert_eq!(
        p2.source,
        PlayerSource::MidiClip {
            clip_id: ClipId::from("mc2"),
            instrument_id: Some("plugin:i2".into()),
        },
        "b2 must resolve to mc2's player, not merely SOME player"
    );
    assert_ne!(p1.id, p2.id, "distinct clips must not share a player");
}

#[test]
fn a_binding_whose_clip_is_gone_migrates_to_nothing_and_does_not_fail_the_open() {
    let (cp, dir) = control_plane_with_a_binding_pointing_at_a_deleted_clip();
    cp.open_project_epoch(Path::new(&dir)).expect("the project still opens");
    assert!(cp.players().is_empty());
    // The old behaviour a dangling `Clip` target had — nothing fires it,
    // silently, and only a `log::warn!` says why (`launch_fire_from`'s
    // `Clip` arm) — is kept rather than replaced by DROPPING the binding:
    // losing the user's note-to-pad mapping outright is a worse failure
    // than leaving it exactly as broken as it always was.
    assert!(
        matches!(
            cp.launch_snapshot().maps[0].bindings[0].target,
            LaunchTarget::Clip { .. }
        ),
        "the pad mapping survives even though it cannot be migrated"
    );
}

/// The idempotency gate: opening the SAME project twice must not mint a
/// second player. Under this migration's in-memory-only design (matching
/// `adopt_midi_from_dir`'s own schema migrations — nothing here forces a
/// resave), each open independently loads the pre-migration document from
/// disk and migrates it fresh, so this asserts the count stays right
/// across repeat opens of an unmodified project, not merely that a single
/// call produced the right count once.
#[test]
fn opening_the_same_project_twice_does_not_double_the_player() {
    let (cp, dir) = control_plane_with_a_saved_project_containing_a_clip_binding();

    cp.open_project_epoch(Path::new(&dir)).unwrap();
    assert_eq!(cp.players().len(), 1);

    cp.open_project_epoch(Path::new(&dir)).unwrap();
    let players = cp.players();
    assert_eq!(players.len(), 1, "a second open of the same project must not double the pad");
    assert_eq!(
        players[0].source,
        PlayerSource::MidiClip {
            clip_id: ClipId::from("mc1"),
            instrument_id: Some("plugin:i1".into()),
        }
    );
    assert_eq!(
        cp.launch_snapshot().maps[0].bindings[0].target,
        LaunchTarget::Player { player_id: players[0].id.clone() }
    );
}

/// Fix round 1, Critical 1: `adopt_midi_from_dir`'s `Ok(None)` branch —
/// taken by every project that has never had a midi save, which is every
/// freshly created one — is guarded by `midi.loaded_dir.is_some()`, and
/// THAT guard is what clears the previous project's clips/harmony/launch
/// maps. Open A (clips + launch bindings), then open B, a project that has
/// never been midi-saved: B must show none of A's midi state, and the
/// migration must not mint players from A's bindings into B's store.
#[test]
fn opening_a_never_midi_saved_project_after_one_with_bindings_does_not_inherit_its_midi_state() {
    let (cp, dir_a) = control_plane_with_a_saved_project_containing_a_clip_binding();
    cp.open_project_epoch(Path::new(&dir_a)).unwrap();
    assert_eq!(cp.players().len(), 1, "A migrated its own binding");

    let parent = tmp_parent("never-midi-saved-b");
    let (mut proj_b, dir_b) = project::create(&parent, "B", 48_000, 120.0).unwrap();
    proj_b.tracks = vec![test_track("t1", None)];
    project::save(&dir_b, &proj_b).unwrap();
    // No `persist::save_into_project` call: B's project.json carries no
    // v2+ midi fields at all — `load_from_project` returns `Ok(None)` for
    // it, the branch Critical 1 found unguarded.

    cp.open_project_epoch(Path::new(&dir_b)).unwrap();

    assert!(
        cp.players().is_empty(),
        "B must not inherit A's migrated player"
    );
    let maps = cp.launch_snapshot().maps;
    assert!(
        maps.iter().all(|m| m.bindings.is_empty()),
        "B must not inherit A's launch bindings: {maps:?}"
    );
}

/// The real double-mint risk: a project that ALREADY carries a migrated
/// player (as if an earlier session migrated it and saved) must not gain a
/// second one on open. This is the scenario `PlayerId::mint`'s randomness
/// makes a false "opens twice" pass silently hide — the player here has a
/// fixed id from the fixture, not a fresh one migration would produce.
#[test]
fn a_project_already_migrated_and_saved_does_not_mint_a_second_player_on_open() {
    let (cp, _eng) = fixture();
    let parent = tmp_parent("already-migrated");
    let (mut proj, dir) = project::create(&parent, "P", 48_000, 120.0).unwrap();
    proj.tracks = vec![test_track("t1", Some("plugin:i1"))];
    let existing_player_id = PlayerId::from("already-there");
    let mut p = Player::new(existing_player_id.clone(), "PAD");
    p.source = PlayerSource::MidiClip {
        clip_id: ClipId::from("mc1"),
        instrument_id: Some("plugin:i1".into()),
    };
    proj.players = vec![p];
    project::save(&dir, &proj).unwrap();

    let midi = MidiStore {
        clips: vec![test_midi_clip("mc1", "t1")],
        launch_maps: vec![LaunchMap {
            bindings: vec![LaunchBinding {
                id: "b1".into(),
                name: "b1".into(),
                note: 36,
                channel: None,
                target: LaunchTarget::Player { player_id: existing_player_id.clone() },
            }],
            ..LaunchMap::default_map()
        }],
        ..MidiStore::default()
    };
    aura_lib::midi::persist::save_into_project(&dir, &midi).unwrap();

    cp.open_project_epoch(Path::new(&dir)).unwrap();
    let players = cp.players();
    assert_eq!(players.len(), 1, "the already-migrated player is not duplicated");
    assert_eq!(players[0].id, existing_player_id);
}
