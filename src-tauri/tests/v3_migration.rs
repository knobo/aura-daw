//! Gate C/D test 7 (part 1 — the tempo/meter half; Task 7 adds the
//! placement/content half): lossless v2->v3 migration against a fixture
//! corpus, including the v3 meterMap default (round-2 §3.3, ADR 0002).

use std::fs;
use std::path::Path;

fn fixture_dir(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/project_v2").join(name)
}

fn copy_fixture_to_tmp(name: &str) -> std::path::PathBuf {
    let src = fixture_dir(name);
    // A UUID discriminator, not just the fixture name + pid: two tests in
    // this file use the SAME fixture name ("single_tempo" — one exercises
    // the tempo/meter migration, the other the schema gate) and run on
    // different THREADS of the same process under cargo test's default
    // parallelism, so name+pid alone collided (caught by running the full
    // suite, not this file in isolation — a genuine test-isolation bug,
    // not a flaky rerun).
    let dst = std::env::temp_dir()
        .join(format!("aura-v3-migration-{name}-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
    let _ = fs::remove_dir_all(&dst);
    fs::create_dir_all(&dst).unwrap();
    for entry in fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
    dst
}

#[test]
fn v2_single_tempo_project_migrates_losslessly_to_v3() {
    let dir = copy_fixture_to_tmp("single_tempo");
    let v3 = aura_lib::midi::persist::load_from_project(&dir).unwrap().expect("v2+ present");
    assert_eq!(v3.ppq, 960);
    assert_eq!(v3.tempo_events.len(), 1);
    // 128.0 bpm quantizes to an exact period and back within spec error.
    assert!((v3.tempo_events[0].bpm - 128.0).abs() < 3e-7);
    assert_eq!(
        v3.meter_events,
        vec![aura_lib::midi::types::MeterEvent { tick: 0, num: 4, den: 4 }],
        "no meterMap in the v2 fixture -> the [{{0,4,4}}] default (round-2 §3.3)"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn v2_multi_tempo_project_migrates_and_resaves_as_v3() {
    let dir = copy_fixture_to_tmp("multi_tempo");
    let v3 = aura_lib::midi::persist::load_from_project(&dir).unwrap().expect("v2+ present");
    assert_eq!(v3.tempo_events.len(), 2);

    // Resave, then reload: schemaVersion is now 3, project.json.v2.bak
    // exists, and the SAME (already-quantized) periods come back with zero
    // drift (Task 2's period_from_bpm/bpm_from_period idempotence).
    let store = aura_lib::midi::MidiStore {
        ppq: v3.ppq,
        tempo_events: v3.tempo_events.clone(),
        meter_events: v3.meter_events.clone(),
        clips: v3.clips.clone(),
        launch_bindings: Vec::new(),
            launch_drive_clip_ids: Vec::new(),
        loaded_dir: None,
        dirty: false,
    };
    aura_lib::midi::persist::save_into_project(&dir, &store).unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    assert_eq!(raw["schemaVersion"], 3);
    assert!(dir.join("project.json.v2.bak").exists(), "v2 backup written on first v3 upgrade");
    let expected_period_0 = aura_lib::time::period_from_bpm(v3.tempo_events[0].bpm);
    let expected_period_1 = aura_lib::time::period_from_bpm(v3.tempo_events[1].bpm);
    assert_eq!(raw["tempoMap"][0]["periodStart"], expected_period_0);
    assert_eq!(raw["tempoMap"][0]["periodEnd"], expected_period_0);
    assert_eq!(raw["tempoMap"][1]["periodStart"], expected_period_1);
    assert_eq!(raw["meterMap"][0]["num"], 4);
    assert_eq!(raw["meterMap"][0]["den"], 4);

    let reloaded = aura_lib::midi::persist::load_from_project(&dir).unwrap().unwrap();
    assert_eq!(
        reloaded.tempo_events.len(),
        v3.tempo_events.len(),
        "same number of tempo events after the round-trip"
    );
    for (a, b) in reloaded.tempo_events.iter().zip(v3.tempo_events.iter()) {
        assert_eq!(a.tick, b.tick);
        assert!((a.bpm - b.bpm).abs() < 3e-7, "zero drift on re-save: {} vs {}", a.bpm, b.bpm);
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn v2_project_with_clips_migrates_clips_into_content_and_placements() {
    let dir = copy_fixture_to_tmp("with_clips");
    let v3 = aura_lib::midi::persist::load_from_project(&dir).unwrap().expect("v2+ present");
    assert_eq!(v3.clips.len(), 1);
    let clip = &v3.clips[0];
    assert!(!clip.content_id.as_str().is_empty(), "content id minted on migration");
    assert!(!clip.lane_id.as_str().is_empty(), "lane id minted (default lane) on migration");

    // Re-migrating the SAME v2 file twice mints the SAME content/lane ids
    // (deterministic minting, same discipline as assign_source_ids —
    // round-2 §2.2's precedent applied here).
    let dir2 = copy_fixture_to_tmp("with_clips");
    let v3b = aura_lib::midi::persist::load_from_project(&dir2).unwrap().unwrap();
    assert_eq!(v3.clips[0].content_id, v3b.clips[0].content_id, "deterministic content id minting");
    assert_eq!(v3.clips[0].lane_id, v3b.clips[0].lane_id, "deterministic lane id minting");

    let store = aura_lib::midi::MidiStore {
        ppq: v3.ppq,
        tempo_events: v3.tempo_events.clone(),
        meter_events: v3.meter_events.clone(),
        clips: v3.clips.clone(),
        launch_bindings: Vec::new(),
            launch_drive_clip_ids: Vec::new(),
        loaded_dir: None,
        dirty: false,
    };
    aura_lib::midi::persist::save_into_project(&dir, &store).unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
    assert!(raw.get("midiClips").is_none(), "v3 writes content+placements, not midiClips");
    assert_eq!(raw["content"].as_array().unwrap().len(), 1);
    assert_eq!(raw["placements"].as_array().unwrap().len(), 1);
    assert_eq!(raw["lanes"].as_array().unwrap().len(), 1, "one default lane for the one track");
    assert_eq!(raw["content"][0]["kind"], "midi");
    assert_eq!(raw["content"][0]["nextNoteId"], 1);
    assert_eq!(raw["placements"][0]["contentId"], raw["content"][0]["id"]);
    assert_eq!(raw["placements"][0]["laneId"], raw["lanes"][0]["id"]);
    assert_eq!(raw["lanes"][0]["trackId"], "t1");

    let reloaded = aura_lib::midi::persist::load_from_project(&dir).unwrap().unwrap();
    assert_eq!(reloaded.clips[0].content_id, clip.content_id, "round-trips through the split and back");
    assert_eq!(reloaded.clips[0].lane_id, clip.lane_id);
    assert_eq!(reloaded.clips[0].timeline_start_ticks, clip.timeline_start_ticks);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&dir2);
}

#[test]
fn project_rs_schema_gate_accepts_v3() {
    let dir = copy_fixture_to_tmp("single_tempo");
    let (project, _) = aura_lib::audio::project::load(&dir).unwrap();
    assert!((1..=3).contains(&project.schema_version));
    let _ = fs::remove_dir_all(&dir);
}
