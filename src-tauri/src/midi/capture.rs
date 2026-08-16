//! Captured note edges -> a `MidiClip` (pure, no engine, no hardware).
//!
//! [`take_notes`] does the whole tick-math + note-pairing policy for a
//! finished take (`crate::audio::midi_in::Capture`): retrigger closes the
//! previous note, unmatched offs are ignored, dangling ons are closed at the
//! take end, lengths clamp to >= 1 tick, velocities clamp to `1..=127`, ids
//! are minted `1..=n` in output order, notes are sorted by `(tick, key)`.
//! [`take_clip`] wraps that into a fresh `MidiClip` (the one place this
//! module touches `uuid`).

use crate::audio::midi_in::Capture;
use crate::ids::{ContentId, LaneId};
use crate::midi::tempo::TempoMap;
use crate::midi::types::{MidiClip, MidiNote};

/// The take's notes, in ticks RELATIVE to the take's own clip start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeNotes {
    pub notes: Vec<MidiNote>,
    /// Watermark for the new clip: `notes.len() as u32 + 1` (ADR 0001 —
    /// ids start at 1 and are never reused).
    pub next_note_id: u32,
    /// Absolute placement of the take on the timeline.
    pub start_ticks: u64,
    /// Take length in ticks, always >= 1.
    pub length_ticks: u64,
}

/// One note-on still sounding, waiting for its matching off (or the take
/// end/a retrigger to close it).
struct OpenNote {
    tick: u32,
    key: u8,
    velocity: u8,
}

/// Convert a finished take's captured edges into [`TakeNotes`]: the pure
/// tick math + note-pairing policy (see module docs for the rule list).
pub fn take_notes(capture: &Capture, map: &TempoMap) -> TakeNotes {
    let start_ticks = map.samples_to_tick(capture.start_sample);
    let take_end_ticks = map.samples_to_tick(capture.end_sample).saturating_sub(start_ticks);
    let take_end_u32 = u32::try_from(take_end_ticks).unwrap_or(u32::MAX);

    let rel_tick = |sample: u64| -> u32 {
        let t = map.samples_to_tick(sample).saturating_sub(start_ticks);
        u32::try_from(t).unwrap_or(u32::MAX)
    };

    // (tick, key, length, velocity) — accumulated in event order, sorted
    // by (tick, key) once the pairing pass is done.
    let mut closed: Vec<(u32, u8, u32, u8)> = Vec::new();
    let mut open: Vec<OpenNote> = Vec::new();

    for ev in &capture.events {
        let tick = rel_tick(ev.sample);
        // A note-on for a key already sounding closes the previous note at
        // the new tick (retrigger); a note-off closes it too. Either way,
        // an already-open note for this key is found and closed the same
        // way — a note-off with no matching note-on is simply ignored.
        if let Some(idx) = open.iter().position(|n| n.key == ev.key) {
            let prev = open.remove(idx);
            let length = tick.saturating_sub(prev.tick).max(1);
            closed.push((prev.tick, prev.key, length, prev.velocity));
        }
        if ev.on {
            open.push(OpenNote { tick, key: ev.key, velocity: ev.velocity });
        }
    }

    // Note-ons still sounding at the end are closed at the take end tick.
    for prev in open {
        let length = take_end_u32.saturating_sub(prev.tick).max(1);
        closed.push((prev.tick, prev.key, length, prev.velocity));
    }

    // Sorted by (tick, key) — the invariant `MidiClip.notes` carries.
    closed.sort_by_key(|(tick, key, _, _)| (*tick, *key));

    let mut last_note_end: u64 = 0;
    let mut notes = Vec::with_capacity(closed.len());
    for (i, (tick, key, length, velocity)) in closed.into_iter().enumerate() {
        let end = tick as u64 + length as u64;
        if end > last_note_end {
            last_note_end = end;
        }
        notes.push(MidiNote {
            tick,
            length_ticks: length,
            key,
            velocity: velocity.clamp(1, 127),
            channel: 0,
            note_id: crate::ids::NoteId(i as u32 + 1),
        });
    }

    let next_note_id = notes.len() as u32 + 1;
    let length_ticks = take_end_ticks.max(1).max(last_note_end);

    TakeNotes { notes, next_note_id, start_ticks, length_ticks }
}

/// The take's clip, or `None` when nothing was played (no notes).
pub fn take_clip(capture: &Capture, name: &str, map: &TempoMap) -> Option<MidiClip> {
    let take = take_notes(capture, map);
    if take.notes.is_empty() {
        return None;
    }
    Some(MidiClip {
        id: uuid::Uuid::new_v4().to_string().into(),
        track_id: capture.track_id.clone().into(),
        name: name.to_string(),
        timeline_start_ticks: take.start_ticks,
        length_ticks: take.length_ticks,
        notes: take.notes,
        next_note_id: take.next_note_id,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(&capture.track_id),
        content_length_ticks: None,
        transpose_semitones: 0,
        velocity_offset: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::midi_in::{Capture, CapturedEvent};
    use crate::midi::tempo::TempoMap;
    use crate::midi::types::{DEFAULT_PPQ, TempoEvent};

    /// 120 bpm @ 48 kHz: one beat = 24 000 samples = `DEFAULT_PPQ` (960) ticks.
    fn map() -> TempoMap {
        TempoMap::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: 120.0 }], 48_000).unwrap()
    }

    fn cap(start: u64, end: u64, events: Vec<CapturedEvent>) -> Capture {
        Capture { track_id: "t-1".into(), start_sample: start, end_sample: end, events }
    }

    fn on(sample: u64, key: u8, vel: u8) -> CapturedEvent {
        CapturedEvent { sample, on: true, key, velocity: vel }
    }
    fn off(sample: u64, key: u8) -> CapturedEvent {
        CapturedEvent { sample, on: false, key, velocity: 0 }
    }

    #[test]
    fn pairs_on_off_into_one_note_relative_to_the_take_start() {
        // Take starts one beat in; the note starts one beat after THAT.
        let c = cap(24_000, 96_000, vec![on(48_000, 60, 100), off(60_000, 60)]);
        let t = take_notes(&c, &map());
        assert_eq!(t.start_ticks, 960);
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.notes[0].tick, 960, "one beat after the take start");
        assert_eq!(t.notes[0].length_ticks, 480, "half a beat long");
        assert_eq!(t.notes[0].key, 60);
        assert_eq!(t.notes[0].velocity, 100);
        assert_eq!(t.notes[0].note_id, crate::ids::NoteId(1));
        assert_eq!(t.next_note_id, 2);
    }

    #[test]
    fn dangling_note_on_is_closed_at_the_take_end() {
        let c = cap(0, 48_000, vec![on(24_000, 64, 90)]);
        let t = take_notes(&c, &map());
        assert_eq!(t.notes.len(), 1);
        assert_eq!(t.notes[0].tick, 960);
        assert_eq!(t.notes[0].length_ticks, 960, "closed at the take end (2 beats in)");
    }

    #[test]
    fn unmatched_note_off_is_ignored() {
        let c = cap(0, 48_000, vec![off(1_000, 60), on(2_000, 60, 80), off(3_000, 60)]);
        let t = take_notes(&c, &map());
        assert_eq!(t.notes.len(), 1);
    }

    #[test]
    fn retrigger_closes_the_previous_note_first() {
        let c = cap(0, 96_000, vec![on(0, 60, 100), on(24_000, 60, 110), off(48_000, 60)]);
        let t = take_notes(&c, &map());
        assert_eq!(t.notes.len(), 2, "retrigger yields two notes: {:?}", t.notes);
        assert_eq!((t.notes[0].tick, t.notes[0].length_ticks), (0, 960));
        assert_eq!((t.notes[1].tick, t.notes[1].length_ticks), (960, 960));
        assert_eq!(t.notes[1].velocity, 110);
    }

    #[test]
    fn zero_length_note_clamps_to_one_tick() {
        let c = cap(0, 48_000, vec![on(1_000, 60, 100), off(1_000, 60)]);
        let t = take_notes(&c, &map());
        assert_eq!(t.notes[0].length_ticks, 1);
    }

    #[test]
    fn notes_are_sorted_by_tick_then_key_with_sequential_ids() {
        let c = cap(0, 96_000, vec![
            on(24_000, 67, 100), off(48_000, 67),
            on(24_000, 60, 100), off(48_000, 60),
        ]);
        let t = take_notes(&c, &map());
        assert_eq!(t.notes.iter().map(|n| n.key).collect::<Vec<_>>(), vec![60, 67]);
        assert_eq!(t.notes.iter().map(|n| n.note_id.0).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(t.next_note_id, 3);
        for n in &t.notes { n.validate().expect("every emitted note is valid"); }
    }

    #[test]
    fn take_clip_carries_placement_identity_and_lane() {
        let c = cap(24_000, 96_000, vec![on(24_000, 60, 100), off(48_000, 60)]);
        let clip = take_clip(&c, "MIDI Take 1", &map()).expect("notes were played");
        assert_eq!(clip.track_id.as_str(), "t-1");
        assert_eq!(clip.name, "MIDI Take 1");
        assert_eq!(clip.timeline_start_ticks, 960);
        assert_eq!(clip.length_ticks, 2_880, "take covers 3 beats");
        assert_eq!(clip.next_note_id, 2);
        assert!(!clip.content_id.as_str().is_empty(), "every fresh clip mints a content id");
        assert_eq!(clip.lane_id, crate::ids::LaneId::default_for_track("t-1"));
        assert_eq!(clip.content_length_ticks, None);
    }

    #[test]
    fn an_empty_capture_yields_no_clip() {
        assert!(take_clip(&cap(0, 48_000, vec![]), "MIDI Take 1", &map()).is_none());
        // Only an unmatched off: still nothing to register.
        assert!(take_clip(&cap(0, 48_000, vec![off(100, 60)]), "MIDI Take 1", &map()).is_none());
    }
}
