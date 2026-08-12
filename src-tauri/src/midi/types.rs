//! MIDI / musical-time wire types (tauri-free, testable standalone).
//!
//! JSON mirrors: `docs/ipc-schemas/midi-clip.schema.json` and the v2 fields in
//! `docs/ipc-schemas/project-v2.schema.json` (camelCase on the wire).
//!
//! TIME UNITS (frozen for phase 2, pays down debt D-02):
//! * All *musical* positions/lengths are integer ticks at the project PPQ.
//! * Audio clip positions stay sample-based (v1 semantics, unchanged).
//! * The tick <-> sample bijection is [`crate::midi::tempo::TempoMap`]; only
//!   the control plane converts — the RT thread never does tempo math.

use serde::{Deserialize, Serialize};

/// Default ticks-per-quarter-note for new (v2) projects.
/// 960 divides cleanly by 2..=10 dotted/triplet grids and matches common DAWs.
pub const DEFAULT_PPQ: u32 = 960;

/// One tempo change. A project's tempo map is a sorted list of these; the
/// first entry MUST be at tick 0 (v1 projects migrate to a one-entry map).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoEvent {
    pub tick: u64,
    pub bpm: f64,
}

/// One MIDI note. 16 bytes in the AMEV binary chunk encoding (see
/// [`crate::midi::events`]); JSON on the IPC surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiNote {
    /// Onset, in ticks relative to the clip start.
    pub tick: u32,
    /// Duration in ticks (> 0).
    pub length_ticks: u32,
    /// MIDI key number 0..=127 (60 = C4).
    pub key: u8,
    /// MIDI velocity 1..=127.
    pub velocity: u8,
    /// MIDI channel 0..=15 (informational for the internal synth).
    #[serde(default)]
    pub channel: u8,
}

/// A MIDI clip: a placement of note data on a `kind: "midi"` track.
///
/// Phase-2 scaffold keeps notes in memory on the clip; *persistence* writes
/// them to `events/<clipId>.bin` AMEV chunks referenced from project.json v2
/// (`eventsRef`), never inline JSON (SCALABILITY §3). Pattern instancing
/// (`patternId`) is reserved for the pattern/playlist milestone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiClip {
    pub id: String,
    pub track_id: String,
    pub name: String,
    /// Placement on the timeline, in ticks.
    pub timeline_start_ticks: u64,
    /// Clip length in ticks.
    pub length_ticks: u64,
    /// Note events, sorted by (tick, key). In-memory / IPC representation;
    /// persisted as an AMEV chunk (`eventsRef` in project.json v2).
    pub notes: Vec<MidiNote>,
}

impl MidiNote {
    /// Validate ranges; returns a human-readable error for the IPC surface.
    pub fn validate(&self) -> Result<(), String> {
        if self.key > 127 {
            return Err(format!("note key out of range: {}", self.key));
        }
        if self.velocity == 0 || self.velocity > 127 {
            return Err(format!("note velocity out of range: {}", self.velocity));
        }
        if self.channel > 15 {
            return Err(format!("note channel out of range: {}", self.channel));
        }
        if self.length_ticks == 0 {
            return Err("note lengthTicks must be > 0".into());
        }
        Ok(())
    }
}
