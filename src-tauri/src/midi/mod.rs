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
//! * [`section_table`] — precomputed constant-tempo segments (round-2 §3.4).
//! * [`persist`]  — project.json v2/v3 fields + AMEV chunk save/load/migration.
//! * [`midifile`] — .mid import/export (midly).
//! * [`amt`]      — AMT infilling params/result/merge (job kind `amtInfill`).
//! * this file    — `#[tauri::command]` glue + the managed [`MidiState`].
//!
//! Frozen command names (registered in lib.rs): `set_tempo_map`,
//! `midi_add_clip`, `midi_set_notes`, `midi_get_clips`.
//!
//! Persistence model: midi edits auto-persist into the open project
//! (`project.json` v2/v3 + AMEV chunks) on every mutation; when the open
//! project changes (open_project happened since the last midi command), the
//! store lazily reloads from disk before serving. Every mutation triggers an
//! engine graph rebuild so MIDI is immediately audible.

pub mod amt;
pub mod events;
pub mod midifile;
pub mod persist;
pub mod playback;
pub mod schedule;
pub mod section_table;
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
pub use types::{MeterEvent, MidiClip, MidiNote, TempoEvent, TempoPeriodEvent, DEFAULT_PPQ};

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
    /// Persisted time signature (round-2 §3.3/O-10), v3+. Defaults to
    /// `[{tick:0,num:4,den:4}]` for stores that never loaded a v3 file.
    pub meter_events: Vec<MeterEvent>,
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
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
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
/// `events` (bpm-projected) stays for wire compatibility — `set_tempo_map`'s
/// SIGNATURE is frozen, so its body stays a wrapper (PHASE4-PLAN rule 3);
/// the additive fields below are v3's real, shipped section-table contract
/// (round-2 §3.6): the frontend consumes `section_table` instead of
/// re-deriving a bijection from `events`/`meter_map` itself (Task 9).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoMapState {
    pub ppq: u32,
    pub events: Vec<TempoEvent>,
    pub meter_map: Vec<MeterEvent>,
    pub period_events: Vec<TempoPeriodEvent>,
    pub section_table: Vec<SectionRow>,
    pub section_table_rule_version: u32,
}

/// Wire DTO mirroring [`section_table::Section`] field-for-field — the
/// internal struct stays internal (this crate's established pattern: wire
/// types are never the same struct as the internal representation).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionRow {
    pub start_tick: u64,
    pub start_sample: u64,
    pub start_beat: f64,
    pub start_bar: u32,
    pub period: u64,
}

impl From<&section_table::Section> for SectionRow {
    fn from(s: &section_table::Section) -> Self {
        Self {
            start_tick: s.start_tick,
            start_sample: s.start_sample,
            start_beat: s.start_beat,
            start_bar: s.start_bar,
            period: s.period,
        }
    }
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
                midi.meter_events = v2.meter_events;
                midi.clips = v2.clips;
            }
            Ok(None) => {
                if midi.loaded_dir.is_some() {
                    let d0 = persist::v1_migration_defaults(fallback_bpm);
                    midi.ppq = d0.ppq;
                    midi.tempo_events = d0.tempo_events;
                    midi.meter_events = d0.meter_events;
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

/// Finding 1: an auto-persist into `dir` is safe only when the store is
/// actually synced to it — i.e. `sync_midi_store` adopted this exact dir
/// last, not a stale one left over after it REFUSED to resync (dirty flag,
/// or a failed load; see [`sync_midi_store`]). Persisting when the two
/// disagree would write one project's in-memory clips into another
/// project's dir and GC that project's chunks. A plain equality check, but
/// pulled out as a pure fn so the guard is unit-testable without a full
/// session/State harness.
fn synced_to_dir(loaded_dir: &Option<PathBuf>, dir: &Option<PathBuf>) -> bool {
    loaded_dir == dir
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
            if synced_to_dir(&session.midi.loaded_dir, &dir) {
                match persist::save_into_project(d, &session.midi) {
                    Ok(()) => session.midi.dirty = false,
                    Err(e) => {
                        session.midi.dirty = true;
                        log::warn!("midi: persisting to {} failed: {e}", d.display());
                    }
                }
            } else {
                // sync_midi_store refused to adopt `dir` above — persisting
                // regardless would cross-contaminate project B's files with
                // project A's in-memory clips (see `synced_to_dir`'s doc).
                log::warn!(
                    "midi: skipping persist to {} — store not synced to it (loaded_dir={:?}); \
                     edit stays in memory only",
                    d.display(),
                    session.midi.loaded_dir
                );
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

/// Pure core of `set_tempo_map` (round-2 §3.6, Gate C/D frontend exit
/// condition): validates `events` against a nominal rate, builds the v3
/// section table from the just-set tempo events + the store's CURRENT
/// meter map (this command doesn't touch meter — no `set_meter_map`
/// command exists yet, round-2's own text: "meter UI can wait"), and
/// returns the full additive wire shape. Split out from the
/// `#[tauri::command]` wrapper so it's testable without a `tauri::State`
/// harness — the same pattern `sync_midi_store`/`assign_incoming_note_ids`
/// already use in this file.
pub(crate) fn build_tempo_map_state(
    ppq: u32,
    events: &[TempoEvent],
    meter_events: &[MeterEvent],
) -> Result<TempoMapState, String> {
    // Validate against a nominal rate; the map is rebuilt per engine rate.
    let tempo_map = TempoMap::new(ppq, events.to_vec(), 48_000)?;
    let meter_map = tempo::MeterMap::new(meter_events.to_vec())
        .unwrap_or_else(|_| tempo::MeterMap::default_map());
    let table = section_table::SectionTable::build(&tempo_map, &meter_map);
    Ok(TempoMapState {
        ppq,
        events: tempo_map.events(),
        meter_map: meter_map.events().to_vec(),
        period_events: tempo_map.period_events().to_vec(),
        section_table: table.sections().iter().map(SectionRow::from).collect(),
        section_table_rule_version: section_table::RULE_VERSION,
    })
}

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
        let result = build_tempo_map_state(ppq, &events, &s.meter_events)?;
        s.ppq = ppq;
        s.tempo_events = events.clone();
        Ok(result)
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
        let lane_id = crate::ids::LaneId::default_for_track(&track_id);
        let clip = MidiClip {
            id: uuid::Uuid::new_v4().to_string().into(),
            track_id: track_id.into(),
            name: name.unwrap_or_else(|| format!("MIDI Clip {}", n + 1)),
            timeline_start_ticks,
            length_ticks,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id,
            content_length_ticks: None,
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
/// keep-rule is unit-testable without a tauri State harness. `pub(crate)`
/// (Plan E Task 5) so `control::session::apply_raw`'s `Op::MidiSetNotes` arm
/// can call it directly — it's already pure (`&mut MidiClip` + payload, no
/// midi-lock machinery), so no move/adapt was needed, only a visibility
/// widening.
pub(crate) fn assign_incoming_note_ids(
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

/// Pure core of `midi_set_clip_bounds`: validates and applies a clip's new
/// placement (+ optional content length) against an owned clip slice —
/// unit-testable without a tauri State harness (mirrors
/// `assign_incoming_note_ids`'s split for `midi_set_notes`). `None` for
/// `content_length_ticks` explicitly CLEARS a previously-set content length
/// back to "same as placement" — the command always sends the caller's
/// current intent, never merges partial updates.
fn apply_clip_bounds(
    clips: &mut [MidiClip],
    clip_id: &crate::ids::ClipId,
    timeline_start_ticks: u64,
    length_ticks: u64,
    content_length_ticks: Option<u64>,
) -> Result<MidiClip, String> {
    if length_ticks == 0 {
        return Err("lengthTicks must be > 0".into());
    }
    if content_length_ticks == Some(0) {
        return Err("contentLengthTicks must be > 0 when present".into());
    }
    let clip = clips
        .iter_mut()
        .find(|c| &c.id == clip_id)
        .ok_or_else(|| format!("unknown MIDI clip: {clip_id}"))?;
    clip.timeline_start_ticks = timeline_start_ticks;
    clip.length_ticks = length_ticks;
    clip.content_length_ticks = content_length_ticks;
    Ok(clip.clone())
}

/// Move and/or resize a clip's placement (and optionally pin its content
/// length) — one additive command serving both the edge-drag gesture (sets
/// placement + content length atomically) and plain clip moves, which
/// closes a pre-existing hole: `midi.svelte.ts::moveClip()` was
/// frontend-only, so a dragged clip never reached the scheduler or the
/// project file (spec §5).
#[tauri::command]
pub fn midi_set_clip_bounds(
    clip_id: crate::ids::ClipId,
    timeline_start_ticks: u64,
    length_ticks: u64,
    content_length_ticks: Option<u64>,
    state: State<'_, MidiState>,
    audio: State<'_, AudioState>,
) -> Result<MidiClip, String> {
    with_synced_store(&audio, &state, true, move |s| {
        apply_clip_bounds(&mut s.clips, &clip_id, timeline_start_ticks, length_ticks, content_length_ticks)
    })
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

    let (session, _shared, _tables) = audio.control_parts();
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
                    c.lane_id = crate::ids::LaneId::default_for_track(id);
                    c.timeline_start_ticks += start;
                }
            }
            None => {
                for c in &mut clips {
                    let track = crate::control::ops::add_track(
                        &mut s.store,
                        Some(c.name.clone()),
                        Some("midi".into()),
                    )?;
                    c.lane_id = crate::ids::LaneId::default_for_track(track.id.as_str());
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

    // ---- build_tempo_map_state (Task 9: the shipped section table) --------

    #[test]
    fn build_tempo_map_state_carries_a_nonempty_section_table_at_the_shipped_rule_version() {
        let events = vec![TempoEvent { tick: 0, bpm: 120.0 }];
        let meter = vec![MeterEvent { tick: 0, num: 4, den: 4 }];
        let state = build_tempo_map_state(960, &events, &meter).unwrap();
        assert_eq!(state.ppq, 960);
        assert_eq!(state.events.len(), 1);
        assert!((state.events[0].bpm - 120.0).abs() < 1e-6);
        assert_eq!(state.meter_map, meter);
        assert_eq!(state.period_events.len(), 1);
        assert_eq!(state.period_events[0].period_start, crate::time::period_from_bpm(120.0));
        assert!(!state.section_table.is_empty(), "the shipped section table is never empty for a valid map");
        assert_eq!(state.section_table[0].start_tick, 0);
        assert_eq!(state.section_table[0].start_sample, 0);
        assert_eq!(state.section_table_rule_version, section_table::RULE_VERSION);
    }

    #[test]
    fn build_tempo_map_state_defaults_the_meter_map_when_the_store_has_a_malformed_one() {
        // Defensive: an empty meter_events slice (shouldn't happen — every
        // MidiStore constructor sets a default — but this function must not
        // panic if it ever does) falls back to [{0,4,4}].
        let events = vec![TempoEvent { tick: 0, bpm: 120.0 }];
        let state = build_tempo_map_state(960, &events, &[]).unwrap();
        assert_eq!(state.meter_map, vec![MeterEvent { tick: 0, num: 4, den: 4 }]);
    }

    // ---- midi_set_clip_bounds's core (apply_clip_bounds), pure and directly testable ----

    fn clip_for_bounds(id: &str, length_ticks: u64) -> MidiClip {
        MidiClip {
            id: id.into(), track_id: "t1".into(), name: "c".into(),
            timeline_start_ticks: 0, length_ticks, notes: Vec::new(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t1"),
            content_length_ticks: None,
        }
    }

    #[test]
    fn apply_clip_bounds_moves_and_resizes() {
        let mut clips = vec![clip_for_bounds("c1", 1920), clip_for_bounds("c2", 500)];
        let updated = apply_clip_bounds(&mut clips, &"c1".into(), 960, 3840, Some(1920)).unwrap();
        assert_eq!(updated.timeline_start_ticks, 960);
        assert_eq!(updated.length_ticks, 3840);
        assert_eq!(updated.content_length_ticks, Some(1920));
        // Written into the slice, not just the return value.
        assert_eq!(clips[0].timeline_start_ticks, 960);
        assert_eq!(clips[0].content_length_ticks, Some(1920));
        // The other clip is untouched.
        assert_eq!(clips[1].timeline_start_ticks, 0);
    }

    #[test]
    fn apply_clip_bounds_can_clear_content_length_back_to_absent() {
        let mut clips = vec![clip_for_bounds("c1", 1920)];
        clips[0].content_length_ticks = Some(480);
        apply_clip_bounds(&mut clips, &"c1".into(), 0, 1920, None).unwrap();
        assert_eq!(clips[0].content_length_ticks, None, "explicit None clears a previously-set content length");
    }

    #[test]
    fn apply_clip_bounds_rejects_zero_length_and_zero_content_length() {
        let mut clips = vec![clip_for_bounds("c1", 1920)];
        assert!(apply_clip_bounds(&mut clips, &"c1".into(), 0, 0, None).is_err());
        assert!(apply_clip_bounds(&mut clips, &"c1".into(), 0, 100, Some(0)).is_err());
        // Rejected calls must not have mutated the clip.
        assert_eq!(clips[0].length_ticks, 1920);
    }

    #[test]
    fn apply_clip_bounds_rejects_unknown_clip() {
        let mut clips = vec![clip_for_bounds("c1", 1920)];
        assert!(apply_clip_bounds(&mut clips, &"no-such-clip".into(), 0, 100, None).is_err());
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
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t1"),
            content_length_ticks: None,
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

    // ---- Finding 1: with_synced_store's persist guard ----

    #[test]
    fn synced_to_dir_only_true_when_loaded_dir_matches() {
        let a = Some(std::path::PathBuf::from("/project/a"));
        let b = Some(std::path::PathBuf::from("/project/b"));
        assert!(synced_to_dir(&a, &a), "same dir -> safe to persist");
        assert!(!synced_to_dir(&a, &b), "stale loaded_dir (still A) must not persist into B");
        assert!(!synced_to_dir(&None, &b), "never synced (None) must not persist into B");
        assert!(synced_to_dir(&None, &None), "in-memory-only store, no project open: trivially synced");
    }

    #[test]
    fn synced_to_dir_matches_the_failed_resync_scenarios() {
        // Exercise the exact two `sync_midi_store` failure paths from H-2/M-5
        // and confirm the persist guard correctly refuses both — this is the
        // end-to-end shape of finding 1's bug: sync refuses to adopt `dir`,
        // and the caller must not persist into it regardless.

        // Failed load: loaded_dir stays None, dir is Some -> refuse.
        let parent = std::env::temp_dir().join(format!(
            "aura-midi-mod-f1-load-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let mut midi = MidiStore::default();
        let dir = Some(parent.clone());
        sync_midi_store(&mut midi, &dir, 120.0);
        assert!(!synced_to_dir(&midi.loaded_dir, &dir), "failed load must not enable persist");
        let _ = std::fs::remove_dir_all(&parent);

        // Dirty refusal: loaded_dir stays at the OLD dir, dir is the NEW one
        // -> refuse (this is the exact cross-project-write scenario).
        let mut midi = MidiStore::default();
        midi.dirty = true;
        midi.loaded_dir = Some(std::path::PathBuf::from("/old/project"));
        let new_dir = Some(std::path::PathBuf::from("/new/project"));
        sync_midi_store(&mut midi, &new_dir, 120.0);
        assert!(
            !synced_to_dir(&midi.loaded_dir, &new_dir),
            "dirty refusal must not enable persist into the new project"
        );
    }
}
