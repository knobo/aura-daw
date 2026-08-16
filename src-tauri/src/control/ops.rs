//! Shared control-plane operations (tauri-free, testable standalone).
//!
//! These functions are the ONE implementation of each mutation: the frozen
//! per-command Tauri surface AND the MCP tool handlers both funnel here
//! (ARCHITECTURE §11). They operate on the engine-owned primitives
//! (`Store` + `ParamTable` + `SharedRt`) and follow the §10 constraints:
//! mix changes are param-table writes (never graph rebuilds); structural
//! changes are the caller's cue to send `ControlMsg::Rebuild`.

use std::sync::atomic::Ordering::Relaxed;

use serde::{Deserialize, Serialize};

use crate::audio::rt::SharedRt;
use crate::audio::types::{Store, TrackState, TransportState};

/// One batched mix change (op-log-shaped per debt D-03: a UI gesture or MCP
/// tool call carries MANY of these in one request). `None` = leave unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackMixChange {
    pub track_id: String,
    pub gain_db: Option<f64>,
    pub pan: Option<f64>,
    pub muted: Option<bool>,
    pub soloed: Option<bool>,
    pub armed: Option<bool>,
}

impl TrackMixChange {
    pub fn new(track_id: impl Into<String>) -> Self {
        Self {
            track_id: track_id.into(),
            gain_db: None,
            pan: None,
            muted: None,
            soloed: None,
            armed: None,
        }
    }
}

pub const TRACK_COLORS: [&str; 6] =
    ["#7c9cff", "#ff9c7c", "#7cffb0", "#e07cff", "#ffe07c", "#7cd8ff"];

/// Structural core: insert `track` into the store at `index` (clamped to
/// the current length, so an out-of-range index appends). No slot
/// allocation, no param reset — those stay in the `add_track` wrapper below
/// until Plan B moves slot bookkeeping to the command's effect layer
/// (round-2 §2.4). This is the part `control::session::apply_raw` reuses
/// for `Op::TrackAdd`, so both paths insert a row the same way.
pub(crate) fn insert_track(store: &mut Store, track: TrackState, index: usize) {
    let index = index.min(store.tracks.len());
    store.tracks.insert(index, track);
}

/// Build a fresh track row — everything `add_track` does EXCEPT inserting
/// it into `store.tracks`. Split out (Task 7) so `ControlPlane::add_track`
/// can insert the row through the transaction channel (`Op::TrackAdd`, via
/// `Session::transact`) instead of a direct `Vec::insert`.
///
/// Round-2 §2.4: slot allocation and the RT param reset are GONE — slots
/// are derived fresh from display order on every rebuild
/// (`types::derive_slots`) and a fresh row's params come from that same
/// rebuild's build-time population (unity gain / center pan / no flags),
/// an exact behavioral match for what the deleted `params.reset_slot` used
/// to write. There is therefore no `MAX_TRACKS` limit here either — the
/// const itself is gone (Task 7: `ParamTable` is sized per-graph). Returns
/// `(track, index)`; `index` is the row's intended position (end of the
/// current list).
pub(crate) fn new_track_row(
    store: &Store,
    name: Option<String>,
    kind: Option<String>,
) -> Result<(TrackState, usize), String> {
    let kind = kind.unwrap_or_else(|| "audio".into());
    if !matches!(kind.as_str(), "audio" | "midi" | "automation") {
        return Err(format!("unsupported track kind: {kind}"));
    }
    let n = store.tracks.len();
    let id = uuid::Uuid::new_v4().to_string();
    let track = TrackState {
        id: id.into(),
        name: name.unwrap_or_else(|| format!("Track {}", n + 1)),
        kind,
        gain_db: 0.0,
        pan: 0.0,
        muted: false,
        soloed: false,
        armed: false,
        color: TRACK_COLORS[n % TRACK_COLORS.len()].into(),
        instrument_id: None,
    };
    Ok((track, n))
}

/// Create a track in the store. STRUCTURAL — the caller must send
/// `ControlMsg::Rebuild` afterwards (the next rebuild derives the row's
/// slot and populates its params — round-2 §2.4). `kind`: "audio" (default)
/// or "midi" (phase 2) or "automation" (Track F; no mixer slot). "bus"
/// is reserved. Used directly by callers that
/// mutate the store outside the transaction channel (currently only
/// `seed_demo_project`, which builds several tracks under one lock before
/// any engine/event side effect); the frozen `add_track` COMMAND goes
/// through `ControlPlane::add_track` -> `commit` -> `new_track_row` instead.
pub fn add_track(
    store: &mut Store,
    name: Option<String>,
    kind: Option<String>,
) -> Result<TrackState, String> {
    let (track, index) = new_track_row(store, name, kind)?;
    insert_track(store, track.clone(), index);
    Ok(track)
}

/// The §4.3 Tx-tier sibling of [`add_track`]: builds the row (against the
/// transaction's read-only store view, `Tx::store`) and inserts it through
/// the transaction channel (`Op::TrackAdd`, via [`crate::control::session::Tx::apply`])
/// instead of a direct `Vec::insert` — so a caller composing several
/// mutations into ONE commit (e.g. `midi_import_file`'s per-clip
/// auto-created tracks, `seed_demo_project`'s three demo tracks) can create
/// tracks without a second lock or a second `project://changed`/rebuild.
/// Undo-able like any other op: `apply`'s computed inverse is a
/// `TrackRemove` for this exact row.
pub fn add_track_tx(
    tx: &mut crate::control::session::Tx<'_>,
    name: Option<String>,
    kind: Option<String>,
) -> Result<TrackState, String> {
    let (track, index) = new_track_row(tx.store(), name, kind)?;
    tx.apply(crate::control::op::Op::TrackAdd {
        track: track.clone(),
        index,
        clips: Vec::new(),
        clip_indices: Vec::new(),
        automation_clips: Vec::new(),
        bindings: Vec::new(),
    })?;
    Ok(track)
}

/// Compose the live transport snapshot (store fields + RT atomics).
pub fn transport_snapshot(store: &Store, shared: &SharedRt) -> TransportState {
    let count_in = shared.countin_left.load(Relaxed);
    let state = if shared.recording.load(Relaxed) {
        "recording".into()
    } else if shared.playing.load(Relaxed) || count_in > 0 {
        "playing".into()
    } else {
        store.transport.state.clone()
    };
    TransportState {
        state,
        position_samples: shared.position.load(Relaxed),
        sample_rate: shared.sample_rate.load(Relaxed),
        tempo_bpm: store.transport.tempo_bpm,
        loop_enabled: shared.loop_enabled.load(Relaxed),
        loop_start_samples: shared.loop_start.load(Relaxed),
        loop_end_samples: shared.loop_end.load(Relaxed),
        song_end_samples: shared.song_end.load(Relaxed),
        stop_at_end: shared.stop_at_end.load(Relaxed),
        count_in_left_samples: count_in,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_tracks(n: usize) -> Store {
        let mut store = Store::default();
        for i in 0..n {
            add_track(&mut store, Some(format!("T{i}")), None).unwrap();
        }
        store
    }

    #[test]
    fn add_track_rejects_unknown_kind_and_accepts_midi() {
        let mut store = store_with_tracks(0);
        assert!(add_track(&mut store, None, Some("bus".into())).is_err());
        let t = add_track(&mut store, None, Some("midi".into())).unwrap();
        assert_eq!(t.kind, "midi");
    }

    #[test]
    fn add_track_accepts_automation_kind() {
        let mut store = store_with_tracks(0);
        let t = add_track(&mut store, Some("Auto".into()), Some("automation".into())).unwrap();
        assert_eq!(t.kind, "automation");
        assert_eq!(store.tracks[0].kind, "automation");
    }
}
