//! Project v4 persistence for the modulation graph (design §7).
//!
//! `project.json` gains `modulation: { curves, bindings, automationClips,
//! modulators, macros }`. Point payloads stay out of JSON — they are the
//! same AMEV chunks under `<project>/automation/<uuid>.bin` that Track D
//! used for lanes (`encode_points` / `decode_points`), one chunk per curve,
//! referenced as `pointsRef`.
//!
//! Writing always emits schemaVersion 4 and drops `automation[]` (one-way
//! upgrade). Reading v3 still works forever: an `automation[]` project is
//! migrated in memory via [`migrate_v3_lanes`]; the file is left alone until
//! the next save. A corrupt or traversal-unsafe chunk degrades to an empty
//! curve, never a failed open.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::midi::types::DEFAULT_PPQ;
use crate::plugins::automation::{
    decode_points, encode_points, AutomationLane, AutomationPoint, AUTOMATION_DIR,
    TRACK_TARGET_PREFIX,
};

use super::model::{
    AutomationClip, Binding, BindingMode, Curve, Domain, Range, RangeSnapshot, Source, TargetRef,
    TrackParam,
};
use super::ModulationDoc;

// ---------------------------------------------------------------------------
// Wire rows (points live in chunks; never inline)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedCurve {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    length_ticks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    points_ref: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedModulation {
    #[serde(default)]
    curves: Vec<PersistedCurve>,
    #[serde(default)]
    bindings: Vec<Binding>,
    #[serde(default)]
    automation_clips: Vec<AutomationClip>,
    /// Reserved (design §8.2). Always empty this round; kept on the wire so
    /// a future writer is an addition, not a format break.
    #[serde(default)]
    modulators: Vec<Value>,
    /// Reserved (design §8.3).
    #[serde(default)]
    macros: Vec<Value>,
}

// ---------------------------------------------------------------------------
// v3 → document migration
// ---------------------------------------------------------------------------

/// Translate Track D `automation[]` lanes into a [`ModulationDoc`].
///
/// - `track:<id>` + param 0 → multiply binding on track gain (points already
///   0..1; Track D ruling 6).
/// - plugin instance + paramId → absolute binding. When `params` returns the
///   native range, points are normalized and `rangeSnapshot` recorded;
///   otherwise `domain: native` and the points stay in the param's units so
///   the project still sounds right (design §7).
///
/// Binding ids carry over from the lane so journals and the UI keep
/// addressing the same row. Curve ids are minted as `cur_<laneId>`.
pub fn migrate_v3_lanes(
    lanes: &[AutomationLane],
    params: &dyn Fn(&str, u32) -> Option<(f32, f32)>,
) -> ModulationDoc {
    let mut doc = ModulationDoc::default();
    for lane in lanes {
        let curve_id = format!("cur_{}", lane.id);
        let mut points = lane.points.clone();
        let (target, mode, domain, range_snapshot) =
            if let Some(track_id) = lane.target_node.strip_prefix(TRACK_TARGET_PREFIX) {
                // Design §7: track:<id> + param 0 → multiply on gain.
                // Non-gain track params never resolved in v3 either; still
                // mint a gain multiply so the points survive as a shape
                // (`lane.param_id` is not consulted — only gain was playable).
                (
                    TargetRef::TrackParam {
                        track_id: track_id.to_string(),
                        param: TrackParam::Gain,
                    },
                    BindingMode::Multiply,
                    Domain::Normalized,
                    None,
                )
            } else {
                // Plugin param — absolute override (Track D ruling 2).
                match params(&lane.target_node, lane.param_id) {
                    Some((min, max)) if max > min && min.is_finite() && max.is_finite() => {
                        let span = max - min;
                        for p in points.iter_mut() {
                            p.value = (p.value - min) / span;
                        }
                        (
                            TargetRef::PluginParam {
                                instance_id: lane.target_node.clone(),
                                param_id: lane.param_id,
                            },
                            BindingMode::Absolute,
                            Domain::Normalized,
                            Some(RangeSnapshot { min, max }),
                        )
                    }
                    _ => (
                        TargetRef::PluginParam {
                            instance_id: lane.target_node.clone(),
                            param_id: lane.param_id,
                        },
                        BindingMode::Absolute,
                        Domain::Native,
                        None,
                    ),
                }
            };

        doc.curves.push(Curve {
            id: curve_id.clone(),
            name: String::new(),
            length_ticks: None,
            points,
        });
        doc.bindings.push(Binding {
            id: lane.id.clone(),
            source: Source::Curve { curve_id },
            target,
            mode,
            depth: 1.0,
            range: Range::default(),
            domain,
            range_snapshot,
            enabled: true,
        });
    }
    doc
}

// ---------------------------------------------------------------------------
// Save / load
// ---------------------------------------------------------------------------

/// Write `doc` into `<dir>/project.json` as `modulation{}`, point chunks
/// under `<dir>/automation/`, set `schemaVersion` to 4, and remove any
/// `automation[]` key (one-way upgrade, design §7). Stale `*.bin` chunks
/// are GC'd after a successful write.
pub fn save_into_project(dir: &Path, doc: &ModulationDoc) -> Result<(), String> {
    let ppq = fs::read(dir.join("project.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("ppq").and_then(Value::as_u64))
        .map(|p| p as u32)
        .unwrap_or(DEFAULT_PPQ);

    let adir = dir.join(AUTOMATION_DIR);
    let mut wire_curves = Vec::with_capacity(doc.curves.len());
    let mut live_chunks: Vec<String> = Vec::new();
    for curve in &doc.curves {
        let mut row = PersistedCurve {
            id: curve.id.clone(),
            name: curve.name.clone(),
            length_ticks: curve.length_ticks,
            points_ref: None,
        };
        if !curve.points.is_empty() {
            fs::create_dir_all(&adir).map_err(|e| e.to_string())?;
            let chunk_name = format!("{}.bin", uuid::Uuid::new_v4());
            fs::write(adir.join(&chunk_name), encode_points(ppq, &curve.points))
                .map_err(|e| format!("write automation chunk: {e}"))?;
            row.points_ref = Some(format!("{AUTOMATION_DIR}/{chunk_name}"));
            live_chunks.push(chunk_name);
        }
        wire_curves.push(row);
    }

    let wire = PersistedModulation {
        curves: wire_curves,
        bindings: doc.bindings.clone(),
        automation_clips: doc.automation_clips.clone(),
        modulators: Vec::new(),
        macros: Vec::new(),
    };
    let value = serde_json::to_value(&wire).map_err(|e| e.to_string())?;

    // `update_project_v2` stamps schemaVersion 2 first; overwrite to 4 in
    // the same edit so a v3 project is never left downgraded to 2.
    crate::midi::persist::update_project_v2(dir, |obj| {
        obj.insert("modulation".into(), value);
        obj.insert("schemaVersion".into(), json!(4));
        obj.remove("automation");
        Ok(())
    })?;

    // GC stale chunks (best-effort, after a successful write).
    if let Ok(entries) = fs::read_dir(&adir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".bin") && !live_chunks.iter().any(|c| c == &name) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// Load the modulation document from `<dir>/project.json`.
///
/// * `modulation{}` present → decode it (v4).
/// * else `automation[]` present → migrate via [`migrate_v3_lanes`] (ranges
///   unknown here; callers that have live param tables can re-migrate).
/// * neither → empty document.
///
/// Corrupt / traversal-unsafe chunks degrade to empty curves.
pub fn load_from_project(dir: &Path) -> Result<ModulationDoc, String> {
    let file = dir.join("project.json");
    let bytes = fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", file.display()))?;
    let project_ppq = root
        .get("ppq")
        .and_then(Value::as_u64)
        .map(|p| p as u32)
        .unwrap_or(DEFAULT_PPQ);

    if let Some(mod_val) = root.get("modulation") {
        return load_modulation_object(dir, mod_val, project_ppq);
    }

    if let Some(arr) = root.get("automation") {
        let Some(arr) = arr.as_array() else {
            return Err("automation field is not an array".into());
        };
        let mut lanes = Vec::with_capacity(arr.len());
        for v in arr {
            let row: V3PersistedLane = match serde_json::from_value(v.clone()) {
                Ok(r) => r,
                Err(e) => {
                    log::warn!("modulation migrate: skipping malformed lane row ({e}): {v}");
                    continue;
                }
            };
            let points = match &row.points_ref {
                Some(rel) => match read_chunk(dir, rel, project_ppq) {
                    Ok(points) => points,
                    Err(e) => {
                        log::warn!("automation lane {}: {e}; loading without points", row.id);
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };
            lanes.push(AutomationLane {
                id: row.id,
                target_node: row.target_node,
                param_id: row.param_id,
                points,
            });
        }
        // Pure file load has no live plugin table; ranges unknown → native
        // for plugin lanes. Session open can re-migrate with real ranges.
        return Ok(migrate_v3_lanes(&lanes, &|_, _| None));
    }

    Ok(ModulationDoc::default())
}

fn load_modulation_object(
    dir: &Path,
    mod_val: &Value,
    project_ppq: u32,
) -> Result<ModulationDoc, String> {
    let wire: PersistedModulation = serde_json::from_value(mod_val.clone())
        .map_err(|e| format!("parse modulation: {e}"))?;
    let mut curves = Vec::with_capacity(wire.curves.len());
    for row in wire.curves {
        let points = match &row.points_ref {
            Some(rel) => match read_chunk(dir, rel, project_ppq) {
                Ok(points) => points,
                Err(e) => {
                    log::warn!("modulation curve {}: {e}; loading without points", row.id);
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        curves.push(Curve {
            id: row.id,
            name: row.name,
            length_ticks: row.length_ticks,
            points,
        });
    }
    Ok(ModulationDoc {
        curves,
        bindings: wire.bindings,
        automation_clips: wire.automation_clips,
    })
}

/// Lane row shape under the retired `automation[]` key.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct V3PersistedLane {
    id: String,
    target_node: String,
    param_id: u32,
    #[serde(default)]
    points_ref: Option<String>,
}

fn read_chunk(dir: &Path, rel: &str, project_ppq: u32) -> Result<Vec<AutomationPoint>, String> {
    // pointsRef must stay inside the project ("automation/<name>.bin").
    let name = rel
        .strip_prefix("automation/")
        .filter(|n| !n.contains('/') && !n.contains('\\') && !n.contains("..") && n.ends_with(".bin"))
        .ok_or_else(|| format!("invalid pointsRef {rel:?}"))?;
    let path = dir.join(AUTOMATION_DIR).join(name);
    let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let (chunk_ppq, mut points) = decode_points(&bytes)?;
    if chunk_ppq != project_ppq && chunk_ppq > 0 {
        for p in points.iter_mut() {
            p.tick = rescale(p.tick, chunk_ppq, project_ppq);
        }
    }
    Ok(points)
}

#[inline]
fn rescale(t: u32, from_ppq: u32, to_ppq: u32) -> u32 {
    ((t as u64 * to_ppq as u64 + from_ppq as u64 / 2) / from_ppq as u64) as u32
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::project;
    use crate::plugins::automation;

    #[test]
    fn v3_track_gain_lane_migrates_to_a_multiply_binding_with_the_points_untouched() {
        let lanes = vec![AutomationLane {
            id: "lane-1".into(),
            target_node: "track:t1".into(),
            param_id: 0,
            points: vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 960, value: 0.25 },
            ],
        }];
        let doc = migrate_v3_lanes(&lanes, &|_, _| None);
        assert_eq!(doc.curves.len(), 1);
        assert_eq!(doc.curves[0].points[1].value, 0.25, "gain points are ALREADY 0..1");
        assert_eq!(doc.bindings.len(), 1);
        let b = &doc.bindings[0];
        assert_eq!(b.mode, BindingMode::Multiply, "Track D ruling 6: a multiplier on the fader");
        assert_eq!(b.depth, 1.0);
        assert_eq!(
            b.target,
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain }
        );
        assert_eq!(b.id, "lane-1", "the lane id carries over so journals and UI keep addressing it");
    }

    #[test]
    fn v3_plugin_lane_normalizes_when_the_param_range_is_known() {
        let lanes = vec![AutomationLane {
            id: "lane-2".into(),
            target_node: "inst-1".into(),
            param_id: 7,
            points: vec![AutomationPoint { tick: 0, value: 5_000.0 }],
        }];
        let doc = migrate_v3_lanes(&lanes, &|inst, idx| {
            (inst == "inst-1" && idx == 7).then_some((0.0, 10_000.0))
        });
        assert_eq!(doc.curves[0].points[0].value, 0.5);
        assert_eq!(
            doc.bindings[0].mode,
            BindingMode::Absolute,
            "Track D ruling 2: overrides the knob"
        );
        assert_eq!(doc.bindings[0].domain, Domain::Normalized);
        assert_eq!(
            doc.bindings[0].range_snapshot,
            Some(RangeSnapshot { min: 0.0, max: 10_000.0 })
        );
    }

    #[test]
    fn v3_plugin_lane_with_an_unknown_range_stays_native_so_the_project_still_sounds_right() {
        let lanes = vec![AutomationLane {
            id: "lane-3".into(),
            target_node: "inst-gone".into(),
            param_id: 2,
            points: vec![AutomationPoint { tick: 0, value: 440.0 }],
        }];
        let doc = migrate_v3_lanes(&lanes, &|_, _| None);
        assert_eq!(
            doc.curves[0].points[0].value, 440.0,
            "left alone — normalizing it would be a guess"
        );
        assert_eq!(doc.bindings[0].domain, Domain::Native);
        assert_eq!(doc.bindings[0].range_snapshot, None);
    }

    #[test]
    fn save_then_load_round_trips_and_writes_schema_version_4() {
        let parent = std::env::temp_dir().join(format!(
            "aura-mod-persist-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&parent).unwrap();
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let doc = ModulationDoc {
            curves: vec![
                Curve {
                    id: "cur-a".into(),
                    name: "gain".into(),
                    length_ticks: None,
                    points: vec![
                        AutomationPoint { tick: 0, value: 1.0 },
                        AutomationPoint { tick: 3840, value: 0.0 },
                    ],
                },
                Curve {
                    id: "cur-empty".into(),
                    name: String::new(),
                    length_ticks: Some(960),
                    points: vec![],
                },
            ],
            bindings: vec![Binding {
                id: "bnd-a".into(),
                source: Source::Curve { curve_id: "cur-a".into() },
                target: TargetRef::TrackParam {
                    track_id: "t1".into(),
                    param: TrackParam::Gain,
                },
                mode: BindingMode::Multiply,
                depth: 1.0,
                range: Range::default(),
                domain: Domain::Normalized,
                range_snapshot: None,
                enabled: true,
            }],
            automation_clips: vec![AutomationClip {
                id: "acl-1".into(),
                track_id: "at-1".into(),
                curve_id: "cur-a".into(),
                timeline_start_ticks: 0,
                length_ticks: 3840,
                content_length_ticks: None,
            }],
        };
        save_into_project(&dir, &doc).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 4, "upgraded to v4");
        assert!(raw.get("automation").is_none(), "automation[] removed on v4 write");
        let curves = raw["modulation"]["curves"].as_array().unwrap();
        assert_eq!(curves.len(), 2);
        let pref = curves[0]["pointsRef"].as_str().unwrap();
        assert!(pref.starts_with("automation/") && pref.ends_with(".bin"));
        assert!(dir.join(pref).exists());
        assert!(curves[0].get("points").is_none(), "points NEVER inline");
        assert!(curves[1].get("pointsRef").is_none());
        assert!(raw["modulation"]["modulators"].as_array().unwrap().is_empty());
        assert!(raw["modulation"]["macros"].as_array().unwrap().is_empty());

        let loaded = load_from_project(&dir).unwrap();
        assert_eq!(loaded.curves.len(), 2);
        assert_eq!(loaded.curves[0].points, doc.curves[0].points);
        assert!(loaded.curves[1].points.is_empty());
        assert_eq!(loaded.bindings, doc.bindings);
        assert_eq!(loaded.automation_clips, doc.automation_clips);

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn loading_a_v3_project_directory_yields_the_migrated_doc_and_leaves_the_file_alone_until_save() {
        let parent = std::env::temp_dir().join(format!(
            "aura-mod-v3-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&parent).unwrap();
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let lanes = vec![AutomationLane {
            id: "lane-1".into(),
            target_node: "track:t1".into(),
            param_id: 0,
            points: vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 960, value: 0.25 },
            ],
        }];
        automation::save_into_project(&dir, &lanes).unwrap();
        let before = fs::read(dir.join("project.json")).unwrap();

        let doc = load_from_project(&dir).unwrap();
        assert_eq!(doc.curves.len(), 1);
        assert_eq!(doc.curves[0].points[1].value, 0.25);
        assert_eq!(doc.bindings[0].mode, BindingMode::Multiply);
        assert_eq!(doc.bindings[0].id, "lane-1");
        assert_eq!(
            doc.bindings[0].target,
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain }
        );

        let after_load = fs::read(dir.join("project.json")).unwrap();
        assert_eq!(before, after_load, "load migrates in memory; file untouched until save");

        // Save is the one-way upgrade.
        save_into_project(&dir, &doc).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 4);
        assert!(raw.get("automation").is_none());
        assert!(raw.get("modulation").is_some());

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn stale_point_chunks_are_gcd_after_a_successful_save() {
        let parent = std::env::temp_dir().join(format!(
            "aura-mod-gc-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&parent).unwrap();
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let mut doc = ModulationDoc {
            curves: vec![Curve {
                id: "cur-a".into(),
                name: String::new(),
                length_ticks: None,
                points: vec![
                    AutomationPoint { tick: 0, value: 1.0 },
                    AutomationPoint { tick: 3840, value: 0.0 },
                ],
            }],
            bindings: vec![],
            automation_clips: vec![],
        };
        save_into_project(&dir, &doc).unwrap();
        let first_chunks: Vec<_> = fs::read_dir(dir.join(AUTOMATION_DIR))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bin"))
            .collect();
        assert_eq!(first_chunks.len(), 1);

        // Resave with changed points → new chunk, stale GC'd.
        doc.curves[0].points[1].value = 0.5;
        save_into_project(&dir, &doc).unwrap();
        let chunks: Vec<_> = fs::read_dir(dir.join(AUTOMATION_DIR))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bin"))
            .collect();
        assert_eq!(chunks.len(), 1, "stale chunk GC'd");

        let loaded = load_from_project(&dir).unwrap();
        assert_eq!(loaded.curves[0].points[1].value, 0.5);

        let _ = fs::remove_dir_all(&parent);
    }
}
