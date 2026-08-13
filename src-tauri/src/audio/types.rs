//! Pure data/IPC types for the audio engine (tauri-free, testable standalone).
//!
//! JSON mirrors live in `docs/ipc-schemas/*.schema.json` — those schemas are
//! the source of truth for the wire format (`camelCase`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ids::{ClipId, TrackId};

/// Hard cap on mixer tracks; slot indices and meter blocks are sized by this.
/// 64 conveniently matches a `u64` presence mask.
pub const MAX_TRACKS: usize = 64;

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

pub struct Store {
    pub transport: TransportState,
    pub tracks: Vec<TrackState>,
    pub clips: Vec<Clip>,
    /// Absolute path of the open `.aura` directory (None = no project).
    pub project_dir: Option<PathBuf>,
    pub project_name: Option<String>,
    pub created_at: Option<String>,
    /// track id -> RT parameter slot (0..MAX_TRACKS).
    pub slots: HashMap<TrackId, usize>,
    slot_used: [bool; MAX_TRACKS],
}

impl Default for Store {
    fn default() -> Self {
        Self {
            transport: TransportState::default(),
            tracks: Vec::new(),
            clips: Vec::new(),
            project_dir: None,
            project_name: None,
            created_at: None,
            slots: HashMap::new(),
            slot_used: [false; MAX_TRACKS],
        }
    }
}

impl Store {
    /// Allocate a free RT slot for a track id. Returns None when full.
    pub fn alloc_slot(&mut self, track_id: &str) -> Option<usize> {
        if let Some(&s) = self.slots.get(track_id) {
            return Some(s);
        }
        let slot = self.slot_used.iter().position(|u| !u)?;
        self.slot_used[slot] = true;
        self.slots.insert(TrackId::from(track_id), slot);
        Some(slot)
    }

    pub fn free_slot(&mut self, track_id: &str) {
        if let Some(slot) = self.slots.remove(track_id) {
            self.slot_used[slot] = false;
        }
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.soloed)
    }

    /// (slot, track_id) pairs in display order.
    pub fn track_slots(&self) -> Vec<(usize, String)> {
        self.tracks
            .iter()
            .filter_map(|t| self.slots.get(&t.id).map(|&s| (s, t.id.to_string())))
            .collect()
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

    #[test]
    fn slot_allocation_reuses_freed_slots() {
        let mut s = Store::default();
        let a = s.alloc_slot("a").unwrap();
        let b = s.alloc_slot("b").unwrap();
        assert_ne!(a, b);
        // idempotent
        assert_eq!(s.alloc_slot("a").unwrap(), a);
        s.free_slot("a");
        let c = s.alloc_slot("c").unwrap();
        assert_eq!(c, a, "freed slot is reused");
    }

    #[test]
    fn slot_allocation_exhausts_at_max() {
        let mut s = Store::default();
        for i in 0..MAX_TRACKS {
            assert!(s.alloc_slot(&format!("t{i}")).is_some());
        }
        assert!(s.alloc_slot("overflow").is_none());
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
