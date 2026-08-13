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

use crate::audio::rt::{ParamTable, SharedRt};
use crate::audio::types::{Store, TrackState, TransportState, MAX_TRACKS};

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

/// Allocate an RT slot and build a fresh track row — everything `add_track`
/// does EXCEPT inserting it into `store.tracks`. Split out (Task 7) so
/// `ControlPlane::add_track` can insert the row through the transaction
/// channel (`Op::TrackAdd`, via `Session::transact`) instead of a direct
/// `Vec::insert`, while slot allocation + the RT param reset — like
/// `remove_track`'s slot free — stay outside the op layer this plan
/// (round-2 §2.4; `control::session`'s `Op::TrackAdd` arm never touches
/// slots). Returns `(track, index)`; `index` is the row's intended position
/// (end of the current list).
pub(crate) fn new_track_row(
    store: &mut Store,
    params: &ParamTable,
    name: Option<String>,
    kind: Option<String>,
) -> Result<(TrackState, usize), String> {
    let kind = kind.unwrap_or_else(|| "audio".into());
    if !matches!(kind.as_str(), "audio" | "midi") {
        return Err(format!("unsupported track kind: {kind}"));
    }
    let n = store.tracks.len();
    let id = uuid::Uuid::new_v4().to_string();
    let slot = store
        .alloc_slot(&id)
        .ok_or_else(|| format!("track limit reached ({MAX_TRACKS})"))?;
    params.reset_slot(slot);
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

/// Create a track in the store and reset its RT param slot. STRUCTURAL —
/// the caller must send `ControlMsg::Rebuild` afterwards. `kind`: "audio"
/// (default) or "midi" (phase 2; "bus" reserved). Used directly by callers
/// that mutate the store outside the transaction channel (currently only
/// `seed_demo_project`, which builds several tracks under one lock before
/// any engine/event side effect); the frozen `add_track` COMMAND goes
/// through `ControlPlane::add_track` -> `commit` -> `new_track_row` instead.
pub fn add_track(
    store: &mut Store,
    params: &ParamTable,
    name: Option<String>,
    kind: Option<String>,
) -> Result<TrackState, String> {
    let (track, index) = new_track_row(store, params, name, kind)?;
    insert_track(store, track.clone(), index);
    Ok(track)
}

/// Compose the live transport snapshot (store fields + RT atomics).
pub fn transport_snapshot(store: &Store, shared: &SharedRt) -> TransportState {
    TransportState {
        state: store.transport.state.clone(),
        position_samples: shared.position.load(Relaxed),
        sample_rate: shared.sample_rate.load(Relaxed),
        tempo_bpm: store.transport.tempo_bpm,
        loop_enabled: shared.loop_enabled.load(Relaxed),
        loop_start_samples: shared.loop_start.load(Relaxed),
        loop_end_samples: shared.loop_end.load(Relaxed),
        song_end_samples: shared.song_end.load(Relaxed),
        stop_at_end: shared.stop_at_end.load(Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_tracks(n: usize) -> (Store, ParamTable) {
        let mut store = Store::default();
        let params = ParamTable::default();
        for i in 0..n {
            add_track(&mut store, &params, Some(format!("T{i}")), None).unwrap();
        }
        (store, params)
    }

    #[test]
    fn add_track_rejects_unknown_kind_and_accepts_midi() {
        let (mut store, params) = store_with_tracks(0);
        assert!(add_track(&mut store, &params, None, Some("bus".into())).is_err());
        let t = add_track(&mut store, &params, None, Some("midi".into())).unwrap();
        assert_eq!(t.kind, "midi");
    }
}
