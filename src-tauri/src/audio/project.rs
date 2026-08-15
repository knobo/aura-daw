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

use parking_lot::Mutex;

use super::types::{Clip, Project, Store, TransportState};
use crate::control::Session;
use crate::ids::SourceId;

pub const PROJECT_FILE: &str = "project.json";

/// Fixed namespace for deterministic legacy `SourceId` minting
/// ([`assign_source_ids`]). A project-specific constant, minted once and
/// frozen forever — changing it would re-mint every legacy project's source
/// ids on next open, breaking the decode-cache's stability guarantee.
const AURA_SOURCE_NS: uuid::Uuid = uuid::uuid!("da93d478-7f57-41f5-a1a2-ffc2d1fc6c12");

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

/// A writable default parent for projects nobody picked a folder for
/// (the engine's auto-project, and `ControlPlane::create_project_epoch`
/// when called with no explicit location) — `~/Music/AURA` or
/// `$HOME/AURA` as a fallback.
pub(crate) fn default_project_parent() -> Result<PathBuf, String> {
    let parent = dirs::audio_dir()
        .or_else(dirs::home_dir)
        .ok_or("cannot determine a directory for the default project")?
        .join("AURA");
    fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
    Ok(parent)
}

/// Mint an "Untitled"/"Untitled-N" name that doesn't collide with an
/// existing `<parent>/<name>.aura` — shared by [`ensure_default_project`]
/// and `ControlPlane::create_project_epoch`.
pub(crate) fn unique_untitled_name(parent: &Path) -> String {
    let mut name = "Untitled".to_string();
    let mut n = 1;
    while parent.join(format!("{name}.aura")).exists() {
        n += 1;
        name = format!("Untitled-{n}");
    }
    name
}

/// Auto-create a default project when the session has none open yet (round-2
/// §4.5 carve-out: an epoch boundary, "document birth"). The ONE swap site,
/// reached through exactly one front door: `ControlPlane::ensure_project_epoch`
/// (Task 6's sanctioned epoch fn). The engine control thread's own
/// `ensure_project` (auto-project on recording start, engine.rs) no longer
/// calls this directly — Task 13 rewired it through the closure lib.rs
/// installs, which invokes that same front door. Returns `Ok(None)` when a project is
/// ALREADY open (no-op — the caller decides what "no work to do" means for
/// its own return type); `Ok(Some(project))` when one was minted, with
/// `project_dir`/`project_name`/`created_at` swapped into the store under
/// one short lock (no disk I/O held under it beyond `create`'s own, which
/// runs BEFORE the lock is taken).
pub fn ensure_default_project(
    session: &Mutex<Session>,
    sample_rate: u32,
) -> Result<Option<Project>, String> {
    if session.lock().store.project_dir.is_some() {
        return Ok(None);
    }
    let parent = default_project_parent()?;
    let name = unique_untitled_name(&parent);
    let (project, dir) = create(&parent, &name, sample_rate, 120.0)?;
    {
        let mut session = session.lock();
        // epoch boundary: the store swap for the "ensure" epoch happens
        // HERE, but Task 17's history-clear + journal rotation does NOT —
        // it lives in `ControlPlane::ensure_project_epoch`, the single
        // front door every caller of this fn goes through (this module has
        // no `ControlPlane` and must not grow one; the journal also writes
        // to disk, which may never happen under the session lock this
        // block holds). A `Some(project)` return there means this swap
        // actually ran, so the hook fires exactly as often as the epoch
        // counter below bumps. Fix round 1 (Task 7 review finding 2):
        // bump `session.epoch` here too — `ControlPlane::execute_persist`
        // uses it to detect a commit's persist racing this document swap.
        session.epoch += 1;
        session.store.project_dir = Some(dir);
        session.store.project_name = Some(project.name.clone());
        session.store.created_at = project.created_at.clone();
    }
    Ok(Some(project))
}

/// Atomically write project.json into `dir`.
///
/// v2/v3 seam (PHASE2-PLAN §3.C named integration point, serde_json::Value
/// round-trip): when the existing file is schemaVersion >= 2, the typed v1
/// fields are overlaid onto it so the v2/v3 fields written by
/// `midi::persist` (`ppq`, `tempoMap`, `meterMap`, `midiClips`,
/// `instruments`, ...) survive v1-path saves, and the
/// `tempoBpm == tempoMap[0].bpm` (v2) / `tempoBpm` mirrors
/// `tempoMap[0].periodStart` (v3, round-2 §3.3) invariant is maintained.
pub fn save(dir: &Path, project: &Project) -> Result<(), String> {
    let mut value = serde_json::to_value(project).map_err(|e| e.to_string())?;
    let dst = dir.join(PROJECT_FILE);
    if let Ok(bytes) = fs::read(&dst) {
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(mut base) => {
                let existing_schema_version =
                    base.get("schemaVersion").and_then(|v| v.as_u64()).unwrap_or(1);
                if existing_schema_version >= 2 {
                    if let (Some(bmap), Some(nmap)) = (base.as_object_mut(), value.as_object()) {
                        for (k, v) in nmap {
                            if k != "schemaVersion" {
                                bmap.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    if let Some(first) = base.get_mut("tempoMap").and_then(|v| v.get_mut(0)) {
                        if existing_schema_version >= 3 {
                            // v3: tempoMap rows are period-shaped
                            // (`periodStart`/`periodEnd`, round-2 §3.3), not
                            // `bpm` — quantize the typed v1-path's bpm the
                            // same way `midi::persist::save_into_project`
                            // does, so a v1-path tempo change (e.g. an
                            // auto-save right after recording) doesn't leave
                            // a stale period alongside a misleading `bpm`
                            // field bolted onto a v3 row (that field never
                            // existed in v3 and nothing reads it).
                            let period = crate::time::period_from_bpm(project.tempo_bpm);
                            first["periodStart"] = serde_json::json!(period);
                            first["periodEnd"] = serde_json::json!(period);
                        } else {
                            first["bpm"] = serde_json::json!(project.tempo_bpm);
                        }
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
    // v1, v2 and v3 are all readable here: the typed struct carries the v1
    // fields; the v2/v3 midi fields (tempoMap, meterMap, midiClips, ...)
    // are read by `midi::persist` straight from the file (unknown fields
    // are ignored per D-06, never rejected).
    if !(1..=3).contains(&project.schema_version) {
        return Err(format!("unsupported project schemaVersion {}", project.schema_version));
    }
    project.path = Some(dir.to_string_lossy().into_owned());
    // Round-2 §2.2: legacy clips (no sourceId on disk) get one deterministically
    // minted per unique source_path, so every clip entering the store from this
    // point on carries a real identity for the decode cache.
    assign_source_ids(&mut project.clips);
    // Round-2 §5: same treatment for content_id/lane_id (ADR 0004) — every
    // clip entering the store carries real placement/content addressing.
    assign_content_and_lane_ids(&mut project.clips);
    Ok((project, dir))
}

/// Fixed namespace for deterministic content-id minting on legacy audio
/// clips (round-2 §5, ADR 0004) — same discipline as `AURA_SOURCE_NS`: a
/// separate namespace from MIDI's own content-id minting
/// (`midi::persist::CONTENT_NS`) is fine — `ContentId` is just a UUID
/// space, not required to share one minting function across domains — and
/// keeps this module's identity assignment self-contained.
const AURA_CONTENT_NS: uuid::Uuid = uuid::uuid!("2b6e9f31-4d7c-4e0a-8f2b-6a1d3c5e7f90");

/// Mint `content_id`/`lane_id` for every clip that doesn't already carry
/// one (round-2 §5, ADR 0004). Audio clips are content-backed too — a thin
/// content object wrapping the `SourceId` — so the placement schema stays
/// uniform with MIDI's; scope ruling (this plan's preamble): addressing is
/// real from these fields on, but the JSON stays a single clip row (no
/// content[]/placements[] array split for audio yet). `lane_id` uses the
/// SAME `LaneId::default_for_track` function MIDI clips use, so a track's
/// default lane is one id regardless of which domain's clip asks for it.
pub(crate) fn assign_content_and_lane_ids(clips: &mut [Clip]) {
    for clip in clips.iter_mut() {
        if clip.content_id.as_str().is_empty() {
            clip.content_id = crate::ids::ContentId(
                uuid::Uuid::new_v5(&AURA_CONTENT_NS, clip.id.as_str().as_bytes()).to_string(),
            );
        }
        if clip.lane_id.as_str().is_empty() {
            clip.lane_id = crate::ids::LaneId::default_for_track(clip.track_id.as_str());
        }
    }
}

/// Mint one `SourceId` per unique (normalized) `source_path` for every clip
/// that doesn't already carry one (round-2 §2.2, H-3/M-6). Minting is
/// DETERMINISTIC (UUIDv5 over the normalized path under [`AURA_SOURCE_NS`]),
/// so the same legacy project opens with the SAME ids every session without
/// requiring a save-on-open — two clips sharing a `source_path` end up
/// sharing an id "for free" (same hash input), no bookkeeping map needed.
/// Clips whose `source_path` cannot be normalized (path-traversal, L-1) are
/// left unassigned with a loud warning rather than minting over a
/// potentially-wrong path.
pub(crate) fn assign_source_ids(clips: &mut [Clip]) {
    for clip in clips.iter_mut() {
        if !clip.source_id.as_str().is_empty() {
            continue; // already assigned — never re-minted
        }
        match normalize_source_path(&clip.source_path) {
            Ok(normalized) => clip.source_id = mint_deterministic_source_id(&normalized),
            Err(e) => log::warn!(
                "assign_source_ids: clip {} left unassigned: {e}",
                clip.id
            ),
        }
    }
}

/// Deterministic legacy-id mint: `SourceId(UuidV5(AURA_SOURCE_NS, normalized_path))`.
fn mint_deterministic_source_id(normalized_path: &str) -> SourceId {
    SourceId(uuid::Uuid::new_v5(&AURA_SOURCE_NS, normalized_path.as_bytes()).to_string())
}

/// Normalize a clip's `source_path` into a stable, project-relative POSIX
/// form for deterministic id minting (L-1): collapses `.` segments and
/// REJECTS any `..` component or a leading absolute-path anchor outright —
/// a normalized path must never escape the project, and an absolute path is
/// left unassigned rather than salvaged.
///
/// Reviewer finding 3: an earlier version stripped a leading `/` down to its
/// relative tail, which was wrong in two ways — `/audio/x.wav` and
/// `audio/x.wav` would normalize to the SAME string and mint the SAME
/// `SourceId` for two potentially DIFFERENT files (the one-source-one-path
/// invariant broken in the forbidden direction), and `project_dir.join(rel)`
/// on the caller side discards the join base for an absolute `rel`, which
/// can point straight out of the project. Absolute paths are now rejected
/// exactly like `..`, never salvaged.
pub(crate) fn normalize_source_path(source_path: &str) -> Result<String, String> {
    let slashed = source_path.replace('\\', "/");
    if slashed.starts_with('/') {
        return Err(format!("source_path is absolute, refusing to normalize: {source_path:?}"));
    }
    // Finding 7: a Windows drive-absolute path (`C:\audio\x.wav` ->
    // `C:/audio/x.wav` after the backslash swap above) doesn't start with
    // `/`, so it slipped past the check above — its first segment is a
    // drive letter ending in `:` (e.g. "C:"). Reject those exactly like a
    // POSIX-absolute path: minting a SourceId or joining a project dir with
    // a drive-absolute path escapes the project the same way `/...` does.
    if slashed.split('/').next().is_some_and(|first| first.ends_with(':')) {
        return Err(format!(
            "source_path is drive-absolute, refusing to normalize: {source_path:?}"
        ));
    }
    let mut out: Vec<&str> = Vec::new();
    for seg in slashed.split('/') {
        match seg {
            "" | "." => continue, // collapse repeated separators / current-dir segments
            ".." => return Err(format!("source_path escapes the project: {source_path:?}")),
            s => out.push(s),
        }
    }
    if out.is_empty() {
        return Err(format!("source_path is empty after normalization: {source_path:?}"));
    }
    Ok(out.join("/"))
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

    /// MAX_TRACKS removed in Plan B (round-2 §2.4): slot assignment is
    /// per-graph now (`ParamTable::with_slots`), so `validate` no longer
    /// caps track count — a 64-track ceiling would be a miss against the
    /// product's own scale ambition.
    #[test]
    fn projects_larger_than_sixty_four_tracks_validate() {
        let parent = tmp_parent("wide-project");
        let (mut project, _) = create(&parent, "V", 48_000, 120.0).unwrap();
        project.tracks = (0..200).map(track_n).collect();
        assert!(validate(&project).is_ok());
        let _ = fs::remove_dir_all(&parent);
    }

    /// Review fix: a project with duplicate track ids is rejected UP FRONT
    /// by `validate`, before `open_project` mutates any in-memory state.
    /// The cap is gone; this half of the guard stays.
    #[test]
    fn validate_rejects_duplicate_track_ids() {
        let parent = tmp_parent("validate-dup");
        let (mut project, _) = create(&parent, "V", 48_000, 120.0).unwrap();
        project.tracks = (0..2).map(track_n).collect();
        project.tracks[1].id = project.tracks[0].id.clone();
        assert!(validate(&project).is_err(), "duplicate ids rejected");
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

    // ---- source-id assignment (round-2 §2.2) -------------------------------

    fn clip_n(id: &str, source_path: &str, source_id: &str) -> Clip {
        Clip {
            id: id.into(),
            track_id: "t0".into(),
            name: id.into(),
            source_path: source_path.into(),
            source_id: source_id.into(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 48_000,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: 48_000,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: Default::default(),
            lane_id: Default::default(),
        }
    }

    #[test]
    fn legacy_clips_get_deterministic_content_and_lane_ids() {
        // Content is keyed by CLIP id (1:1 with its placement — no
        // split/merge/copy content-op exists yet, round-2 §2.1's remint
        // rules bind those from the day they land, not before); lane is
        // keyed by TRACK id (round-2 §5's default-lane rule, same function
        // MIDI's LaneId::default_for_track already uses).
        let mut clips = vec![clip_n("c0", "audio/x.wav", ""), clip_n("c1", "audio/y.wav", "")];
        clips[1].track_id = "t1".into();
        assign_content_and_lane_ids(&mut clips);
        assert!(!clips[0].content_id.as_str().is_empty());
        assert!(!clips[1].content_id.as_str().is_empty());
        assert_ne!(clips[0].content_id, clips[1].content_id, "each clip gets its own content id");
        assert_eq!(
            clips[0].lane_id,
            crate::ids::LaneId::default_for_track("t0"),
            "lane id matches the shared default_for_track function MIDI uses"
        );
        assert_ne!(clips[0].lane_id, clips[1].lane_id, "different track -> different lane");

        // Deterministic across independent runs (same discipline as
        // assign_source_ids — M-6).
        let mut clips2 = vec![clip_n("c0", "audio/x.wav", "")];
        assign_content_and_lane_ids(&mut clips2);
        assert_eq!(clips2[0].content_id, clips[0].content_id, "deterministic across runs");
        assert_eq!(clips2[0].lane_id, clips[0].lane_id);

        // Already-assigned ids are never re-minted.
        let mut pre = clip_n("c0", "audio/x.wav", "");
        pre.content_id = crate::ids::ContentId("pre-existing-content".into());
        pre.lane_id = crate::ids::LaneId("pre-existing-lane".into());
        let mut pre_vec = vec![pre];
        assign_content_and_lane_ids(&mut pre_vec);
        assert_eq!(pre_vec[0].content_id.as_str(), "pre-existing-content");
        assert_eq!(pre_vec[0].lane_id.as_str(), "pre-existing-lane");
    }

    #[test]
    fn legacy_clips_get_one_source_id_per_unique_path() {
        // Two clips sharing audio/x.wav and one on audio/y.wav, none with a
        // sourceId (legacy file): after load-fixup the two sharers have the
        // SAME minted id, the third a different one; a clip that already has
        // an id keeps it.
        let mut p = Project {
            schema_version: 1,
            name: "legacy".into(),
            path: None,
            created_at: None,
            modified_at: None,
            sample_rate: 48_000,
            tempo_bpm: 120.0,
            time_signature: Some((4, 4)),
            tracks: Vec::new(),
            clips: vec![
                clip_n("c0", "audio/x.wav", ""),
                clip_n("c1", "audio/x.wav", ""),
                clip_n("c2", "audio/y.wav", ""),
                clip_n("c3", "audio/z.wav", "pre-existing"),
            ],
            transport: None,
        };
        assign_source_ids(&mut p.clips);
        assert_eq!(p.clips[0].source_id, p.clips[1].source_id, "same path -> same id");
        assert_ne!(p.clips[0].source_id, p.clips[2].source_id, "different path -> different id");
        assert!(!p.clips[0].source_id.as_str().is_empty());
        assert_eq!(p.clips[3].source_id.as_str(), "pre-existing", "already-assigned id kept");

        // Deterministic minting (M-6): two independent runs over an
        // identically-shaped clip list yield IDENTICAL ids — no save-on-open
        // required for a legacy project to open with stable ids every time.
        let mut p2_clips = vec![clip_n("c0", "audio/x.wav", ""), clip_n("c2", "audio/y.wav", "")];
        assign_source_ids(&mut p2_clips);
        assert_eq!(p2_clips[0].source_id, p.clips[0].source_id, "deterministic across runs");
        assert_eq!(p2_clips[1].source_id, p.clips[2].source_id, "deterministic across runs");
    }

    #[test]
    fn source_path_traversal_is_rejected_not_minted_over() {
        let mut clips = vec![clip_n("c0", "../../etc/passwd", ""), clip_n("c1", "audio/ok.wav", "")];
        assign_source_ids(&mut clips);
        assert!(clips[0].source_id.as_str().is_empty(), "traversal path left unassigned");
        assert!(!clips[1].source_id.as_str().is_empty());
    }

    /// Reviewer finding 3: an absolute `source_path` must be REJECTED, not
    /// salvaged by stripping the leading `/` — otherwise `/audio/x.wav` and
    /// `audio/x.wav` (two potentially DIFFERENT files) would normalize to
    /// the same string and mint the SAME SourceId (the one-source-one-path
    /// invariant broken in the forbidden direction), and the caller's
    /// `project_dir.join(source_path)` on an absolute path discards the
    /// project dir entirely (escapes the project).
    #[test]
    fn absolute_source_path_is_rejected_and_does_not_merge_with_the_relative_form() {
        let mut clips = vec![
            clip_n("c0", "/audio/x.wav", ""),
            clip_n("c1", "audio/x.wav", ""),
        ];
        assign_source_ids(&mut clips);
        assert!(clips[0].source_id.as_str().is_empty(), "absolute path left unassigned, not minted over");
        assert!(!clips[1].source_id.as_str().is_empty(), "the relative sibling still mints normally");
        assert_ne!(
            clips[0].source_id, clips[1].source_id,
            "an absolute path must NEVER merge with its relative-looking twin"
        );
    }

    /// Finding 7: a Windows drive-absolute path (`C:\...`) doesn't start
    /// with `/`, so the POSIX-absolute check above alone let it through —
    /// after the `\` -> `/` swap it becomes `C:/audio/x.wav`, which minted a
    /// SourceId and, on the caller's `project_dir.join(...)` side, escapes
    /// the project (a Windows-absolute join discards the join base just
    /// like a POSIX-absolute one). Must be rejected the same way.
    #[test]
    fn windows_drive_absolute_source_path_is_rejected() {
        let mut clips = vec![
            clip_n("c0", "C:\\audio\\x.wav", ""),
            clip_n("c1", "audio/x.wav", ""),
        ];
        assign_source_ids(&mut clips);
        assert!(
            clips[0].source_id.as_str().is_empty(),
            "Windows drive-absolute path left unassigned, not minted over"
        );
        assert!(!clips[1].source_id.as_str().is_empty(), "the relative sibling still mints normally");
        assert_ne!(
            clips[0].source_id, clips[1].source_id,
            "a drive-absolute path must NEVER merge with its relative-looking twin"
        );
    }

    #[test]
    fn load_fixup_assigns_source_ids_from_disk() {
        let parent = tmp_parent("source-ids");
        let (mut project, dir) = create(&parent, "Legacy", 48_000, 120.0).unwrap();
        project.clips = vec![clip_n("c0", "audio/a.wav", ""), clip_n("c1", "audio/a.wav", "")];
        save(&dir, &project).unwrap();
        // The file on disk has no sourceId field at all (pre-existing
        // project) — the wire type's #[serde(default)] covers the absence,
        // and `load`'s fixup mints deterministically on the way in.
        let (loaded, _) = load(&dir).unwrap();
        assert!(!loaded.clips[0].source_id.as_str().is_empty());
        assert_eq!(loaded.clips[0].source_id, loaded.clips[1].source_id);
        let _ = fs::remove_dir_all(&parent);
    }
}
