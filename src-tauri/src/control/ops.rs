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

use crate::audio::rt::{ParamTable, SharedRt, FLAG_MUTE, FLAG_SOLO};
use crate::audio::types::{Store, TrackState, TransportState, MAX_TRACKS};
use crate::audio::mixer;

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

/// Apply a batch of mix changes atomically: all track ids are validated
/// before anything is written (a bad id fails the whole batch). Values are
/// clamped to the schema ranges. Returns the updated tracks in batch order.
pub fn apply_track_mix(
    store: &mut Store,
    params: &ParamTable,
    changes: &[TrackMixChange],
) -> Result<Vec<TrackState>, String> {
    for c in changes {
        if !store.tracks.iter().any(|t| t.id == c.track_id) {
            return Err(format!("unknown track: {}", c.track_id));
        }
    }
    let mut updated = Vec::with_capacity(changes.len());
    for c in changes {
        let slot = store.slots.get(&c.track_id).copied();
        let t = store
            .tracks
            .iter_mut()
            .find(|t| t.id == c.track_id)
            .expect("validated above");
        if let Some(g) = c.gain_db {
            t.gain_db = g.clamp(-160.0, 24.0);
        }
        if let Some(p) = c.pan {
            t.pan = p.clamp(-1.0, 1.0);
        }
        if let Some(m) = c.muted {
            t.muted = m;
        }
        if let Some(s) = c.soloed {
            t.soloed = s;
        }
        if let Some(a) = c.armed {
            t.armed = a;
        }
        let snapshot = t.clone();
        if let Some(slot) = slot {
            params.set_gain_linear(slot, mixer::db_to_linear(snapshot.gain_db));
            params.set_pan(slot, snapshot.pan as f32);
            params.set_flag(slot, FLAG_MUTE, snapshot.muted);
            params.set_flag(slot, FLAG_SOLO, snapshot.soloed);
        }
        updated.push(snapshot);
    }
    params.any_solo.store(store.any_solo(), Relaxed);
    Ok(updated)
}

pub const TRACK_COLORS: [&str; 6] =
    ["#7c9cff", "#ff9c7c", "#7cffb0", "#e07cff", "#ffe07c", "#7cd8ff"];

/// Create a track in the store and reset its RT param slot. STRUCTURAL —
/// the caller must send `ControlMsg::Rebuild` afterwards.
/// `kind`: "audio" (default) or "midi" (phase 2; "bus" reserved).
pub fn add_track(
    store: &mut Store,
    params: &ParamTable,
    name: Option<String>,
    kind: Option<String>,
) -> Result<TrackState, String> {
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
        id,
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
    store.tracks.push(track.clone());
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
    fn batched_mix_applies_all_and_clamps() {
        let (mut store, params) = store_with_tracks(2);
        let a = store.tracks[0].id.clone();
        let b = store.tracks[1].id.clone();
        let updated = apply_track_mix(
            &mut store,
            &params,
            &[
                TrackMixChange { gain_db: Some(100.0), pan: Some(-2.0), ..TrackMixChange::new(&a) },
                TrackMixChange { muted: Some(true), soloed: Some(true), ..TrackMixChange::new(&b) },
            ],
        )
        .unwrap();
        assert_eq!(updated.len(), 2);
        assert_eq!(updated[0].gain_db, 24.0, "gain clamped");
        assert_eq!(updated[0].pan, -1.0, "pan clamped");
        assert!(updated[1].muted && updated[1].soloed);
        assert!(params.any_solo.load(Relaxed));
    }

    #[test]
    fn batched_mix_is_atomic_on_unknown_track() {
        let (mut store, params) = store_with_tracks(1);
        let a = store.tracks[0].id.clone();
        let err = apply_track_mix(
            &mut store,
            &params,
            &[
                TrackMixChange { gain_db: Some(-6.0), ..TrackMixChange::new(&a) },
                TrackMixChange::new("nope"),
            ],
        )
        .unwrap_err();
        assert!(err.contains("unknown track"));
        assert_eq!(store.tracks[0].gain_db, 0.0, "no partial application");
    }

    #[test]
    fn add_track_rejects_unknown_kind_and_accepts_midi() {
        let (mut store, params) = store_with_tracks(0);
        assert!(add_track(&mut store, &params, None, Some("bus".into())).is_err());
        let t = add_track(&mut store, &params, None, Some("midi".into())).unwrap();
        assert_eq!(t.kind, "midi");
    }
}
