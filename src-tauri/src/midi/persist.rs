//! Project v2/v3 persistence of MIDI data (PHASE2-PLAN §3.C, SCALABILITY
//! §3–4; v3 bump: round-2 §3.3, O-9/O-10, ADR 0002).
//!
//! `project.json` gains the v2 fields `ppq`, `midiClips` (see
//! `docs/ipc-schemas/project-v2.schema.json`) and, from schemaVersion 3,
//! an integer-period `tempoMap` (superticks per quarter, not bpm — round-2
//! §3.3) plus a persisted `meterMap` (time signature, default `[{0,4,4}]`).
//! Note payloads NEVER go inline: each clip's notes are written as an
//! immutable AMEV chunk `events/<uuid>.bin` referenced by `eventsRef`;
//! edits write a new chunk and stale chunks are garbage-collected after a
//! successful save.
//!
//! The v1 fields are owned by `audio::project` (typed reader/writer); this
//! module only ever touches project.json through a `serde_json::Value`
//! read-modify-write (atomic tmp+rename), so both writers preserve each
//! other's fields. Before the FIRST upgrade of a v1 file, a verbatim copy is
//! kept as `project.json.v1.bak` (SCALABILITY §4); before the first v2->v3
//! upgrade, `project.json.v2.bak` (same discipline, round-2 §3.3).
//!
//! v1 -> v2 migration is mechanical: `ppq = 960`, one-entry tempo map from
//! `tempoBpm`, no midi clips, no meter map. v2 -> v3 quantizes each bpm
//! event to the nearest integer period ONCE
//! (`crate::time::period_from_bpm`) and defaults the meter map to
//! `[{0,4,4}]`. [`load_from_project`] performs both IN MEMORY only; the
//! file is upgraded on the first midi save.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value};

use super::events;
use super::types::{first_note_id, MeterEvent, MidiClip, TempoEvent, TempoPeriodEvent, DEFAULT_PPQ};
use super::MidiStore;

const PROJECT_FILE: &str = "project.json";
const V1_BACKUP: &str = "project.json.v1.bak";
const V2_BACKUP: &str = "project.json.v2.bak";
const EVENTS_DIR: &str = "events";

/// The `[{0,4,4}]` default meter map (round-2 §3.3) — used both as the v3
/// write-time default (fresh stores) and the read-time default (v1/v2
/// files, which never had a meter map at all).
fn default_meter_events() -> Vec<MeterEvent> {
    vec![MeterEvent { tick: 0, num: 4, den: 4 }]
}

/// Fixed namespace for deterministic content-id minting on v2->v3
/// migration (round-2 §5, ADR 0004) — same discipline as
/// `audio::project::AURA_SOURCE_NS`: re-migrating the same un-resaved v2
/// file twice must mint the SAME content id, or every re-open looks like a
/// diff. Keyed by the clip's own id (content is 1:1 with its placement
/// until a split/merge/copy content-op exists — none does yet).
const CONTENT_NS: uuid::Uuid = uuid::uuid!("6f1a8b2e-8c4d-4a7a-9e1a-2c9f6b7d0a11");

fn mint_content_id(clip_id: &str) -> crate::ids::ContentId {
    crate::ids::ContentId(uuid::Uuid::new_v5(&CONTENT_NS, clip_id.as_bytes()).to_string())
}

/// v2/v3 midi fields loaded from a project. The name predates the v3 bump
/// (Task 6 adds `meter_events`, keeps the rest) — `tempo_events` stays
/// bpm-typed even for a v3 file: `TempoMap` (Task 3) is the actual period-
/// math authority, this struct is a control-plane cache of display values
/// quantized losslessly through `crate::time::{period_from_bpm,
/// bpm_from_period}`'s proven idempotence (Task 2).
#[derive(Debug, Clone, PartialEq)]
pub struct V3Data {
    pub ppq: u32,
    pub tempo_events: Vec<TempoEvent>,
    pub meter_events: Vec<MeterEvent>,
    pub clips: Vec<MidiClip>,
    pub launch_maps: Vec<super::launch::LaunchMap>,
    /// The Composer's harmony document (Plan H1). Additive and OPTIONAL: the
    /// key is written only when the document is non-empty, so a project that
    /// never used the Composer resaves byte-diff-free and `schemaVersion`
    /// stays where Track F left it (ruling H-9).
    pub harmony: crate::theory::HarmonyDoc,
}

/// Persisted clip row (midi-clip.schema.json `$defs/persistedClip`) — the
/// v2/legacy `midiClips` shape, read forever but never written from v3 on.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedClip {
    id: String,
    track_id: String,
    name: String,
    timeline_start_ticks: u64,
    length_ticks: u64,
    #[serde(default)]
    events_ref: Option<String>,
    /// Note-id watermark row copy (C-1): the JSON row is the DURABLE
    /// authority — it survives independently of the AMEV chunk, so an
    /// emptied clip (no chunk written, old chunk GC'd) does not lose its
    /// watermark. The loaded clip's watermark is `max(row, chunk)`.
    #[serde(default = "first_note_id")]
    next_note_id: u32,
    /// ADR 0004's content half (spec §3): absent = same as `length_ticks`.
    /// Only ever written to the row when `Some` (see `save_into_project`) so
    /// an old project resaves byte-diff-free.
    #[serde(default)]
    content_length_ticks: Option<u64>,
    #[serde(default)]
    transpose_semitones: i16,
    #[serde(default)]
    velocity_offset: i16,
}

/// v3 content row (round-2 §5, ADR 0004): the note payload half of the
/// split. `kind` is read but not branched on yet — every content row this
/// crate writes is `"midi"`; a future audio-content-array round (this
/// plan's Task 8 keeps audio single-row) would add `"audio"` rows here.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedContent {
    id: String,
    #[serde(default)]
    events_ref: Option<String>,
    #[serde(default = "first_note_id")]
    next_note_id: u32,
    /// ADR 0004's content half (spec §3): absent = same as the placement's
    /// `length_ticks`. Only ever written when `Some` (see
    /// `save_into_project`) so an old project resaves byte-diff-free.
    #[serde(default)]
    content_length_ticks: Option<u64>,
}

/// v3 placement row: the position/identity half of the split.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedPlacement {
    id: String,
    content_id: String,
    lane_id: String,
    name: String,
    timeline_start_ticks: u64,
    length_ticks: u64,
    #[serde(default)]
    transpose_semitones: i16,
    #[serde(default)]
    velocity_offset: i16,
}

/// v3 lane row: resolves a `LaneId` to its `TrackId`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedLane {
    id: String,
    track_id: String,
}

/// Write the midi store's state into `<dir>/project.json` (schemaVersion at
/// least 3 — round-2 §3.3/§0.1 O-9; never stamped below the version already
/// on disk) and the AMEV chunks under `<dir>/events/`.
pub fn save_into_project(dir: &Path, midi: &MidiStore) -> Result<(), String> {
    save_snapshot_into_project(
        dir,
        &V3Data {
            ppq: midi.ppq,
            tempo_events: midi.tempo_events.clone(),
            meter_events: midi.meter_events.clone(),
            launch_maps: midi.launch_maps.clone(),
            harmony: midi.harmony.clone(),
            clips: midi.clips.clone(),
        },
    )
}

/// Same as [`save_into_project`], taking a `V3Data` snapshot instead of a
/// `&MidiStore` — the persist-as-effect seam (round-2 §4): `Session::
/// midi_snapshot` clones exactly these fields under the session lock, and
/// `ControlPlane::execute_persist` calls this AFTER the lock is released,
/// so no disk I/O ever happens while the lock is held. Identical body to
/// the retired direct-`MidiStore` version this now backs.
pub fn save_snapshot_into_project(dir: &Path, midi: &V3Data) -> Result<(), String> {
    let file = dir.join(PROJECT_FILE);
    let bytes =
        fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let mut root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", file.display()))?;
    if !root.is_object() {
        return Err(format!("{}: not a JSON object", file.display()));
    }
    let was_v1 = root.get("schemaVersion").and_then(Value::as_u64) == Some(1);
    if was_v1 && !dir.join(V1_BACKUP).exists() {
        fs::copy(&file, dir.join(V1_BACKUP))
            .map_err(|e| format!("write {V1_BACKUP}: {e}"))?;
    }
    let was_v2 = root.get("schemaVersion").and_then(Value::as_u64) == Some(2);
    if was_v2 && !dir.join(V2_BACKUP).exists() {
        fs::copy(&file, dir.join(V2_BACKUP))
            .map_err(|e| format!("write {V2_BACKUP}: {e}"))?;
    }

    // Chunks first (orphans from a failed save are GC'd on the next one).
    let events_dir = dir.join(EVENTS_DIR);
    fs::create_dir_all(&events_dir).map_err(|e| e.to_string())?;
    let mut live_chunks = Vec::with_capacity(midi.clips.len());
    // v3 content/placement split (round-2 §5, ADR 0004): each clip becomes
    // one content row (note payload: eventsRef + the watermark, which
    // MOVES here from the placement — round-2 §5's table) and one
    // placement row (position/length/lane/name). `midiClips` is retired as
    // a WRITE target from v3 on (it stays readable forever for v2 files —
    // see `load_from_project`).
    let mut content_rows = Vec::with_capacity(midi.clips.len());
    let mut placement_rows = Vec::with_capacity(midi.clips.len());
    let mut lane_ids_seen = std::collections::HashSet::new();
    let mut lane_rows = Vec::new();
    for clip in &midi.clips {
        let mut content = json!({
            "id": clip.content_id,
            "kind": "midi",
            "nextNoteId": clip.next_note_id,
        });
        // ADR 0004 content/placement split: the loop length is the content
        // half, so it lives on the content row. Only write the key when
        // Some — a never-looped project resaves byte-diff-free (D-06).
        if let Some(cl) = clip.content_length_ticks {
            content["contentLengthTicks"] = json!(cl);
        }
        if !clip.notes.is_empty() {
            let chunk_name = format!("{}.bin", uuid::Uuid::new_v4());
            let chunk = events::encode_notes(midi.ppq, &clip.notes, clip.next_note_id);
            fs::write(events_dir.join(&chunk_name), chunk)
                .map_err(|e| format!("write events chunk: {e}"))?;
            content["eventsRef"] = json!(format!("{EVENTS_DIR}/{chunk_name}"));
            live_chunks.push(chunk_name);
        }
        content_rows.push(content);
        let mut placement = json!({
            "id": clip.id,
            "contentId": clip.content_id,
            "laneId": clip.lane_id,
            "name": clip.name,
            "timelineStartTicks": clip.timeline_start_ticks,
            "lengthTicks": clip.length_ticks,
        });
        if clip.transpose_semitones != 0 {
            placement["transposeSemitones"] = json!(clip.transpose_semitones);
        }
        if clip.velocity_offset != 0 {
            placement["velocityOffset"] = json!(clip.velocity_offset);
        }
        placement_rows.push(placement);
        // One row per DISTINCT lane actually referenced — today that's one
        // per track-with-a-clip (every clip on a track shares that
        // track's default lane, by construction: `LaneId::default_for_
        // track` is a pure function of track_id). A track with zero clips
        // gets no lane row here; this function only ever sees clips, not
        // the track list. Documented gap, not a silent one: a future round
        // that needs "every track has a lane, even an empty one" should
        // mint it at track-creation time, not derive it here at save time.
        if lane_ids_seen.insert(clip.lane_id.0.clone()) {
            lane_rows.push(json!({ "id": clip.lane_id, "trackId": clip.track_id }));
        }
    }

    // v3 tempoMap (round-2 §3.3): each bpm event quantizes ONCE, here, into
    // a constant-period TempoPeriodEvent (period_start == period_end — no
    // ramp-editing command exists yet to produce anything else). Task 2's
    // idempotence guarantee (period_from_bpm(bpm_from_period(p)) == p)
    // means a value that already passed through this quantization once
    // (loaded back via `load_from_project`'s v3 branch, then re-saved)
    // never drifts on subsequent saves.
    let period_events: Vec<TempoPeriodEvent> = midi
        .tempo_events
        .iter()
        .map(|e| {
            let p = crate::time::period_from_bpm(e.bpm);
            TempoPeriodEvent { tick: e.tick, period_start: p, period_end: p }
        })
        .collect();

    let obj = root.as_object_mut().expect("checked above");
    stamp_schema_version(obj, 3);
    obj.insert("ppq".into(), json!(midi.ppq));
    obj.insert("tempoMap".into(), serde_json::to_value(&period_events).unwrap());
    obj.insert("meterMap".into(), serde_json::to_value(&midi.meter_events).unwrap());
    obj.insert("sectionTableRuleVersion".into(), json!(super::section_table::RULE_VERSION));
    // Invariant (v3): tempoBpm mirrors tempoMap[0]'s period as a DISPLAY
    // value only — never re-quantized on its own, never the source of
    // truth (that's tempoMap's period).
    if let Some(first) = midi.tempo_events.first() {
        obj.insert("tempoBpm".into(), json!(first.bpm));
    }
    obj.insert("content".into(), Value::Array(content_rows));
    obj.insert("placements".into(), Value::Array(placement_rows));
    obj.insert("lanes".into(), Value::Array(lane_rows));
    obj.insert(
        "launchMaps".into(),
        serde_json::to_value(&midi.launch_maps).unwrap(),
    );
    // Additive, and only when there is something to say — see `V3Data::harmony`.
    if midi.harmony.is_empty() {
        obj.remove("harmony");
    } else {
        obj.insert("harmony".into(), serde_json::to_value(&midi.harmony).unwrap());
    }
    obj.remove("midiClips"); // v3 stops writing the v2 key; still read forever (see load_from_project)

    atomic_write_json(dir, &root)?;

    // GC chunks no longer referenced (best-effort).
    if let Ok(entries) = fs::read_dir(&events_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".bin") && !live_chunks.iter().any(|c| c == &name) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// Load the midi fields from `<dir>/project.json`.
///
/// * v2/v3 file → `Ok(Some(V3Data))` with clips' notes decoded from their
///   AMEV chunks (a missing/corrupt chunk logs a warning and yields an
///   empty clip rather than failing the whole project). A v2 file's bpm
///   `tempoMap` loads unchanged (no meter map ever existed pre-v3, so
///   `meter_events` defaults to `[{0,4,4}]` — round-2 §3.3); a v3 file's
///   period `tempoMap` projects back to bpm via
///   `crate::time::bpm_from_period` for this struct's display-value field
///   (`TempoMap`, Task 3, is the actual period-math authority).
/// * v1 file → `Ok(None)`; the caller decides whether to adopt in-memory
///   state (fresh session) or reset to the mechanical migration defaults,
///   which [`v1_migration_defaults`] provides.
///
/// Task 6 (Plan E): this loader is PURE midi/v2+ parsing only — it used to
/// also cascade into `plugins::state::adopt_open_project`/
/// `plugins::automation::adopt_open_project` as a side effect (the
/// "plugin-teardown-inside-a-read horror", round-2 inventory row 9: a
/// supposedly pure `midi_get_clips` read could tear down live host state).
/// The adopt-chain now runs explicitly, and ONLY, from `ControlPlane`'s
/// sanctioned epoch functions (`open_project_epoch` et al.), after the
/// session lock that calls this loader has already been dropped.
pub fn load_from_project(dir: &Path) -> Result<Option<V3Data>, String> {
    let file = dir.join(PROJECT_FILE);
    let bytes =
        fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", file.display()))?;
    let version = root.get("schemaVersion").and_then(Value::as_u64).unwrap_or(1);
    if version < 2 {
        return Ok(None);
    }
    let ppq = root
        .get("ppq")
        .and_then(Value::as_u64)
        .map(|p| p as u32)
        .unwrap_or(DEFAULT_PPQ);
    let tempo_events: Vec<TempoEvent> = if version >= 3 {
        let period_events: Vec<TempoPeriodEvent> = match root.get("tempoMap") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| format!("tempoMap: {e}"))?,
            None => vec![TempoPeriodEvent {
                tick: 0,
                period_start: crate::time::period_from_bpm(120.0),
                period_end: crate::time::period_from_bpm(120.0),
            }],
        };
        period_events
            .into_iter()
            .map(|e| TempoEvent { tick: e.tick, bpm: crate::time::bpm_from_period(e.period_start) })
            .collect()
    } else {
        match root.get("tempoMap") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| format!("tempoMap: {e}"))?,
            None => vec![TempoEvent {
                tick: 0,
                bpm: root.get("tempoBpm").and_then(Value::as_f64).unwrap_or(120.0),
            }],
        }
    };
    let meter_events: Vec<MeterEvent> = match root.get("meterMap") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("meterMap: {e}"))?,
        None => default_meter_events(), // no meterMap in the file (v1/v2, or a v3 that omitted it)
    };
    let clips = if root.get("content").is_some() && root.get("placements").is_some() {
        parse_clips_v3_native(dir, &root, ppq)?
    } else {
        parse_clips_legacy(dir, &root, ppq)?
    };
    let maps = match root.get("launchMaps") {
        Some(v) => Some(
            serde_json::from_value(v.clone()).map_err(|e| format!("launchMaps: {e}"))?,
        ),
        None => None,
    };
    let legacy_bindings = match root.get("launchBindings") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("launchBindings: {e}"))?,
        None => Vec::new(),
    };
    let legacy_drive = match root.get("launchDriveClipIds") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("launchDriveClipIds: {e}"))?,
        None => Vec::new(),
    };
    let launch_maps = super::launch::migrate_legacy_maps(maps, legacy_bindings, legacy_drive);
    // A malformed harmony block loses the harmony, not the project: it is
    // derived-adjacent (chords the user can re-enter), and refusing to open a
    // song because one chord symbol rotted would be the wrong trade.
    let harmony = match root.get("harmony") {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            log::warn!("harmony: {e}; opening without it");
            crate::theory::HarmonyDoc::default()
        }),
        None => crate::theory::HarmonyDoc::default(),
    };
    Ok(Some(V3Data { ppq, tempo_events, meter_events, clips, launch_maps, harmony }))
}

/// The v2/legacy `midiClips` shape — one row IS the placement+content
/// conflation §5 splits apart. Content/lane ids are minted deterministically
/// (round-2 §2.2's precedent) so re-migrating the same un-resaved v2 file
/// twice produces identical ids.
fn parse_clips_legacy(dir: &Path, root: &Value, ppq: u32) -> Result<Vec<MidiClip>, String> {
    let rows: Vec<PersistedClip> = match root.get("midiClips") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("midiClips: {e}"))?,
        None => Vec::new(),
    };
    let mut clips = Vec::with_capacity(rows.len());
    for row in rows {
        // C-1: the row's watermark is the durable authority; the chunk's own
        // copy (when a chunk exists and reads cleanly) can only push it
        // forward, never override a higher row value.
        let (notes, chunk_watermark) = match &row.events_ref {
            Some(rel) => match read_chunk(dir, rel, ppq) {
                Ok((notes, watermark)) => (notes, Some(watermark)),
                Err(e) => {
                    log::warn!("midi clip {}: {e}; loading without notes", row.id);
                    (Vec::new(), None)
                }
            },
            None => (Vec::new(), None),
        };
        let next_note_id = match chunk_watermark {
            Some(w) => row.next_note_id.max(w),
            None => row.next_note_id,
        };
        let content_id = mint_content_id(&row.id);
        let lane_id = crate::ids::LaneId::default_for_track(&row.track_id);
        clips.push(MidiClip {
            id: row.id.into(),
            track_id: row.track_id.into(),
            name: row.name,
            timeline_start_ticks: row.timeline_start_ticks,
            length_ticks: row.length_ticks.max(1),
            notes,
            next_note_id,
            content_id,
            lane_id,
            content_length_ticks: row.content_length_ticks,
            transpose_semitones: row.transpose_semitones,
            velocity_offset: row.velocity_offset,
        });
    }
    Ok(clips)
}

/// The v3-native `content`/`placements`/`lanes` shape (round-2 §5, ADR
/// 0004): join placements to content on `contentId`, resolve each
/// placement's track through its `laneId` via the `lanes` index. A
/// placement whose `contentId`/`laneId` has no matching row is a
/// structurally corrupt v3 file — same policy as a missing `eventsRef`
/// (C-2 precedent): log and skip that ONE placement, never fail the whole
/// project load.
fn parse_clips_v3_native(dir: &Path, root: &Value, ppq: u32) -> Result<Vec<MidiClip>, String> {
    let content_rows: Vec<PersistedContent> = match root.get("content") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("content: {e}"))?,
        None => Vec::new(),
    };
    let placement_rows: Vec<PersistedPlacement> = match root.get("placements") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("placements: {e}"))?,
        None => Vec::new(),
    };
    let lane_rows: Vec<PersistedLane> = match root.get("lanes") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("lanes: {e}"))?,
        None => Vec::new(),
    };
    let lane_track: std::collections::HashMap<String, String> =
        lane_rows.into_iter().map(|l| (l.id, l.track_id)).collect();
    let content_by_id: std::collections::HashMap<String, PersistedContent> =
        content_rows.into_iter().map(|c| (c.id.clone(), c)).collect();

    let mut clips = Vec::with_capacity(placement_rows.len());
    for placement in placement_rows {
        let Some(content) = content_by_id.get(&placement.content_id) else {
            log::warn!(
                "midi placement {}: no matching content row {} — skipping",
                placement.id, placement.content_id
            );
            continue;
        };
        let Some(track_id) = lane_track.get(&placement.lane_id) else {
            log::warn!(
                "midi placement {}: no matching lane row {} — skipping",
                placement.id, placement.lane_id
            );
            continue;
        };
        // C-1: the content row's watermark is the durable authority; the
        // chunk's own copy (when readable) can only push it forward.
        let (notes, chunk_watermark) = match &content.events_ref {
            Some(rel) => match read_chunk(dir, rel, ppq) {
                Ok((notes, watermark)) => (notes, Some(watermark)),
                Err(e) => {
                    log::warn!("midi content {}: {e}; loading without notes", content.id);
                    (Vec::new(), None)
                }
            },
            None => (Vec::new(), None),
        };
        let next_note_id = match chunk_watermark {
            Some(w) => content.next_note_id.max(w),
            None => content.next_note_id,
        };
        clips.push(MidiClip {
            id: placement.id.into(),
            track_id: track_id.clone().into(),
            name: placement.name,
            timeline_start_ticks: placement.timeline_start_ticks,
            length_ticks: placement.length_ticks.max(1),
            notes,
            next_note_id,
            content_id: placement.content_id.into(),
            lane_id: placement.lane_id.into(),
            content_length_ticks: content.content_length_ticks,
            transpose_semitones: placement.transpose_semitones,
            velocity_offset: placement.velocity_offset,
        });
    }
    Ok(clips)
}

/// The mechanical v1 migration: ppq 960, one-entry tempo map, default
/// meter map, no clips.
pub fn v1_migration_defaults(tempo_bpm: f64) -> V3Data {
    V3Data {
        ppq: DEFAULT_PPQ,
        meter_events: default_meter_events(),
        tempo_events: vec![TempoEvent { tick: 0, bpm: tempo_bpm }],
        clips: Vec::new(),
        launch_maps: Vec::new(),
        harmony: crate::theory::HarmonyDoc::default(),
    }
}

/// Returns the decoded notes AND the chunk's own note-id watermark (C-1:
/// `load_from_project` combines it with the row's watermark via `max`).
fn read_chunk(dir: &Path, rel: &str, project_ppq: u32) -> Result<(Vec<super::MidiNote>, u32), String> {
    // eventsRef must stay inside the project (schema: "events/<name>.bin").
    let name = rel
        .strip_prefix("events/")
        .filter(|n| !n.contains('/') && !n.contains("..") && n.ends_with(".bin"))
        .ok_or_else(|| format!("invalid eventsRef {rel:?}"))?;
    let path = dir.join(EVENTS_DIR).join(name);
    let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let decoded = match events::decode_notes(&bytes) {
        Ok(d) => d,
        Err(e) => {
            // C-2: preserve the evidence. The GC in `save_into_project` only
            // deletes chunk names it currently considers live; renaming away
            // from `.bin` takes this chunk out of that consideration so a
            // corrupt-but-recoverable file is never silently destroyed by
            // the very next save.
            let bad = path.with_extension("bin.bad");
            if let Err(re) = fs::rename(&path, &bad) {
                log::warn!(
                    "midi: could not rename corrupt chunk {} to {}: {re}",
                    path.display(),
                    bad.display()
                );
            } else {
                log::warn!("midi: corrupt chunk {} renamed to {}", path.display(), bad.display());
            }
            return Err(e);
        }
    };
    let mut notes = decoded.notes;
    if decoded.ppq != project_ppq && decoded.ppq > 0 {
        // Rescale ticks to the project ppq (chunks written before a ppq
        // change stay valid).
        for n in notes.iter_mut() {
            n.tick = rescale(n.tick, decoded.ppq, project_ppq);
            n.length_ticks = rescale(n.length_ticks, decoded.ppq, project_ppq).max(1);
        }
    }
    Ok((notes, decoded.next_note_id))
}

#[inline]
fn rescale(t: u32, from_ppq: u32, to_ppq: u32) -> u32 {
    ((t as u64 * to_ppq as u64 + from_ppq as u64 / 2) / from_ppq as u64) as u32
}

/// Atomic read-modify-write of `<dir>/project.json` for ADDITIVE v2 fields
/// (zone P4: `plugins[]`, `automation[]`). The file is upgraded to
/// schemaVersion 2 first when it is still v1 (verbatim `project.json.v1.bak`
/// backup, same rule as [`save_into_project`]) because
/// `audio::project::save` only preserves unknown fields on v2 files — a
/// plugin/automation save into a v1 project would otherwise be dropped by
/// the next typed v1-path save. A file already at v3/v4 keeps that version
/// (`max(existing, 2)`). Other fields are preserved untouched, so the
/// midi, audio and P4 writers can interleave freely.
pub fn update_project_v2(
    dir: &Path,
    apply: impl FnOnce(&mut serde_json::Map<String, Value>) -> Result<(), String>,
) -> Result<(), String> {
    let file = dir.join(PROJECT_FILE);
    let bytes =
        fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let mut root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", file.display()))?;
    if !root.is_object() {
        return Err(format!("{}: not a JSON object", file.display()));
    }
    let was_v1 = root.get("schemaVersion").and_then(Value::as_u64) == Some(1);
    if was_v1 && !dir.join(V1_BACKUP).exists() {
        fs::copy(&file, dir.join(V1_BACKUP))
            .map_err(|e| format!("write {V1_BACKUP}: {e}"))?;
    }
    let obj = root.as_object_mut().expect("checked above");
    stamp_schema_version(obj, 2);
    apply(obj)?;
    atomic_write_json(dir, &root)
}

/// Never stamp `schemaVersion` below what's already on disk. Each writer
/// has a floor (midi = 3, additive v2 fields = 2); a later writer must
/// not undo an earlier upgrade (design §7: writing always emits v4).
fn stamp_schema_version(obj: &mut serde_json::Map<String, Value>, writer_floor: u64) {
    let existing = obj.get("schemaVersion").and_then(Value::as_u64).unwrap_or(0);
    obj.insert("schemaVersion".into(), json!(existing.max(writer_floor)));
}

/// Atomic project.json write: tmp + fsync + rename (same discipline as
/// `audio::project::save`).
fn atomic_write_json(dir: &Path, root: &Value) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(root).map_err(|e| e.to_string())?;
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

/// Absolute events dir for a project (`<dir>/events`).
pub fn events_dir(dir: &Path) -> PathBuf {
    dir.join(EVENTS_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::project;
    use crate::midi::types::MidiNote;

    fn tmp_parent(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("aura-midi-persist-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn store_with(clips: Vec<MidiClip>) -> MidiStore {
        MidiStore {
            harmony: Default::default(),
            ppq: DEFAULT_PPQ,
            tempo_events: vec![
                TempoEvent { tick: 0, bpm: 100.0 },
                TempoEvent { tick: 3840, bpm: 140.0 },
            ],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips,
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        }
    }

    fn clip(track: &str, notes: Vec<MidiNote>) -> MidiClip {
        let next_note_id = notes.iter().map(|n| n.note_id.0).max().unwrap_or(0) + 1;
        MidiClip {
            id: crate::ids::ClipId::mint(),
            track_id: track.into(),
            name: "Clip".into(),
            timeline_start_ticks: 960,
            length_ticks: 3840,
            notes,
            next_note_id,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track(track),
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        }
    }

    fn some_notes(n: usize) -> Vec<MidiNote> {
        (0..n)
            .map(|i| MidiNote {
                tick: (i * 240) as u32,
                length_ticks: 120,
                key: (24 + (i % 64)) as u8,
                velocity: (1 + (i % 127)) as u8,
                channel: (i % 16) as u8,
                note_id: crate::ids::NoteId((i + 1) as u32),
            })
            .collect()
    }

    #[test]
    fn v3_save_load_roundtrip_with_amev_chunks_and_backup() {
        let parent = tmp_parent("roundtrip");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let midi = store_with(vec![clip("t1", some_notes(10)), clip("t2", vec![])]);

        save_into_project(&dir, &midi).unwrap();
        assert!(dir.join(V1_BACKUP).exists(), "v1 backup written on upgrade");
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 3);
        assert_eq!(raw["ppq"], 960);
        assert_eq!(raw["tempoBpm"], 100.0, "tempoBpm == tempoMap[0].bpm");
        assert_eq!(
            raw["tempoMap"][0]["periodStart"],
            crate::time::period_from_bpm(100.0),
            "v3 tempoMap carries the quantized integer period, not bpm"
        );
        assert_eq!(raw["meterMap"][0], serde_json::json!({"tick": 0, "num": 4, "den": 4}));
        assert_eq!(raw["sectionTableRuleVersion"], crate::midi::section_table::RULE_VERSION);
        assert!(raw.get("midiClips").is_none(), "v3 writes content+placements, not midiClips");
        assert_eq!(raw["content"].as_array().unwrap().len(), 2);
        assert_eq!(raw["placements"].as_array().unwrap().len(), 2);
        assert_eq!(raw["lanes"].as_array().unwrap().len(), 2, "one default lane per distinct track (t1, t2)");
        let ev_ref = raw["content"][0]["eventsRef"].as_str().unwrap();
        assert!(ev_ref.starts_with("events/") && ev_ref.ends_with(".bin"));
        assert!(dir.join(ev_ref).exists());
        assert!(
            raw["content"][0].get("notes").is_none(),
            "notes NEVER inline in project.json"
        );
        // Empty clip has no chunk...
        assert!(raw["content"][1].get("eventsRef").is_none());
        // ...but BOTH content rows carry the watermark unconditionally (C-1;
        // the watermark lives on content now, round-2 §5's table).
        assert_eq!(raw["content"][0]["nextNoteId"], midi.clips[0].next_note_id);
        assert_eq!(raw["content"][1]["nextNoteId"], midi.clips[1].next_note_id);
        assert_eq!(raw["placements"][0]["contentId"], raw["content"][0]["id"]);
        assert_eq!(raw["placements"][0]["laneId"], raw["lanes"][0]["id"]);

        let v2 = load_from_project(&dir).unwrap().expect("v2 present");
        assert_eq!(v2.ppq, midi.ppq);
        assert_eq!(v2.tempo_events, midi.tempo_events);
        assert_eq!(v2.clips.len(), 2);
        assert_eq!(v2.clips[0].notes, midi.clips[0].notes);
        assert_eq!(v2.clips[0].next_note_id, midi.clips[0].next_note_id);
        assert_eq!(v2.clips[1].next_note_id, midi.clips[1].next_note_id);
        assert_eq!(v2.clips[0].timeline_start_ticks, 960);
        assert_eq!(v2.clips[0].content_id, midi.clips[0].content_id, "round-trips through the split");
        assert_eq!(v2.clips[0].lane_id, midi.clips[0].lane_id);
        assert!(v2.clips[1].notes.is_empty());
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn content_length_ticks_round_trips_when_present() {
        let parent = tmp_parent("content-length-present");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut c = clip("t1", some_notes(2));
        c.length_ticks = 7680;
        c.content_length_ticks = Some(3840);
        let midi = store_with(vec![c]);
        save_into_project(&dir, &midi).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["content"][0]["contentLengthTicks"], 3840);

        let v2 = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(v2.clips[0].content_length_ticks, Some(3840));
        assert_eq!(v2.clips[0].length_ticks, 7680);
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn content_length_ticks_absent_round_trips_as_none() {
        let parent = tmp_parent("content-length-absent");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let midi = store_with(vec![clip("t1", some_notes(1))]); // content_length_ticks: None
        save_into_project(&dir, &midi).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert!(
            raw["content"][0].get("contentLengthTicks").is_none(),
            "absent stays absent on disk — never writes null or the placement length"
        );

        let v2 = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(v2.clips[0].content_length_ticks, None);
        let _ = fs::remove_dir_all(&parent);
    }

    /// Step 4 / brief-pinned test: sparse ids (a deleted-note gap) round-trip
    /// exactly, and the watermark never resurrects the gap.
    #[test]
    fn note_ids_and_watermark_survive_save_load() {
        let parent = tmp_parent("ids-watermark");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let sparse_notes = vec![
            MidiNote { tick: 0, length_ticks: 100, key: 60, velocity: 100, channel: 0, note_id: crate::ids::NoteId(1) },
            MidiNote { tick: 200, length_ticks: 100, key: 64, velocity: 100, channel: 0, note_id: crate::ids::NoteId(3) },
        ];
        let mut c = clip("t1", sparse_notes);
        c.next_note_id = 4; // note 2 was deleted — the gap must not be reused
        let midi = store_with(vec![c]);

        save_into_project(&dir, &midi).unwrap();
        let mut loaded = load_from_project(&dir).unwrap().unwrap();
        let ids: Vec<u32> = loaded.clips[0].notes.iter().map(|n| n.note_id.0).collect();
        assert_eq!(ids, vec![1, 3], "sparse ids preserved exactly");
        assert_eq!(loaded.clips[0].next_note_id, 4, "watermark preserved exactly");
        assert_eq!(loaded.clips[0].mint_note_id().0, 4, "never resurrects the gap (2)");
        let _ = fs::remove_dir_all(&parent);
    }

    /// C-1's exact corruption path, pinned: emptying a clip's notes deletes
    /// its chunk (via GC) on the NEXT save, but the watermark — living in
    /// the JSON row unconditionally — must survive independently.
    #[test]
    fn watermark_survives_emptying_a_clips_notes() {
        let parent = tmp_parent("watermark-survives-empty");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let notes: Vec<MidiNote> = (1..=5u32)
            .map(|id| MidiNote {
                tick: id * 100, length_ticks: 50, key: 60, velocity: 100, channel: 0,
                note_id: crate::ids::NoteId(id),
            })
            .collect();
        let mut c = clip("t1", notes);
        c.next_note_id = 6;
        let mut midi = store_with(vec![c]);
        save_into_project(&dir, &midi).unwrap();

        // Empty the clip and save again: no chunk is written this time, and
        // the old one gets GC'd — the row must still carry nextNoteId: 6.
        midi.clips[0].notes.clear();
        save_into_project(&dir, &midi).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert!(raw["content"][0].get("eventsRef").is_none(), "emptied clip writes no chunk");
        assert_eq!(raw["content"][0]["nextNoteId"], 6, "watermark survives the emptied chunk");

        let mut loaded = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(loaded.clips[0].next_note_id, 6, "reload sees the row watermark");
        assert!(loaded.clips[0].notes.is_empty());
        assert_eq!(loaded.clips[0].mint_note_id().0, 6, "new note gets 6, never 1");
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn resave_gcs_stale_chunks_and_keeps_backup_untouched(){
        let parent = tmp_parent("gc");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut midi = store_with(vec![clip("t1", some_notes(4))]);
        save_into_project(&dir, &midi).unwrap();
        let bak1 = fs::read(dir.join(V1_BACKUP)).unwrap();
        midi.clips[0].notes = some_notes(6);
        save_into_project(&dir, &midi).unwrap();
        let chunks: Vec<_> = fs::read_dir(events_dir(&dir))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bin"))
            .collect();
        assert_eq!(chunks.len(), 1, "stale chunk GC'd");
        assert_eq!(fs::read(dir.join(V1_BACKUP)).unwrap(), bak1, "backup written once");
        let v2 = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(v2.clips[0].notes.len(), 6);
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn v1_project_loads_as_none_and_migration_defaults_match_plan() {
        let parent = tmp_parent("v1");
        let (_p, dir) = project::create(&parent, "Old", 48_000, 93.5).unwrap();
        assert!(load_from_project(&dir).unwrap().is_none());
        let d = v1_migration_defaults(93.5);
        assert_eq!(d.ppq, 960);
        assert_eq!(d.tempo_events, vec![TempoEvent { tick: 0, bpm: 93.5 }]);
        assert!(d.clips.is_empty());
        let _ = fs::remove_dir_all(&parent);
    }

    /// The architect-granted seam in audio/project.rs: a v1-path save (e.g.
    /// auto-save after recording) must PRESERVE the v2/v3 fields, and keep
    /// the tempoBpm-mirrors-tempoMap[0] invariant — v3's period, not v2's
    /// bpm (round-2 §3.3, see the `existing_schema_version >= 3` branch in
    /// `audio::project::save`).
    #[test]
    fn v1_typed_save_preserves_v3_fields() {
        let parent = tmp_parent("preserve");
        let (mut p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let midi = store_with(vec![clip("t1", some_notes(3))]);
        save_into_project(&dir, &midi).unwrap();

        // Now a v1-path save with a tempo change.
        p.tempo_bpm = 87.0;
        project::save(&dir, &p).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 3, "stays v3");
        assert_eq!(raw["placements"].as_array().unwrap().len(), 1);
        assert_eq!(raw["ppq"], 960);
        assert_eq!(raw["tempoBpm"], 87.0);
        assert_eq!(
            raw["tempoMap"][0]["periodStart"],
            crate::time::period_from_bpm(87.0),
            "v1-path tempo change updates the v3 period, not a stray bpm field"
        );
        // And the v3 loader still works after the v1-path save.
        let v3 = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(v3.clips[0].notes.len(), 3);
        assert!((v3.tempo_events[0].bpm - 87.0).abs() < 3e-7);
        // v1 reader accepts the v2/v3 file.
        let (loaded, _) = project::load(&dir).unwrap();
        assert_eq!(loaded.tempo_bpm, 87.0);
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn malicious_events_ref_is_rejected() {
        let parent = tmp_parent("evil");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let midi = store_with(vec![]);
        save_into_project(&dir, &midi).unwrap();
        // Inject a hostile ref into the v3-native content/placements/lanes
        // shape (a real v3 file after this save has empty arrays for all
        // three — the injected rows exercise the SAME traversal guard
        // `parse_clips_v3_native` shares with the legacy path via
        // `read_chunk`).
        let mut raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        raw["content"] = serde_json::json!([{
            "id": "content-x",
            "eventsRef": "events/../../etc/passwd.bin"
        }]);
        raw["placements"] = serde_json::json!([{
            "id": "x", "contentId": "content-x", "laneId": "lane-x",
            "name": "n", "timelineStartTicks": 0, "lengthTicks": 10
        }]);
        raw["lanes"] = serde_json::json!([{ "id": "lane-x", "trackId": "t" }]);
        fs::write(dir.join(PROJECT_FILE), serde_json::to_vec(&raw).unwrap()).unwrap();
        let v2 = load_from_project(&dir).unwrap().unwrap();
        assert!(v2.clips[0].notes.is_empty(), "traversal ref ignored");
        let _ = fs::remove_dir_all(&parent);
    }

    /// Reviewer finding 4: the `.bin.bad` rename path (C-2) had no test
    /// coverage. A chunk that decodes to structural corruption (not just a
    /// missing file) must be renamed out of GC's consideration — evidence
    /// preserved — while the project still loads with that clip empty.
    #[test]
    fn corrupt_chunk_is_renamed_to_bin_bad_and_clip_loads_empty() {
        let parent = tmp_parent("bin-bad");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let midi = store_with(vec![clip("t1", some_notes(3))]);
        save_into_project(&dir, &midi).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        let ev_ref = raw["content"][0]["eventsRef"].as_str().unwrap().to_string();
        let chunk_path = dir.join(&ev_ref);
        let bad_path = chunk_path.with_extension("bin.bad");
        assert!(chunk_path.is_file(), "chunk exists before corruption");
        assert!(!bad_path.exists());

        // Corrupt the chunk in place: valid magic/version/header so it parses
        // past the header, but claims 5 records with zero record bytes
        // following — a structural (truncation) failure in `decode_notes`,
        // not just an I/O error or a magic mismatch.
        let mut corrupt = Vec::new();
        corrupt.extend_from_slice(&events::AMEV_MAGIC.to_le_bytes());
        corrupt.extend_from_slice(&events::AMEV_VERSION.to_le_bytes());
        corrupt.extend_from_slice(&0u16.to_le_bytes()); // columnMask
        corrupt.extend_from_slice(&960u32.to_le_bytes()); // ppq
        corrupt.extend_from_slice(&5u32.to_le_bytes()); // count = 5, but NO record bytes follow
        fs::write(&chunk_path, &corrupt).unwrap();
        assert!(events::decode_notes(&corrupt).is_err(), "the corrupted bytes really are structurally invalid");

        let v2 = load_from_project(&dir).unwrap().unwrap();
        assert!(v2.clips[0].notes.is_empty(), "corrupt chunk degrades to an empty clip, not a failed open");
        assert!(!chunk_path.exists(), "the original .bin is gone (renamed away, not deleted)");
        assert!(bad_path.is_file(), "the evidence survives at .bin.bad");
        assert_eq!(
            fs::read(&bad_path).unwrap(),
            corrupt,
            "renamed file's bytes are exactly the corrupt original"
        );

        // GC on the NEXT save must not have anything to do with the .bad
        // file (it's not a `.bin`), and must not resurrect the clip's notes
        // from thin air — the watermark row is still authoritative (C-1),
        // notes stay empty.
        save_into_project(&dir, &midi).unwrap();
        assert!(bad_path.is_file(), "GC does not touch .bin.bad (only *.bin names it tracks as live)");
        let _ = fs::remove_dir_all(&parent);
    }

    /// Zone-P4 seam: `update_project_v2` upgrades v1 files (with backup),
    /// preserves every other writer's fields, and survives a later midi save
    /// AND a later typed v1-path save.
    #[test]
    fn update_project_v2_upgrades_backs_up_and_interleaves_with_other_writers() {
        let parent = tmp_parent("update-v2");
        let (mut p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        update_project_v2(&dir, |obj| {
            obj.insert("plugins".into(), serde_json::json!([{ "id": "i1", "uid": "lv2:x" }]));
            Ok(())
        })
        .unwrap();
        assert!(dir.join(V1_BACKUP).exists(), "v1 backup written on upgrade");
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 2);
        assert_eq!(raw["plugins"][0]["id"], "i1");
        assert_eq!(raw["name"], "Song", "v1 fields preserved");

        // A midi save on top keeps the plugins field...
        let midi = store_with(vec![clip("t1", some_notes(2))]);
        save_into_project(&dir, &midi).unwrap();
        // ...and a typed v1-path save keeps both.
        p.tempo_bpm = 99.0;
        project::save(&dir, &p).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["plugins"][0]["uid"], "lv2:x");
        assert_eq!(raw["placements"].as_array().unwrap().len(), 1);
        assert_eq!(raw["tempoBpm"], 99.0);

        // An apply error leaves the file untouched.
        let before = fs::read(dir.join(PROJECT_FILE)).unwrap();
        assert!(update_project_v2(&dir, |_| Err("nope".into())).is_err());
        assert_eq!(fs::read(dir.join(PROJECT_FILE)).unwrap(), before);
        let _ = fs::remove_dir_all(&parent);
    }

    /// Design §7: writing always emits v4. A midi-only save after a
    /// modulation persist must not stamp `schemaVersion` back to 3.
    #[test]
    fn midi_save_does_not_downgrade_v4_schema_version() {
        let parent = tmp_parent("no-downgrade-v4");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let doc = crate::modulation::ModulationDoc {
            curves: vec![crate::modulation::Curve {
                id: "cur-a".into(),
                name: String::new(),
                length_ticks: None,
                points: vec![],
            }],
            bindings: vec![],
            automation_clips: vec![],
        };
        crate::modulation::save_into_project(&dir, &doc).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 4);
        assert!(raw.get("modulation").is_some());

        save_into_project(&dir, &store_with(vec![])).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(
            raw["schemaVersion"], 4,
            "midi persist must write max(existing, 3), not hard-stamp 3"
        );
        assert!(raw.get("modulation").is_some(), "modulation{{}} survives a midi-only save");
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn launch_maps_round_trip_and_legacy_bindings_migrate() {
        let parent = tmp_parent("launch-maps");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut midi = store_with(vec![]);
        midi.launch_maps = vec![crate::midi::launch::LaunchMap {
            id: "drums".into(),
            name: "Drums".into(),
            bindings: vec![crate::midi::launch::LaunchBinding {
                id: "lb-1".into(),
                name: "Kick".into(),
                note: 36,
                channel: None,
                target: crate::midi::launch::LaunchTarget::Region {
                    start_ticks: 0,
                    length_ticks: 960,
                    track_ids: vec!["t1".into()],
                },
            }],
            drive_clip_ids: vec!["clip-1".into()],
            play_mode: crate::midi::launch::LaunchPlayMode::Gate,
        }];
        save_into_project(&dir, &midi).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        assert!(raw.get("launchMaps").is_some());
        assert!(raw.get("launchBindings").is_none(), "legacy key is not rewritten");
        let loaded = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(loaded.launch_maps, midi.launch_maps);

        let mut root: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        root.as_object_mut().unwrap().remove("launchMaps");
        root.as_object_mut().unwrap().insert(
            "launchBindings".into(),
            json!([{
                "id": "old",
                "name": "Scene 1",
                "note": 60,
                "channel": null,
                "target": { "kind": "clip", "clipId": "c-1" }
            }]),
        );
        root.as_object_mut()
            .unwrap()
            .insert("launchDriveClipIds".into(), json!(["c-1"]));
        fs::write(dir.join(PROJECT_FILE), serde_json::to_vec_pretty(&root).unwrap()).unwrap();
        let migrated = load_from_project(&dir).unwrap().unwrap();
        assert_eq!(migrated.launch_maps.len(), 1);
        assert_eq!(migrated.launch_maps[0].id, crate::midi::launch::DEFAULT_MAP_ID);
        assert_eq!(migrated.launch_maps[0].bindings[0].id, "old");
        assert_eq!(migrated.launch_maps[0].drive_clip_ids, vec!["c-1".to_string()]);
        let _ = fs::remove_dir_all(&parent);
    }
}
