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

/// v2/v3 midi fields loaded from a project. The name predates the v3 bump
/// (Task 6 adds `meter_events`, keeps the rest) — `tempo_events` stays
/// bpm-typed even for a v3 file: `TempoMap` (Task 3) is the actual period-
/// math authority, this struct is a control-plane cache of display values
/// quantized losslessly through `crate::time::{period_from_bpm,
/// bpm_from_period}`'s proven idempotence (Task 2).
#[derive(Debug, Clone)]
pub struct V3Data {
    pub ppq: u32,
    pub tempo_events: Vec<TempoEvent>,
    pub meter_events: Vec<MeterEvent>,
    pub clips: Vec<MidiClip>,
}

/// Persisted clip row (midi-clip.schema.json `$defs/persistedClip`).
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
}

/// Write the midi store's state into `<dir>/project.json` (upgrading it to
/// schemaVersion 3 — round-2 §3.3/§0.1 O-9) and the AMEV chunks under
/// `<dir>/events/`.
pub fn save_into_project(dir: &Path, midi: &MidiStore) -> Result<(), String> {
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
    let mut clip_rows = Vec::with_capacity(midi.clips.len());
    for clip in &midi.clips {
        let mut row = json!({
            "id": clip.id,
            "trackId": clip.track_id,
            "name": clip.name,
            "timelineStartTicks": clip.timeline_start_ticks,
            "lengthTicks": clip.length_ticks,
        });
        // C-1: the watermark is written to the JSON row UNCONDITIONALLY,
        // outside the "has notes" guard — an emptied clip writes no chunk
        // (and its old chunk gets GC'd below), so the row is the only place
        // the watermark can survive. The chunk copy (when written) keeps
        // chunks self-describing for readers that only ever see the chunk.
        row["nextNoteId"] = json!(clip.next_note_id);
        if !clip.notes.is_empty() {
            let chunk_name = format!("{}.bin", uuid::Uuid::new_v4());
            let chunk = events::encode_notes(midi.ppq, &clip.notes, clip.next_note_id);
            fs::write(events_dir.join(&chunk_name), chunk)
                .map_err(|e| format!("write events chunk: {e}"))?;
            row["eventsRef"] = json!(format!("{EVENTS_DIR}/{chunk_name}"));
            live_chunks.push(chunk_name);
        }
        clip_rows.push(row);
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
    obj.insert("schemaVersion".into(), json!(3));
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
    obj.insert("midiClips".into(), Value::Array(clip_rows));

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
/// PROJECT-ADOPTION SEAM (zone P4): this loader runs exactly when a project
/// is adopted (open_project eagerly via `notify_project_opened`, or the lazy
/// midi resync), so it also restores the project's persisted PLUGIN
/// instances and AUTOMATION lanes into the app-global registries. Both
/// hooks are inert until the app registers those globals — unit tests use
/// local registries/stores and never observe them.
pub fn load_from_project(dir: &Path) -> Result<Option<V3Data>, String> {
    let file = dir.join(PROJECT_FILE);
    let bytes =
        fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", file.display()))?;
    crate::plugins::state::adopt_open_project(dir);
    crate::plugins::automation::adopt_open_project(dir);
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
    let rows: Vec<PersistedClip> = match root.get("midiClips") {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| format!("midiClips: {e}"))?,
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
        clips.push(MidiClip {
            id: row.id.into(),
            track_id: row.track_id.into(),
            name: row.name,
            timeline_start_ticks: row.timeline_start_ticks,
            length_ticks: row.length_ticks.max(1),
            notes,
            next_note_id,
        });
    }
    Ok(Some(V3Data { ppq, tempo_events, meter_events, clips }))
}

/// The mechanical v1 migration: ppq 960, one-entry tempo map, default
/// meter map, no clips.
pub fn v1_migration_defaults(tempo_bpm: f64) -> V3Data {
    V3Data {
        ppq: DEFAULT_PPQ,
        meter_events: default_meter_events(),
        tempo_events: vec![TempoEvent { tick: 0, bpm: tempo_bpm }],
        clips: Vec::new(),
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
/// the next typed v1-path save. Other fields are preserved untouched, so
/// the midi, audio and P4 writers can interleave freely.
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
    obj.insert("schemaVersion".into(), json!(2));
    apply(obj)?;
    atomic_write_json(dir, &root)
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
            ppq: DEFAULT_PPQ,
            tempo_events: vec![
                TempoEvent { tick: 0, bpm: 100.0 },
                TempoEvent { tick: 3840, bpm: 140.0 },
            ],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips,
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
        assert_eq!(raw["midiClips"].as_array().unwrap().len(), 2);
        let ev_ref = raw["midiClips"][0]["eventsRef"].as_str().unwrap();
        assert!(ev_ref.starts_with("events/") && ev_ref.ends_with(".bin"));
        assert!(dir.join(ev_ref).exists());
        assert!(
            raw["midiClips"][0].get("notes").is_none(),
            "notes NEVER inline in project.json"
        );
        // Empty clip has no chunk...
        assert!(raw["midiClips"][1].get("eventsRef").is_none());
        // ...but BOTH rows carry the watermark unconditionally (C-1).
        assert_eq!(raw["midiClips"][0]["nextNoteId"], midi.clips[0].next_note_id);
        assert_eq!(raw["midiClips"][1]["nextNoteId"], midi.clips[1].next_note_id);

        let v2 = load_from_project(&dir).unwrap().expect("v2 present");
        assert_eq!(v2.ppq, midi.ppq);
        assert_eq!(v2.tempo_events, midi.tempo_events);
        assert_eq!(v2.clips.len(), 2);
        assert_eq!(v2.clips[0].notes, midi.clips[0].notes);
        assert_eq!(v2.clips[0].next_note_id, midi.clips[0].next_note_id);
        assert_eq!(v2.clips[1].next_note_id, midi.clips[1].next_note_id);
        assert_eq!(v2.clips[0].timeline_start_ticks, 960);
        assert!(v2.clips[1].notes.is_empty());
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
        assert!(raw["midiClips"][0].get("eventsRef").is_none(), "emptied clip writes no chunk");
        assert_eq!(raw["midiClips"][0]["nextNoteId"], 6, "watermark survives the emptied chunk");

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
        assert_eq!(raw["midiClips"].as_array().unwrap().len(), 1);
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
        // Inject a hostile ref.
        let mut raw: Value =
            serde_json::from_slice(&fs::read(dir.join(PROJECT_FILE)).unwrap()).unwrap();
        raw["midiClips"] = serde_json::json!([{
            "id": "x", "trackId": "t", "name": "n",
            "timelineStartTicks": 0, "lengthTicks": 10,
            "eventsRef": "events/../../etc/passwd.bin"
        }]);
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
        let ev_ref = raw["midiClips"][0]["eventsRef"].as_str().unwrap().to_string();
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
        assert_eq!(raw["midiClips"].as_array().unwrap().len(), 1);
        assert_eq!(raw["tempoBpm"], 99.0);

        // An apply error leaves the file untouched.
        let before = fs::read(dir.join(PROJECT_FILE)).unwrap();
        assert!(update_project_v2(&dir, |_| Err("nope".into())).is_err());
        assert_eq!(fs::read(dir.join(PROJECT_FILE)).unwrap(), before);
        let _ = fs::remove_dir_all(&parent);
    }
}
