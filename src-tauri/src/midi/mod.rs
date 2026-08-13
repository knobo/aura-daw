//! AURA MIDI module — OWNED BY THE MIDI/AMT AGENT (phase 2, zone C).
//!
//! Layout:
//!
//! * [`types`]    — tick-based wire types (`TempoEvent`, `MidiNote`,
//!                  `MidiClip`), mirrors `docs/ipc-schemas/midi-clip.schema.json`.
//! * [`tempo`]    — `TempoMap`: the tick<->sample bijection (debt D-02 paydown).
//! * [`events`]   — AMEV binary event chunks (`events/<id>.bin` in the project).
//! * [`synth`]    — built-in poly synth (`AudioProcessor`, `BlockNoteEvent`
//!                  contract shared with zone D's sampler).
//! * [`schedule`] — ticks -> absolute-sample note edges -> per-block events.
//! * [`playback`] — engine integration: control-side pre-render of midi
//!                  clips into `RtTrack`s for the RCU graph snapshot.
//! * [`persist`]  — project.json v2 fields + AMEV chunk save/load/migration.
//! * [`midifile`] — .mid import/export (midly).
//! * [`amt`]      — AMT infilling params/result/merge (job kind `amtInfill`).
//! * this file    — `#[tauri::command]` glue + the managed [`MidiState`].
//!
//! Frozen command names (registered in lib.rs): `set_tempo_map`,
//! `midi_add_clip`, `midi_set_notes`, `midi_get_clips`.
//!
//! Persistence model: midi edits auto-persist into the open project
//! (`project.json` v2 + AMEV chunks) on every mutation; when the open
//! project changes (open_project happened since the last midi command), the
//! store lazily reloads from disk before serving. Every mutation triggers an
//! engine graph rebuild so MIDI is immediately audible.

pub mod amt;
pub mod events;
pub mod midifile;
pub mod persist;
pub mod playback;
pub mod schedule;
pub mod synth;
pub mod tempo;
pub mod types;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::audio::engine::ControlMsg;
use crate::audio::AudioState;
use crate::control::Session;

pub use tempo::TempoMap;
pub use types::{MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};

// ---------------------------------------------------------------------------
// Shared state (constructed with Default and `.manage()`d by lib.rs)
// ---------------------------------------------------------------------------

/// In-memory MIDI store handle. lib.rs relies only on the type name and
/// `Default` construction; the session itself is wired in during setup (see
/// [`MidiState::shared`]) once `AudioState` has built it — same pattern as
/// `AudioState`'s own `OnceLock<EngineHandle>`.
pub struct MidiState {
    session: OnceLock<Arc<Mutex<Session>>>,
}

#[derive(Debug)]
pub struct MidiStore {
    pub ppq: u32,
    pub tempo_events: Vec<TempoEvent>,
    pub clips: Vec<MidiClip>,
    /// Project dir this store was last synced with (None = in-memory only).
    pub loaded_dir: Option<PathBuf>,
    /// Set when the last auto-persist ([`with_synced_store`]'s mutating
    /// path) failed to write to disk (M-5). While set, memory is the ONLY
    /// authoritative copy — [`sync_midi_store`] refuses to overwrite it from
    /// disk (which could otherwise silently discard the unpersisted edit on
    /// the next project-dir change). Cleared by the next successful save.
    pub dirty: bool,
}

impl Default for MidiStore {
    fn default() -> Self {
        Self {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            clips: Vec::new(),
            loaded_dir: None,
            dirty: false,
        }
    }
}

impl Default for MidiState {
    fn default() -> Self {
        Self { session: OnceLock::new() }
    }
}

impl MidiState {
    /// Wire the shared session (ARCHITECTURE §11 — store + midi behind one
    /// lock) into this managed state and register it with the
    /// engine-rebuild hook (playback integration) — lib.rs calls this
    /// exactly once during setup, right after `AudioState` builds the
    /// session.
    pub fn shared(&self, session: Arc<Mutex<Session>>) -> Arc<Mutex<Session>> {
        let _ = self.session.set(session.clone());
        playback::register_store(session.clone());
        session
    }

    fn session(&self) -> &Arc<Mutex<Session>> {
        self.session
            .get()
            .expect("MidiState::shared must run during setup before any command")
    }
}

/// Wire shape returned by `set_tempo_map` / embedded in project v2.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoMapState {
    pub ppq: u32,
    pub events: Vec<TempoEvent>,
}

// ---------------------------------------------------------------------------
// Command plumbing
// ---------------------------------------------------------------------------

/// Sync the midi store with the (possibly changed) open project dir:
/// * v2 project -> load its midi fields (AMEV chunks decoded),
/// * v1 project -> reset to the mechanical migration defaults, EXCEPT when
///   this session had no project yet (fresh in-memory edits are adopted into
///   the project and persisted on the next mutation),
/// * no project -> keep in-memory state.
fn sync_midi_store(midi: &mut MidiStore, dir: &Option<PathBuf>, fallback_bpm: f64) {
    if midi.loaded_dir == *dir {
        return;
    }
    if midi.dirty {
        // M-5: a previous auto-persist failed — memory holds the only copy
        // of that edit. Reloading now (from the old dir's disk state, or
        // adopting a newly-opened project) would silently destroy it.
        log::warn!(
            "midi: refusing to resync ({:?} -> {dir:?}) — memory has unpersisted edits from a failed save",
            midi.loaded_dir
        );
        return;
    }
    if let Some(d) = dir {
        match persist::load_from_project(d) {
            Ok(Some(v2)) => {
                midi.ppq = v2.ppq;
                midi.tempo_events = v2.tempo_events;
                midi.clips = v2.clips;
            }
            Ok(None) => {
                if midi.loaded_dir.is_some() {
                    let d0 = persist::v1_migration_defaults(fallback_bpm);
                    midi.ppq = d0.ppq;
                    midi.tempo_events = d0.tempo_events;
                    midi.clips = d0.clips;
                }
            }
            Err(e) => {
                log::warn!("midi: cannot read project midi state: {e}");
                // H-2: do NOT mark synced on a failed load — otherwise the
                // next mutation persists the OLD project's clips (and
                // watermarks) into the new dir.
                return;
            }
        }
    }
    midi.loaded_dir = dir.clone();
}

/// REQUESTED ARCHITECT SEAM: one-line eager-resync hook for
/// `audio::open_project` (frozen for zone C) — call as
/// `crate::midi::notify_project_opened(dir.clone(), tempo_bpm);` after the
/// store adopts the loaded project. Until that lands, every `midi_*` command
/// lazily resyncs, so only `get_project_state` served BEFORE the first midi
/// command can observe stale midi fields after an open.
pub fn notify_project_opened(dir: Option<PathBuf>, fallback_bpm: f64) {
    if let Some(session) = playback::registered_store() {
        sync_midi_store(&mut session.lock().midi, &dir, fallback_bpm);
    }
}

/// Run `f` against the midi store, synced with the open project (see
/// [`sync_midi_store`]). After a mutating `f`, persist into the project
/// (when one is open) and ask the engine for a graph rebuild (STRUCTURAL
/// change, §10.1). One session guard covers the whole block — store and
/// midi live behind the same lock.
fn with_synced_store<R>(
    audio: &AudioState,
    state: &MidiState,
    mutating: bool,
    f: impl FnOnce(&mut MidiStore) -> Result<R, String>,
) -> Result<R, String> {
    let mut session = state.session().lock();
    let (dir, bpm) = (session.store.project_dir.clone(), session.store.transport.tempo_bpm);

    sync_midi_store(&mut session.midi, &dir, bpm);

    let result = f(&mut session.midi)?;

    if mutating {
        if let Some(d) = &dir {
            match persist::save_into_project(d, &session.midi) {
                Ok(()) => session.midi.dirty = false,
                Err(e) => {
                    session.midi.dirty = true;
                    log::warn!("midi: persisting to {} failed: {e}", d.display());
                }
            }
        }
    }
    drop(session);
    if mutating {
        if let Some(engine) = audio.engine_handle() {
            engine.send(ControlMsg::Rebuild);
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Commands (names frozen; bodies/signatures evolve inside this module)
// ---------------------------------------------------------------------------

/// Replace the project tempo map. `events` must be sorted, start at tick 0,
/// bpm > 0 (validated via `TempoMap::new`). Batch-shaped by design (D-03).
/// Also keeps the legacy `transport.tempoBpm` == `tempoMap[0].bpm` invariant.
#[tauri::command]
pub fn set_tempo_map(
    ppq: Option<u32>,
    events: Vec<TempoEvent>,
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<TempoMapState, String> {
    let result = with_synced_store(&audio, &state, true, |s| {
        let ppq = ppq.unwrap_or(s.ppq);
        // Validate against a nominal rate; the map is rebuilt per engine rate.
        TempoMap::new(ppq, events.clone(), 48_000)?;
        s.ppq = ppq;
        s.tempo_events = events.clone();
        Ok(TempoMapState { ppq, events: events.clone() })
    })?;
    // Legacy single-tempo field follows the map (project-v2 invariant).
    let (session, _, _) = audio.control_parts();
    session.lock().store.transport.tempo_bpm = result.events[0].bpm;
    Ok(result)
}

/// Create an empty MIDI clip placement on a track (ticks, never samples).
/// The track must exist and be `kind: "midi"`.
#[tauri::command]
pub fn midi_add_clip(
    track_id: String,
    name: Option<String>,
    timeline_start_ticks: u64,
    length_ticks: u64,
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<MidiClip, String> {
    if length_ticks == 0 {
        return Err("lengthTicks must be > 0".into());
    }
    {
        // Validate against the control-plane store (ARCHITECTURE §11).
        let (session, _, _) = audio.control_parts();
        let session = session.lock();
        let track = session
            .store
            .tracks
            .iter()
            .find(|t| t.id == track_id)
            .ok_or_else(|| format!("unknown track: {track_id}"))?;
        if track.kind != "midi" {
            return Err(format!(
                "track {track_id} is kind \"{}\" (midi clips need a midi track)",
                track.kind
            ));
        }
    }
    with_synced_store(&audio, &state, true, move |s| {
        let n = s.clips.len();
        let clip = MidiClip {
            id: uuid::Uuid::new_v4().to_string().into(),
            track_id: track_id.into(),
            name: name.unwrap_or_else(|| format!("MIDI Clip {}", n + 1)),
            timeline_start_ticks,
            length_ticks,
            notes: Vec::new(),
            next_note_id: 1,
        };
        s.clips.push(clip.clone());
        Ok(clip)
    })
}

/// Replace the full note list of a clip (batch-shaped: one invoke per edit
/// gesture, never one invoke per note — D-03; also the application point for
/// AMT infill results, see [`amt::merge_infill`]). Notes are validated and
/// sorted by (tick, key); the returned clip is the undo-friendly full value.
#[tauri::command]
pub fn midi_set_notes(
    clip_id: String,
    notes: Vec<MidiNote>,
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<MidiClip, String> {
    for n in &notes {
        n.validate()?;
    }
    with_synced_store(&audio, &state, true, move |s| {
        let clip = s
            .clips
            .iter_mut()
            .find(|c| c.id == clip_id)
            .ok_or_else(|| format!("unknown MIDI clip: {clip_id}"))?;
        // Everything is computed on LOCALS first (assign_incoming_note_ids
        // is pure); `clip.notes`/`clip.next_note_id` are assigned only as
        // the last statements, so no partial state is observable if this
        // closure returns early.
        let (local_notes, next_watermark) =
            assign_incoming_note_ids(&clip.notes, clip.next_note_id, notes);
        clip.notes = local_notes;
        clip.next_note_id = next_watermark;
        Ok(clip.clone())
    })
}

/// Pure core of `midi_set_notes`'s identity keep-rule (round-2 §2.1,
/// hardened H-1/M-9): an incoming id is KEPT iff it is non-zero, appears
/// exactly once in the payload, AND is the id of a note CURRENTLY in
/// `existing` — a stale/duplicated/foreign id is always minted fresh, never
/// resurrects a deleted note's identity. Returns the sorted, id-resolved
/// note list and the advanced watermark; extracted as a pure function so the
/// keep-rule is unit-testable without a tauri State harness.
fn assign_incoming_note_ids(
    existing: &[MidiNote],
    next_note_id: u32,
    mut incoming: Vec<MidiNote>,
) -> (Vec<MidiNote>, u32) {
    incoming.sort_by_key(|n| (n.tick, n.key));

    let mut incoming_id_counts: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for n in &incoming {
        if n.note_id.0 != 0 {
            *incoming_id_counts.entry(n.note_id.0).or_insert(0) += 1;
        }
    }
    let existing_ids: std::collections::HashSet<u32> =
        existing.iter().map(|n| n.note_id.0).filter(|&id| id != 0).collect();

    let mut next_watermark = next_note_id;
    let mut local_notes = Vec::with_capacity(incoming.len());
    for mut n in incoming {
        let keep = n.note_id.0 != 0
            && incoming_id_counts.get(&n.note_id.0).copied().unwrap_or(0) == 1
            && existing_ids.contains(&n.note_id.0);
        if !keep {
            n.note_id = crate::ids::NoteId(next_watermark);
            next_watermark += 1;
        }
        local_notes.push(n);
    }
    (local_notes, next_watermark)
}

#[tauri::command]
pub fn midi_get_clips(
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<Vec<MidiClip>, String> {
    with_synced_store(&audio, &state, false, |s| Ok(s.clips.clone()))
}

/// Import a standard MIDI file (.mid) onto the tick timeline (architect
/// merge over zone C's `midifile` API). Each note-carrying SMF track becomes
/// one clip; `track_id` places every clip on that (midi) track, otherwise a
/// midi track is auto-created per clip. `at_ticks` offsets the placement
/// (default 0). The file's tempo map replaces the project's ONLY when the
/// file carried explicit tempo events. Returns the created clips.
#[tauri::command]
pub fn midi_import_file(
    path: String,
    track_id: Option<String>,
    at_ticks: Option<u64>,
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<Vec<MidiClip>, String> {
    let p = std::path::Path::new(&path);
    if !p.is_absolute() {
        return Err(format!("path must be absolute: {path}"));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("read {path}: {e}"))?;

    let (session, _shared, params) = audio.control_parts();
    let ppq = {
        let mut s = session.lock();
        let dir = s.store.project_dir.clone();
        let bpm = s.store.transport.tempo_bpm;
        sync_midi_store(&mut s.midi, &dir, bpm);
        s.midi.ppq
    };
    let imported = midifile::import_smf(&bytes, ppq)?;
    if imported.clips.is_empty() {
        return Err("MIDI file contains no note-carrying tracks".into());
    }

    // Resolve/assign target tracks.
    let start = at_ticks.unwrap_or(0);
    let mut clips = imported.clips.clone();
    {
        let mut s = session.lock();
        match &track_id {
            Some(id) => {
                let track = s
                    .store
                    .tracks
                    .iter()
                    .find(|t| &t.id == id)
                    .ok_or_else(|| format!("unknown track: {id}"))?;
                if track.kind != "midi" {
                    return Err(format!(
                        "track {id} is kind \"{}\" (midi clips need a midi track)",
                        track.kind
                    ));
                }
                for c in &mut clips {
                    c.track_id = id.clone().into();
                    c.timeline_start_ticks += start;
                }
            }
            None => {
                for c in &mut clips {
                    let track = crate::control::ops::add_track(
                        &mut s.store,
                        &params,
                        Some(c.name.clone()),
                        Some("midi".into()),
                    )?;
                    c.track_id = track.id;
                    c.timeline_start_ticks += start;
                }
            }
        }
    }

    let adopt_tempo = imported.explicit_tempo.then(|| imported.tempo_events.clone());
    let first_bpm = imported.tempo_events.first().map(|e| e.bpm);
    let result_clips = clips.clone();
    let result = with_synced_store(&audio, &state, true, move |s| {
        if let Some(events) = adopt_tempo {
            s.tempo_events = events;
        }
        s.clips.extend(clips);
        Ok(result_clips)
    })?;
    // Legacy single-tempo invariant (see set_tempo_map).
    if imported.explicit_tempo {
        if let Some(bpm0) = first_bpm {
            session.lock().store.transport.tempo_bpm = bpm0;
        }
    }
    Ok(result)
}

/// Export the project's MIDI (tempo map + clips) as a format-1 .mid file at
/// `path`. `clip_ids` restricts the export (default: every clip). Returns
/// the written path.
#[tauri::command]
pub fn midi_export_file(
    path: String,
    clip_ids: Option<Vec<String>>,
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_absolute() {
        return Err(format!("path must be absolute: {path}"));
    }
    let bytes = with_synced_store(&audio, &state, false, |s| {
        let clips: Vec<MidiClip> = match &clip_ids {
            None => s.clips.clone(),
            Some(ids) => ids
                .iter()
                .map(|id| {
                    s.clips
                        .iter()
                        .find(|c| &c.id == id)
                        .cloned()
                        .ok_or_else(|| format!("unknown MIDI clip: {id}"))
                })
                .collect::<Result<_, _>>()?,
        };
        if clips.is_empty() {
            return Err("no MIDI clips to export".into());
        }
        midifile::export_smf(s.ppq, &s.tempo_events, &clips)
    })?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, &bytes).map_err(|e| format!("write {path}: {e}"))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::NoteId;

    fn note(tick: u32, key: u8, id: u32) -> MidiNote {
        MidiNote { tick, length_ticks: 10, key, velocity: 100, channel: 0, note_id: NoteId(id) }
    }

    // ---- midi_set_notes's keep-rule (H-1/M-9), pure and directly testable ----

    #[test]
    fn assign_incoming_note_ids_keeps_real_present_unique_ids() {
        let existing = vec![note(0, 60, 1), note(100, 62, 2)];
        // A payload that edits note 1 in place (kept) and adds a brand-new
        // note (id 0, always minted).
        let incoming = vec![note(0, 60, 1), note(200, 64, 0)];
        let (out, watermark) = assign_incoming_note_ids(&existing, 3, incoming);
        assert_eq!(out[0].note_id.0, 1, "present real id kept");
        assert_eq!(out[1].note_id.0, 3, "zero id always minted");
        assert_eq!(watermark, 4);
    }

    #[test]
    fn assign_incoming_note_ids_mints_stale_and_foreign_and_duplicate_ids() {
        let existing = vec![note(0, 60, 1)]; // note 2 was deleted since the client last synced
        // Stale id (2, deleted), duplicate id (5 appears twice) — neither survives.
        let incoming = vec![note(0, 60, 2), note(100, 62, 5), note(200, 64, 5)];
        let (out, watermark) = assign_incoming_note_ids(&existing, 10, incoming);
        assert_eq!(out[0].note_id.0, 10, "stale id (deleted note) never resurrected [H-1]");
        assert_eq!(out[1].note_id.0, 11, "duplicate-in-payload id minted fresh");
        assert_eq!(out[2].note_id.0, 12, "duplicate-in-payload id minted fresh");
        assert_eq!(watermark, 13);
    }

    // ---- sync_midi_store (H-2, M-5) — plain fns, no tauri State needed ----

    #[test]
    fn sync_midi_store_failed_load_does_not_mark_synced() {
        // A directory that looks like a project dir but has no readable
        // project.json — load_from_project errors.
        let parent = std::env::temp_dir()
            .join(format!("aura-midi-mod-h2-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();

        let mut midi = MidiStore::default();
        let dir = Some(parent.clone());
        sync_midi_store(&mut midi, &dir, 120.0);
        assert_eq!(midi.loaded_dir, None, "H-2: a failed load must not mark the store synced");

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn sync_midi_store_refuses_to_resync_while_dirty() {
        let mut midi = MidiStore::default();
        midi.clips.push(crate::midi::MidiClip {
            id: "c1".into(),
            track_id: "t1".into(),
            name: "unsaved".into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: Vec::new(),
            next_note_id: 1,
        });
        midi.dirty = true;
        midi.loaded_dir = Some(std::path::PathBuf::from("/old/project"));

        let new_dir = Some(std::path::PathBuf::from("/new/project"));
        sync_midi_store(&mut midi, &new_dir, 120.0);

        assert_eq!(midi.clips.len(), 1, "M-5: dirty memory is not silently discarded");
        assert_eq!(
            midi.loaded_dir,
            Some(std::path::PathBuf::from("/old/project")),
            "refuses to adopt the new dir while dirty"
        );
    }
}
