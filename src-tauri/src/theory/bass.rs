//! Bass lines: the harmony's other face.
//!
//! The bass is not "the chord's root, low". It is the voice that *decides* the
//! inversion — a C major triad over an E bass is a different chord in context —
//! and the one that carries the metre. Two mechanisms, both cheap and both real
//! theory:
//!
//! * **Roots on the strong beats**, so the harmony is unambiguous. Which beats
//!   are strong comes from `metre::Grid::weights`, the same array the drums and
//!   the comp use (ruling H-10).
//! * **Approach notes into the next root** on the weakest position before a
//!   chord change: a step or a semitone away, which is what makes a walking
//!   line sound like it is going somewhere rather than restating.

use crate::midi::MidiNote;

use super::harmony::HarmonyDoc;
use super::metre::{velocity_for_weight, Grid};
use super::tpc::Pitch;
use super::Annotation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BassStyle {
    /// One root per chord, held. The most legible, and the right default under
    /// a busy arrangement.
    Root,
    /// Root on the downbeat, fifth on the second strong beat — the oldest bass
    /// pattern there is.
    RootFifth,
    /// A note per beat: chord tones on the strong ones, an approach note into
    /// the next chord on the last.
    Walking,
    /// One long note per chord, an octave lower — a pedal.
    Pedal,
}

impl BassStyle {
    pub fn parse(s: &str) -> Option<BassStyle> {
        match s.trim().to_ascii_lowercase().replace(['-', '_', ' '], "").as_str() {
            "root" => Some(BassStyle::Root),
            "rootfifth" | "rootandfifth" => Some(BassStyle::RootFifth),
            "walking" | "walk" => Some(BassStyle::Walking),
            "pedal" => Some(BassStyle::Pedal),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            BassStyle::Root => "root",
            BassStyle::RootFifth => "rootFifth",
            BassStyle::Walking => "walking",
            BassStyle::Pedal => "pedal",
        }
    }

    fn why(self) -> &'static str {
        match self {
            BassStyle::Root => {
                "One root per chord. The bass's first job is to make the harmony unambiguous, and \
                 nothing does that better than the root on the downbeat."
            }
            BassStyle::RootFifth => {
                "Root on beat one, fifth on the next strong beat. The fifth is the one other note \
                 that cannot change what chord this is, which is why this pattern has survived \
                 four hundred years of everything."
            }
            BassStyle::Walking => {
                "A note per beat: chord tones on the strong beats, and an APPROACH note on the \
                 last one — a step or a semitone from the next chord's root. The approach is the \
                 whole trick: it makes the next chord sound arrived at rather than merely next."
            }
            BassStyle::Pedal => {
                "One sustained note under the changes. A pedal makes the harmony above it sound \
                 like tension rather than movement — which is exactly what it is for."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BassOptions {
    pub lo: u8,
    pub hi: u8,
    pub style: BassStyle,
}

impl Default for BassOptions {
    fn default() -> Self {
        // E1..E3: a bass guitar's range, below the default comp register.
        Self { lo: 28, hi: 52, style: BassStyle::Root }
    }
}

/// Generate a bass line under the harmony document.
pub fn bass_notes(
    harmony: &HarmonyDoc,
    grid: &Grid,
    start_tick: u64,
    opts: &BassOptions,
) -> (Vec<MidiNote>, Vec<Annotation>) {
    let spans: Vec<_> = harmony.chords.iter().filter(|c| c.end_tick() > start_tick).collect();
    if spans.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let weights = grid.weights();
    let max_weight = grid.max_weight();
    let beat_ticks = grid.ticks_per_step() as u64 * grid.steps_per_beat as u64;
    let mut notes: Vec<MidiNote> = Vec::new();
    // Each chord's bass note is placed at the octave nearest the line's ANCHOR
    // — the first chord's placement — rather than nearest the previous note.
    // Chaining off the previous note looks equivalent and is not: the errors
    // accumulate, and a progression that keeps stepping to the closer octave
    // walks itself into the bottom of the register and stays there (a real
    // sketch produced `C2 G1 A1 F1`). Anchoring keeps every root within a
    // fifth or so of home. WITHIN a bar, walking tones still follow the note
    // before them, which is where local smoothness actually matters.
    let anchor = octave_into(spans[0].chord.bass.unwrap_or(spans[0].chord.root), opts);
    // Where the line currently is, so a new chord starts near it rather than
    // near a fixed octave.
    let mut here = anchor;

    for (i, span) in spans.iter().enumerate() {
        // The bass plays the chord's BASS note when one is named (that is what
        // a slash chord means), otherwise the root.
        let bass_tpc = span.chord.bass.unwrap_or(span.chord.root);
        let root_key = octave_near_bounded(bass_tpc, here, anchor, opts);
        here = root_key;
        let rel = span.tick.saturating_sub(start_tick);
        // The approach note aims at where the NEXT chord's bass will actually
        // be, so it has to be resolved with the same rule — from wherever this
        // bar ends up, which the walking arm updates below.
        let next_tpc = spans.get(i + 1).map(|s| s.chord.bass.unwrap_or(s.chord.root));

        match opts.style {
            BassStyle::Root | BassStyle::Pedal => {
                let key = if opts.style == BassStyle::Pedal {
                    // A pedal holds the FIRST chord's bass under everything.
                    octave_into(
                        spans[0].chord.bass.unwrap_or(spans[0].chord.root),
                        opts,
                    )
                } else {
                    root_key
                };
                notes.push(note(rel, span.length_ticks, key, weights[0], max_weight));
            }
            BassStyle::RootFifth => {
                notes.push(note(rel, beat_ticks.min(span.length_ticks), root_key, weights[0], max_weight));
                // The second strong beat of the span, if there is one.
                let half = span.length_ticks / 2;
                if half >= beat_ticks {
                    let fifth = octave_near_bounded(bass_tpc.plus_fifths(1), root_key, anchor, opts);
                    here = fifth;
                    let step = ((half / grid.ticks_per_step() as u64) as usize) % weights.len();
                    notes.push(note(rel + half, span.length_ticks - half, fifth, weights[step], max_weight));
                }
            }
            BassStyle::Walking => {
                let beats = (span.length_ticks / beat_ticks.max(1)).max(1);
                let tones = span.chord.tones();
                let mut walking_prev = root_key;
                for b in 0..beats {
                    let at = rel + b * beat_ticks;
                    let step = ((at / grid.ticks_per_step() as u64) as usize) % weights.len();
                    let last = b + 1 == beats;
                    let key = match (last, next_tpc) {
                        // The approach note: a step from where the next chord's
                        // bass will be, resolved from where this bar ended.
                        (true, Some(target_tpc)) => {
                            let target =
                                octave_near_bounded(target_tpc, walking_prev, anchor, opts);
                            approach(target, opts)
                        }
                        // Beat 1 is the chord's own bass; the rest walk to the
                        // nearest placement of the next chord tone.
                        _ if b == 0 => root_key,
                        _ => octave_near_bounded(
                            tones[(b as usize) % tones.len()],
                            walking_prev,
                            anchor,
                            opts,
                        ),
                    };
                    walking_prev = key;
                    notes.push(note(at, beat_ticks, key, weights[step], max_weight));
                }
                here = walking_prev;
            }
        }
    }
    notes.sort_by_key(|n| (n.tick, n.key));

    let mut annotations = vec![Annotation::new(
        0,
        spans.first().map(|s| s.length_ticks).unwrap_or(0),
        format!("bass · {}", opts.style.id()),
        opts.style.why().to_string(),
    )];
    if opts.style == BassStyle::Walking && spans.len() > 1 {
        annotations.push(Annotation::new(
            spans[0].end_tick().saturating_sub(start_tick + beat_ticks),
            beat_ticks,
            "approach",
            format!(
                "This beat is not a chord tone: it is a step away from {}, the next chord's bass. \
                 Approach notes are the reason a walking line pulls forward.",
                spans[1].chord.bass.unwrap_or(spans[1].chord.root).pretty(),
            ),
        ));
    }
    (notes, annotations)
}

fn note(tick: u64, length: u64, key: u8, weight: u8, max_weight: u8) -> MidiNote {
    MidiNote {
        tick: tick as u32,
        length_ticks: length.max(1) as u32,
        key,
        velocity: velocity_for_weight(weight, max_weight, 76, 30),
        channel: 0,
        note_id: crate::ids::NoteId(0),
    }
}

/// Place a spelling in the bass register, as low as it fits. The anchor for
/// the FIRST note of a line; every note after it goes through
/// [`octave_near`] instead.
fn octave_into(tpc: super::Tpc, opts: &BassOptions) -> u8 {
    let mut p = Pitch::at_or_above(tpc, opts.lo as i16);
    while p.midi() > opts.hi as i16 && p.midi() - 12 >= opts.lo as i16 {
        p.octave -= 1;
    }
    p.midi().clamp(opts.lo as i16, opts.hi as i16) as u8
}

/// Nearest to `near`, but never further than a fifth from `anchor` — the two
/// rules a bass line needs at once.
///
/// Nearest-to-the-previous-note alone accumulates: each chord steps to its
/// closer octave and the line walks into the floor of the register (`C2 G1 A1
/// F1`). Nearest-to-the-anchor alone ignores where the line actually IS, so a
/// bar that climbed to `G2` gets yanked back down to `G1` at the bar line — a
/// 13-semitone drop. Both were real, both came from reading a dumped sketch.
fn octave_near_bounded(tpc: super::Tpc, near: u8, anchor: u8, opts: &BassOptions) -> u8 {
    let base = Pitch::at_or_above(tpc, opts.lo as i16).midi();
    let candidates: Vec<i16> = (0..)
        .map(|k| base + 12 * k)
        .take_while(|m| *m <= opts.hi as i16)
        .collect();
    candidates
        .iter()
        .copied()
        .filter(|m| (m - anchor as i16).abs() <= 7)
        .min_by_key(|m| (m - near as i16).abs())
        .or_else(|| candidates.iter().copied().min_by_key(|m| (m - anchor as i16).abs()))
        .unwrap_or(base)
        .clamp(opts.lo as i16, opts.hi as i16) as u8
}

/// A note a step or a semitone below `target`, inside the register. Below
/// rather than above because a rising approach is the strong one — it borrows
/// the leading tone's pull.
fn approach(target: u8, opts: &BassOptions) -> u8 {
    for d in [1i16, 2, -1, -2] {
        let cand = target as i16 - d;
        if (opts.lo as i16..=opts.hi as i16).contains(&cand) {
            return cand as u8;
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::chord::Chord;
    use crate::theory::metre::Meter;
    use crate::theory::scale::Key;

    fn grid() -> Grid {
        Grid::new(Meter::FOUR_FOUR, 960, 4)
    }

    fn harmony() -> HarmonyDoc {
        HarmonyDoc::from_chords(
            Key::c_major(),
            0,
            3840,
            &["C", "Am", "F", "G7"].map(|s| Chord::parse(s).unwrap()),
        )
    }

    #[test]
    fn root_style_plays_one_root_per_chord() {
        let (notes, ann) = bass_notes(&harmony(), &grid(), 0, &BassOptions::default());
        assert_eq!(notes.len(), 4);
        assert_eq!(
            notes.iter().map(|n| n.key % 12).collect::<Vec<_>>(),
            vec![0, 9, 5, 7],
            "C A F G"
        );
        assert!(notes.iter().all(|n| n.length_ticks == 3840));
        assert!(ann[0].why.contains("unambiguous"));
    }

    #[test]
    fn everything_stays_in_the_bass_register() {
        let opts = BassOptions { lo: 28, hi: 52, style: BassStyle::Walking };
        for style in [BassStyle::Root, BassStyle::RootFifth, BassStyle::Walking, BassStyle::Pedal] {
            let (notes, _) = bass_notes(&harmony(), &grid(), 0, &BassOptions { style, ..opts });
            assert!(!notes.is_empty(), "{}", style.id());
            assert!(notes.iter().all(|n| (28..=52).contains(&n.key)), "{}: {:?}", style.id(), notes.iter().map(|n| n.key).collect::<Vec<_>>());
        }
    }

    #[test]
    fn root_fifth_puts_the_fifth_on_the_second_strong_beat() {
        let (notes, _) =
            bass_notes(&harmony(), &grid(), 0, &BassOptions { style: BassStyle::RootFifth, ..Default::default() });
        assert_eq!(notes.len(), 8, "two notes per bar");
        let bar1: Vec<u8> = notes.iter().filter(|n| n.tick < 3840).map(|n| n.key % 12).collect();
        assert_eq!(bar1, vec![0, 7], "C then G");
        assert_eq!(notes.iter().find(|n| n.tick == 1920).map(|n| n.key % 12), Some(7));
    }

    #[test]
    fn a_walking_line_approaches_the_next_root_by_step() {
        let (notes, ann) =
            bass_notes(&harmony(), &grid(), 0, &BassOptions { style: BassStyle::Walking, ..Default::default() });
        assert_eq!(notes.len(), 16, "four beats a bar");
        // The last beat of bar 1 must be within two semitones of Am's root.
        let last_of_bar1 = notes.iter().filter(|n| n.tick < 3840).max_by_key(|n| n.tick).unwrap();
        let a_root = notes.iter().find(|n| n.tick == 3840).unwrap().key;
        let d = (last_of_bar1.key as i16 - a_root as i16).abs();
        assert!((1..=2).contains(&d), "approach was {d} semitones");
        assert!(ann.iter().any(|a| a.label == "approach"));
        // Strong beats are chord tones of their own chord.
        let c_pcs = Chord::parse("C").unwrap().pitch_classes();
        assert!(c_pcs.contains(&(notes[0].key % 12)));
    }

    #[test]
    fn a_line_moves_by_the_shortest_path_and_does_not_sink() {
        // Two bugs at once, both found by reading a dumped sketch:
        //   * every note was independently pushed to the bottom of the range,
        //     so a walking bar came out `C2 E1 G1` (octave leaps) instead of
        //     `C2 E2 G2`;
        //   * chaining each chord's root off the PREVIOUS one accumulated, and
        //     the line walked itself into the floor of the register (`C2 G1 A1
        //     F1`) and stayed there.
        let (notes, _) = bass_notes(
            &harmony(),
            &grid(),
            0,
            &BassOptions { style: BassStyle::Walking, ..Default::default() },
        );
        for w in notes.windows(2) {
            let d = (w[1].key as i16 - w[0].key as i16).abs();
            assert!(d <= 12, "a bass line should not leap an octave: {} -> {}", w[0].key, w[1].key);
        }
        // Every chord's downbeat stays within a fifth of the first one — the
        // line has a home register instead of drifting out of one.
        let downbeats: Vec<u8> =
            notes.iter().filter(|n| n.tick % 3840 == 0).map(|n| n.key).collect();
        assert_eq!(downbeats.len(), 4);
        for k in &downbeats {
            assert!(
                (*k as i16 - downbeats[0] as i16).abs() <= 7,
                "downbeats drifted: {downbeats:?}"
            );
        }
    }

    #[test]
    fn a_pedal_holds_the_first_bass_under_every_chord() {
        let (notes, _) =
            bass_notes(&harmony(), &grid(), 0, &BassOptions { style: BassStyle::Pedal, ..Default::default() });
        assert_eq!(notes.len(), 4);
        assert!(notes.iter().all(|n| n.key == notes[0].key), "one note, four times");
        assert_eq!(notes[0].key % 12, 0);
    }

    #[test]
    fn a_slash_chord_puts_its_named_bass_in_the_bass() {
        let h = HarmonyDoc::from_chords(
            Key::c_major(),
            0,
            3840,
            &["C/E", "C/G"].map(|s| Chord::parse(s).unwrap()),
        );
        let (notes, _) = bass_notes(&h, &grid(), 0, &BassOptions::default());
        assert_eq!(notes.iter().map(|n| n.key % 12).collect::<Vec<_>>(), vec![4, 7], "E then G");
    }

    #[test]
    fn ticks_are_clip_relative_and_empty_input_is_silence() {
        let h = HarmonyDoc::from_chords(Key::c_major(), 7680, 3840, &[Chord::parse("F").unwrap()]);
        let (notes, _) = bass_notes(&h, &grid(), 7680, &BassOptions::default());
        assert_eq!(notes[0].tick, 0);
        assert!(bass_notes(&HarmonyDoc::default(), &grid(), 0, &BassOptions::default()).0.is_empty());
    }

    #[test]
    fn styles_parse_from_the_wire() {
        assert_eq!(BassStyle::parse("walking"), Some(BassStyle::Walking));
        assert_eq!(BassStyle::parse("rootFifth"), Some(BassStyle::RootFifth));
        assert_eq!(BassStyle::parse("root-fifth"), Some(BassStyle::RootFifth));
        assert_eq!(BassStyle::parse("nonsense"), None);
    }
}
