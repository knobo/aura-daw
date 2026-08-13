//! Project (`*.aura` directory) persistence — see docs/ARCHITECTURE.md §7.
//!
//! ```text
//! MySong.aura/
//! ├── project.json   # project.schema.json
//! ├── audio/         # recorded takes <clipId>.wav
//! ├── stems/         # sidecar output
//! └── cache/         # regenerable (waveforms/, transcripts/)
//! ```
//!
//! Saves are atomic: write `project.json.tmp`, fsync, rename.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::types::{Project, Store, TransportState};

pub const PROJECT_FILE: &str = "project.json";

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Create `<parent>/<name>.aura/` with the standard subdirectories and an
/// initial project.json. Fails if the project already exists.
pub fn create(parent: &Path, name: &str, sample_rate: u32, tempo_bpm: f64) -> Result<(Project, PathBuf), String> {
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(format!("invalid project name: {name:?}"));
    }
    let dir = parent.join(format!("{name}.aura"));
    if dir.join(PROJECT_FILE).exists() {
        return Err(format!("project already exists: {}", dir.display()));
    }
    for sub in ["audio", "stems", "cache/waveforms", "cache/transcripts"] {
        fs::create_dir_all(dir.join(sub)).map_err(|e| e.to_string())?;
    }
    let now = now_rfc3339();
    let project = Project {
        schema_version: 1,
        name: name.to_string(),
        path: Some(dir.to_string_lossy().into_owned()),
        created_at: Some(now.clone()),
        modified_at: Some(now),
        sample_rate,
        tempo_bpm,
        time_signature: Some((4, 4)),
        tracks: Vec::new(),
        clips: Vec::new(),
        transport: Some(TransportState {
            sample_rate,
            tempo_bpm,
            ..Default::default()
        }),
    };
    save(&dir, &project)?;
    Ok((project, dir))
}

/// Atomically write project.json into `dir`.
///
/// v2 seam (PHASE2-PLAN §3.C named integration point, serde_json::Value
/// round-trip): when the existing file is schemaVersion >= 2, the typed v1
/// fields are overlaid onto it so the v2 fields written by `midi::persist`
/// (`ppq`, `tempoMap`, `midiClips`, `instruments`, ...) survive v1-path
/// saves, and the `tempoBpm == tempoMap[0].bpm` invariant is maintained.
pub fn save(dir: &Path, project: &Project) -> Result<(), String> {
    let mut value = serde_json::to_value(project).map_err(|e| e.to_string())?;
    let dst = dir.join(PROJECT_FILE);
    if let Ok(bytes) = fs::read(&dst) {
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(mut base) => {
                if base.get("schemaVersion").and_then(|v| v.as_u64()).unwrap_or(1) >= 2 {
                    if let (Some(bmap), Some(nmap)) = (base.as_object_mut(), value.as_object()) {
                        for (k, v) in nmap {
                            if k != "schemaVersion" {
                                bmap.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    if let Some(first) = base.get_mut("tempoMap").and_then(|v| v.get_mut(0)) {
                        first["bpm"] = serde_json::json!(project.tempo_bpm);
                    }
                    value = base;
                }
            }
            Err(e) => {
                // Review fix: an unparseable existing file may still BE a v2
                // project (with midi/tempo fields we would silently destroy
                // by writing pure v1). Preserve the original bytes next to
                // the file before overwriting, and say so loudly.
                let bak = dir.join(format!("{PROJECT_FILE}.corrupt.bak"));
                match fs::write(&bak, &bytes) {
                    Ok(()) => log::warn!(
                        "project.json is unparseable ({e}); original preserved at {} — \
                         any v2 fields could not be carried over into this save",
                        bak.display()
                    ),
                    Err(werr) => {
                        return Err(format!(
                            "project.json is unparseable ({e}) and backing it up failed \
                             ({werr}); refusing to overwrite"
                        ))
                    }
                }
            }
        }
    }
    let json = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{PROJECT_FILE}.tmp"));
    let dst = dir.join(PROJECT_FILE);
    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(&json).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(())
}

/// Validate a loaded project BEFORE any in-memory state is mutated
/// (review fix: `open_project` used to replace `Store.tracks/clips` and only
/// then fail slot allocation, leaving inconsistent state). All conditions
/// that could make adoption fail midway are checked here, up front.
pub fn validate(project: &Project) -> Result<(), String> {
    if project.tracks.len() > super::types::MAX_TRACKS {
        return Err(format!(
            "project has {} tracks; this build supports at most {}",
            project.tracks.len(),
            super::types::MAX_TRACKS
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for t in &project.tracks {
        if !seen.insert(&t.id) {
            return Err(format!("duplicate track id in project: {}", t.id));
        }
    }
    Ok(())
}

/// Load a project from a `.aura` dir or a direct path to project.json.
/// Returns the parsed project (with `path` rewritten) and the project dir.
pub fn load(path: &Path) -> Result<(Project, PathBuf), String> {
    let (dir, file) = if path.is_dir() {
        (path.to_path_buf(), path.join(PROJECT_FILE))
    } else {
        (
            path.parent()
                .ok_or_else(|| "project path has no parent".to_string())?
                .to_path_buf(),
            path.to_path_buf(),
        )
    };
    let bytes = fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let mut project: Project =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse project.json: {e}"))?;
    // v1 and v2 are both readable here: the typed struct carries the v1
    // fields; the v2 midi fields are read by `midi::persist` straight from
    // the file (unknown fields are ignored per D-06, never rejected).
    if !(1..=2).contains(&project.schema_version) {
        return Err(format!("unsupported project schemaVersion {}", project.schema_version));
    }
    project.path = Some(dir.to_string_lossy().into_owned());
    Ok((project, dir))
}

/// Snapshot the control-plane store into a serializable Project.
pub fn from_store(store: &Store, position_samples: u64, sample_rate: u32) -> Result<Project, String> {
    let dir = store
        .project_dir
        .as_ref()
        .ok_or_else(|| "no project open".to_string())?;
    let mut transport = store.transport.clone();
    transport.state = "stopped".into();
    transport.position_samples = position_samples;
    transport.sample_rate = sample_rate;
    Ok(Project {
        schema_version: 1,
        name: store
            .project_name
            .clone()
            .unwrap_or_else(|| "Untitled".into()),
        path: Some(dir.to_string_lossy().into_owned()),
        created_at: store.created_at.clone(),
        modified_at: Some(now_rfc3339()),
        sample_rate,
        tempo_bpm: transport.tempo_bpm,
        time_signature: Some((4, 4)),
        tracks: store.tracks.clone(),
        clips: store.clips.clone(),
        transport: Some(transport),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::TrackState;

    fn tmp_parent(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("aura-proj-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn create_save_load_roundtrip() {
        let parent = tmp_parent("roundtrip");
        let (mut project, dir) = create(&parent, "MySong", 48_000, 120.0).unwrap();
        assert!(dir.join("audio").is_dir());
        assert!(dir.join("cache/waveforms").is_dir());
        assert!(dir.join("project.json").is_file());
        assert!(!dir.join("project.json.tmp").exists(), "tmp renamed away");

        project.tracks.push(TrackState {
            id: "11111111-1111-4111-8111-111111111111".into(),
            name: "Vocals".into(),
            kind: "audio".into(),
            gain_db: -3.0,
            pan: 0.25,
            muted: false,
            soloed: true,
            armed: true,
            color: "#aabbcc".into(),
            instrument_id: None,
        });
        project.tempo_bpm = 92.5;
        save(&dir, &project).unwrap();

        let (loaded, ldir) = load(&dir).unwrap();
        assert_eq!(ldir, dir);
        assert_eq!(loaded.name, "MySong");
        assert_eq!(loaded.tempo_bpm, 92.5);
        assert_eq!(loaded.tracks.len(), 1);
        assert_eq!(loaded.tracks[0].name, "Vocals");
        assert_eq!(loaded.time_signature, Some((4, 4)));

        // also loadable via the file path directly
        let (loaded2, _) = load(&dir.join("project.json")).unwrap();
        assert_eq!(loaded2.tracks[0].gain_db, -3.0);
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn create_refuses_duplicates_and_bad_names() {
        let parent = tmp_parent("dupes");
        create(&parent, "Song", 48_000, 120.0).unwrap();
        assert!(create(&parent, "Song", 48_000, 120.0).is_err());
        assert!(create(&parent, "a/b", 48_000, 120.0).is_err());
        assert!(create(&parent, "", 48_000, 120.0).is_err());
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn from_store_requires_project_and_forces_stopped() {
        let mut store = Store::default();
        assert!(from_store(&store, 0, 48_000).is_err());
        store.project_dir = Some(PathBuf::from("/tmp/x.aura"));
        store.project_name = Some("x".into());
        store.transport.state = "playing".into();
        let p = from_store(&store, 999, 44_100).unwrap();
        let t = p.transport.unwrap();
        assert_eq!(t.state, "stopped");
        assert_eq!(t.position_samples, 999);
        assert_eq!(p.sample_rate, 44_100);
    }

    fn track_n(i: usize) -> TrackState {
        TrackState {
            id: format!("t{i}").into(),
            name: format!("T{i}"),
            kind: "audio".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        }
    }

    /// Review fix: a project exceeding MAX_TRACKS (or with duplicate track
    /// ids) is rejected UP FRONT by `validate`, before `open_project`
    /// mutates any in-memory state.
    #[test]
    fn validate_rejects_overfull_and_duplicate_projects() {
        let parent = tmp_parent("validate");
        let (mut project, _) = create(&parent, "V", 48_000, 120.0).unwrap();
        project.tracks = (0..super::super::types::MAX_TRACKS).map(track_n).collect();
        assert!(validate(&project).is_ok(), "exactly MAX_TRACKS is fine");
        project.tracks.push(track_n(usize::MAX - 1)); // 65th track
        let err = validate(&project).unwrap_err();
        assert!(err.contains("65 tracks"), "clear message: {err}");

        let mut dup = project.clone();
        dup.tracks.truncate(2);
        dup.tracks[1].id = dup.tracks[0].id.clone();
        assert!(validate(&dup).is_err(), "duplicate ids rejected");
        let _ = fs::remove_dir_all(&parent);
    }

    /// Review fix: saving over an UNPARSEABLE existing project.json must
    /// preserve the original bytes (it may be a v2 project whose midi fields
    /// we cannot carry over) instead of silently downgrading.
    #[test]
    fn save_over_corrupt_file_backs_up_original_bytes() {
        let parent = tmp_parent("corrupt");
        let (project, dir) = create(&parent, "C", 48_000, 120.0).unwrap();
        let garbage = b"{ this is NOT json ... v2 fields lived here";
        fs::write(dir.join(PROJECT_FILE), garbage).unwrap();

        save(&dir, &project).unwrap();
        // New file is valid again...
        let (reloaded, _) = load(&dir).unwrap();
        assert_eq!(reloaded.name, "C");
        // ...and the corrupt original survived byte-for-byte.
        let bak = fs::read(dir.join("project.json.corrupt.bak")).unwrap();
        assert_eq!(bak, garbage);
        let _ = fs::remove_dir_all(&parent);
    }

    /// The v2-preservation path still works: a parseable v2 file keeps its
    /// midi fields across a v1-shaped save.
    #[test]
    fn save_preserves_v2_fields_on_parseable_files() {
        let parent = tmp_parent("v2keep");
        let (mut project, dir) = create(&parent, "K", 48_000, 120.0).unwrap();
        let v2 = serde_json::json!({
            "schemaVersion": 2,
            "name": "K",
            "sampleRate": 48_000,
            "tempoBpm": 120.0,
            "tracks": [],
            "clips": [],
            "ppq": 960,
            "tempoMap": [{"tick": 0, "bpm": 120.0}],
            "midiClips": [{"id": "mc1"}]
        });
        fs::write(
            dir.join(PROJECT_FILE),
            serde_json::to_vec_pretty(&v2).unwrap(),
        )
        .unwrap();
        project.tempo_bpm = 90.0;
        save(&dir, &project).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(saved["schemaVersion"], 2);
        assert_eq!(saved["midiClips"][0]["id"], "mc1", "v2 fields preserved");
        assert_eq!(saved["tempoMap"][0]["bpm"], 90.0, "tempo invariant maintained");
        assert_eq!(saved["tempoBpm"], 90.0);
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn wire_format_matches_schema_field_names() {
        let parent = tmp_parent("wire");
        let (project, _) = create(&parent, "Wire", 48_000, 120.0).unwrap();
        let v = serde_json::to_value(&project).unwrap();
        assert_eq!(v["schemaVersion"], 1);
        assert!(v.get("sampleRate").is_some());
        assert!(v.get("tempoBpm").is_some());
        assert!(v.get("timeSignature").is_some());
        assert_eq!(v["timeSignature"], serde_json::json!([4, 4]));
        let _ = fs::remove_dir_all(&parent);
    }
}
