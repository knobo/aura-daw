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

use crate::ids::{ClipId, ContentId, LaneId, NoteId, TrackId};

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

/// One tempo change in the v3 integer-period model (round-2 §3.3): period
/// is the duration of one quarter note in superticks
/// (`crate::time::SUPERTICKS_PER_SECOND`), sample-rate independent.
/// `period_start == period_end` is constant tempo; otherwise a linear-in-
/// period ramp across `[tick, next_event.tick)` (§3.3 "Ramps" — linear in
/// seconds-per-beat, not in bpm).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoPeriodEvent {
    pub tick: u64,
    pub period_start: u64,
    pub period_end: u64,
}

/// One time-signature change (round-2 §3.3/O-10). Sorted list, first at
/// tick 0; default is `[{0,4,4}]`. Persisting this is what fixes the
/// active data-loss bug (dossier 10 trap 3): today's code silently
/// clobbers the user's signature to 4/4 on every save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeterEvent {
    pub tick: u64,
    pub num: u8,
    pub den: u8,
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
    /// Stable per-clip note identity (round-2 §2.1). `0` is the wire
    /// sentinel for "not yet assigned" — every note inside the document has
    /// id >= 1, minted from its clip's persisted watermark
    /// ([`MidiClip::mint_note_id`]) and NEVER reused (ADR 0001).
    #[serde(default)]
    pub note_id: NoteId,
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
    pub id: ClipId,
    pub track_id: TrackId,
    pub name: String,
    /// Placement on the timeline, in ticks.
    pub timeline_start_ticks: u64,
    /// Clip length in ticks.
    pub length_ticks: u64,
    /// Note events, sorted by (tick, key). In-memory / IPC representation;
    /// persisted as an AMEV chunk (`eventsRef` in project.json v2).
    pub notes: Vec<MidiNote>,
    /// Persisted note-id watermark (round-2 §2.1): the next id
    /// [`MidiClip::mint_note_id`] hands out. Ids are NEVER reused —
    /// recomputing `max(notes) + 1` on load is forbidden (ADR 0001), which is
    /// why this rides along as its own durable field (both in the JSON row
    /// and the AMEV chunk header) rather than being derived.
    #[serde(default = "first_note_id")]
    pub next_note_id: u32,
    /// Content identity (round-2 §5, ADR 0004): first populated by the C/D
    /// content/placement split (`midi::persist`'s v3 read path mints one
    /// deterministically per clip on migration; every fresh clip mints its
    /// own too). `#[serde(default)]` covers DESERIALIZATION of pre-v3 rows
    /// only — a v3+ reader always sets this explicitly (see `persist.rs`),
    /// so the empty-string default is never actually observed there; every
    /// non-serde struct-literal constructor of `MidiClip` in this crate
    /// must set it explicitly (Rust does not apply `#[serde(default)]` to
    /// plain struct literals).
    #[serde(default)]
    pub content_id: ContentId,
    /// Lane reference (round-2 §5): resolves to a track via a `lanes[]`
    /// row in the v3 file. Same `#[serde(default)]` caveat as `content_id`.
    #[serde(default)]
    pub lane_id: LaneId,
}

/// Serde default for [`MidiClip::next_note_id`] (and, imported,
/// `persist::PersistedClip::next_note_id` — same sentinel invariant, one
/// definition): id 0 is the wire sentinel for "unassigned", so real ids
/// start at 1.
pub(crate) fn first_note_id() -> u32 {
    1
}

impl MidiClip {
    /// Hand out the next id and advance the persisted watermark. Ids start
    /// at 1 (0 is the "unassigned" wire sentinel) and are NEVER reused —
    /// recomputing max+1 on load is forbidden (ADR 0001).
    pub fn mint_note_id(&mut self) -> NoteId {
        let id = NoteId(self.next_note_id);
        self.next_note_id += 1;
        id
    }

    /// Mint ids for any note still carrying `NoteId(0)` (unassigned); error
    /// on duplicate non-zero ids already present (a caller bug or a corrupt
    /// merge, never silently resolved).
    pub fn ensure_note_ids(&mut self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for n in &self.notes {
            if n.note_id.0 != 0 && !seen.insert(n.note_id.0) {
                return Err(format!("duplicate note id {}", n.note_id.0));
            }
        }
        let mut next = self.next_note_id;
        for n in &mut self.notes {
            if n.note_id.0 == 0 {
                n.note_id = NoteId(next);
                next += 1;
            }
        }
        self.next_note_id = next;
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_period_event_serializes_camel_case_and_transparent_period_fields() {
        let e = TempoPeriodEvent { tick: 3840, period_start: 4_233_600_000, period_end: 4_233_600_000 };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["tick"], 3840);
        assert_eq!(v["periodStart"], 4_233_600_000u64);
        assert_eq!(v["periodEnd"], 4_233_600_000u64);
        let back: TempoPeriodEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn meter_event_serializes_camel_case() {
        let e = MeterEvent { tick: 7680, num: 3, den: 4 };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["tick"], 7680);
        assert_eq!(v["num"], 3);
        assert_eq!(v["den"], 4);
        let back: MeterEvent = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn watermark_never_reuses_ids() {
        let mut clip = MidiClip {
            id: "c-1".into(), track_id: "t-1".into(), name: "c".into(),
            timeline_start_ticks: 0, length_ticks: 3840,
            notes: vec![], next_note_id: 1,
            content_id: ContentId::mint(), lane_id: LaneId::default_for_track("t-1"),
        };
        let a = clip.mint_note_id();
        let b = clip.mint_note_id();
        assert_eq!((a.0, b.0), (1, 2));
        // Deleting the highest note and minting again must NOT reuse 2 —
        // the watermark only ever advances (ADR 0001 never-reuse).
        assert_eq!(clip.mint_note_id().0, 3);
        assert_eq!(clip.next_note_id, 4);
    }

    #[test]
    fn ensure_note_ids_mints_only_unassigned_and_rejects_duplicates() {
        let mut clip = MidiClip {
            id: "c-1".into(), track_id: "t-1".into(), name: "c".into(),
            timeline_start_ticks: 0, length_ticks: 3840,
            notes: vec![
                MidiNote { tick: 0, length_ticks: 1, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) },
                MidiNote { tick: 1, length_ticks: 1, key: 61, velocity: 100, channel: 0, note_id: NoteId(5) },
            ],
            next_note_id: 6,
            content_id: ContentId::mint(), lane_id: LaneId::default_for_track("t-1"),
        };
        clip.ensure_note_ids().unwrap();
        assert_eq!(clip.notes[0].note_id.0, 6, "unassigned got minted");
        assert_eq!(clip.notes[1].note_id.0, 5, "existing id preserved");
        clip.notes.push(MidiNote { tick: 2, length_ticks: 1, key: 62, velocity: 100, channel: 0, note_id: NoteId(5) });
        assert!(clip.ensure_note_ids().is_err(), "duplicate real ids rejected");
    }
}
