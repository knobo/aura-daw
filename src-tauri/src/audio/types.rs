//! Pure data/IPC types for the audio engine (tauri-free, testable standalone).
//!
//! JSON mirrors live in `docs/ipc-schemas/*.schema.json` — those schemas are
//! the source of truth for the wire format (`camelCase`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::{ClipId, SourceId, TrackId};

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// Mirrors docs/ipc-schemas/transport-state.schema.json
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportState {
    /// "stopped" | "playing" | "recording"
    pub state: String,
    /// Playhead position in samples at `sample_rate`.
    pub position_samples: u64,
    pub sample_rate: u32,
    pub tempo_bpm: f64,
    pub loop_enabled: bool,
    pub loop_start_samples: u64,
    pub loop_end_samples: u64,
    /// Last audible sample of the current material (0 = nothing to play).
    /// Reported so the UI navigates to the same end the engine stops at,
    /// rather than deriving its own from clip bounds alone. Derived state:
    /// defaulted, never required from a project file.
    #[serde(default)]
    pub song_end_samples: u64,
    /// Stop the transport when the playhead reaches `song_end_samples`.
    /// Defaulted so projects written before this field still load.
    #[serde(default = "stop_at_end_default")]
    pub stop_at_end: bool,
}

/// Auto-stop is on unless a project says otherwise: a transport that runs
/// forever into silence is the more surprising default.
fn stop_at_end_default() -> bool {
    true
}

impl Default for TransportState {
    fn default() -> Self {
        Self {
            state: "stopped".into(),
            position_samples: 0,
            sample_rate: 48_000,
            tempo_bpm: 120.0,
            loop_enabled: false,
            loop_start_samples: 0,
            loop_end_samples: 0,
            song_end_samples: 0,
            stop_at_end: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Meters
// ---------------------------------------------------------------------------

/// Mirrors docs/ipc-schemas/meter-frame.schema.json
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterFrame {
    /// Monotonic frame counter (per subscription).
    pub seq: u64,
    /// Engine playhead position (samples) when this frame was captured.
    pub position_samples: u64,
    pub tracks: Vec<TrackMeter>,
    /// Master bus meter (track_id == "master").
    pub master: TrackMeter,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackMeter {
    pub track_id: String,
    /// Linear peak (full scale = 1.0; may exceed 1.0 when clipping), left.
    pub peak_l: f32,
    pub peak_r: f32,
    /// Linear RMS over the batch window.
    pub rms_l: f32,
    pub rms_r: f32,
    pub clipped: bool,
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

/// Mirrors docs/ipc-schemas/track-state.schema.json
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackState {
    pub id: TrackId,
    pub name: String,
    /// "audio" (future: "midi", "bus")
    pub kind: String,
    /// Fader gain in dB (-inf encoded as -160.0).
    pub gain_db: f64,
    /// -1.0 (hard left) .. 1.0 (hard right)
    pub pan: f64,
    pub muted: bool,
    pub soloed: bool,
    pub armed: bool,
    /// UI hint, hex "#rrggbb".
    pub color: String,
    /// Sampler instrument bound to this (midi) track — additive phase-2
    /// field (readers tolerate its absence). When set and loaded in the
    /// `SamplerBank`, the engine renders the track through the sampler;
    /// otherwise the built-in `PolySynth` is the fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instrument_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// Mirrors docs/ipc-schemas/audio-device.schema.json
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    /// Stable identifier (currently the cpal device name).
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub max_channels: u16,
    pub default_sample_rate: u32,
}

// ---------------------------------------------------------------------------
// Clips
// ---------------------------------------------------------------------------

/// Mirrors docs/ipc-schemas/clip.schema.json
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Clip {
    pub id: ClipId,
    pub track_id: TrackId,
    pub name: String,
    /// Relative to the .aura project dir, POSIX separators.
    pub source_path: String,
    /// Decode-cache/asset identity (round-2 §2.2). Empty (`Default`) means
    /// "unassigned" — a legacy clip before `assign_source_ids` runs, or a
    /// construction site that hasn't minted one yet. One `SourceId` names
    /// exactly one `source_path`; the empty sentinel must never reach the
    /// engine cache ([`crate::audio::engine`]'s `stale_sources`).
    #[serde(default)]
    pub source_id: SourceId,
    pub source_channels: u16,
    pub source_sample_rate: u32,
    /// Length of the source file in SOURCE samples.
    pub source_length_samples: u64,
    pub timeline_start_samples: u64,
    pub offset_samples: u64,
    pub length_samples: u64,
    pub gain_db: f64,
    pub fade_in_samples: u64,
    pub fade_out_samples: u64,
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

/// Mirrors docs/ipc-schemas/project.schema.json (project.json format v1).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub schema_version: u32,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    pub sample_rate: u32,
    pub tempo_bpm: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_signature: Option<(u8, u8)>,
    pub tracks: Vec<TrackState>,
    pub clips: Vec<Clip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<TransportState>,
}

// ---------------------------------------------------------------------------
// Control-plane store (shared between Tauri commands and the engine control
// thread behind a parking_lot::Mutex; never touched by the RT threads).
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Store {
    pub transport: TransportState,
    pub tracks: Vec<TrackState>,
    pub clips: Vec<Clip>,
    /// Absolute path of the open `.aura` directory (None = no project).
    pub project_dir: Option<PathBuf>,
    pub project_name: Option<String>,
    pub created_at: Option<String>,
}

/// Derive RT parameter slots from display order (round-2 §2.4). Pure: no
/// stored allocation state, so there is nothing to free and therefore
/// nothing that can be reused while a stale graph still reads it — the
/// O-13 alias window this replaces is dead by construction. Every rebuild
/// calls this fresh against the CURRENT track list and builds its OWN
/// `ParamTable`/`GraphTables` from the result; a retired graph keeps
/// reading the table it was built with, so a later renumbering (tracks
/// added/removed) can never bleed into it.
pub fn derive_slots(tracks: &[TrackState]) -> HashMap<TrackId, usize> {
    tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.clone(), i))
        .collect()
}

impl Store {
    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.soloed)
    }

    /// Absolute path for a project-relative source path.
    pub fn abs_path(&self, rel: &str) -> Option<PathBuf> {
        self.project_dir.as_ref().map(|d| d.join(rel))
    }

    /// Waveform pyramid cache dir for a clip: `<proj>/cache/waveforms/<clipId>`.
    pub fn waveform_cache_dir(&self, clip_id: &str) -> Option<PathBuf> {
        self.project_dir
            .as_ref()
            .map(|d| d.join("cache").join("waveforms").join(clip_id))
    }

    pub fn armed_track_ids(&self) -> Vec<String> {
        self.tracks.iter().filter(|t| t.armed).map(|t| t.id.to_string()).collect()
    }

    pub fn cache_dir_for(dir: &Path, clip_id: &str) -> PathBuf {
        dir.join("cache").join("waveforms").join(clip_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_track(id: &str) -> TrackState {
        TrackState {
            id: id.into(),
            name: id.into(),
            kind: "audio".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        }
    }

    #[test]
    fn slots_are_display_order_and_never_reused_across_generations() {
        let tracks = vec![test_track("a"), test_track("b"), test_track("c")];
        let s = derive_slots(&tracks);
        assert_eq!((s["a"], s["b"], s["c"]), (0, 1, 2));
        // Remove "a": the NEXT derivation renumbers — that is fine, because
        // the numbering is scoped to one graph (each graph has its own table);
        // cross-generation aliasing is impossible by construction, which the
        // engine-level test (Task 8) proves end to end.
        let s2 = derive_slots(&tracks[1..]);
        assert_eq!((s2["b"], s2["c"]), (0, 1));
    }

    #[test]
    fn wire_format_is_camel_case() {
        let t = TransportState::default();
        let v = serde_json::to_value(&t).unwrap();
        assert!(v.get("positionSamples").is_some());
        assert!(v.get("loopEndSamples").is_some());
        let m = MeterFrame {
            seq: 1,
            position_samples: 2,
            tracks: vec![],
            master: TrackMeter { track_id: "master".into(), ..Default::default() },
        };
        let v = serde_json::to_value(&m).unwrap();
        assert!(v["master"].get("peakL").is_some());
        assert!(v["master"].get("trackId").is_some());
    }
}
