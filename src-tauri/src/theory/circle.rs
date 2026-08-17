//! The circle of fifths as an instrument, not a diagram.
//!
//! Three facts do all the work here (product doc §4.4):
//!
//! 1. **A key's whole diatonic chord vocabulary is a CONTIGUOUS arc.**
//!    C major is `F C G | D A E | B` — three majors, three minors, one
//!    diminished, in circle order. Rotating the arc *is* modulating, and the
//!    slots just outside it are exactly the borrowed chords.
//! 2. **Distance on the circle is relatedness** — one step is one accidental,
//!    which is why the dominant, subdominant, relative and parallel keys are
//!    the modulations that sound easy.
//! 3. **Counter-clockwise motion is the strongest harmonic drive in tonal
//!    music** — `V→I` is one step, and `iii–vi–ii–V–I` is four.
//!
//! Everything this module returns is data the UI *draws*; no geometry lives
//! here and no theory lives in the widget (ADR 0006).

use super::analysis::{analyze, Function};
use super::chord::{diatonic_chord, Chord, ChordQuality};
use super::scale::{Key, ScaleType};
use super::tpc::Tpc;

/// One of the twelve slots of the circle, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Wedge {
    /// Line-of-fifths index of this slot, spelled for the current key (E
    /// major gets `D♯`, B♭ major gets `E♭`).
    pub fifths: i16,
    /// The major key that lives here.
    pub major: Tpc,
    /// Its relative minor — the inner ring of every circle-of-fifths poster.
    pub minor: Tpc,
    /// Inside the current key's diatonic arc?
    pub in_key: bool,
    /// The chord the current key builds on this slot, when the slot is in the
    /// arc. `None` outside the arc — see [`Wedge::borrowed_chord`].
    pub chord: Option<Chord>,
    /// Roman numeral of `chord`, or of `borrowed_chord` when outside the arc.
    pub roman: Option<String>,
    /// The chord this slot offers as a BORROWED chord when it is outside the
    /// arc: the major triad on that root, which is what "borrowing" means in
    /// practice (♭VII, ♭III, ♭VI, or a secondary dominant).
    pub borrowed_chord: Option<Chord>,
    /// `tonic` / `dominant` / `predominant` / `chromatic`, for colouring.
    pub function: Option<&'static str>,
    /// True for the slot the key's tonic sits on — the UI rotates this to the
    /// top rather than hard-coding C.
    pub is_tonic: bool,
}

/// The key's diatonic arc: the contiguous fifths range its scale occupies.
/// Only meaningful for diatonic modes; a non-diatonic scale reports the arc of
/// its diatonic parent (the same convention [`Key::key_signature_fifths`]
/// uses, and for the same reason).
pub fn arc(key: &Key) -> std::ops::RangeInclusive<i16> {
    let centre = key.key_signature_fifths();
    // A major key's arc runs from one fifth below its tonic to five above.
    (centre - 1)..=(centre + 5)
}

/// The twelve wedges, in clockwise (rising-fifths) order, spelled for `key`.
///
/// The window is `centre-5 ..= centre+6`: twelve consecutive fifths, which is
/// every pitch class exactly once, spelled the way this key's signature would
/// spell it. That is why the same widget shows `D♯` in E major and `E♭` in
/// B♭ major without a special case.
pub fn wedges(key: &Key) -> Vec<Wedge> {
    let centre = key.key_signature_fifths();
    let in_arc = arc(key);
    (centre - 5..=centre + 6)
        .map(|f| {
            let major = Tpc(f);
            let in_key = in_arc.contains(&f);
            // Which scale degree of `key` this slot is, when it is in the arc.
            let degree = key
                .spelled()
                .iter()
                .position(|t| t.pitch_class() == major.pitch_class());
            let chord = if in_key {
                degree.and_then(|d| diatonic_chord(key, d, false))
            } else {
                None
            };
            let borrowed_chord =
                (!in_key).then(|| Chord::new(major, ChordQuality::Maj));
            let shown = chord.or(borrowed_chord);
            let roman = shown.map(|c| analyze(&c, key).roman);
            let function = chord.map(|c| match analyze(&c, key).function {
                Function::Tonic => "tonic",
                Function::Predominant => "predominant",
                Function::Dominant => "dominant",
                Function::Chromatic => "chromatic",
            });
            Wedge {
                fifths: f,
                major,
                minor: major.plus_fifths(3),
                in_key,
                chord,
                roman,
                borrowed_chord,
                function,
                is_tonic: major.pitch_class() == key.tonic.pitch_class(),
            }
        })
        .collect()
}

/// Steps between two keys on the circle, by key signature: `0` for relatives
/// (C major / A minor share a signature), `1` for C→G, `6` for the tritone.
/// Always `0..=6` — the circle wraps, so seven steps clockwise is five
/// counter-clockwise.
pub fn distance(a: &Key, b: &Key) -> i16 {
    let d = (b.key_signature_fifths() - a.key_signature_fifths()).rem_euclid(12);
    d.min(12 - d)
}

/// Relative major ↔ relative minor: the same seven notes, a different home.
pub fn relative(key: &Key) -> Key {
    if key.scale.is_minorish() {
        Key::new(key.tonic.plus_fifths(-3), ScaleType::Ionian)
    } else {
        Key::new(key.tonic.plus_fifths(3), ScaleType::Aeolian)
    }
}

/// Parallel major ↔ minor: the same tonic, three accidentals apart. The
/// distance is exactly why a parallel switch is a dramatic move and a
/// relative one is not.
pub fn parallel(key: &Key) -> Key {
    if key.scale.is_minorish() {
        Key::new(key.tonic, ScaleType::Ionian)
    } else {
        Key::new(key.tonic, ScaleType::Aeolian)
    }
}

/// The keys one easy move away, each with the sentence that says what the
/// move does. This is the modulation menu.
pub fn neighbours(key: &Key) -> Vec<(Key, String)> {
    let mut out = vec![
        (
            Key::new(key.tonic.plus_fifths(1), key.scale),
            "One step clockwise: the dominant key. One new sharp, and the brightest of the \
             easy moves — everything feels like it lifted."
                .to_string(),
        ),
        (
            Key::new(key.tonic.plus_fifths(-1), key.scale),
            "One step counter-clockwise: the subdominant key. One new flat, and it settles \
             rather than lifts."
                .to_string(),
        ),
    ];
    let rel = relative(key);
    out.push((
        rel,
        format!(
            "The relative {}: the same seven notes as {}, so nothing clashes — only the centre \
             of gravity moves. The cheapest modulation there is.",
            if rel.scale.is_minorish() { "minor" } else { "major" },
            key.label(),
        ),
    ));
    let par = parallel(key);
    out.push((
        par,
        format!(
            "The parallel {}: same tonic, three accidentals away. Dramatic, because the third \
             of the home chord flips.",
            if par.scale.is_minorish() { "minor" } else { "major" },
        ),
    ));
    out
}

/// Chords a key can borrow, with why. The list is deliberately short and
/// canonical: these are the borrowings a beginner will actually recognise
/// from records.
pub fn borrowed_chords(key: &Key) -> Vec<(Chord, String)> {
    let t = key.tonic;
    if key.scale.is_minorish() {
        return vec![
            (
                Chord::new(t.plus_fifths(1), ChordQuality::Dom7),
                "The borrowed dominant (V7): natural minor's own v is minor and does not pull. \
                 Raising its third gives a leading tone and a real cadence — that is all \
                 harmonic minor ever was."
                    .to_string(),
            ),
            (
                Chord::new(t.plus_fifths(4), ChordQuality::Maj),
                "The major IV in a minor key (dorian colour): brighter than iv without leaving \
                 the mode's centre."
                    .to_string(),
            ),
            (
                Chord::new(t, ChordQuality::Maj),
                "The major tonic at the very end — a Picardy third. Four hundred years old and \
                 it still works."
                    .to_string(),
            ),
        ];
    }
    vec![
        (
            Chord::new(t.plus_fifths(-2), ChordQuality::Maj),
            "♭VII, borrowed from mixolydian: one step counter-clockwise past IV. It moves to I \
             without a leading tone, which is why rock lives on it."
                .to_string(),
        ),
        (
            Chord::new(t.plus_fifths(-1), ChordQuality::Min),
            "iv, borrowed from the parallel minor. Its ♭6 is the saddest note available inside \
             a major key, and it lands beautifully on I."
                .to_string(),
        ),
        (
            Chord::new(t.plus_fifths(-4), ChordQuality::Maj),
            "♭VI, from the parallel minor: a wide, cinematic step. Usually goes on to ♭VII or \
             back to I."
                .to_string(),
        ),
        (
            Chord::new(t.plus_fifths(-3), ChordQuality::Maj),
            "♭III, from the parallel minor. Shares two notes with the tonic minor, so it is a \
             soft way out of major."
                .to_string(),
        ),
        (
            Chord::new(t.plus_fifths(2), ChordQuality::Dom7),
            "V7/V: the dominant of the dominant. It makes the V that follows sound like a \
             destination instead of a stop on the way."
                .to_string(),
        ),
    ]
}

/// A walk around the circle: `steps` roots starting at `start`, `direction`
/// `+1` clockwise (rising fifths) or `-1` counter-clockwise (the direction
/// harmony actually moves).
pub fn walk(start: Tpc, steps: usize, direction: i16) -> Vec<Tpc> {
    (0..steps as i16).map(|i| start.plus_fifths(i * direction.signum())).collect()
}

/// Chords common to two keys — the pivots a smooth modulation turns on. Sorted
/// by how strongly each one belongs to the DESTINATION (a pivot that is the
/// new key's dominant does more work than one that is its iii).
pub fn pivot_chords(from: &Key, to: &Key) -> Vec<(Chord, String)> {
    let mut out: Vec<(Chord, String, u8)> = Vec::new();
    for (_, chord) in super::chord::diatonic_chords(from, false) {
        let here = analyze(&chord, from);
        let there = analyze(&chord, to);
        if there.borrowed {
            continue; // not actually native to the destination — no pivot
        }
        let rank = match there.function {
            Function::Dominant => 0,
            Function::Predominant => 1,
            Function::Tonic => 2,
            Function::Chromatic => 3,
        };
        out.push((
            chord,
            format!(
                "{} is {} in {} and {} in {} — play it once and the ear has already moved.",
                chord.pretty(),
                here.roman,
                from.label(),
                there.roman,
                to.label(),
            ),
            rank,
        ));
    }
    out.sort_by_key(|(_, _, r)| *r);
    out.into_iter().map(|(c, w, _)| (c, w)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_major_arc_is_f_to_b() {
        let key = Key::c_major();
        let names: Vec<String> = wedges(&key)
            .into_iter()
            .filter(|w| w.in_key)
            .map(|w| w.major.name())
            .collect();
        assert_eq!(names, ["F", "C", "G", "D", "A", "E", "B"]);
    }

    #[test]
    fn the_arc_holds_the_keys_seven_chords_in_circle_order() {
        let chords: Vec<String> = wedges(&Key::c_major())
            .into_iter()
            .filter_map(|w| w.chord.map(|c| c.symbol()))
            .collect();
        assert_eq!(chords, ["F", "C", "G", "Dm", "Am", "Em", "Bdim"]);
    }

    #[test]
    fn wedges_are_twelve_distinct_pitch_classes_spelled_for_the_key() {
        for key in [
            Key::c_major(),
            Key::new(Tpc::E, ScaleType::Ionian),
            Key::new(Tpc(-2), ScaleType::Ionian),
            Key::new(Tpc::A, ScaleType::Aeolian),
        ] {
            let ws = wedges(&key);
            assert_eq!(ws.len(), 12, "{}", key.label());
            let mut pcs: Vec<u8> = ws.iter().map(|w| w.major.pitch_class()).collect();
            pcs.sort_unstable();
            pcs.dedup();
            assert_eq!(pcs.len(), 12, "{} must cover every pitch class once", key.label());
            assert_eq!(ws.iter().filter(|w| w.in_key).count(), 7);
            assert_eq!(ws.iter().filter(|w| w.is_tonic).count(), 1);
        }
        // E major spells the sharp side: D#, not Eb.
        let e = wedges(&Key::new(Tpc::E, ScaleType::Ionian));
        assert!(e.iter().any(|w| w.major.name() == "D#"), "E major has D#");
        assert!(!e.iter().any(|w| w.major.name() == "Eb"));
        // Bb major spells the flat side.
        let bb = wedges(&Key::new(Tpc(-2), ScaleType::Ionian));
        assert!(bb.iter().any(|w| w.major.name() == "Eb"));
    }

    #[test]
    fn slots_outside_the_arc_offer_borrowed_chords() {
        let ws = wedges(&Key::c_major());
        let bb = ws.iter().find(|w| w.major.name() == "Bb").unwrap();
        assert!(!bb.in_key);
        assert_eq!(bb.borrowed_chord.unwrap().symbol(), "Bb");
        assert_eq!(bb.roman.as_deref(), Some("♭VII"));
        assert!(bb.chord.is_none(), "not a diatonic chord of C major");
    }

    #[test]
    fn relative_minor_shares_the_key_signature() {
        let c = Key::c_major();
        let am = relative(&c);
        assert_eq!(am.canonical(), "A aeolian");
        assert_eq!(distance(&c, &am), 0, "same signature = zero steps");
        assert_eq!(relative(&am), c, "and back again");
    }

    #[test]
    fn distance_is_signature_steps_and_wraps() {
        let c = Key::c_major();
        assert_eq!(distance(&c, &Key::new(Tpc::G, ScaleType::Ionian)), 1);
        assert_eq!(distance(&c, &Key::new(Tpc::F, ScaleType::Ionian)), 1);
        assert_eq!(distance(&c, &Key::new(Tpc::E, ScaleType::Ionian)), 4);
        assert_eq!(distance(&c, &Key::new(Tpc(6), ScaleType::Ionian)), 6, "the tritone is the far side");
        assert_eq!(distance(&c, &parallel(&c)), 3, "parallel minor is three accidentals");
    }

    #[test]
    fn a_circle_walk_returns_home_after_twelve_steps() {
        let w = walk(Tpc::C, 13, -1);
        assert_eq!(w.first().unwrap().pitch_class(), w.last().unwrap().pitch_class());
        let names: Vec<String> = walk(Tpc::C, 5, 1).iter().map(|t| t.name()).collect();
        assert_eq!(names, ["C", "G", "D", "A", "E"], "clockwise rises by fifths");
        let ccw: Vec<String> = walk(Tpc::C, 4, -1).iter().map(|t| t.name()).collect();
        assert_eq!(ccw, ["C", "F", "Bb", "Eb"], "counter-clockwise falls by fifths");
    }

    #[test]
    fn borrowed_chords_are_named_and_explained() {
        let got: Vec<String> =
            borrowed_chords(&Key::c_major()).iter().map(|(c, _)| c.symbol()).collect();
        assert_eq!(got, ["Bb", "Fm", "Ab", "Eb", "D7"]);
        assert!(borrowed_chords(&Key::c_major())[0].1.contains("mixolydian"));
        // In a minor key the first thing you borrow is a real dominant.
        let minor = borrowed_chords(&Key::new(Tpc::A, ScaleType::Aeolian));
        assert_eq!(minor[0].0.symbol(), "E7");
        assert!(minor[0].1.contains("leading tone"));
    }

    #[test]
    fn pivots_between_c_major_and_g_major_are_ranked_by_what_they_do_there() {
        let c = Key::c_major();
        let g = Key::new(Tpc::G, ScaleType::Ionian);
        let pivots = pivot_chords(&c, &g);
        let symbols: Vec<String> = pivots.iter().map(|(c, _)| c.symbol()).collect();
        // Only the four chords native to BOTH keys, predominants first (they
        // set up G's dominant, which is what makes the new key land).
        assert_eq!(symbols, ["C", "Am", "Em", "G"]);
        assert!(pivots[0].1.contains("IV in G major"), "{}", pivots[0].1);
        assert!(pivots[1].1.contains("ii in G major"), "{}", pivots[1].1);
        // Dm is C major's ii but G major's v — not native there, so it is not
        // a pivot. Same for Bdim and F.
        for absent in ["Dm", "Bdim", "F"] {
            assert!(!symbols.iter().any(|s| s == absent), "{absent} must not be a pivot");
        }
    }

    #[test]
    fn neighbours_are_four_easy_moves_with_reasons() {
        let n = neighbours(&Key::c_major());
        assert_eq!(n.len(), 4);
        let keys: Vec<String> = n.iter().map(|(k, _)| k.canonical()).collect();
        assert_eq!(keys, ["G ionian", "F ionian", "A aeolian", "C aeolian"]);
        assert!(n.iter().all(|(_, why)| why.len() > 40), "every move explains itself");
    }
}
