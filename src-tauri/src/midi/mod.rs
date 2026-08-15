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
//! `midi_add_clip`, `midi_set_notes`, `midi_set_clip_bounds`,
//! `midi_get_clips`, `midi_import_file`, `midi_export_file`.
//!
//! Persistence model: midi edits auto-persist into the open project
//! (`project.json` v2/v3 + AMEV chunks) on every mutation. Task 6 (Plan E):
//! the store no longer lazily resyncs from disk on a command call — every
//! adopt-chain run (midi + plugins + automation) happens exactly at a
//! `ControlPlane` epoch boundary (open/create/save-as/ensure-project), so
//! `midi.loaded_dir` is always current by the time any command runs.
//!
//! Task 7 (Plan E): every mutating command body is now a thin wrapper —
//! build `Op`s, `ControlPlane::commit` them (`_core` fns below, split out
//! from the `#[tauri::command]` glue so they're testable against a bare
//! `&ControlPlane`, no `tauri::State` harness needed). `commit` owns the
//! engine rebuild, the `PersistEffect` (auto-persist replaces this file's
//! old manual `persist::save_into_project` calls), and the
//! `project://changed` emit — one of each per command invoke. The old
//! `with_synced_store` mutating helper (direct `&mut MidiStore` access
//! outside the transaction channel) is gone; [`read_midi`] is the only
//! surviving store accessor here, for the two commands that were already
//! pure reads (`midi_get_clips`, `midi_export_file`).

pub mod amt;
pub mod capture;
pub mod events;
pub mod midifile;
pub mod persist;
pub mod playback;
pub mod schedule;
pub mod section_table;
pub mod synth;
pub mod tempo;
pub mod types;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;

#[cfg(test)]
use crate::audio::AudioState;
use crate::control::op::{ObjectRef, Op, PropPath, TxMeta};
use crate::control::{ops as control_ops, ControlPlane, Session};

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
    /// Set when the last auto-persist (Plan E Task 7: the `PersistEffect`
    /// executed by `ControlPlane::commit`/`execute_persist`) failed to write
    /// to disk (M-5). While set, memory is the ONLY authoritative copy —
    /// [`adopt_midi_from_dir`] refuses to overwrite it from disk (which
    /// could otherwise silently discard the unpersisted edit on the next
    /// epoch boundary). Cleared by the next successful save.
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
    /// session. Also registers the same session for `plugins::automation`'s
    /// project-adoption seam (Plan E Task 10 — mirrors `playback`'s
    /// registration right above; both are app-setup-only, never touched by
    /// unit tests, which construct `Session`s directly).
    pub fn shared(&self, session: Arc<Mutex<Session>>) -> Arc<Mutex<Session>> {
        let _ = self.session.set(session.clone());
        playback::register_store(session.clone());
        crate::plugins::automation::register_session(session.clone());
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

/// Adopt `dir`'s midi/v2+ fields into `midi`, replacing memory wholesale —
/// the ONE place midi state resyncs from disk (Task 6: called ONLY from
/// `ControlPlane`'s sanctioned epoch functions — `open_project_epoch` is the
/// only caller that actually needs the disk read; `create_project_epoch`/
/// `save_project_as_epoch` mark `loaded_dir` directly, they don't read).
/// Never called from a read path anymore (that was the "plugin-teardown-
/// inside-a-read horror", round-2 inventory row 9 — `load_from_project`
/// used to cascade into `plugins::state::adopt_open_project`, tearing down
/// live host state as a side effect of what looked like a pure `get_clips`).
///
/// Same M-5 dirty-guard semantics as the retired `sync_midi_store` this
/// replaces: a previous auto-persist failure leaves memory as the ONLY
/// authoritative copy, so this refuses to overwrite it from disk.
pub(crate) fn adopt_midi_from_dir(midi: &mut MidiStore, dir: &Path, fallback_bpm: f64) {
    if midi.loaded_dir.as_deref() == Some(dir) {
        return;
    }
    if midi.dirty {
        log::warn!(
            "midi: refusing to resync ({:?} -> {dir:?}) — memory has unpersisted edits from a failed save",
            midi.loaded_dir
        );
        return;
    }
    match persist::load_from_project(dir) {
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
    midi.loaded_dir = Some(dir.to_path_buf());
}

/// Run `f` against the midi store as a PURE READ: lock, read, drop — no
/// resync, no persist, no engine rebuild. Task 6: the only midi read paths
/// left (`midi_get_clips`, `midi_export_file`) go through this; staleness
/// is impossible because eager epoch adoption keeps `loaded_dir` current at
/// every document swap, so there is nothing left for a read to "catch up".
fn read_midi<R>(
    state: &MidiState,
    f: impl FnOnce(&MidiStore) -> Result<R, String>,
) -> Result<R, String> {
    let session = state.session().lock();
    f(&session.midi)
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
/// Also keeps the legacy `transport.tempoBpm` in sync with the new map
/// (owned by `Op::TempoSet`'s apply arm now — no separate writeback here,
/// Plan E Task 7). Honest correction (fix round 1): this is NOT an exact
/// `transport.tempoBpm == tempoMap[0].bpm` invariant — the apply arm mirrors
/// the RAW `events[0].bpm` this fn passes it (the caller's literal input),
/// while the `TempoMapState` returned to the caller (`built`, below) carries
/// `build_tempo_map_state`'s PERIOD-QUANTIZED bpm
/// (`bpm_from_period(period_from_bpm(bpm))`) — the two can differ by up to
/// the quantization's own error bound (<1e-6 bpm, pinned in
/// `time::tests::bpm_quantizes_to_an_integer_period_and_back_within_spec_error`).
/// This is not a regression: it's the SAME raw-bpm behavior
/// `midi_import_file_core` already relies on (its `Op::TempoSet` also
/// carries the file's literal parsed bpm, never a quantized round-trip) —
/// this fn now unifies with that rather than diverging from it.
///
/// Split from the `#[tauri::command]` wrapper (same pattern
/// `build_tempo_map_state`/`assign_incoming_note_ids` already use in this
/// file) so it's testable against a bare `&ControlPlane`, no `tauri::State`
/// harness needed.
pub(crate) fn set_tempo_map_core(
    control: &ControlPlane,
    ppq: Option<u32>,
    events: Vec<TempoEvent>,
) -> Result<TempoMapState, String> {
    let mut result: Option<TempoMapState> = None;
    control.commit(TxMeta::user("set tempo map"), |tx| {
        let resolved_ppq = ppq.unwrap_or(tx.midi().ppq);
        // meter: the command's signature carries no meter field, so the
        // current store's meter map travels through unchanged (read via
        // `tx.midi()`, inside the same lock as the write below — no separate
        // pre-tx read that could race a concurrent meter change).
        let meter = tx.midi().meter_events.clone();
        let built = build_tempo_map_state(resolved_ppq, &events, &meter)?;
        tx.apply(Op::TempoSet { ppq: resolved_ppq, events: events.clone(), meter })?;
        result = Some(built);
        Ok(())
    })?;
    Ok(result.expect("commit only returns Ok after the closure ran to completion"))
}

#[tauri::command]
pub fn set_tempo_map(
    ppq: Option<u32>,
    events: Vec<TempoEvent>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<TempoMapState, String> {
    set_tempo_map_core(&control, ppq, events)
}

/// Core of `midi_add_clip` (see [`set_tempo_map_core`]'s doc for the split
/// rationale). Validates the target track (must exist, `kind: "midi"`) via
/// `tx.store()`, then commits `Op::MidiClipAdd`.
pub(crate) fn midi_add_clip_core(
    control: &ControlPlane,
    track_id: String,
    name: Option<String>,
    timeline_start_ticks: u64,
    length_ticks: u64,
) -> Result<MidiClip, String> {
    if length_ticks == 0 {
        return Err("lengthTicks must be > 0".into());
    }
    let mut result: Option<MidiClip> = None;
    control.commit(TxMeta::user("add midi clip"), |tx| {
        {
            let track = tx
                .store()
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
        let n = tx.midi().clips.len();
        let lane_id = crate::ids::LaneId::default_for_track(&track_id);
        let clip = MidiClip {
            id: uuid::Uuid::new_v4().to_string().into(),
            track_id: track_id.clone().into(),
            name: name.clone().unwrap_or_else(|| format!("MIDI Clip {}", n + 1)),
            timeline_start_ticks,
            length_ticks,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id,
            content_length_ticks: None,
        };
        tx.apply(Op::MidiClipAdd { clip: clip.clone(), index: n })?;
        result = Some(clip);
        Ok(())
    })?;
    Ok(result.expect("commit only returns Ok after the closure ran to completion"))
}

/// Create an empty MIDI clip placement on a track (ticks, never samples).
/// The track must exist and be `kind: "midi"`.
#[tauri::command]
pub fn midi_add_clip(
    track_id: String,
    name: Option<String>,
    timeline_start_ticks: u64,
    length_ticks: u64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<MidiClip, String> {
    midi_add_clip_core(&control, track_id, name, timeline_start_ticks, length_ticks)
}

/// Core of `midi_set_notes`: commits `Op::MidiSetNotes` — the server-side
/// diff (the keep-rule, [`assign_incoming_note_ids`]) lives in
/// `apply_raw`'s arm, not here; this just reads the post-apply clip back out
/// through `tx.midi()` for the return value. `meta.label` stays the fixed
/// string `"set midi notes"` (not per-call unique) so Task 17's 350ms
/// fallback merge can coalesce same-clip note edits by (kind, object,
/// actor, label).
pub(crate) fn midi_set_notes_core(
    control: &ControlPlane,
    clip_id: String,
    notes: Vec<MidiNote>,
) -> Result<MidiClip, String> {
    for n in &notes {
        n.validate()?;
    }
    let mut result: Option<MidiClip> = None;
    control.commit(TxMeta::user("set midi notes"), |tx| {
        tx.apply(Op::MidiSetNotes { clip: clip_id.clone().into(), notes: notes.clone() })?;
        result = Some(
            tx.midi()
                .clips
                .iter()
                .find(|c| c.id == clip_id)
                .cloned()
                .expect("MidiSetNotes just applied against this clip id"),
        );
        Ok(())
    })?;
    Ok(result.expect("commit only returns Ok after the closure ran to completion"))
}

/// Replace the full note list of a clip (batch-shaped: one invoke per edit
/// gesture, never one invoke per note — D-03; also the application point for
/// AMT infill results, see [`amt::merge_infill`]). Notes are validated and
/// sorted by (tick, key); the returned clip is the undo-friendly full value.
#[tauri::command]
pub fn midi_set_notes(
    clip_id: String,
    notes: Vec<MidiNote>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<MidiClip, String> {
    midi_set_notes_core(&control, clip_id, notes)
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

/// Core of `midi_set_clip_bounds`: up to three `Op::Set{MidiClip, ...}` in
/// ONE commit (`TimelineStartTicks`, `LengthTicks`, `ContentLengthTicks` —
/// always all three: `content_length_ticks: None` is a meaningful CLEAR, not
/// "leave unchanged", so it always needs writing too, same semantics
/// `apply_clip_bounds` had). BINDING RULING (Task 5's review): the
/// `Op::Set{MidiClip, ContentLengthTicks}` apply arm does NOT reject 0 (it
/// just clamps `LengthTicks`, never `ContentLengthTicks`) — so this wrapper
/// pre-checks `content_length_ticks == Some(0)` itself, before any op is
/// applied, to keep that reject without touching the arm's landed semantics.
pub(crate) fn midi_set_clip_bounds_core(
    control: &ControlPlane,
    clip_id: crate::ids::ClipId,
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
    let object = ObjectRef::MidiClip(clip_id.clone());
    let mut result: Option<MidiClip> = None;
    control.commit(TxMeta::user("set midi clip bounds"), |tx| {
        tx.apply(Op::Set {
            object: object.clone(),
            path: PropPath::TimelineStartTicks,
            from: serde_json::Value::Null,
            to: serde_json::json!(timeline_start_ticks),
        })?;
        tx.apply(Op::Set {
            object: object.clone(),
            path: PropPath::LengthTicks,
            from: serde_json::Value::Null,
            to: serde_json::json!(length_ticks),
        })?;
        tx.apply(Op::Set {
            object: object.clone(),
            path: PropPath::ContentLengthTicks,
            from: serde_json::Value::Null,
            to: serde_json::json!(content_length_ticks),
        })?;
        result = Some(
            tx.midi()
                .clips
                .iter()
                .find(|c| c.id == clip_id)
                .cloned()
                .expect("the three Sets above just applied against this clip id"),
        );
        Ok(())
    })?;
    Ok(result.expect("commit only returns Ok after the closure ran to completion"))
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
    control: State<'_, Arc<ControlPlane>>,
) -> Result<MidiClip, String> {
    midi_set_clip_bounds_core(&control, clip_id, timeline_start_ticks, length_ticks, content_length_ticks)
}

/// Core of `midi_rename_clip`: ONE `Op::Set{MidiClip, Name}`. The trim and
/// the empty-name reject live in `write_midi_prop` (the write side is the
/// validation authority, like `LengthTicks`'s clamp), so the recorded op —
/// and therefore the inverse — carries the trimmed value, never the
/// caller's raw string. `meta.label` is the fixed string `"rename midi
/// clip"` so successive renames of the same clip coalesce into one history
/// entry instead of one per keystroke-commit.
pub(crate) fn midi_rename_clip_core(
    control: &ControlPlane,
    clip_id: crate::ids::ClipId,
    name: String,
) -> Result<MidiClip, String> {
    let mut result: Option<MidiClip> = None;
    control.commit(TxMeta::user("rename midi clip"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::MidiClip(clip_id.clone()),
            path: PropPath::Name,
            from: serde_json::Value::Null,
            to: serde_json::json!(name),
        })?;
        result = Some(
            tx.midi()
                .clips
                .iter()
                .find(|c| c.id == clip_id)
                .cloned()
                .expect("the Set above just applied against this clip id"),
        );
        Ok(())
    })?;
    Ok(result.expect("commit only returns Ok after the closure ran to completion"))
}

/// Rename a MIDI clip. Empty (or whitespace-only) names are rejected.
#[tauri::command]
pub fn midi_rename_clip(
    clip_id: crate::ids::ClipId,
    name: String,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<MidiClip, String> {
    midi_rename_clip_core(&control, clip_id, name)
}

/// Core of `midi_remove_clip`: ONE `Op::MidiClipRemove`, the structural
/// analogue of `midi_rename_clip_core`'s `Op::Set`. `apply_raw`'s
/// `Op::MidiClipRemove` arm finds the clip by id (store truth wins) and
/// computes the inverse `Op::MidiClipAdd`, so undo restores the clip
/// byte-identically — same free-inverse shape the audio-side `remove_clip`
/// gets from `Op::ClipRemove`.
pub(crate) fn midi_remove_clip_core(
    control: &ControlPlane,
    clip_id: crate::ids::ClipId,
) -> Result<(), String> {
    control.commit(TxMeta::user("remove midi clip"), |tx| {
        let clip = tx
            .midi()
            .clips
            .iter()
            .find(|c| c.id == clip_id)
            .cloned()
            .ok_or_else(|| format!("unknown MIDI clip: {clip_id}"))?;
        tx.apply(Op::MidiClipRemove { clip, index: 0 })
    })?;
    // A clip-scoped MIDI-out route (if any) has nothing in the document
    // model retiring it — same carve-out as `remove_track`'s
    // `clear_midi_routing_for`. `midi_out::run_thread`'s self-heal would
    // also catch this within ~250 ms, but clearing it eagerly here (now
    // that this explicit delete path exists) means a routed clip's port
    // release isn't left waiting on that window.
    control.clear_midi_route_for_clip(clip_id.as_str());
    Ok(())
}

/// Remove a MIDI clip from its track.
#[tauri::command]
pub fn midi_remove_clip(
    clip_id: crate::ids::ClipId,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<(), String> {
    midi_remove_clip_core(&control, clip_id)
}

#[tauri::command]
pub fn midi_get_clips(state: State<'_, MidiState>) -> Result<Vec<MidiClip>, String> {
    read_midi(&state, |s| Ok(s.clips.clone()))
}

/// Core of `midi_import_file` — the §4.4 "prepare outside" exemplar: the
/// whole file read + SMF parse + tick rescale happens with NO session lock
/// held (only a single, quick, released-before-parsing lock to snapshot the
/// project's current `ppq`, same as pre-Task-7). Then ONE
/// `control.commit(…)`: `ops::add_track_tx` for any auto-created tracks (the
/// `track_id: None` case — one midi track per clip, as before), an optional
/// `Op::TempoSet` (only when the file carried explicit tempo events), and an
/// `Op::MidiClipAdd` per clip. The old three-separate-locks dance is gone —
/// everything structural lands in ONE rev bump.
pub(crate) fn midi_import_file_core(
    control: &ControlPlane,
    path: String,
    track_id: Option<String>,
    at_ticks: Option<u64>,
) -> Result<Vec<MidiClip>, String> {
    let p = std::path::Path::new(&path);
    if !p.is_absolute() {
        return Err(format!("path must be absolute: {path}"));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("read {path}: {e}"))?;

    // Prepare outside: parse the SMF fully (rescaled against the project's
    // CURRENT ppq) with no session lock held during the parse itself.
    let ppq = control.project_state().ppq;
    let imported = midifile::import_smf(&bytes, ppq)?;
    if imported.clips.is_empty() {
        return Err("MIDI file contains no note-carrying tracks".into());
    }

    let start = at_ticks.unwrap_or(0);
    let mut clips = imported.clips.clone();
    for c in &mut clips {
        c.timeline_start_ticks += start;
    }
    let adopt_tempo = imported.explicit_tempo.then(|| imported.tempo_events.clone());

    let mut result: Vec<MidiClip> = Vec::with_capacity(clips.len());
    control.commit(TxMeta::user("import midi file"), |tx| {
        match &track_id {
            Some(id) => {
                {
                    let track = tx
                        .store()
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
                }
                let lane_id = crate::ids::LaneId::default_for_track(id);
                for c in &mut clips {
                    c.track_id = id.clone().into();
                    c.lane_id = lane_id.clone();
                }
            }
            None => {
                for c in &mut clips {
                    let track = control_ops::add_track_tx(tx, Some(c.name.clone()), Some("midi".into()))?;
                    c.lane_id = crate::ids::LaneId::default_for_track(track.id.as_str());
                    c.track_id = track.id;
                }
            }
        }

        if let Some(events) = &adopt_tempo {
            let meter = tx.midi().meter_events.clone();
            tx.apply(Op::TempoSet { ppq, events: events.clone(), meter })?;
        }

        for c in clips.drain(..) {
            let index = tx.midi().clips.len();
            result.push(c.clone());
            tx.apply(Op::MidiClipAdd { clip: c, index })?;
        }
        Ok(())
    })?;
    Ok(result)
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
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Vec<MidiClip>, String> {
    midi_import_file_core(&control, path, track_id, at_ticks)
}

/// Export the project's MIDI (tempo map + clips) as a format-1 .mid file at
/// `path`. `clip_ids` restricts the export (default: every clip). Returns
/// the written path.
#[tauri::command]
pub fn midi_export_file(
    path: String,
    clip_ids: Option<Vec<String>>,
    state: State<'_, MidiState>,
) -> Result<String, String> {
    let p = std::path::PathBuf::from(&path);
    if !p.is_absolute() {
        return Err(format!("path must be absolute: {path}"));
    }
    let bytes = read_midi(&state, |s| {
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

    // ---- adopt_midi_from_dir (H-2, M-5; renamed from sync_midi_store,
    // Task 6) — plain fns, no tauri State needed ----

    #[test]
    fn adopt_midi_from_dir_failed_load_does_not_mark_synced() {
        // A directory that looks like a project dir but has no readable
        // project.json — load_from_project errors.
        let parent = std::env::temp_dir()
            .join(format!("aura-midi-mod-h2-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();

        let mut midi = MidiStore::default();
        adopt_midi_from_dir(&mut midi, &parent, 120.0);
        assert_eq!(midi.loaded_dir, None, "H-2: a failed load must not mark the store synced");

        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn adopt_midi_from_dir_refuses_to_resync_while_dirty() {
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

        let new_dir = std::path::PathBuf::from("/new/project");
        adopt_midi_from_dir(&mut midi, &new_dir, 120.0);

        assert_eq!(midi.clips.len(), 1, "M-5: dirty memory is not silently discarded");
        assert_eq!(
            midi.loaded_dir,
            Some(std::path::PathBuf::from("/old/project")),
            "refuses to adopt the new dir while dirty"
        );
    }

    // ---- Task 6 (Gate E test-4 precursor): declared MIDI read paths are
    // pure — the "plugin-teardown-inside-a-read horror" (round-2 inventory
    // row 9) is gone because no read path calls a resync anymore. This is
    // the RED/GREEN evidence `tests/pure_readers.rs` can't reach directly:
    // `read_midi` is crate-private (same constraint `EngineHandle::for_tests`
    // hits in control/mod.rs — that test's own comment documents the ruling
    // this follows), so the genuinely behavioral proof lives here, in-crate.

    /// Builds an `AudioState` + `MidiState` pair sharing one session (same
    /// wiring `MidiState::shared` performs during real app setup), seeded
    /// with a store already "adopted" from `dir_a` (a real on-disk project
    /// with one clip). Returns the pair plus `dir_a`/`dir_b`, where `dir_b`
    /// is a SECOND real project with a DIFFERENT clip — never adopted.
    fn midi_purity_fixture() -> (AudioState, MidiState, std::path::PathBuf, std::path::PathBuf) {
        use crate::audio::project;

        let parent = std::env::temp_dir().join(format!(
            "aura-midi-mod-purity-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let (_, dir_a) = project::create(&parent, "A", 48_000, 120.0).unwrap();
        let (_, dir_b) = project::create(&parent, "B", 48_000, 120.0).unwrap();

        let mut store_a = MidiStore::default();
        store_a.clips.push(dummy_clip("from-a"));
        persist::save_into_project(&dir_a, &store_a).unwrap();
        let mut store_b = MidiStore::default();
        store_b.clips.push(dummy_clip("from-b"));
        persist::save_into_project(&dir_b, &store_b).unwrap();

        let audio = AudioState::default();
        let midi_state = MidiState::default();
        let (session, _shared, _tables) = audio.control_parts();
        midi_state.shared(session.clone());
        {
            let mut s = session.lock();
            s.store.project_dir = Some(dir_a.clone());
            adopt_midi_from_dir(&mut s.midi, &dir_a, 120.0);
        }
        (audio, midi_state, dir_a, dir_b)
    }

    fn dummy_clip(id: &str) -> MidiClip {
        MidiClip {
            id: id.into(),
            track_id: "t1".into(),
            name: id.into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t1"),
            content_length_ticks: None,
        }
    }

    /// THE red/green case: `store.project_dir` moves to `dir_b` WITHOUT
    /// going through a `ControlPlane` epoch function (simulating "the old
    /// lazy-resync trigger" — a document swap the eager-adopt invariant is
    /// supposed to make impossible in practice, forced here by hand). A
    /// read through `read_midi` (Task 6's replacement for
    /// `with_synced_store(mutating=false)`) must NOT touch the midi store:
    /// still A's clip, `loaded_dir` still `dir_a`. Before Task 6,
    /// `with_synced_store`'s read path called `sync_midi_store` first,
    /// which WOULD have swapped in B's clip here — this is the exact
    /// behavior this test pins as gone.
    #[test]
    fn read_midi_never_resyncs_even_when_project_dir_moved_underneath_it() {
        let (audio, midi_state, dir_a, dir_b) = midi_purity_fixture();
        let (session, _, _) = audio.control_parts();
        session.lock().store.project_dir = Some(dir_b.clone());

        let before = read_midi(&midi_state, |s| Ok(s.clips.clone())).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].id.as_str(), "from-a", "a read must never resync from disk");

        let after = read_midi(&midi_state, |s| Ok(s.clips.clone())).unwrap();
        assert_eq!(before, after, "two reads back to back must be byte-identical (idempotent)");
        assert_eq!(
            session.lock().midi.loaded_dir,
            Some(dir_a.clone()),
            "loaded_dir must not move just from reading — only an epoch function adopts"
        );

        let _ = std::fs::remove_dir_all(dir_a.parent().unwrap());
    }

    // -----------------------------------------------------------------
    // Plan E Task 7: every mutating command routed through the channel —
    // integration-style tests against a real `ControlPlane` fixture (no
    // `tauri::State` harness, per `set_tempo_map_core`'s doc). Mirrors
    // `control::mod::tests::test_plane_with_tracks` (control/mod.rs), which
    // can't be reused directly (private to that module's `#[cfg(test)] mod
    // tests`) — duplicated here at the smaller scope this file needs (no
    // `GraphTables::slots` seeding: none of these ops push `param_writes`).
    // -----------------------------------------------------------------

    type RecordedEvents = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    fn test_control_plane() -> (ControlPlane, RecordedEvents) {
        use crate::audio::engine::EngineHandle;
        use crate::audio::rt::{GraphTables, SharedRt};
        use crate::audio::types::Store;
        use std::time::Duration;

        let store = Store::default();
        let session = Arc::new(Mutex::new(Session::new(store, MidiStore::default())));
        let shared = Arc::new(SharedRt::default());
        let tables = GraphTables::empty();
        let (engine, _engine_rx) = EngineHandle::for_tests();
        let events: RecordedEvents = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let cp = ControlPlane::new(
            session,
            shared,
            tables,
            engine,
            Arc::new(crate::sidecars::jobs::JobManager::new(2, Duration::ZERO)),
            Box::new(move |e, p| sink.lock().push((e.to_string(), p))),
            Arc::new(crate::control::HistoryLog::new()),
        );
        (cp, events)
    }

    fn changed_count(events: &RecordedEvents) -> usize {
        events.lock().iter().filter(|(name, _)| name == "project://changed").count()
    }

    /// A `ControlPlane` seeded with one midi track, via the real public
    /// `add_track` (same channel path a real "add track" command uses) —
    /// not a direct store poke.
    fn plane_with_midi_track() -> (ControlPlane, RecordedEvents, crate::ids::TrackId) {
        let (cp, events) = test_control_plane();
        let track = cp.add_track(Some("Keys".into()), Some("midi".into()), TxMeta::user("seed track")).unwrap();
        (cp, events, track.id)
    }

    // ---- midi_rename_clip_core ----

    #[test]
    fn midi_rename_clip_core_writes_trimmed_name_with_one_event() {
        let (cp, events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();
        let before = changed_count(&events);

        let renamed = midi_rename_clip_core(&cp, clip.id.clone(), "  Chorus  ".into()).unwrap();
        assert_eq!(renamed.name, "Chorus", "the write side trims");

        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.name, "Chorus", "rename lands in document truth");
        assert_eq!(changed_count(&events) - before, 1, "exactly one project://changed per invoke");
    }

    #[test]
    fn midi_rename_clip_core_rejects_empty_and_unknown_clip() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();

        assert!(midi_rename_clip_core(&cp, clip.id.clone(), "   ".into()).is_err());
        assert!(midi_rename_clip_core(&cp, "no-such-clip".into(), "X".into()).is_err());

        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.name, "Riff", "a rejected rename leaves the name untouched");
    }

    /// The inverse must carry the PREVIOUS name, so undo restores it —
    /// and the recorded op must carry the trimmed value, not the raw input.
    #[test]
    fn midi_rename_clip_inverse_restores_the_previous_name() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();
        midi_rename_clip_core(&cp, clip.id.clone(), "  Chorus  ".into()).unwrap();

        cp.commit(TxMeta::user("undo rename"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::MidiClip(clip.id.clone()),
                path: PropPath::Name,
                from: serde_json::Value::Null,
                to: serde_json::json!("Riff"),
            })
        })
        .unwrap();

        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.name, "Riff");
    }

    // ---- midi_remove_clip_core ----

    #[test]
    fn midi_remove_clip_core_removes_the_clip_with_one_event() {
        let (cp, events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();
        let before = changed_count(&events);

        midi_remove_clip_core(&cp, clip.id.clone()).unwrap();

        let snap = cp.project_state();
        assert!(snap.midi_clips.iter().all(|c| c.id != clip.id), "clip is gone from document truth");
        assert_eq!(changed_count(&events) - before, 1, "exactly one project://changed per invoke");
    }

    #[test]
    fn midi_remove_clip_core_rejects_unknown_clip() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();

        assert!(midi_remove_clip_core(&cp, "no-such-clip".into()).is_err());

        let snap = cp.project_state();
        assert!(snap.midi_clips.iter().any(|c| c.id == clip.id), "an unknown-id removal leaves truth untouched");
    }

    /// `midi_remove_clip_core` clears a clip-scoped MIDI-out route eagerly
    /// (`ControlPlane::clear_midi_route_for_clip`) rather than relying only
    /// on `midi_out::run_thread`'s 250 ms self-heal.
    #[test]
    fn midi_remove_clip_core_clears_a_clip_scoped_midi_out_route() {
        use std::sync::Arc;
        let (cp, _events, track_id) = plane_with_midi_track();
        let out = Arc::new(crate::midi_out::MidiOut::default());
        cp.attach_midi_out(Arc::clone(&out));
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();
        out.set_route(
            crate::midi_out::RouteScope::Clip(clip.id.to_string()),
            Some(crate::midi_out::RouteTarget { port_id: "x#0".into(), channel: 0 }),
        );
        assert!(out.routes().contains_key(&crate::midi_out::RouteScope::Clip(clip.id.to_string())));

        midi_remove_clip_core(&cp, clip.id.clone()).unwrap();

        assert!(
            !out.routes().contains_key(&crate::midi_out::RouteScope::Clip(clip.id.to_string())),
            "the route to the deleted clip is cleared eagerly, not left for self-heal"
        );
    }

    /// The inverse (`Op::MidiClipAdd`) must restore the clip byte-identically
    /// — same precedent as `remove_track_inverse_restores_row_and_clips_
    /// byte_identically` (control/mod.rs).
    #[test]
    fn midi_remove_clip_inverse_restores_the_clip() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();

        midi_remove_clip_core(&cp, clip.id.clone()).unwrap();

        cp.commit(TxMeta::user("undo remove"), |tx| {
            tx.apply(Op::MidiClipAdd { clip: clip.clone(), index: 0 })
        })
        .unwrap();

        let snap = cp.project_state();
        let restored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(restored.name, "Riff");
        assert_eq!(restored.timeline_start_ticks, 0);
        assert_eq!(restored.length_ticks, 960);
    }

    // ---- midi_set_clip_bounds_core ----

    #[test]
    fn midi_set_clip_bounds_core_writes_document_truth_with_one_event() {
        let (cp, events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 1920).unwrap();
        let before = changed_count(&events);

        let updated =
            midi_set_clip_bounds_core(&cp, clip.id.clone(), 960, 3840, Some(1920)).unwrap();
        assert_eq!(updated.timeline_start_ticks, 960);
        assert_eq!(updated.length_ticks, 3840);
        assert_eq!(updated.content_length_ticks, Some(1920));

        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.timeline_start_ticks, 960);
        assert_eq!(stored.length_ticks, 3840);
        assert_eq!(stored.content_length_ticks, Some(1920));

        assert_eq!(changed_count(&events) - before, 1, "exactly one project://changed per invoke");
    }

    /// BINDING RULING (Task 5's review, carried into Task 7): the wrapper
    /// must reject `contentLengthTicks == 0` even though the `Op::Set`
    /// apply arm (session.rs) does not — it only clamps `LengthTicks`, never
    /// `ContentLengthTicks`. Pins the reject at the command-core level.
    #[test]
    fn midi_set_clip_bounds_core_rejects_zero_content_length_ticks() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 1920).unwrap();

        let err = midi_set_clip_bounds_core(&cp, clip.id.clone(), 0, 1920, Some(0)).unwrap_err();
        assert!(err.contains("contentLengthTicks"), "got: {err}");

        // Rejected calls must not have mutated the clip.
        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.length_ticks, 1920);
        assert_eq!(stored.content_length_ticks, None);
    }

    #[test]
    fn midi_set_clip_bounds_core_rejects_zero_length_ticks() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 1920).unwrap();
        assert!(midi_set_clip_bounds_core(&cp, clip.id, 0, 0, None).is_err());
    }

    #[test]
    fn midi_set_clip_bounds_undo_restores_previous_bounds() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 1920).unwrap();

        let object = ObjectRef::MidiClip(clip.id.clone());
        let committed = cp
            .commit(TxMeta::user("bounds"), |tx| {
                tx.apply(Op::Set {
                    object: object.clone(),
                    path: PropPath::TimelineStartTicks,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(960u64),
                })?;
                tx.apply(Op::Set {
                    object,
                    path: PropPath::LengthTicks,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(3840u64),
                })
            })
            .unwrap();

        cp.commit(TxMeta::user("undo"), |tx| {
            for op in committed.inverses {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();

        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.timeline_start_ticks, 0, "undo restores placement");
        assert_eq!(stored.length_ticks, 1920, "undo restores length");
    }

    // ---- midi_add_clip_core ----

    #[test]
    fn midi_add_clip_core_writes_document_truth_with_one_event() {
        let (cp, events, track_id) = plane_with_midi_track();
        let before = changed_count(&events);

        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), Some("Riff".into()), 0, 960).unwrap();
        assert_eq!(clip.name, "Riff");
        assert_eq!(clip.length_ticks, 960);

        let snap = cp.project_state();
        assert!(snap.midi_clips.iter().any(|c| c.id == clip.id), "clip lands in document truth");
        assert_eq!(changed_count(&events) - before, 1, "exactly one project://changed per invoke");
    }

    #[test]
    fn midi_add_clip_core_rejects_zero_length_and_non_midi_track() {
        let (cp, _events, track_id) = plane_with_midi_track();
        assert!(midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 0).is_err());

        let audio_track =
            cp.add_track(Some("Guitar".into()), Some("audio".into()), TxMeta::user("seed")).unwrap();
        assert!(midi_add_clip_core(&cp, audio_track.id.as_str().into(), None, 0, 960).is_err());
    }

    #[test]
    fn midi_add_clip_undo_removes_the_clip() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 960).unwrap();

        let committed = cp
            .commit(TxMeta::user("undo add"), |tx| {
                tx.apply(Op::MidiClipRemove { clip: clip.clone(), index: 0 })
            })
            .unwrap();
        let _ = committed; // MidiClipRemove is its own undo of MidiClipAdd — nothing further needed here.

        let snap = cp.project_state();
        assert!(!snap.midi_clips.iter().any(|c| c.id == clip.id));
    }

    // ---- midi_set_notes_core ----

    #[test]
    fn midi_set_notes_core_writes_document_truth_with_one_event() {
        let (cp, events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 1920).unwrap();
        let before = changed_count(&events);

        let incoming = vec![note(0, 60, 0), note(200, 64, 0)];
        let updated = midi_set_notes_core(&cp, clip.id.as_str().into(), incoming).unwrap();
        assert_eq!(updated.notes.len(), 2);
        assert!(updated.notes.iter().all(|n| n.note_id.0 != 0), "server minted ids for the zero-id payload");

        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.notes.len(), 2);
        assert_eq!(changed_count(&events) - before, 1, "exactly one project://changed per invoke");
    }

    #[test]
    fn midi_set_notes_core_undo_restores_previous_notes_respecting_the_watermark() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 1920).unwrap();
        let first = midi_set_notes_core(&cp, clip.id.as_str().into(), vec![note(0, 60, 0)]).unwrap();
        assert_eq!(first.notes[0].note_id.0, 1);

        let committed = cp
            .commit(TxMeta::user("set notes"), |tx| {
                tx.apply(Op::MidiSetNotes {
                    clip: clip.id.clone(),
                    notes: vec![note(0, 60, first.notes[0].note_id.0), note(200, 64, 0)],
                })
            })
            .unwrap();

        cp.commit(TxMeta::user("undo"), |tx| {
            for op in committed.inverses {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();

        let snap = cp.project_state();
        let stored = snap.midi_clips.iter().find(|c| c.id == clip.id).unwrap();
        assert_eq!(stored.notes.len(), 1, "undo restores the previous note VALUES");
        assert_eq!(stored.notes[0].note_id.0, 1, "restored note keeps its real id");
        // Scope ruling 3 / ADR 0001: the watermark is NEVER rewound by undo —
        // a further set_notes call still mints ids starting from 3, not 2.
        let after_undo_add =
            midi_set_notes_core(&cp, clip.id.as_str().into(), vec![note(0, 60, 1), note(400, 67, 0)]).unwrap();
        let minted = after_undo_add.notes.iter().find(|n| n.tick == 400).unwrap();
        assert_eq!(minted.note_id.0, 3, "watermark advanced monotonically, never rewound by undo");
    }

    #[test]
    fn midi_set_notes_core_rejects_invalid_notes() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let clip = midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 1920).unwrap();
        let bad = vec![MidiNote { tick: 0, length_ticks: 0, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) }];
        assert!(midi_set_notes_core(&cp, clip.id.as_str().into(), bad).is_err());
    }

    // ---- set_tempo_map_core ----

    #[test]
    fn set_tempo_map_core_writes_document_truth_and_the_legacy_bpm_mirror_with_one_event() {
        let (cp, events) = test_control_plane();
        let before = changed_count(&events);

        let result = set_tempo_map_core(
            &cp,
            Some(960),
            vec![TempoEvent { tick: 0, bpm: 140.0 }, TempoEvent { tick: 1920, bpm: 100.0 }],
        )
        .unwrap();
        assert_eq!(result.ppq, 960);
        assert!((result.events[0].bpm - 140.0).abs() < 1e-6);

        let snap = cp.project_state();
        assert_eq!(snap.ppq, 960);
        assert!((snap.tempo_events[0].bpm - 140.0).abs() < 1e-6);
        assert!(
            (snap.transport.tempo_bpm - 140.0).abs() < 1e-6,
            "the legacy transport.tempoBpm mirror is owned by Op::TempoSet's apply arm now, \
             not a separate writeback in the command body"
        );
        assert_eq!(changed_count(&events) - before, 1, "exactly one project://changed per invoke");
    }

    #[test]
    fn set_tempo_map_core_preserves_the_stores_current_meter_map() {
        let (cp, _events) = test_control_plane();
        // No `set_meter_map` command exists — `set_tempo_map`'s signature
        // carries no meter field, so the store's current meter travels
        // through `Op::TempoSet` unchanged (read via `tx.midi()`).
        let default_meter = cp.project_state().meter_map;
        set_tempo_map_core(&cp, Some(960), vec![TempoEvent { tick: 0, bpm: 90.0 }]).unwrap();
        assert_eq!(cp.project_state().meter_map, default_meter);
    }

    #[test]
    fn set_tempo_map_core_undo_restores_previous_map() {
        let (cp, _events) = test_control_plane();
        let committed = cp
            .commit(TxMeta::user("tempo"), |tx| {
                tx.apply(Op::TempoSet {
                    ppq: 960,
                    events: vec![TempoEvent { tick: 0, bpm: 150.0 }],
                    meter: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
                })
            })
            .unwrap();

        cp.commit(TxMeta::user("undo"), |tx| {
            for op in committed.inverses {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();

        let snap = cp.project_state();
        assert!((snap.tempo_events[0].bpm - 120.0).abs() < 1e-6, "undo restores the default 120 bpm map");
        assert!((snap.transport.tempo_bpm - 120.0).abs() < 1e-6);
    }

    // ---- midi_import_file_core ----

    /// Writes a minimal format-1 SMF (one tempo event + one note-carrying
    /// track) to a temp file via this crate's own exporter — reusing
    /// `midifile::export_smf` as the fixture builder keeps this test from
    /// hand-rolling SMF bytes.
    fn write_fixture_smf() -> std::path::PathBuf {
        let clip = dummy_clip_with_note("import-fixture");
        let bytes = midifile::export_smf(960, &[TempoEvent { tick: 0, bpm: 128.0 }], &[clip]).unwrap();
        let path = std::env::temp_dir().join(format!(
            "aura-midi-mod-import-{}-{}.mid",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn dummy_clip_with_note(id: &str) -> MidiClip {
        MidiClip {
            id: id.into(),
            track_id: "t1".into(),
            name: id.into(),
            timeline_start_ticks: 0,
            length_ticks: 1920,
            notes: vec![note(0, 60, 0)],
            next_note_id: 2,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t1"),
            content_length_ticks: None,
        }
    }

    #[test]
    fn midi_import_file_core_creates_tracks_clips_and_tempo_in_one_rev_bump() {
        let (cp, events) = test_control_plane();
        let path = write_fixture_smf();
        let rev_before = cp.commit(TxMeta::user("noop"), |_tx| Ok(())).unwrap().rev;
        let before = changed_count(&events);

        let clips = midi_import_file_core(&cp, path.to_string_lossy().into_owned(), None, None).unwrap();
        assert_eq!(clips.len(), 1);

        let snap = cp.project_state();
        assert_eq!(snap.tracks.len(), 1, "a midi track was auto-created for the imported clip");
        assert_eq!(snap.tracks[0].kind, "midi");
        assert!(snap.midi_clips.iter().any(|c| c.id == clips[0].id));
        assert!((snap.tempo_events[0].bpm - 128.0).abs() < 1e-6, "explicit tempo event adopted");

        // ONE rev bump for the whole import (track add + tempo + clip add,
        // all inside a single commit).
        let committed_now = cp.commit(TxMeta::user("noop2"), |_tx| Ok(())).unwrap();
        assert_eq!(committed_now.rev, rev_before + 2, "one commit for the import, one for this probe");
        assert_eq!(changed_count(&events) - before, 2, "one project://changed for the import, one for the probe");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn midi_import_file_core_places_onto_an_existing_track_without_creating_one() {
        let (cp, _events, track_id) = plane_with_midi_track();
        let path = write_fixture_smf();

        let clips =
            midi_import_file_core(&cp, path.to_string_lossy().into_owned(), Some(track_id.as_str().into()), Some(480))
                .unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].track_id, track_id);
        assert_eq!(clips[0].timeline_start_ticks, 480, "at_ticks offsets the placement");

        let snap = cp.project_state();
        assert_eq!(snap.tracks.len(), 1, "no extra track was created");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn midi_import_file_core_rejects_relative_paths_and_missing_files() {
        let (cp, _events) = test_control_plane();
        assert!(midi_import_file_core(&cp, "relative/path.mid".into(), None, None).is_err());
        assert!(midi_import_file_core(
            &cp,
            std::env::temp_dir().join("no-such-aura-fixture.mid").to_string_lossy().into_owned(),
            None,
            None
        )
        .is_err());
    }

    // ---- seed_demo_project ----

    #[test]
    fn seed_demo_project_creates_the_demo_content_in_one_commit_with_one_event() {
        let (cp, events) = test_control_plane();
        let rev_before = cp.commit(TxMeta::user("noop"), |_tx| Ok(())).unwrap().rev;
        let before = changed_count(&events);

        let snap = cp.seed_demo_project().unwrap();
        assert_eq!(snap.tracks.len(), 3, "pad, lead, bass");
        assert_eq!(snap.midi_clips.len(), 3);
        assert!(snap.midi_clips.iter().any(|c| !c.notes.is_empty()), "demo clips carry notes");

        let committed_now = cp.commit(TxMeta::user("noop2"), |_tx| Ok(())).unwrap();
        assert_eq!(committed_now.rev, rev_before + 2, "one commit for the seed, one for this probe");
        assert_eq!(changed_count(&events) - before, 2, "seed_demo_project now emits project://changed (was missing)");
    }

    #[test]
    fn seed_demo_project_refuses_when_the_session_already_has_content() {
        let (cp, _events, track_id) = plane_with_midi_track();
        midi_add_clip_core(&cp, track_id.as_str().into(), None, 0, 960).unwrap();
        midi_set_notes_core(&cp, cp.project_state().midi_clips[0].id.as_str().into(), vec![note(0, 60, 0)]).unwrap();

        assert!(cp.seed_demo_project().is_err());
    }
}
