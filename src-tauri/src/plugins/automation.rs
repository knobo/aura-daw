//! Parameter-automation groundwork (phase 3, zone P4) — the SCALABILITY §3
//! `automation[]` schema draft made real: data model, tick-domain points,
//! project persistence, and the playback-side ramp contract.
//!
//! ## Data model (SCALABILITY §3 / §2 "Parameter automation model")
//!
//! An [`AutomationLane`] targets one parameter of one node —
//! `targetNode` is a STABLE node id (a plugin instance id for plugin
//! params; `"track:<trackId>"` addresses the track's live instrument node
//! for built-in params), `paramId` is the flat `ParamId(u32)` every node
//! exposes (plugin-state.schema.json `$defs/param.id`). Points live in
//! MUSICAL time (ticks @ project ppq) and are compiled control-side through
//! [`TempoMap`] into absolute-sample events ([`compile_lane`]) — ticks never
//! cross onto the RT thread (ARCHITECTURE §13/§15.1).
//!
//! ## Persistence (project v2, additive)
//!
//! `project.json` gains `automation: [{id, targetNode, paramId, pointsRef}]`;
//! point payloads NEVER go inline — they are AMEV chunks (kind 1 =
//! automation records, value column) under `<project>/automation/<uuid>.bin`
//! (its own directory per SCALABILITY §4 — the midi saver GCs `events/`
//! independently). Same rules as midi clips: refs are path-traversal-safe,
//! stale chunks are GC'd after a successful save, corrupt chunks degrade to
//! an empty lane instead of failing the open.
//!
//! ## Playback contract (groundwork)
//!
//! The plan's param contract (contract 5: batched `plugin_set_param` ->
//! wait-free ring -> RT node) extends to automation as *pre-scheduled
//! sample-accurate ramps*: the control thread compiles lanes to
//! [`AbsParamEvent`] lists; per render block the value is a linear ramp
//! between breakpoints ([`value_at`]). [`GainAutomatedNode`] applies that
//! contract to ANY [`LiveInstrument`] today (PolySynth/sampler/plugin node)
//! by scaling the node's block output per sample — the proven end-to-end
//! path (see the rendered-output tests). Position: `mixer::render_live`
//! delivers the true block base position + discontinuity flag through
//! `LiveInstrument::set_block_context` before every run (the architect-merge
//! seam), so ramps stay position-true across seeks and loop wraps; the
//! internal per-frame cursor covers callers that bypass the seam.
//! Production graph rebuilds do not yet attach lanes to nodes (roadmap:
//! automation editing UI round).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::audio::dsp::{AudioProcessor, LiveInstrument, ProcessBlock};
use crate::audio::types::AutomationMode;
use crate::midi::events::{AMEV_MAGIC, AMEV_VERSION, COLUMNS_CORE, KIND_AUTOMATION};
use crate::midi::synth::BlockNoteEvent;
use crate::midi::types::DEFAULT_PPQ;
use crate::midi::TempoMap;

/// Project subdirectory for automation point chunks (SCALABILITY §4).
pub const AUTOMATION_DIR: &str = "automation";

// ---------------------------------------------------------------------------
// Data model (wire = camelCase JSON, mirrors the §3 schema draft)
// ---------------------------------------------------------------------------

/// One automation breakpoint in musical time. Between points the value ramps
/// linearly; before the first / after the last point it holds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationPoint {
    pub tick: u32,
    pub value: f32,
}

/// One parameter's automation curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationLane {
    pub id: String,
    /// Stable node id: plugin instance uuid, or `"track:<trackId>"` for the
    /// track's built-in live instrument (stable UUIDs — SCALABILITY §2 rule).
    pub target_node: String,
    /// Flat per-node parameter id (plugin-state.schema.json `param.id`).
    pub param_id: u32,
    /// Sorted by tick, strictly increasing (enforced on set/load).
    #[serde(default)]
    pub points: Vec<AutomationPoint>,
}

/// PROJECT-ADOPTION SEAM cross-cutting reach (Plan E Task 10, mirrors
/// `midi::playback`'s `PLAYBACK_SESSION`): `adopt_open_project` runs from
/// call sites (`midi::persist::load_from_project`,
/// `ControlPlane::create_project`/`save_project_as`) that have no session
/// handle of their own to pass in, so the shared `Session` is reached
/// through this process-global instead — registered once, from
/// `MidiState::shared` (the same app-setup hook that wires
/// `midi::playback::register_store`), never touched by unit tests (which
/// construct `Session`s locally and call `adopt_open_project` only after
/// registering their own — see this module's tests).
static SESSION: OnceLock<Arc<Mutex<crate::control::Session>>> = OnceLock::new();

/// Register the app-wide session for the project-adoption seam. First
/// registration wins; tests register a local session instead of relying on
/// this being set (unset = `adopt_open_project` is inert, same contract the
/// retired `STORE` global had).
pub fn register_session(session: Arc<Mutex<crate::control::Session>>) {
    let _ = SESSION.set(session);
}

pub fn registered_session() -> Option<&'static Arc<Mutex<crate::control::Session>>> {
    SESSION.get()
}

/// Validate + normalize a lane in place: finite values, points sorted by
/// tick, duplicate ticks collapsed (last one wins — batch semantics).
pub fn normalize_lane(lane: &mut AutomationLane) -> Result<(), String> {
    if lane.target_node.is_empty() {
        return Err("targetNode must not be empty".into());
    }
    let is_track_gain = lane.param_id == TRACK_PARAM_GAIN
        && lane.target_node.starts_with(TRACK_TARGET_PREFIX);
    for p in &lane.points {
        if !p.value.is_finite() {
            return Err(format!("non-finite automation value at tick {}", p.tick));
        }
        if is_track_gain && p.value < 0.0 {
            return Err(format!("negative track-gain multiplier at tick {}", p.tick));
        }
    }
    lane.points.sort_by_key(|p| p.tick);
    lane.points.dedup_by(|b, a| {
        if a.tick == b.tick {
            a.value = b.value; // keep the later entry's value
            true
        } else {
            false
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// AMEV point chunks (kind 1 records; same container as midi note chunks)
// ---------------------------------------------------------------------------

/// Encode points as an AMEV chunk (16-byte records, kind=automation, value
/// column carries the point value).
pub fn encode_points(ppq: u32, points: &[AutomationPoint]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + points.len() * 16);
    out.write_u32::<LittleEndian>(AMEV_MAGIC).unwrap();
    out.write_u16::<LittleEndian>(AMEV_VERSION).unwrap();
    out.write_u16::<LittleEndian>(COLUMNS_CORE).unwrap();
    out.write_u32::<LittleEndian>(ppq).unwrap();
    out.write_u32::<LittleEndian>(points.len() as u32).unwrap();
    for p in points {
        out.write_u32::<LittleEndian>(p.tick).unwrap();
        out.write_u32::<LittleEndian>(0).unwrap(); // duration (unused)
        out.push(KIND_AUTOMATION);
        out.push(0);
        out.push(0);
        out.push(0);
        out.write_f32::<LittleEndian>(p.value).unwrap();
    }
    out
}

/// Decode an AMEV chunk's automation records -> `(ppq, points)`. Note
/// records are skipped (mirror of `events::decode_notes`). Appended column
/// blocks (e.g. `events::COLUMNS_NOTE_ID` on a note chunk) are walked and
/// skipped wholesale — automation points carry no per-point ids yet, and a
/// reader that errored on an unrecognised mask bit would break on every
/// note chunk it was (incorrectly) asked to read [M-4].
pub fn decode_points(bytes: &[u8]) -> Result<(u32, Vec<AutomationPoint>), String> {
    let mut r = std::io::Cursor::new(bytes);
    let magic = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    if magic != AMEV_MAGIC {
        return Err(format!("not an AMEV chunk (magic {magic:#010x})"));
    }
    let version = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    if version > AMEV_VERSION {
        return Err(format!(
            "AMEV version {version} is newer than supported {AMEV_VERSION}"
        ));
    }
    // Advisory for bits automation doesn't interpret — but a mask bit with
    // no matching block consumed is still structural corruption (checked
    // generically below, same rule as `events::decode_notes`).
    let mask = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    let ppq = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    let count = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;

    let core_bytes = (count as u64).saturating_mul(16);
    if core_bytes > bytes.len() as u64 {
        return Err(format!(
            "AMEV chunk truncated: {count} records need {core_bytes} bytes, chunk has {}",
            bytes.len()
        ));
    }
    let mut points = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let tick = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
        let _duration = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
        let mut rest = [0u8; 4];
        std::io::Read::read_exact(&mut r, &mut rest).map_err(|e| e.to_string())?;
        let value = r.read_f32::<LittleEndian>().map_err(|e| e.to_string())?;
        if rest[0] == KIND_AUTOMATION {
            points.push(AutomationPoint { tick, value });
        }
    }

    // Walk and skip every appended column block (self-describing, D-06);
    // automation never interprets any of them today — the shared walker
    // (mirrors decode_notes) owns the truncation/ordering/duplicate-bit
    // checks, this callback just discards the payload.
    let bits_consumed = crate::midi::events::walk_column_blocks(bytes, &mut r, |_col_bit, byte_len, r| {
        let mut buf = vec![0u8; byte_len as usize];
        std::io::Read::read_exact(r, &mut buf).map_err(|e| e.to_string())
    })?;

    // Reviewer finding 1 (CRITICAL): a mask bit set with no matching block
    // consumed is structural corruption, never silently tolerated — the
    // general form of the rule (mirrors decode_notes), even though
    // automation doesn't interpret any column's content.
    let missing_blocks = mask & !COLUMNS_CORE & !bits_consumed;
    if missing_blocks != 0 {
        return Err(format!(
            "AMEV mask 0x{mask:04x} claims column(s) 0x{missing_blocks:04x} but no matching block was found"
        ));
    }

    Ok((ppq, points))
}

// ---------------------------------------------------------------------------
// Project persistence (additive `automation[]` + chunk files)
// ---------------------------------------------------------------------------

/// Absolute automation dir for a project (`<dir>/automation`).
pub fn automation_dir(dir: &Path) -> PathBuf {
    dir.join(AUTOMATION_DIR)
}

/// Persisted lane row (`{id, targetNode, paramId, pointsRef}`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedLane {
    id: String,
    target_node: String,
    param_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    points_ref: Option<String>,
}

/// Write a lane SNAPSHOT into `<dir>/project.json` (`automation[]`) and
/// point chunks under `<dir>/automation/`, GC'ing stale chunks afterwards.
/// Upgrades v1 files to schemaVersion 2 (same discipline as midi persist).
///
/// Takes a plain snapshot (not the document itself, round-2 §4: no disk I/O
/// under the session lock) — `ControlPlane::execute_persist` clones
/// `session.automation.lanes` under a short lock and calls this AFTER the
/// lock is released, same shape as `midi::persist::save_snapshot_into_project`.
pub fn save_into_project(dir: &Path, lanes: &[AutomationLane]) -> Result<(), String> {
    // Chunk ppq: adopt the project's current ppq so ticks stay consistent
    // with midi clips (rescaled on load when it changed since).
    let ppq = fs::read(dir.join("project.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("ppq").and_then(Value::as_u64))
        .map(|p| p as u32)
        .unwrap_or(DEFAULT_PPQ);

    let adir = automation_dir(dir);
    let mut rows = Vec::with_capacity(lanes.len());
    let mut live_chunks: Vec<String> = Vec::new();
    for lane in lanes {
        let mut row = PersistedLane {
            id: lane.id.clone(),
            target_node: lane.target_node.clone(),
            param_id: lane.param_id,
            points_ref: None,
        };
        if !lane.points.is_empty() {
            fs::create_dir_all(&adir).map_err(|e| e.to_string())?;
            let chunk_name = format!("{}.bin", uuid::Uuid::new_v4());
            fs::write(adir.join(&chunk_name), encode_points(ppq, &lane.points))
                .map_err(|e| format!("write automation chunk: {e}"))?;
            row.points_ref = Some(format!("{AUTOMATION_DIR}/{chunk_name}"));
            live_chunks.push(chunk_name);
        }
        rows.push(row);
    }

    let value = serde_json::to_value(&rows).map_err(|e| e.to_string())?;
    crate::midi::persist::update_project_v2(dir, |obj| {
        obj.insert("automation".into(), value);
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

/// Load `automation[]` lanes from `<dir>/project.json`. `Ok(None)` when the
/// project carries no automation field (callers keep in-memory state).
/// Chunk ticks are rescaled to the project ppq when they differ; a corrupt
/// or traversal-unsafe chunk ref degrades to an empty lane, never a failed
/// open (midi persist hardening rules).
pub fn load_lanes(dir: &Path) -> Result<Option<Vec<AutomationLane>>, String> {
    let file = dir.join("project.json");
    let bytes = fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", file.display()))?;
    let project_ppq = root
        .get("ppq")
        .and_then(Value::as_u64)
        .map(|p| p as u32)
        .unwrap_or(DEFAULT_PPQ);
    let Some(arr) = root.get("automation") else { return Ok(None) };
    let Some(arr) = arr.as_array() else {
        return Err("automation field is not an array".into());
    };
    let mut lanes = Vec::with_capacity(arr.len());
    for v in arr {
        let row: PersistedLane = match serde_json::from_value(v.clone()) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("automation: skipping malformed lane row ({e}): {v}");
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
        let mut lane = AutomationLane {
            id: row.id,
            target_node: row.target_node,
            param_id: row.param_id,
            points,
        };
        if let Err(e) = normalize_lane(&mut lane) {
            log::warn!("automation lane {}: {e}; loading without points", lane.id);
            lane.points.clear();
        }
        lanes.push(lane);
    }
    Ok(Some(lanes))
}

fn read_chunk(dir: &Path, rel: &str, project_ppq: u32) -> Result<Vec<AutomationPoint>, String> {
    // pointsRef must stay inside the project ("automation/<name>.bin").
    let name = rel
        .strip_prefix("automation/")
        .filter(|n| !n.contains('/') && !n.contains('\\') && !n.contains("..") && n.ends_with(".bin"))
        .ok_or_else(|| format!("invalid pointsRef {rel:?}"))?;
    let path = automation_dir(dir).join(name);
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

/// PROJECT-OPEN SEAM (Task 6: called explicitly from `ControlPlane`'s
/// sanctioned epoch functions, e.g. `open_project_epoch`/`create_project_at`
/// — AFTER the session lock drops, never from a read path): adopt the
/// opened project's automation lanes into `session.automation.lanes`
/// (Task 10). Inert until the app registers the session (unit tests
/// register a local one, or don't, and never observe this).
///
/// Unlike the retired lazily-resyncing `AutomationStore`, there is no
/// `loaded_dir` special case here: this fn only ever runs at the exact
/// moment a project is (re)adopted (an epoch boundary — never
/// speculatively, on every command, the way the old lazy-sync
/// `with_synced` used to check), so a project with no persisted
/// `automation[]` field genuinely means "this (now open) project has
/// none" — clearing unconditionally is correct.
pub fn adopt_open_project(dir: &Path) {
    let Some(session) = registered_session() else { return };
    match load_lanes(dir) {
        Ok(Some(lanes)) => {
            let n = lanes.len();
            {
                let mut s = session.lock();
                s.automation.lanes = lanes;
                // snapshot republish: adopt replaces the lane document
                // outside `transact`; same lock as the write.
                s.republish_full();
            }
            if n > 0 {
                log::info!("automation: adopted {n} lane(s) from {}", dir.display());
            }
        }
        Ok(None) => {
            let mut s = session.lock();
            s.automation.lanes.clear();
            // snapshot republish: the clear arm is a write too (I-7).
            s.republish_full();
        }
        Err(e) => log::warn!("automation: cannot restore from {}: {e}", dir.display()),
    }
}

// ---------------------------------------------------------------------------
// Tick -> sample compilation + the per-block ramp contract
// ---------------------------------------------------------------------------

/// One compiled breakpoint in absolute engine samples (control-side product
/// of [`compile_lane`]; the RT side only interpolates — no ticks, no tempo
/// math, mirroring `AbsNoteEvent`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AbsParamEvent {
    pub sample: u64,
    pub value: f32,
}

/// Compile a lane's tick-domain points to absolute-sample breakpoints via
/// the tempo map (CONTROL THREAD ONLY). Points collapsing onto the same
/// sample keep the last value.
pub fn compile_lane(lane: &AutomationLane, map: &TempoMap) -> Vec<AbsParamEvent> {
    let mut out: Vec<AbsParamEvent> = Vec::with_capacity(lane.points.len());
    for p in &lane.points {
        let sample = map.tick_to_samples(p.tick as u64);
        match out.last_mut() {
            Some(last) if last.sample == sample => last.value = p.value,
            _ => out.push(AbsParamEvent { sample, value: p.value }),
        }
    }
    out
}

/// The ramp contract: value at `sample` is the linear interpolation between
/// the surrounding breakpoints; holds first/last outside the curve. `None`
/// for an empty curve (parameter untouched).
pub fn value_at(events: &[AbsParamEvent], sample: u64) -> Option<f32> {
    if events.is_empty() {
        return None;
    }
    let idx = events.partition_point(|e| e.sample <= sample);
    Some(segment_value(events, idx, sample))
}

/// Interpolate with a precomputed `partition_point` index (`events[idx-1]
/// .sample <= sample < events[idx].sample` when both exist) — the RT-side
/// form: O(1) per frame with an incrementally maintained index.
#[inline]
fn segment_value(events: &[AbsParamEvent], idx: usize, sample: u64) -> f32 {
    if idx == 0 {
        events[0].value
    } else if idx >= events.len() {
        events[events.len() - 1].value
    } else {
        let a = events[idx - 1];
        let b = events[idx];
        let span = (b.sample - a.sample) as f32;
        let t = (sample - a.sample) as f32 / span;
        a.value + (b.value - a.value) * t
    }
}

// ---------------------------------------------------------------------------
// Lane targets, the RT ramp cursor, and the control-side param driver
// (Track D)
// ---------------------------------------------------------------------------

/// `targetNode` prefix addressing a track's built-in node params.
pub const TRACK_TARGET_PREFIX: &str = "track:";
/// Built-in param ids on a `"track:<id>"` target node. Gain is 0 — the id
/// this module's own tests have used since Plan E Task 10.
pub const TRACK_PARAM_GAIN: u32 = 0;

/// What a lane actually drives.
#[derive(Debug, Clone, PartialEq)]
pub enum LaneTarget {
    /// Track id; the lane's value is a LINEAR GAIN MULTIPLIER applied on top
    /// of the fader (scope ruling 6).
    TrackGain(String),
    PluginParam { instance: String, index: u32 },
}

/// Classify a lane's target. `None` for a `"track:"` target naming a
/// built-in param this build does not know — an unknown id is ignored,
/// never guessed at.
pub fn resolve_target(lane: &AutomationLane) -> Option<LaneTarget> {
    match lane.target_node.strip_prefix(TRACK_TARGET_PREFIX) {
        Some(track_id) if !track_id.is_empty() => match lane.param_id {
            TRACK_PARAM_GAIN => Some(LaneTarget::TrackGain(track_id.to_string())),
            // Unknown built-in param: ignored, never guessed at. New ids are
            // additive here and in the frontend twin (`automation.svelte.ts`).
            _ => None,
        },
        Some(_) => None, // "track:" with no id
        None if !lane.target_node.is_empty() => Some(LaneTarget::PluginParam {
            instance: lane.target_node.clone(),
            index: lane.param_id,
        }),
        None => None,
    }
}

/// RT-side ramp reader: O(1) amortized per frame, correct across BACKWARD
/// jumps (a loop wrap moves the position back mid-block). No allocation, no
/// locks — safe on the audio callback. Agrees with [`value_at`] on every
/// probe by construction: both index the curve with the same
/// `partition_point` invariant and share [`segment_value`].
pub struct RampCursor {
    /// `events.partition_point(|e| e.sample <= last)` — maintained
    /// incrementally while the position only moves forward.
    idx: usize,
    last: u64,
}

impl Default for RampCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl RampCursor {
    pub fn new() -> Self {
        // `u64::MAX` forces the first call to re-seed by binary search.
        Self { idx: 0, last: u64::MAX }
    }

    /// Value at `sample`, or `None` for an empty curve (parameter untouched
    /// — the caller's neutral applies).
    #[inline]
    pub fn value(&mut self, events: &[AbsParamEvent], sample: u64) -> Option<f32> {
        if events.is_empty() {
            return None;
        }
        if sample < self.last {
            // Backward jump (loop wrap, seek): re-seed. O(log n), once.
            self.idx = events.partition_point(|e| e.sample <= sample);
        } else {
            while self.idx < events.len() && events[self.idx].sample <= sample {
                self.idx += 1;
            }
        }
        self.last = sample;
        Some(segment_value(events, self.idx, sample))
    }
}

/// Compile every TRACK-GAIN lane into a slot-indexed ramp table for one
/// rebuild. CONTROL THREAD ONLY — this is where ticks become absolute
/// samples; nothing tick-shaped crosses onto the RT thread
/// (ARCHITECTURE §13/§15.1).
///
/// One slot holds ONE ramp: if two lanes name the same track's gain, the
/// LAST one in `lanes` wins and the earlier is silently dropped (no blend,
/// no error). Nothing produces that today — the UI keeps one gain lane per
/// track — so it is documented rather than rejected.
pub fn compile_gain_ramps(
    lanes: &[AutomationLane],
    map: &TempoMap,
    n_slots: usize,
    slot_of: &dyn Fn(&str) -> Option<usize>,
) -> Vec<Option<Arc<Vec<AbsParamEvent>>>> {
    let mut out: Vec<Option<Arc<Vec<AbsParamEvent>>>> = (0..n_slots).map(|_| None).collect();
    for lane in lanes {
        let Some(LaneTarget::TrackGain(track_id)) = resolve_target(lane) else { continue };
        let Some(slot) = slot_of(&track_id) else { continue };
        if slot >= out.len() {
            continue;
        }
        let events = compile_lane(lane, map);
        if events.is_empty() {
            continue;
        }
        out[slot] = Some(Arc::new(events));
    }
    out
}

/// One host param write the driver decided is due. `format` is carried so
/// the tick path never has to take the session lock to look it up.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamWrite {
    pub instance: String,
    pub format: String,
    pub index: u32,
    pub value: f32,
}

struct CompiledParamLane {
    instance: String,
    format: String,
    index: u32,
    events: Vec<AbsParamEvent>,
    cursor: RampCursor,
    last_emitted: Option<f32>,
    /// Consecutive ticks whose value was suppressed as unchanged.
    ticks_held: u32,
}

/// Control-thread evaluator for plugin-parameter lanes. See scope ruling 2
/// in `docs/superpowers/plans/2026-08-14-automation-audible.md`: block-rate
/// (the engine control loop's ≤2 ms tick), never on the audio callback (a
/// host param write is a blocking round-trip), and HOST-ONLY — automation
/// overrides the stored knob value during playback; the document keeps what
/// the user set. Built fresh at every rebuild from the session's lanes +
/// plugin rows.
#[derive(Default)]
pub struct ParamAutomationDriver {
    lanes: Vec<CompiledParamLane>,
}

impl ParamAutomationDriver {
    /// Values closer than this to the last emitted one are not re-sent.
    pub const EPSILON: f32 = 1e-4;
    /// …but a value held for this many consecutive ticks is re-asserted
    /// anyway. At the engine control loop's ≤2 ms tick that is ~0.5 s.
    pub const REASSERT_TICKS: u32 = 250;

    pub fn empty() -> Self {
        Self { lanes: Vec::new() }
    }

    /// Two lanes naming the SAME instance and the SAME param index both
    /// compile, and the LAST one in `lanes` wins at every tick (the sort
    /// below is stable, so the input order survives it) — no blend, no
    /// error, exactly like `compile_gain_ramps`' track-gain equivalent.
    /// Nothing produces that today: the UI keeps one lane per target.
    pub fn new(
        lanes: &[AutomationLane],
        plugins: &crate::control::session::PluginDoc,
        map: &TempoMap,
    ) -> Self {
        let mut compiled = Vec::new();
        for lane in lanes {
            let Some(LaneTarget::PluginParam { instance, index }) = resolve_target(lane) else {
                continue;
            };
            let Some(row) = plugins.instances.iter().find(|r| r.id == instance) else {
                continue; // the instance is gone; the lane stays on disk, silent
            };
            let events = compile_lane(lane, map);
            if events.is_empty() {
                continue;
            }
            compiled.push(CompiledParamLane {
                instance,
                format: row.format.clone(),
                index,
                events,
                cursor: RampCursor::new(),
                last_emitted: None,
                ticks_held: 0,
            });
        }
        // Group by instance (review I-2) so `tick`'s output arrives in
        // contiguous per-instance runs and the caller can collapse a tick
        // into ONE host round-trip per plugin. Sorting is a per-rebuild
        // cost on the control thread; the tick stays a linear walk.
        compiled.sort_by(|a, b| a.instance.cmp(&b.instance));
        Self { lanes: compiled }
    }

    /// Build from already-compiled modulation param lanes (automation-track
    /// and clip-envelope sources — `lanes_from_doc` skips those). Events
    /// are native host units.
    pub fn from_param_specs(
        specs: &[crate::modulation::ParamLaneSpec],
        plugins: &crate::control::session::PluginDoc,
    ) -> Self {
        let mut compiled = Vec::new();
        for spec in specs {
            if spec.events.is_empty() {
                continue;
            }
            let Some(row) = plugins.instances.iter().find(|r| r.id == spec.instance) else {
                continue;
            };
            compiled.push(CompiledParamLane {
                instance: spec.instance.clone(),
                format: row.format.clone(),
                index: spec.index,
                events: spec.events.clone(),
                cursor: RampCursor::new(),
                last_emitted: None,
                ticks_held: 0,
            });
        }
        compiled.sort_by(|a, b| a.instance.cmp(&b.instance));
        Self { lanes: compiled }
    }

    /// Append v3/compat lanes whose (instance, index) is not already
    /// covered by compiled modulation specs — compiled wins (it is the
    /// expansion that knows about clips and spans).
    pub fn merge_uncovered_lanes(
        &mut self,
        lanes: &[AutomationLane],
        plugins: &crate::control::session::PluginDoc,
        map: &TempoMap,
    ) {
        let covered: std::collections::HashSet<(String, u32)> = self
            .lanes
            .iter()
            .map(|l| (l.instance.clone(), l.index))
            .collect();
        let extra = Self::new(lanes, plugins, map);
        for lane in extra.lanes {
            if !covered.contains(&(lane.instance.clone(), lane.index)) {
                self.lanes.push(lane);
            }
        }
        self.lanes.sort_by(|a, b| a.instance.cmp(&b.instance));
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    /// Append every write due at `position` into `out` (cleared first).
    pub fn tick(&mut self, position: u64, out: &mut Vec<ParamWrite>) {
        out.clear();
        for l in self.lanes.iter_mut() {
            let Some(v) = l.cursor.value(&l.events, position) else {
                continue;
            };
            if let Some(prev) = l.last_emitted {
                if (v - prev).abs() <= Self::EPSILON {
                    // Suppress — but only for a bounded stretch (review
                    // I-3). The host is not ours alone: a knob drag
                    // (`execute_host_forward`) or the plugin's own GUI can
                    // move this param behind our back, and a flat lane
                    // would then never correct it. Re-asserting a held
                    // value keeps "automation overrides the knob during
                    // playback" true without spamming the host.
                    l.ticks_held += 1;
                    if l.ticks_held < Self::REASSERT_TICKS {
                        continue;
                    }
                }
            }
            l.ticks_held = 0;
            l.last_emitted = Some(v);
            out.push(ParamWrite {
                instance: l.instance.clone(),
                format: l.format.clone(),
                index: l.index,
                value: v,
            });
        }
    }
}

/// A live-instrument wrapper applying a compiled gain curve to its inner
/// node's output, sample-accurately (the ONE proven end-to-end automation
/// path of this round; the same wrapper shape serves any scalar param once
/// nodes expose param rings — contract 5).
///
/// RT-safety: `process` does one binary search per block plus O(1) work per
/// frame; no allocation, no locks. The wrapper scales what the inner node
/// ADDED to the (zeroed) scratch block — exactly the `render_live` calling
/// convention.
///
/// Position: `mixer::render_live` re-seeds the cursor with the run's
/// absolute base position via [`LiveInstrument::set_block_context`] before
/// every processed run (seek/loop safe); the internal cursor (seeded via
/// [`GainAutomatedNode::new`] / [`GainAutomatedNode::sync_position`] and
/// advanced per frame) covers callers that bypass the seam.
pub struct GainAutomatedNode {
    inner: Box<dyn LiveInstrument>,
    events: Arc<Vec<AbsParamEvent>>,
    /// Absolute engine-sample position of the next frame `process` renders.
    pos: u64,
}

impl GainAutomatedNode {
    pub fn new(inner: Box<dyn LiveInstrument>, events: Arc<Vec<AbsParamEvent>>, pos: u64) -> Self {
        Self { inner, events, pos }
    }

    /// Re-seed the position cursor (CONTROL THREAD, only while no published
    /// snapshot references the node — the `LiveNodeCell` contract).
    pub fn sync_position(&mut self, pos: u64) {
        self.pos = pos;
    }

    pub fn position(&self) -> u64 {
        self.pos
    }
}

impl AudioProcessor for GainAutomatedNode {
    fn prepare(&mut self, sample_rate: u32, max_block: usize) {
        self.inner.prepare(sample_rate, max_block);
    }

    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        self.inner.process(io);
        let frames = io.frames();
        let ev = &self.events[..];
        if ev.is_empty() {
            self.pos = self.pos.saturating_add(frames as u64);
            return;
        }
        let ch = io.channels.max(1);
        let mut idx = ev.partition_point(|e| e.sample <= self.pos);
        for f in 0..frames {
            let p = self.pos + f as u64;
            while idx < ev.len() && ev[idx].sample <= p {
                idx += 1;
            }
            let g = segment_value(ev, idx, p);
            let base = f * ch;
            for c in 0..ch {
                io.samples[base + c] *= g;
            }
        }
        self.pos = self.pos.saturating_add(frames as u64);
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

impl LiveInstrument for GainAutomatedNode {
    fn queue_event(&mut self, ev: BlockNoteEvent) -> bool {
        self.inner.queue_event(ev)
    }

    fn all_notes_off(&mut self) {
        self.inner.all_notes_off();
    }

    /// The `render_live` seam: re-seed the ramp cursor with the run's true
    /// timeline position (seek/loop safe; RT-safe — two stores + forward).
    fn set_block_context(&mut self, base_pos: u64, discontinuity: bool) {
        self.pos = base_pos;
        self.inner.set_block_context(base_pos, discontinuity);
    }
}

// ---------------------------------------------------------------------------
// Commands (Plan E Task 10: lanes moved into `Session`; both commands go
// through the transaction channel / a plain locked read, no more lazy
// resync, no more auto-persist-inside-the-lock)
// ---------------------------------------------------------------------------

/// All automation lanes (points inline — GET is per-project, not
/// per-frame). PURE read under the session lock: no sync, no `loaded_dir`,
/// no disk I/O — epochs (project open/create) adopt lanes into
/// `session.automation` eagerly (`adopt_open_project`), so by the time any
/// command runs the session already reflects the open project.
#[tauri::command]
pub fn automation_get(
    control: State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<Vec<AutomationLane>, String> {
    Ok(control.automation_lanes())
}

/// Upsert one lane (batch-shaped, D-03: one invoke carries the lane's FULL
/// point set — an edit gesture, never one invoke per point). An empty
/// `points` array deletes the lane. Commits `Op::AutomationSetLane` through
/// the transaction channel (undo/redo-eligible, `persist.automation`
/// executed post-lock by `ControlPlane::execute_persist`). Returns the
/// updated lane list.
#[tauri::command]
pub fn automation_set(
    lane: AutomationLane,
    control: State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<Vec<AutomationLane>, String> {
    control.set_automation_lane(lane, crate::control::op::TxMeta::user("edit automation"))?;
    Ok(control.automation_lanes())
}

/// Records new track-gain automation points while the transport plays and
/// a track's mode is Write/Touch/Latch (spec
/// `docs/superpowers/specs/2026-08-18-automation-write-touch-latch-design.md`
/// §4). CONTROL THREAD ONLY, same reasoning as `ParamAutomationDriver`:
/// ticks are musical time, and the caller (Task 9/10) is what converts a
/// sample-domain playhead position to `tick` via `TempoMap` before calling
/// `sample`. One instance lives for the process; per-track in-progress
/// buffers are looked up by track id.
#[derive(Default)]
pub struct AutomationRecorder {
    in_progress: std::collections::HashMap<String, Vec<AutomationPoint>>,
    /// Latch-only: once a track has been touched during the current pass,
    /// it keeps recording every subsequent tick — even after `touched`
    /// goes back to false — until `finish` clears this. Touch has no
    /// equivalent: it stops the instant `touched` goes false, which is why
    /// this bookkeeping exists only for Latch.
    latch_armed: std::collections::HashMap<String, bool>,
    /// Last value observed while the user was actually touching the
    /// control. Latch holds THIS value after release; it must not keep
    /// reading the shared live fader, which another control source may
    /// change behind the gesture's back.
    latch_values: std::collections::HashMap<String, f32>,
}

impl AutomationRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sample(&mut self, track_id: &str, tick: u32, mode: AutomationMode, touched: bool, value: f32) {
        let value = match mode {
            AutomationMode::Write => value,
            AutomationMode::Touch if touched => value,
            AutomationMode::Touch => return,
            AutomationMode::Latch => {
                let armed = self.latch_armed.entry(track_id.to_string()).or_insert(false);
                if touched {
                    *armed = true;
                    self.latch_values.insert(track_id.to_string(), value);
                }
                if !*armed {
                    return;
                }
                self.latch_values.get(track_id).copied().unwrap_or(value)
            }
            AutomationMode::Off | AutomationMode::Read => return,
        };

        let points = self.in_progress.entry(track_id.to_string()).or_default();
        let extends_existing_hold =
            points.len() >= 2 && points[points.len() - 2].value == value;
        if let Some(last) = points.last_mut() {
            if last.tick == tick {
                last.value = value;
                return;
            }
            if last.value == value {
                // A held segment needs its first AND latest endpoint so its
                // recorded range extends to release/stop, but no point for
                // every 2 ms control tick. Keep at most those two points.
                if extends_existing_hold {
                    last.tick = tick;
                } else {
                    points.push(AutomationPoint { tick, value });
                }
                return;
            }
        }
        points.push(AutomationPoint { tick, value });
    }

    /// Snapshot a pass for an atomic commit attempt without consuming it.
    /// `finish` is called only after the transaction succeeds; ordinary
    /// commit errors therefore leave the points available for retry.
    pub fn is_latch_armed(&self, track_id: &str) -> bool {
        self.latch_armed.get(track_id).copied().unwrap_or(false)
    }

    pub fn pending(&self, track_id: &str) -> Option<Vec<AutomationPoint>> {
        self.in_progress.get(track_id).filter(|p| !p.is_empty()).cloned()
    }

    pub fn finish(&mut self, track_id: &str) -> Option<Vec<AutomationPoint>> {
        self.latch_armed.remove(track_id);
        self.latch_values.remove(track_id);
        let points = self.in_progress.remove(track_id)?;
        if points.is_empty() { None } else { Some(points) }
    }

    /// A document swap invalidates every buffered track id, tick and lane
    /// target at once. Nothing from the old epoch may be committed into the
    /// newly opened project.
    pub fn reset(&mut self) {
        self.in_progress.clear();
        self.latch_armed.clear();
        self.latch_values.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mixer;
    use crate::audio::project;
    use crate::audio::rt::{LiveNodeCell, LiveSource, ParamTable, RtGraph, RtTrack, MAX_LIVE_BLOCK};
    use crate::audio::transport::LoopSpec;
    use crate::midi::schedule::AbsNoteEvent;
    use crate::midi::synth::PolySynth;
    use crate::midi::types::TempoEvent;

    fn lane(points: Vec<AutomationPoint>) -> AutomationLane {
        AutomationLane {
            id: uuid::Uuid::new_v4().to_string(),
            target_node: "track:t1".into(),
            param_id: 0,
            points,
        }
    }

    // ---- chunk format ------------------------------------------------------

    #[test]
    fn point_chunk_roundtrip_and_note_records_skipped() {
        let points = vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 960, value: 0.25 },
            AutomationPoint { tick: 3840, value: 0.0 },
        ];
        let bytes = encode_points(960, &points);
        assert_eq!(bytes.len(), 16 + 3 * 16, "AMEV 16-byte records");
        let (ppq, back) = decode_points(&bytes).unwrap();
        assert_eq!(ppq, 960);
        assert_eq!(back, points);

        // A chunk of NOTE records yields no automation points (and vice
        // versa: decode_notes skips automation records — shared container).
        let notes = crate::midi::events::encode_notes(
            960,
            &[crate::midi::types::MidiNote {
                tick: 0,
                length_ticks: 10,
                key: 60,
                velocity: 100,
                channel: 0,
                note_id: crate::ids::NoteId(1),
            }],
            2,
        );
        assert!(decode_points(&notes).unwrap().1.is_empty());
        let decoded = crate::midi::events::decode_notes(&bytes).unwrap();
        assert!(decoded.notes.is_empty());

        assert!(decode_points(&[0u8; 8]).is_err(), "garbage rejected");
    }

    /// Reviewer finding 1 (CRITICAL): a mask claiming a column is present
    /// with its block missing (truncated away) must Err, mirroring
    /// `events::decode_notes` — even though automation doesn't interpret
    /// the column's content, the mask/block mismatch is structural
    /// corruption on its own.
    #[test]
    fn decode_points_rejects_mask_claiming_a_column_whose_block_is_missing() {
        let notes = vec![crate::midi::types::MidiNote {
            tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0,
            note_id: crate::ids::NoteId(1),
        }];
        let mut bytes = crate::midi::events::encode_notes(960, &notes, 2);
        // Truncate away the appended note-id block WITHOUT clearing the
        // mask bit — the mask still claims 0x0002 is present.
        bytes.truncate(16 + 1 * 16);
        let mask = u16::from_le_bytes([bytes[6], bytes[7]]);
        assert_eq!(mask & crate::midi::events::COLUMNS_NOTE_ID, crate::midi::events::COLUMNS_NOTE_ID);
        assert!(decode_points(&bytes).is_err());
    }

    // ---- persistence -------------------------------------------------------

    #[test]
    fn lanes_save_load_roundtrip_with_chunks_gc_and_midi_interplay() {
        let parent = std::env::temp_dir().join(format!(
            "aura-automation-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&parent).unwrap();
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let mut lanes = vec![
            lane(vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 3840, value: 0.0 },
            ]),
            lane(vec![]), // empty lane row, no chunk
        ];
        save_into_project(&dir, &lanes).unwrap();

        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 2, "upgraded to v2");
        let rows = raw["automation"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let pref = rows[0]["pointsRef"].as_str().unwrap();
        assert!(pref.starts_with("automation/") && pref.ends_with(".bin"));
        assert!(dir.join(pref).exists());
        assert!(rows[0].get("points").is_none(), "points NEVER inline");
        assert!(rows[1].get("pointsRef").is_none());

        // A MIDI save must not clobber the automation array or its chunks
        // (separate dirs, RMW project.json writers).
        let midi = crate::midi::MidiStore::default();
        crate::midi::persist::save_into_project(&dir, &midi).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["automation"].as_array().unwrap().len(), 2);
        assert!(dir.join(pref).exists(), "automation chunk untouched by midi GC");

        let loaded = load_lanes(&dir).unwrap().expect("automation present");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].points, lanes[0].points);
        assert_eq!(loaded[0].param_id, 0);
        assert!(loaded[1].points.is_empty());

        // Resave GCs the stale chunk.
        lanes[0].points[1].value = 0.5;
        save_into_project(&dir, &lanes).unwrap();
        let chunks: Vec<_> = fs::read_dir(automation_dir(&dir))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bin"))
            .collect();
        assert_eq!(chunks.len(), 1, "stale chunk GC'd");
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn malicious_points_ref_and_corrupt_chunks_degrade_gracefully() {
        let parent = std::env::temp_dir().join(format!(
            "aura-automation-evil-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&parent).unwrap();
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        save_into_project(&dir, &[]).unwrap();
        let mut raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        raw["automation"] = serde_json::json!([
            {"id": "a", "targetNode": "n", "paramId": 0,
             "pointsRef": "automation/../../etc/passwd.bin"},
            {"id": "b", "targetNode": "n", "paramId": 1,
             "pointsRef": "automation/missing.bin"}
        ]);
        fs::write(dir.join("project.json"), serde_json::to_vec(&raw).unwrap()).unwrap();
        let lanes = load_lanes(&dir).unwrap().unwrap();
        assert_eq!(lanes.len(), 2, "lanes kept");
        assert!(lanes.iter().all(|l| l.points.is_empty()), "refs ignored");
        let _ = fs::remove_dir_all(&parent);
    }

    // ---- tick -> sample compilation ---------------------------------------

    #[test]
    fn compile_lane_maps_ticks_through_the_tempo_map() {
        // 120 bpm then 60 bpm at tick 3840 (one bar @960ppq).
        let map = TempoMap::new(
            960,
            vec![
                TempoEvent { tick: 0, bpm: 120.0 },
                TempoEvent { tick: 3840, bpm: 60.0 },
            ],
            48_000,
        )
        .unwrap();
        let l = lane(vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 3840, value: 0.5 },
            AutomationPoint { tick: 3840 + 960, value: 0.0 },
        ]);
        let ev = compile_lane(&l, &map);
        assert_eq!(
            ev,
            vec![
                AbsParamEvent { sample: 0, value: 1.0 },
                AbsParamEvent { sample: 96_000, value: 0.5 },
                AbsParamEvent { sample: 96_000 + 48_000, value: 0.0 },
            ]
        );
    }

    #[test]
    fn value_at_interpolates_and_holds_ends() {
        let ev = vec![
            AbsParamEvent { sample: 100, value: 1.0 },
            AbsParamEvent { sample: 200, value: 0.5 },
        ];
        assert_eq!(value_at(&[], 50), None);
        assert_eq!(value_at(&ev, 0), Some(1.0), "holds first before the curve");
        assert_eq!(value_at(&ev, 100), Some(1.0));
        assert_eq!(value_at(&ev, 150), Some(0.75), "linear midpoint");
        assert_eq!(value_at(&ev, 199), Some(1.0 - 0.5 * 0.99));
        assert_eq!(value_at(&ev, 200), Some(0.5));
        assert_eq!(value_at(&ev, 10_000), Some(0.5), "holds last after the curve");
    }

    // ---- lane targets, the RT ramp cursor, the control-side param driver ----

    #[test]
    fn resolve_target_classifies_track_and_plugin_lanes() {
        let mut l = lane(vec![]);
        l.target_node = "track:t-1".into();
        l.param_id = TRACK_PARAM_GAIN;
        assert_eq!(resolve_target(&l), Some(LaneTarget::TrackGain("t-1".into())));

        // an unknown built-in param id is IGNORED, never guessed at
        l.param_id = 99;
        assert_eq!(resolve_target(&l), None);

        let mut p = lane(vec![]);
        p.target_node = "inst-1".into();
        p.param_id = 7;
        assert_eq!(
            resolve_target(&p),
            Some(LaneTarget::PluginParam { instance: "inst-1".into(), index: 7 })
        );
    }

    #[test]
    fn ramp_cursor_matches_value_at_forward_and_after_a_backward_jump() {
        let ev = vec![
            AbsParamEvent { sample: 100, value: 1.0 },
            AbsParamEvent { sample: 200, value: 0.5 },
        ];
        let mut c = RampCursor::new();
        assert_eq!(c.value(&[], 0), None, "empty curve leaves the param untouched");
        for s in [0u64, 50, 100, 150, 199, 200, 10_000] {
            assert_eq!(c.value(&ev, s), value_at(&ev, s), "forward walk at {s}");
        }
        // a loop wrap moves the position BACKWARD: the cursor must re-seed,
        // not keep its advanced index
        assert_eq!(c.value(&ev, 150), Some(0.75), "re-seeded after a backward jump");
        assert_eq!(c.value(&ev, 0), Some(1.0));
    }

    #[test]
    fn param_driver_emits_only_on_change_and_carries_the_host_format() {
        use crate::control::session::PluginDoc;
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut l = lane(vec![
            AutomationPoint { tick: 0, value: 0.0 },
            AutomationPoint { tick: 3840, value: 1.0 }, // 96_000 samples
        ]);
        l.target_node = "inst-1".into();
        l.param_id = 7;

        let mut doc = PluginDoc::default();
        doc.instances.push(crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(),
            uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "active".into(),
            track_id: None,
        });

        let mut d = ParamAutomationDriver::new(&[l], &doc, &map);
        assert!(!d.is_empty());
        let mut out = Vec::new();

        d.tick(0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].format, "lv2");
        assert_eq!(out[0].index, 7);
        assert!((out[0].value - 0.0).abs() < 1e-6);

        d.tick(0, &mut out);
        assert!(out.is_empty(), "an unchanged value is not re-sent");

        d.tick(48_000, &mut out);
        assert_eq!(out.len(), 1);
        assert!((out[0].value - 0.5).abs() < 1e-3, "halfway up the ramp: {}", out[0].value);
    }

    /// Task 9 review, I-3: the epsilon suppression above must not let a
    /// knob grab win the rest of the pass. Anything else that writes the
    /// same param behind the driver's back — the user dragging the knob
    /// (`execute_host_forward`), or the plugin's OWN GUI, which never
    /// touches our channel at all — leaves the host holding a value the
    /// driver thinks it already emitted. A HELD lane value must therefore
    /// be re-asserted periodically, or `drive_param_automation`'s
    /// "automation OVERRIDES the stored knob value during playback" is
    /// false whenever the curve is flat.
    #[test]
    fn param_driver_reasserts_a_held_value_so_a_knob_grab_cannot_win_the_pass() {
        use crate::control::session::PluginDoc;
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut l = lane(vec![
            AutomationPoint { tick: 0, value: 0.5 },
            AutomationPoint { tick: 3840, value: 0.5 }, // dead flat
        ]);
        l.target_node = "inst-1".into();
        l.param_id = 7;
        let mut doc = PluginDoc::default();
        doc.instances.push(crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(),
            uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "active".into(),
            track_id: None,
        });

        let mut d = ParamAutomationDriver::new(&[l], &doc, &map);
        let mut out = Vec::new();
        d.tick(0, &mut out);
        assert_eq!(out.len(), 1, "the first tick asserts the value");

        // The knob grab happens HERE, host-side, invisible to the driver.
        let mut emitted = 0usize;
        for i in 1..=ParamAutomationDriver::REASSERT_TICKS {
            d.tick(i as u64, &mut out);
            emitted += out.len();
        }
        assert_eq!(
            emitted, 1,
            "exactly one re-assert within REASSERT_TICKS ticks — not silence, not every tick"
        );
        assert!((out[0].value - 0.5).abs() < 1e-6, "and it re-asserts the LANE's value");

        // …and then it goes quiet again: the hold counter restarts, so a
        // held value costs one host round-trip per REASSERT_TICKS, not one
        // per tick from here on.
        let n = ParamAutomationDriver::REASSERT_TICKS as u64;
        let mut emitted = 0usize;
        for i in n + 1..2 * n {
            d.tick(i, &mut out);
            emitted += out.len();
        }
        assert_eq!(emitted, 0, "the hold counter restarts after a re-assert");
    }

    /// Task 9 review, I-2: a tick's writes come out GROUPED by instance, so
    /// the caller can issue one host call per plugin per tick instead of one
    /// per param — a CLAP param write is a blocking round-trip on the shared
    /// plugin-main thread. Lanes are authored in any order; the grouping is
    /// the driver's job, once per rebuild, not the tick's.
    #[test]
    fn param_driver_groups_its_writes_by_instance() {
        use crate::control::session::PluginDoc;
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut doc = PluginDoc::default();
        let mut lanes = Vec::new();
        // Interleaved on purpose: a, b, a, b.
        for (i, inst) in ["inst-a", "inst-b", "inst-a", "inst-b"].iter().enumerate() {
            let mut l = lane(vec![
                AutomationPoint { tick: 0, value: 0.0 },
                AutomationPoint { tick: 3840, value: 1.0 },
            ]);
            l.id = format!("l{i}");
            l.target_node = (*inst).into();
            l.param_id = i as u32;
            lanes.push(l);
        }
        for inst in ["inst-a", "inst-b"] {
            doc.instances.push(crate::plugins::PluginInstanceInfo {
                id: inst.into(),
                uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(),
                format: "lv2".into(),
                status: "active".into(),
                track_id: None,
            });
        }

        let mut d = ParamAutomationDriver::new(&lanes, &doc, &map);
        let mut out = Vec::new();
        d.tick(48_000, &mut out);
        assert_eq!(out.len(), 4, "all four lanes are due");
        let runs = out.chunk_by(|a, b| a.instance == b.instance).count();
        assert_eq!(runs, 2, "one contiguous run per instance, not {runs} — got {out:?}");
    }

    #[test]
    fn param_driver_skips_lanes_whose_instance_is_gone_and_track_lanes() {
        use crate::control::session::PluginDoc;
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut orphan = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]);
        orphan.target_node = "inst-missing".into();
        orphan.param_id = 1;
        let track_lane = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]); // "track:t1"
        let d = ParamAutomationDriver::new(&[orphan, track_lane], &PluginDoc::default(), &map);
        assert!(d.is_empty(), "no plugin lane resolves");
    }

    // ---- the ramp applied by a node, sample-exact --------------------------

    /// Constant-1.0 source: makes the wrapper's applied gain directly
    /// observable per sample.
    struct ConstSource;
    impl AudioProcessor for ConstSource {
        fn prepare(&mut self, _r: u32, _b: usize) {}
        fn process(&mut self, io: &mut ProcessBlock<'_>) {
            for s in io.samples.iter_mut() {
                *s += 1.0;
            }
        }
        fn reset(&mut self) {}
    }
    impl LiveInstrument for ConstSource {
        fn queue_event(&mut self, _ev: BlockNoteEvent) -> bool {
            true
        }
        fn all_notes_off(&mut self) {}
    }

    #[test]
    fn gain_node_applies_ramps_sample_accurately_across_blocks() {
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 100, value: 1.0 },
            AbsParamEvent { sample: 200, value: 0.5 },
        ]);
        let mut node = GainAutomatedNode::new(Box::new(ConstSource), ev.clone(), 0);
        node.prepare(48_000, 512);

        // Two 128-frame blocks followed by one 512-frame block: the ramp
        // must be continuous across block boundaries and EXACT per sample.
        let mut rendered = Vec::new();
        for frames in [128usize, 128, 512] {
            let mut buf = vec![0.0f32; frames * 2];
            let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: 48_000, steady: None };
            node.process(&mut io);
            rendered.extend_from_slice(&buf);
        }
        for (i, frame) in rendered.chunks_exact(2).enumerate() {
            let expected = value_at(&ev, i as u64).unwrap();
            assert!(
                (frame[0] - expected).abs() < 1e-6 && (frame[1] - expected).abs() < 1e-6,
                "sample {i}: got {} want {expected}",
                frame[0]
            );
        }
        assert_eq!(node.position(), 128 + 128 + 512);
    }

    // ---- END-TO-END PROOF: PolySynth gain automated over one bar, rendered
    // ---- through the real RT path (mixer::render), verified in the output.

    fn mono_of(buf: &[f32]) -> Vec<f32> {
        buf.iter().step_by(2).copied().collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt()
    }

    fn render_bar(automated: bool) -> Vec<f32> {
        const RATE: u32 = 48_000;
        const BAR: usize = 96_000; // 3840 ticks @120bpm/960ppq = 2 s

        // The persisted-shape lane: gain 1.0 -> 0.0 across one bar, compiled
        // tick -> sample through the TempoMap (the real control-side path).
        let map = TempoMap::from_v1(120.0, RATE).unwrap();
        let l = lane(vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 3840, value: 0.0 },
        ]);
        let ev = compile_lane(&l, &map);
        assert_eq!(ev[1].sample, BAR as u64);

        let mut synth = PolySynth::new();
        synth.prepare(RATE, MAX_LIVE_BLOCK);
        let node: Box<dyn LiveInstrument> = if automated {
            Box::new(GainAutomatedNode::new(Box::new(synth), Arc::new(ev), 0))
        } else {
            Box::new(synth)
        };
        // A4 held across the whole bar (off far beyond the render window).
        let events = vec![
            AbsNoteEvent { sample: 0, key: 69, velocity: 110, channel: 0 },
            AbsNoteEvent { sample: 200_000, key: 69, velocity: 0, channel: 0 },
        ];
        let track = RtTrack {
            slot: 0,
            clips: Vec::new(),
            live: Some(LiveSource { node: LiveNodeCell::new(node), events: Arc::new(events) }),
            inserts: Vec::new(),
            pdc: None,
        };
        let mut g = RtGraph::new(vec![track], 1, Arc::new(ParamTable::default()));
        let mut out = vec![0.0f32; BAR * 2];
        let mut pos = 0u64;
        for chunk in out.chunks_mut(512 * 2) {
            mixer::render(&mut g, pos, &LoopSpec::OFF, chunk, 2, RATE, false, None);
            pos += (chunk.len() / 2) as u64;
        }
        mono_of(&out)
    }

    #[test]
    fn polysynth_gain_automation_over_a_bar_is_audible_in_rendered_output() {
        let automated = render_bar(true);
        let control = render_bar(false);

        // Windows centered at 12.5%, 50% and 92.5% of the bar; the linear
        // gain ramp scales the sine's RMS by (1 - center/96000).
        let early = rms(&automated[9_600..14_400]); // center 12000 -> g 0.875
        let mid = rms(&automated[45_600..50_400]); // center 48000 -> g 0.500
        let late = rms(&automated[86_400..91_200]); // center 88800 -> g 0.075
        assert!(early > 0.02, "automated render audible early ({early})");
        let mid_ratio = mid / early;
        assert!(
            (mid_ratio - 0.5 / 0.875).abs() < 0.05,
            "mid/early RMS follows the ramp: got {mid_ratio}, want ~{}",
            0.5 / 0.875
        );
        let late_ratio = late / early;
        assert!(
            (late_ratio - 0.075 / 0.875).abs() < 0.03,
            "late/early RMS follows the ramp: got {late_ratio}, want ~{}",
            0.075 / 0.875
        );
        // At the end of the bar the gain reaches 0: digital near-silence.
        let tail_peak = automated[95_500..]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(tail_peak < 0.005, "gain reaches zero at the bar end ({tail_peak})");

        // The unautomated control stays flat — the ramp came from the lane,
        // not from the synth's own envelope.
        let c_early = rms(&control[9_600..14_400]);
        let c_late = rms(&control[86_400..91_200]);
        assert!(
            (c_late / c_early - 1.0).abs() < 0.05,
            "control render is flat (ratio {})",
            c_late / c_early
        );
    }

    /// ARCHITECT-MERGE ITEM 4 — the `render_live` position seam: ramps are
    /// POSITION-TRUE, not frame-count-true. A `ConstSource` inner makes the
    /// applied gain directly observable through the REAL `mixer::render`
    /// path (center pan = 1/√2 on each channel).
    ///
    /// * Seek mid-ramp: after a jump to 72000 (75% of the bar) the rendered
    ///   gain matches the lane value AT THE SEEK TARGET — without the seam
    ///   the node's internal cursor would still sit at the pre-seek frame
    ///   count and render ~0.99.
    /// * Loop wrap: a single callback block crossing the loop end re-seeds
    ///   the cursor at the loop start mid-block.
    #[test]
    fn gain_automation_survives_seek_and_loop_wrap_through_render_live() {
        const RATE: u32 = 48_000;
        const K: f32 = std::f32::consts::FRAC_1_SQRT_2; // center-pan gain

        // 1.0 -> 0.0 across one 2 s bar (96000 samples), real tick->sample
        // compilation.
        let map = TempoMap::from_v1(120.0, RATE).unwrap();
        let l = lane(vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 3840, value: 0.0 },
        ]);
        let ev = Arc::new(compile_lane(&l, &map));
        assert_eq!(ev[1].sample, 96_000);

        let node: Box<dyn LiveInstrument> =
            Box::new(GainAutomatedNode::new(Box::new(ConstSource), ev.clone(), 0));
        let track = RtTrack {
            slot: 0,
            clips: Vec::new(),
            live: Some(LiveSource { node: LiveNodeCell::new(node), events: Arc::new(vec![]) }),
            inserts: Vec::new(),
            pdc: None,
        };
        let mut g = RtGraph::new(vec![track], 1, Arc::new(ParamTable::default()));

        let expect = |buf: &[f32], base: u64, msg: &str| {
            for (i, frame) in buf.chunks_exact(2).enumerate() {
                let want = value_at(&ev, base + i as u64).unwrap() * K;
                assert!(
                    (frame[0] - want).abs() < 1e-5,
                    "{msg}: sample {i} got {} want {want}",
                    frame[0]
                );
            }
        };

        // Continuous render from 0.
        let mut buf = vec![0.0f32; 512 * 2];
        mixer::render(&mut g, 0, &LoopSpec::OFF, &mut buf, 2, RATE, false, None);
        expect(&buf, 0, "from start");

        // SEEK to 72000 (gain 0.25 there): discontinuity block.
        mixer::render(&mut g, 72_000, &LoopSpec::OFF, &mut buf, 2, RATE, true, None);
        expect(&buf, 72_000, "after seek mid-ramp");

        // LOOP [24000, 96000): one block crossing the wrap. First 256
        // frames finish the ramp at ~0, then the cursor re-seeds at the
        // loop start (gain 0.75) mid-block.
        let lp = LoopSpec { enabled: true, start: 24_000, end: 96_000 };
        let mut wrap_buf = vec![0.0f32; 1024 * 2];
        mixer::render(&mut g, 95_744, &lp, &mut wrap_buf, 2, RATE, true, None);
        expect(&wrap_buf[..256 * 2], 95_744, "pre-wrap tail");
        expect(&wrap_buf[256 * 2..], 24_000, "post-wrap re-seeded at loop start");
    }

    #[test]
    fn normalize_rejects_bad_lanes_and_sorts_points() {
        let mut l = lane(vec![
            AutomationPoint { tick: 960, value: 0.5 },
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 960, value: 0.75 },
        ]);
        normalize_lane(&mut l).unwrap();
        assert_eq!(
            l.points,
            vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 960, value: 0.75 },
            ],
            "sorted, duplicate tick keeps the later value"
        );
        let mut bad = lane(vec![AutomationPoint { tick: 0, value: f32::NAN }]);
        assert!(normalize_lane(&mut bad).is_err());
        let mut no_target = lane(vec![]);
        no_target.target_node.clear();
        assert!(normalize_lane(&mut no_target).is_err());
    }

    // ---- Plan E Task 10: session move ---------------------------------

    // `automation_get`'s purity (no sync, no `loaded_dir`, no disk I/O, no
    // project-dir requirement) is covered by
    // `automation_get_is_a_pure_session_read_no_disk_no_project_dir` in
    // `control/mod.rs`'s test module (Task 10 fix round 1: that test must
    // call the REAL production entry point, `ControlPlane::automation_lanes`
    // — `recording_control_plane`, the fixture it needs, is private to
    // `control/mod.rs`'s own test module and not visible from here).

    /// PROJECT-ADOPTION SEAM: `adopt_open_project` now writes into
    /// `session.automation.lanes` (through the registered session global)
    /// instead of the retired standalone `AutomationStore`. Inert unless a
    /// session is registered. NOTE (merge of Tasks 7+10): `register_session`
    /// is first-registration-wins and `MidiState::shared` (midi/mod.rs) also
    /// registers — other tests in this binary may win the race, so this test
    /// asserts against WHOEVER is registered (via `registered_session()`)
    /// and identifies its lane by its unique uuid id rather than assuming an
    /// otherwise-empty session.
    #[test]
    fn adopt_open_project_writes_into_the_registered_session() {
        let parent = std::env::temp_dir().join(format!(
            "aura-automation-adopt-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&parent).unwrap();
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let saved = vec![lane(vec![AutomationPoint { tick: 0, value: 1.0 }])];
        save_into_project(&dir, &saved).unwrap();

        let session = Arc::new(Mutex::new(crate::control::Session::new(
            crate::audio::types::Store::default(),
            crate::midi::MidiStore::default(),
        )));
        register_session(session.clone());
        // First registration wins: use whichever session actually holds the
        // global (ours, or one registered by another test in this binary).
        let registered = registered_session().expect("a session is registered");

        adopt_open_project(&dir);
        let lanes = registered.lock().automation.lanes.clone();
        let adopted = lanes
            .iter()
            .find(|l| l.id == saved[0].id)
            .expect("the saved lane (unique uuid id) was adopted from disk");
        assert_eq!(adopted.points, saved[0].points);

        let _ = fs::remove_dir_all(&parent);
    }

    // ---- Task 8: compile_gain_ramps (slot indexing, control-side) ---------

    #[test]
    fn compile_gain_ramps_indexes_by_slot_and_skips_what_cannot_resolve() {
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut a = lane(vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 3840, value: 0.0 },
        ]);
        a.target_node = "track:t-1".into();
        let mut orphan = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]);
        orphan.target_node = "track:t-gone".into();
        let mut plugin_lane = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]);
        plugin_lane.target_node = "inst-1".into();
        let mut empty = lane(vec![]);
        empty.target_node = "track:t-2".into();

        let slot_of = |id: &str| match id {
            "t-1" => Some(0usize),
            "t-2" => Some(1),
            _ => None,
        };
        let ramps = compile_gain_ramps(&[a, orphan, plugin_lane, empty], &map, 2, &slot_of);
        assert_eq!(ramps.len(), 2);
        let r0 = ramps[0].as_ref().expect("t-1 has a compiled ramp");
        assert_eq!(r0[0], AbsParamEvent { sample: 0, value: 1.0 });
        assert_eq!(r0[1], AbsParamEvent { sample: 96_000, value: 0.0 });
        assert!(ramps[1].is_none(), "an empty lane compiles to no ramp");
    }

    // ---- Task 6: AutomationRecorder -----------------------------------------

    #[test]
    fn write_mode_records_every_tick_unconditionally() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 0, AutomationMode::Write, false, 0.5);
        rec.sample("t-1", 10, AutomationMode::Write, false, 0.75);
        rec.sample("t-1", 20, AutomationMode::Write, false, 1.0);
        let points = rec.finish("t-1").expect("write mode must record");
        assert_eq!(
            points,
            vec![
                AutomationPoint { tick: 0, value: 0.5 },
                AutomationPoint { tick: 10, value: 0.75 },
                AutomationPoint { tick: 20, value: 1.0 },
            ]
        );
    }

    #[test]
    fn touch_mode_records_only_while_touched() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 0, AutomationMode::Touch, false, 0.2); // not touched: ignored
        rec.sample("t-1", 10, AutomationMode::Touch, true, 0.6);
        rec.sample("t-1", 20, AutomationMode::Touch, true, 0.8);
        rec.sample("t-1", 30, AutomationMode::Touch, false, 0.9); // released: ignored
        let points = rec.finish("t-1").expect("touch mode recorded while touched");
        assert_eq!(
            points,
            vec![
                AutomationPoint { tick: 10, value: 0.6 },
                AutomationPoint { tick: 20, value: 0.8 },
            ]
        );
    }

    #[test]
    fn off_and_read_never_record() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 0, AutomationMode::Off, true, 0.5);
        rec.sample("t-1", 10, AutomationMode::Read, true, 0.5);
        assert_eq!(rec.finish("t-1"), None);
    }

    #[test]
    fn latch_mode_keeps_recording_after_release_until_finish() {
        // Unlike Touch, Latch must NOT stop recording when `touched` goes back
        // to false — it keeps sampling (holding whatever value it's given,
        // which in production is the live fader value sitting still) until the
        // pass truly ends at `finish`.
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 0, AutomationMode::Latch, false, 0.2); // not yet armed: ignored
        rec.sample("t-1", 10, AutomationMode::Latch, true, 0.6); // touched: arms it
        rec.sample("t-1", 20, AutomationMode::Latch, false, 0.6); // released: KEEPS recording
        rec.sample("t-1", 30, AutomationMode::Latch, false, 0.6); // still recording
        let points = rec.finish("t-1").expect("latch mode must keep recording after release");
        assert_eq!(
            points,
            vec![
                AutomationPoint { tick: 10, value: 0.6 },
                AutomationPoint { tick: 30, value: 0.6 },
            ]
        );
    }

    #[test]
    fn latch_arming_does_not_carry_across_finish() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 0, AutomationMode::Latch, true, 0.5);
        rec.finish("t-1");
        // A second pass must start un-armed again.
        rec.sample("t-1", 100, AutomationMode::Latch, false, 0.9);
        assert_eq!(rec.finish("t-1"), None, "a fresh Latch pass must not record before being touched again");
    }

    #[test]
    fn finish_with_no_samples_returns_none() {
        let mut rec = AutomationRecorder::new();
        assert_eq!(rec.finish("never-touched"), None);
    }

    #[test]
    fn finish_clears_the_track_so_a_second_pass_starts_fresh() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 0, AutomationMode::Write, false, 0.1);
        assert!(rec.finish("t-1").is_some());
        assert_eq!(rec.finish("t-1"), None, "a second finish with no new samples must be None");
    }

    #[test]
    fn held_values_keep_only_the_segment_endpoints() {
        let mut rec = AutomationRecorder::new();
        for tick in 0..10_000 {
            rec.sample("t-1", tick, AutomationMode::Write, false, 0.5);
        }
        assert_eq!(
            rec.finish("t-1").unwrap(),
            vec![
                AutomationPoint { tick: 0, value: 0.5 },
                AutomationPoint { tick: 9_999, value: 0.5 },
            ],
            "a long held fader must not allocate one point per control tick"
        );
    }

    #[test]
    fn latch_holds_the_last_touched_value_when_the_live_fader_changes_elsewhere() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 10, AutomationMode::Latch, true, 0.6);
        rec.sample("t-1", 20, AutomationMode::Latch, false, 0.2);
        rec.sample("t-1", 30, AutomationMode::Latch, false, 0.9);
        assert_eq!(
            rec.finish("t-1").unwrap(),
            vec![
                AutomationPoint { tick: 10, value: 0.6 },
                AutomationPoint { tick: 30, value: 0.6 },
            ]
        );
    }

    #[test]
    fn reset_drops_points_and_latch_state_from_the_previous_document() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 10, AutomationMode::Latch, true, 0.6);
        rec.reset();
        rec.sample("t-1", 20, AutomationMode::Latch, false, 0.9);
        assert_eq!(rec.finish("t-1"), None);
    }

    #[test]
    fn tracks_are_independent() {
        let mut rec = AutomationRecorder::new();
        rec.sample("t-1", 0, AutomationMode::Write, false, 0.1);
        rec.sample("t-2", 0, AutomationMode::Write, false, 0.9);
        assert_eq!(rec.finish("t-1").unwrap(), vec![AutomationPoint { tick: 0, value: 0.1 }]);
        assert_eq!(rec.finish("t-2").unwrap(), vec![AutomationPoint { tick: 0, value: 0.9 }]);
    }
}
