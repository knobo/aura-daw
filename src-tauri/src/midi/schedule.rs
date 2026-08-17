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
use crate::ids::NoteId;

/// A note edge at an absolute engine-sample position. `velocity == 0` is a
/// note-off (same convention as [`BlockNoteEvent`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsNoteEvent {
    /// Absolute timeline position in engine samples.
    pub sample: u64,
    pub key: u8,
    pub velocity: u8,
}

/// One expanded note as a *span* in absolute engine samples, carrying the
/// identity [`AbsNoteEvent`] drops. A consumer that must address a note back
/// (the Pitch Coach report seeks to one) needs that identity; the scheduler
/// does not — so the two views share one expansion and differ only in shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsNote {
    /// Absolute onset in engine samples.
    pub start: u64,
    /// Absolute release in engine samples, always `> start`.
    pub end: u64,
    /// Key after the clip's transpose — what the synth would sound.
    pub key: u8,
    /// Stable FNV-1a hash of the source clip id. Keeps [`AbsNote`] `Copy`
    /// while still telling two clips' note 7 apart.
    pub clip_id_hash: u64,
    /// Identity of the note *inside* the clip. Every repeat of a looped
    /// clip's content reports the SAME `note_id`; `repeat_index` separates
    /// them.
    pub note_id: NoteId,
    /// Which pass over the clip content this note came from (0 = the first).
    pub repeat_index: u32,
}

/// FNV-1a over a clip id's bytes. Not [`std::hash::DefaultHasher`], which is
/// explicitly not stable across Rust releases — this value travels to the
/// frontend inside a report.
pub fn clip_id_hash(clip_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in clip_id.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// One expanded note, before it is shaped into edges or into a span.
struct Expanded {
    on_s: u64,
    off_s: u64,
    key: u8,
    velocity: u8,
    note_id: NoteId,
    repeat_index: u32,
}

/// The single expansion rule behind [`clip_events`] and [`clip_notes`]:
/// content repeats, onset clipping at the placement end, off clamping and
/// the 1-sample floor. Emission is repeat-major, then written order; both
/// callers sort afterwards.
fn expand_notes(clip: &MidiClip, map: &TempoMap, mut emit: impl FnMut(Expanded)) {
    let clip_end_tick = clip.timeline_start_ticks.saturating_add(clip.length_ticks);
    let content_len = clip.effective_content_length_ticks();
    let repeats = clip.length_ticks.div_ceil(content_len).max(1);
    for rep in 0..repeats {
        let rep_start_tick =
            clip.timeline_start_ticks.saturating_add(rep.saturating_mul(content_len));
        for n in &clip.notes {
            if n.velocity == 0 || n.length_ticks == 0 {
                continue;
            }
            let on_tick = rep_start_tick.saturating_add(n.tick as u64);
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
            let (key, velocity) = clip_note_key_vel(clip, n);
            emit(Expanded { on_s, off_s, key, velocity, note_id: n.note_id, repeat_index: rep as u32 });
        }
    }
}

fn clip_note_key_vel(clip: &MidiClip, n: &crate::midi::types::MidiNote) -> (u8, u8) {
    let key = (n.key as i32 + clip.transpose_semitones as i32).clamp(0, 127) as u8;
    let velocity = if n.velocity == 0 {
        0
    } else {
        (n.velocity as i32 + clip.velocity_offset as i32).clamp(1, 127) as u8
    };
    (key, velocity)
}

/// Expand a clip's notes into absolute-sample note-on/off edges, sorted by
/// (sample, offs-before-ons). Rules:
///
/// * Content (one pass over `clip.notes`) repeats every
///   `effective_content_length_ticks()` ticks until the placement
///   (`length_ticks`) ends (spec: drag-to-loop). `content == placement` (the
///   `None` default) is a single repeat — byte-identical to the pre-looping
///   behavior.
/// * Note onsets at/after the clip end are dropped (the clip window clips
///   its content), in every repeat.
/// * Note-offs are clamped to the clip end tick — including the final
///   partial repeat's tail.
/// * Zero-length results (rounding) still get a 1-sample duration so every
///   on has its off strictly after it.
pub fn clip_events(clip: &MidiClip, map: &TempoMap) -> Vec<AbsNoteEvent> {
    let mut out = Vec::with_capacity(clip.notes.len() * 2);
    expand_notes(clip, map, |e| {
        out.push(AbsNoteEvent { sample: e.on_s, key: e.key, velocity: e.velocity });
        out.push(AbsNoteEvent { sample: e.off_s, key: e.key, velocity: 0 });
    });
    // Offs before ons at the same sample position so retriggers of the same
    // key release the previous voice first.
    out.sort_by_key(|e| (e.sample, e.velocity));
    out
}

/// The same expansion as [`clip_events`], shaped as note *spans* that keep
/// their identity, sorted by `(start, key)`.
///
/// The Pitch Coach's reference melody goes through this: the report must be
/// able to say "note 7, second repeat, was 30 cents flat" and seek there,
/// which note-on/note-off edges cannot express. Both functions call
/// [`expand_notes`], so a change to the repeat rule cannot move the
/// scheduler and the report apart — `clip_notes_and_clip_events_agree_on_timing`
/// is the test that pins them together.
pub fn clip_notes(clip: &MidiClip, map: &TempoMap) -> Vec<AbsNote> {
    let hash = clip_id_hash(clip.id.as_str());
    let mut out = Vec::with_capacity(clip.notes.len());
    expand_notes(clip, map, |e| {
        out.push(AbsNote {
            start: e.on_s,
            end: e.off_s,
            key: e.key,
            clip_id_hash: hash,
            note_id: e.note_id,
            repeat_index: e.repeat_index,
        });
    });
    out.sort_by_key(|n| (n.start, n.key));
    out
}

/// Drive-clip notes for the launch mapper: **one pass** over the written
/// notes, no content-loop expansion. A piano-roll with one note must fire
/// once per playhead pass, even if the placement is longer than the content
/// (drag-to-loop). Transport loop wrap still retriggers, because the same
/// onset sample is crossed again.
pub fn clip_drive_events(clip: &MidiClip, map: &TempoMap) -> Vec<AbsNoteEvent> {
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
        let (key, velocity) = clip_note_key_vel(clip, n);
        out.push(AbsNoteEvent { sample: on_s, key, velocity });
        out.push(AbsNoteEvent { sample: off_s, key, velocity: 0 });
    }
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

    fn note_id(id: u32, tick: u32, len: u32, key: u8) -> MidiNote {
        MidiNote { note_id: NoteId(id), ..note(tick, len, key, 100) }
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
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        }
    }

    /// 120 bpm @ 48 kHz, ppq 960: 1 tick = 25 samples exactly.
    fn map_120() -> TempoMap {
        TempoMap::from_v1(120.0, 48_000).unwrap()
    }

    #[test]
    fn repeats_content_across_a_longer_placement() {
        // Content is one beat (960 ticks) long, placement is 2 beats — the
        // single note repeats once, at content-length offset.
        let mut c = clip(0, 1920, vec![note(0, 480, 60, 100)]);
        c.content_length_ticks = Some(960);
        let ev = clip_events(&c, &map_120());
        assert_eq!(
            ev,
            vec![
                AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
                AbsNoteEvent { sample: 480 * 25, key: 60, velocity: 0 },
                AbsNoteEvent { sample: 960 * 25, key: 60, velocity: 100 },
                AbsNoteEvent { sample: (960 + 480) * 25, key: 60, velocity: 0 },
            ],
            "note repeats once at the content-length offset"
        );
    }

    #[test]
    fn partial_final_repeat_clamps_the_note_tail() {
        // Content is one beat (960 ticks); placement is 1.5 beats (1440) —
        // the second repeat's note-on lands inside the placement, but its
        // note-off must clamp to the placement end, not run past it.
        let mut c = clip(0, 1440, vec![note(0, 960, 60, 100)]); // near-full-beat note
        c.content_length_ticks = Some(960);
        let ev = clip_events(&c, &map_120());
        assert_eq!(ev.len(), 4, "both repeats' onsets are before placement end (1440)");
        assert_eq!(ev[0], AbsNoteEvent { sample: 0, key: 60, velocity: 100 });
        assert_eq!(ev[1], AbsNoteEvent { sample: 960 * 25, key: 60, velocity: 0 });
        assert_eq!(ev[2], AbsNoteEvent { sample: 960 * 25, key: 60, velocity: 100 });
        // Off would naturally land at (960+960)*25 = 1920*25, but placement
        // ends at 1440*25 — clamp there.
        assert_eq!(ev[3], AbsNoteEvent { sample: 1440 * 25, key: 60, velocity: 0 });
    }

    #[test]
    fn drive_events_ignore_content_repeats() {
        let mut c = clip(0, 1920, vec![note(0, 480, 60, 100)]);
        c.content_length_ticks = Some(960);
        let ev = clip_drive_events(&c, &map_120());
        assert_eq!(
            ev,
            vec![
                AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
                AbsNoteEvent { sample: 480 * 25, key: 60, velocity: 0 },
            ],
            "one written note → one on/off, even when the placement loops the content"
        );
    }

    #[test]
    fn drive_events_apply_transpose_and_velocity_offset() {
        let mut c = clip(0, 1920, vec![note(0, 480, 60, 100)]);
        c.transpose_semitones = 2;
        c.velocity_offset = -10;
        let ev = clip_drive_events(&c, &map_120());
        assert_eq!(ev[0].key, 62);
        assert_eq!(ev[0].velocity, 90);
        assert_eq!(ev[1].key, 62);
        assert_eq!(ev[1].velocity, 0);
    }

    #[test]
    fn content_equal_to_placement_is_a_no_op() {
        // No content_length_ticks set (None) and an explicit Some equal to
        // length_ticks must produce IDENTICAL output to today's behavior.
        let c = clip(3840, 3840, vec![note(960, 960, 60, 100)]);
        let mut c_explicit = c.clone();
        c_explicit.content_length_ticks = Some(3840);
        let baseline = clip_events(&c, &map_120());
        assert_eq!(clip_events(&c_explicit, &map_120()), baseline, "explicit == placement matches absent");
        assert_eq!(baseline.len(), 2, "single repeat, unchanged from the pre-looping behavior");
    }

    #[test]
    fn tempo_change_mid_repetition_moves_the_later_repeat() {
        // 120bpm for bar 1, 60bpm after. Content is one bar (3840 ticks);
        // placement is two bars — repeat 2 starts exactly at the tempo
        // change and must use the new tempo's sample rate.
        let map = TempoMap::new(
            960,
            vec![TempoEvent { tick: 0, bpm: 120.0 }, TempoEvent { tick: 3840, bpm: 60.0 }],
            48_000,
        )
        .unwrap();
        let mut c = clip(0, 7680, vec![note(0, 480, 64, 90)]);
        c.content_length_ticks = Some(3840);
        let ev = clip_events(&c, &map);
        // Repeat 0: on at tick 0 (120bpm) -> sample 0.
        assert_eq!(ev[0], AbsNoteEvent { sample: 0, key: 64, velocity: 90 });
        // Repeat 1: on at tick 3840 (exactly the tempo change) -> 3840*25 = 96000.
        assert_eq!(ev[2].sample, 96_000);
        assert_eq!(ev[2].velocity, 90);
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
    fn clip_notes_and_clip_events_agree_on_timing() {
        // Content is 2 bars, placement 4 bars, so the content repeats once.
        // The last note's off lands past the placement end in the second
        // repeat and must be clamped identically by both functions.
        let bar = 960 * 4;
        let mut c = clip(
            0,
            (4 * bar) as u64,
            vec![
                note_id(1, 0, 480, 60),
                note_id(2, bar, 480, 62),
                note_id(3, 2 * bar - 240, 960, 64), // runs past the content end
            ],
        );
        c.content_length_ticks = Some((2 * bar) as u64);
        let map = map_120();
        let events = clip_events(&c, &map);
        let notes = clip_notes(&c, &map);

        let mut ons: Vec<u64> = events.iter().filter(|e| e.velocity > 0).map(|e| e.sample).collect();
        ons.sort_unstable();
        let mut starts: Vec<u64> = notes.iter().map(|n| n.start).collect();
        starts.sort_unstable();
        assert_eq!(starts, ons, "the two expansions must not drift");

        let mut offs: Vec<u64> = events.iter().filter(|e| e.velocity == 0).map(|e| e.sample).collect();
        offs.sort_unstable();
        let mut ends: Vec<u64> = notes.iter().map(|n| n.end).collect();
        ends.sort_unstable();
        assert_eq!(ends, offs, "note ends must match note-offs");
        assert_eq!(notes.len(), 6, "three notes over two repeats");
    }

    #[test]
    fn clip_notes_carries_identity_across_repeats() {
        let bar = 960 * 4;
        let mut c = clip(0, (3 * bar) as u64, vec![note_id(7, 0, 480, 60)]);
        c.content_length_ticks = Some(bar as u64);
        let notes = clip_notes(&c, &map_120());
        assert_eq!(notes.len(), 3);
        assert!(notes.iter().all(|n| n.note_id == NoteId(7)), "same source note");
        assert_eq!(notes.iter().map(|n| n.repeat_index).collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(
            notes.iter().all(|n| n.clip_id_hash == clip_id_hash("c1")),
            "every span names its source clip"
        );
        assert_ne!(clip_id_hash("c1"), clip_id_hash("c2"), "different clips, different hash");
    }

    #[test]
    fn clip_notes_applies_transpose_and_orders_a_chord_by_key() {
        let mut c = clip(0, 1920, vec![note_id(1, 0, 480, 64), note_id(2, 0, 480, 60)]);
        c.transpose_semitones = 2;
        let notes = clip_notes(&c, &map_120());
        assert_eq!(
            notes.iter().map(|n| n.key).collect::<Vec<_>>(),
            vec![62, 66],
            "sorted by (start, key), transposed as the scheduler sounds it"
        );
        assert!(notes.iter().all(|n| n.end > n.start), "every span has a positive length");
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
