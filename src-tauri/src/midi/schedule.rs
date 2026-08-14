//! Note scheduling: convert tick-based clip notes into absolute-sample note
//! edges via [`TempoMap`], and slice them into per-block
//! [`BlockNoteEvent`]s.
//!
//! ALL conversion happens on the control thread (pre-roll / pre-render); the
//! RT thread only ever consumes the resulting sample-offset events
//! (ARCHITECTURE §13). Pure and tauri-free.

use super::synth::BlockNoteEvent;
use super::tempo::TempoMap;
use super::types::MidiClip;

/// A note edge at an absolute engine-sample position. `velocity == 0` is a
/// note-off (same convention as [`BlockNoteEvent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsNoteEvent {
    /// Absolute timeline position in engine samples.
    pub sample: u64,
    pub key: u8,
    pub velocity: u8,
}

/// Expand a clip's notes into absolute-sample note-on/off edges, sorted by
/// (sample, offs-before-ons). Rules:
///
/// * Note onsets at/after the clip end are dropped (the clip window clips
///   its content).
/// * Note-offs are clamped to the clip end tick.
/// * Zero-length results (rounding) still get a 1-sample duration so every
///   on has its off strictly after it.
pub fn clip_events(clip: &MidiClip, map: &TempoMap) -> Vec<AbsNoteEvent> {
    let clip_end_tick = clip.timeline_start_ticks.saturating_add(clip.length_ticks);
    let mut out = Vec::with_capacity(clip.notes.len() * 2);
    for n in &clip.notes {
        if n.velocity == 0 || n.length_ticks == 0 {
            continue;
        }
        let on_tick = clip.timeline_start_ticks.saturating_add(n.tick as u64);
        if on_tick >= clip_end_tick {
            continue;
        }
        let off_tick = on_tick
            .saturating_add(n.length_ticks as u64)
            .min(clip_end_tick);
        let on_s = map.tick_to_samples(on_tick);
        let mut off_s = map.tick_to_samples(off_tick);
        if off_s <= on_s {
            off_s = on_s + 1;
        }
        out.push(AbsNoteEvent { sample: on_s, key: n.key, velocity: n.velocity });
        out.push(AbsNoteEvent { sample: off_s, key: n.key, velocity: 0 });
    }
    // Offs before ons at the same sample position so retriggers of the same
    // key release the previous voice first.
    out.sort_by_key(|e| (e.sample, e.velocity));
    out
}

/// Iterate the events that fall inside the block `[block_start,
/// block_start + frames)`, converted to block-relative [`BlockNoteEvent`]s.
/// `events` must be sorted by sample (as [`clip_events`] returns them).
pub fn slice_block<'a>(
    events: &'a [AbsNoteEvent],
    block_start: u64,
    frames: u32,
) -> impl Iterator<Item = BlockNoteEvent> + 'a {
    let block_end = block_start + frames as u64;
    let lo = events.partition_point(|e| e.sample < block_start);
    events[lo..]
        .iter()
        .take_while(move |e| e.sample < block_end)
        .map(move |e| BlockNoteEvent {
            offset: (e.sample - block_start) as u32,
            key: e.key,
            velocity: e.velocity,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::NoteId;
    use crate::midi::types::{MidiNote, TempoEvent};

    fn note(tick: u32, len: u32, key: u8, vel: u8) -> MidiNote {
        MidiNote { tick, length_ticks: len, key, velocity: vel, channel: 0, note_id: NoteId(0) }
    }

    fn clip(start_ticks: u64, len_ticks: u64, notes: Vec<MidiNote>) -> MidiClip {
        MidiClip {
            id: "c1".into(),
            track_id: "t1".into(),
            name: "Clip".into(),
            timeline_start_ticks: start_ticks,
            length_ticks: len_ticks,
            notes,
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t1"),
        }
    }

    /// 120 bpm @ 48 kHz, ppq 960: 1 tick = 25 samples exactly.
    fn map_120() -> TempoMap {
        TempoMap::from_v1(120.0, 48_000).unwrap()
    }

    #[test]
    fn events_at_hand_computed_sample_positions() {
        // Clip starts at one bar (3840 ticks = 96000 samples @120bpm).
        // Note at clip-relative tick 960 (one beat) -> abs tick 4800
        // -> 4800 * 25 = 120000 samples; length one beat -> off at 144000.
        let c = clip(3840, 3840, vec![note(960, 960, 60, 100)]);
        let ev = clip_events(&c, &map_120());
        assert_eq!(
            ev,
            vec![
                AbsNoteEvent { sample: 120_000, key: 60, velocity: 100 },
                AbsNoteEvent { sample: 144_000, key: 60, velocity: 0 },
            ]
        );
    }

    #[test]
    fn tempo_change_mid_clip_moves_later_notes() {
        // 120 bpm for one bar, then 60 bpm. Clip spans two bars from 0.
        let map = TempoMap::new(
            960,
            vec![TempoEvent { tick: 0, bpm: 120.0 }, TempoEvent { tick: 3840, bpm: 60.0 }],
            48_000,
        )
        .unwrap();
        // Beat 1 of bar 2 = tick 3840 -> exactly at the tempo change:
        // 3840 * 25 = 96000. Its off (one beat at 60 bpm = 48000 samples).
        let c = clip(0, 7680, vec![note(3840, 960, 64, 90)]);
        let ev = clip_events(&c, &map);
        assert_eq!(ev[0], AbsNoteEvent { sample: 96_000, key: 64, velocity: 90 });
        assert_eq!(ev[1], AbsNoteEvent { sample: 96_000 + 48_000, key: 64, velocity: 0 });

        // A note STRADDLING the tempo change: on at tick 3360 (120bpm zone),
        // length 960 -> off at 4320 (60bpm zone).
        // on = 3360*25 = 84000; off = 96000 + 480*50 = 120000.
        let c2 = clip(0, 7680, vec![note(3360, 960, 65, 90)]);
        let ev2 = clip_events(&c2, &map);
        assert_eq!(ev2[0].sample, 84_000);
        assert_eq!(ev2[1].sample, 120_000);
    }

    #[test]
    fn clip_window_clips_notes() {
        let c = clip(
            0,
            960, // one beat long
            vec![
                note(0, 4000, 60, 100),   // off clamped to clip end
                note(960, 100, 61, 100),  // onset at clip end -> dropped
                note(2000, 100, 62, 100), // onset past clip end -> dropped
            ],
        );
        let ev = clip_events(&c, &map_120());
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[1], AbsNoteEvent { sample: 24_000, key: 60, velocity: 0 });
    }

    #[test]
    fn off_sorts_before_on_at_same_sample_and_zero_len_gets_one_sample() {
        // Two back-to-back notes on the same key: off of the first == on of
        // the second.
        let c = clip(0, 1920, vec![note(0, 960, 60, 100), note(960, 960, 60, 100)]);
        let ev = clip_events(&c, &map_120());
        assert_eq!(ev[1].sample, 24_000);
        assert_eq!(ev[1].velocity, 0, "off first at the shared boundary");
        assert_eq!(ev[2].sample, 24_000);
        assert_eq!(ev[2].velocity, 100);

        // Degenerate rounding: a 0-tick... 1-tick note at extreme tempo still
        // produces off strictly after on.
        let fast = TempoMap::from_v1(960.0, 8_000).unwrap(); // ~0.5 samples/tick
        let c2 = clip(0, 960, vec![note(0, 1, 60, 100)]);
        let ev2 = clip_events(&c2, &fast);
        assert!(ev2[1].sample > ev2[0].sample);
    }

    #[test]
    fn block_slicing_covers_boundaries_exactly_once() {
        let c = clip(0, 3840, vec![note(512, 512, 60, 100), note(1024, 256, 62, 90)]);
        // Identity-ish map: use 1 tick = 25 samples (120bpm/48k), so on at
        // 12800, off 25600, on 25600, off 32000.
        let ev = clip_events(&c, &map_120());
        // Re-slice the whole timeline into 512-frame blocks and collect.
        let mut collected = Vec::new();
        let mut block_start = 0u64;
        while block_start < 40_000 {
            for b in slice_block(&ev, block_start, 512) {
                collected.push((block_start + b.offset as u64, b.key, b.velocity));
            }
            block_start += 512;
        }
        let expect: Vec<(u64, u8, u8)> =
            ev.iter().map(|e| (e.sample, e.key, e.velocity)).collect();
        assert_eq!(collected, expect, "every event exactly once, correct offsets");

        // An event exactly ON a block boundary belongs to the block it starts.
        let ev2 = vec![AbsNoteEvent { sample: 1024, key: 70, velocity: 100 }];
        assert_eq!(slice_block(&ev2, 512, 512).count(), 0);
        let hits: Vec<_> = slice_block(&ev2, 1024, 512).collect();
        assert_eq!(hits, vec![BlockNoteEvent { offset: 0, key: 70, velocity: 100 }]);
    }
}
