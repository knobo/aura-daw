//! Chords: qualities as fifths-offset tables, symbols, and chord tones.
//!
//! Qualities are stored as fifths offsets from the root for the same reason
//! scales are (`scale.rs`): it is the only representation that spells
//! `Cdim7`'s seventh as `B♭♭` rather than `A`, and the diminished seventh is
//! exactly where a semitone-set chord model starts printing nonsense.

use super::scale::{Key, ScaleType};
use super::tpc::{degree_label, Tpc};

/// Chord qualities the Composer can name, generate and parse. Additive: a new
/// variant needs an offsets row, a suffix and an `ALL` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordQuality {
    Maj,
    Min,
    Dim,
    Aug,
    Five,
    Sus2,
    Sus4,
    Maj6,
    Min6,
    Maj7,
    Dom7,
    Min7,
    MinMaj7,
    HalfDim7,
    Dim7,
    Aug7,
    Dom7Sus4,
    Maj9,
    Dom9,
    Min9,
    Dom7Flat9,
    Dom7Sharp9,
    Dom13,
    Min11,
    Maj7Sharp11,
}

impl ChordQuality {
    pub const ALL: [ChordQuality; 25] = [
        ChordQuality::Maj,
        ChordQuality::Min,
        ChordQuality::Dim,
        ChordQuality::Aug,
        ChordQuality::Five,
        ChordQuality::Sus2,
        ChordQuality::Sus4,
        ChordQuality::Maj6,
        ChordQuality::Min6,
        ChordQuality::Maj7,
        ChordQuality::Dom7,
        ChordQuality::Min7,
        ChordQuality::MinMaj7,
        ChordQuality::HalfDim7,
        ChordQuality::Dim7,
        ChordQuality::Aug7,
        ChordQuality::Dom7Sus4,
        ChordQuality::Maj9,
        ChordQuality::Dom9,
        ChordQuality::Min9,
        ChordQuality::Dom7Flat9,
        ChordQuality::Dom7Sharp9,
        ChordQuality::Dom13,
        ChordQuality::Min11,
        ChordQuality::Maj7Sharp11,
    ];

    /// Fifths offsets from the root, root first. Reference values:
    /// M3 = +4, m3 = −3, P5 = +1, ♭5 = −6, ♯5 = +8, M7 = +5, ♭7 = −2,
    /// ♭♭7 = −9, 9 = +2, ♭9 = −5, ♯9 = +9, 11 = −1, ♯11 = +6, 13 = +3.
    pub fn offsets(self) -> &'static [i16] {
        match self {
            ChordQuality::Maj => &[0, 4, 1],
            ChordQuality::Min => &[0, -3, 1],
            ChordQuality::Dim => &[0, -3, -6],
            ChordQuality::Aug => &[0, 4, 8],
            ChordQuality::Five => &[0, 1],
            ChordQuality::Sus2 => &[0, 2, 1],
            ChordQuality::Sus4 => &[0, -1, 1],
            ChordQuality::Maj6 => &[0, 4, 1, 3],
            ChordQuality::Min6 => &[0, -3, 1, 3],
            ChordQuality::Maj7 => &[0, 4, 1, 5],
            ChordQuality::Dom7 => &[0, 4, 1, -2],
            ChordQuality::Min7 => &[0, -3, 1, -2],
            ChordQuality::MinMaj7 => &[0, -3, 1, 5],
            ChordQuality::HalfDim7 => &[0, -3, -6, -2],
            ChordQuality::Dim7 => &[0, -3, -6, -9],
            ChordQuality::Aug7 => &[0, 4, 8, -2],
            ChordQuality::Dom7Sus4 => &[0, -1, 1, -2],
            ChordQuality::Maj9 => &[0, 4, 1, 5, 2],
            ChordQuality::Dom9 => &[0, 4, 1, -2, 2],
            ChordQuality::Min9 => &[0, -3, 1, -2, 2],
            ChordQuality::Dom7Flat9 => &[0, 4, 1, -2, -5],
            ChordQuality::Dom7Sharp9 => &[0, 4, 1, -2, 9],
            // The 11th is omitted on a 13 chord: it clashes with the 3rd
            // (that is the same avoid-note rule `palette.rs` formalises).
            ChordQuality::Dom13 => &[0, 4, 1, -2, 2, 3],
            ChordQuality::Min11 => &[0, -3, 1, -2, 2, -1],
            ChordQuality::Maj7Sharp11 => &[0, 4, 1, 5, 6],
        }
    }

    /// Symbol suffix after the root: `""` for major, `"m7b5"` for half
    /// diminished. ASCII, because this is also the parse form.
    pub fn suffix(self) -> &'static str {
        match self {
            ChordQuality::Maj => "",
            ChordQuality::Min => "m",
            ChordQuality::Dim => "dim",
            ChordQuality::Aug => "aug",
            ChordQuality::Five => "5",
            ChordQuality::Sus2 => "sus2",
            ChordQuality::Sus4 => "sus4",
            ChordQuality::Maj6 => "6",
            ChordQuality::Min6 => "m6",
            ChordQuality::Maj7 => "maj7",
            ChordQuality::Dom7 => "7",
            ChordQuality::Min7 => "m7",
            ChordQuality::MinMaj7 => "mMaj7",
            ChordQuality::HalfDim7 => "m7b5",
            ChordQuality::Dim7 => "dim7",
            ChordQuality::Aug7 => "7#5",
            ChordQuality::Dom7Sus4 => "7sus4",
            ChordQuality::Maj9 => "maj9",
            ChordQuality::Dom9 => "9",
            ChordQuality::Min9 => "m9",
            ChordQuality::Dom7Flat9 => "7b9",
            ChordQuality::Dom7Sharp9 => "7#9",
            ChordQuality::Dom13 => "13",
            ChordQuality::Min11 => "m11",
            ChordQuality::Maj7Sharp11 => "maj7#11",
        }
    }

    /// Has a major third and a minor seventh — the tritone shape that makes a
    /// chord *want* to resolve down a fifth. Used by the analysis
    /// (secondary dominants) and by the melody's tendency-tone handling.
    pub fn is_dominant_shape(self) -> bool {
        let o = self.offsets();
        o.contains(&4) && o.contains(&-2)
    }

    pub fn has_seventh(self) -> bool {
        let o = self.offsets();
        o.contains(&5) || o.contains(&-2) || o.contains(&-9)
    }

    /// Identify a quality from an unordered fifths-offset set (root at 0) —
    /// how a diatonic chord built by stacking scale degrees learns its own
    /// name. `None` when nothing in the vocabulary matches exactly.
    pub fn from_offsets(offsets: &[i16]) -> Option<ChordQuality> {
        let mut want: Vec<i16> = offsets.to_vec();
        want.sort_unstable();
        want.dedup();
        ChordQuality::ALL.into_iter().find(|q| {
            let mut have: Vec<i16> = q.offsets().to_vec();
            have.sort_unstable();
            have.dedup();
            have == want
        })
    }
}

/// A chord: a spelled root, a quality, and an optional bass note for slash
/// chords / inversions (`C/E`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    pub root: Tpc,
    pub quality: ChordQuality,
    /// `None` = root in the bass. A `Some` that equals the root is
    /// normalised away by [`Chord::new`] so two spellings of the same chord
    /// compare equal.
    pub bass: Option<Tpc>,
}

impl Chord {
    pub fn new(root: Tpc, quality: ChordQuality) -> Self {
        Self { root, quality, bass: None }
    }

    pub fn with_bass(root: Tpc, quality: ChordQuality, bass: Tpc) -> Self {
        Self { root, quality, bass: if bass == root { None } else { Some(bass) } }
    }

    /// Spelled chord tones, root first, in stacking order.
    pub fn tones(&self) -> Vec<Tpc> {
        self.quality.offsets().iter().map(|o| self.root.plus_fifths(*o)).collect()
    }

    /// Pitch classes of the chord tones (bass included — a slash bass can be
    /// a note outside the chord, e.g. `C/D`).
    pub fn pitch_classes(&self) -> Vec<u8> {
        let mut v: Vec<u8> = self.tones().iter().map(|t| t.pitch_class()).collect();
        if let Some(b) = self.bass {
            let pc = b.pitch_class();
            if !v.contains(&pc) {
                v.push(pc);
            }
        }
        v
    }

    /// The chord tone matching a pitch class, with its degree label —
    /// `(B♭, "♭7")` for `pc = 10` on a `C7`. `None` when the pitch class is
    /// not in the chord.
    pub fn tone_for_pc(&self, pc: u8) -> Option<(Tpc, String)> {
        self.quality
            .offsets()
            .iter()
            .map(|o| (self.root.plus_fifths(*o), *o))
            .find(|(t, _)| t.pitch_class() == pc % 12)
            .map(|(t, o)| (t, degree_label(o)))
    }

    /// Guide tones: the third (or the sus'd fourth/second standing in for it)
    /// and the seventh — the tones that carry the chord's identity, and the
    /// ones a melody should land on at a cadence.
    pub fn guide_tones(&self) -> Vec<Tpc> {
        self.quality
            .offsets()
            .iter()
            .filter(|o| matches!(**o, 4 | -3 | -1 | 2 | 5 | -2 | -9))
            .map(|o| self.root.plus_fifths(*o))
            .collect()
    }

    /// Chord symbol: `"Cmaj7"`, `"F#m7b5"`, `"C/E"`. ASCII — this is the wire
    /// and parse form. [`Chord::pretty`] is for display.
    pub fn symbol(&self) -> String {
        let mut s = format!("{}{}", self.root.name(), self.quality.suffix());
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(&b.name());
        }
        s
    }

    /// Display symbol with typographic accidentals. Every `b`/`#` in a
    /// quality suffix is an accidental (`m7b5`, `7#9`), so the substitution
    /// is safe — and it is applied to the SUFFIX only, never to a root that
    /// `Tpc::pretty` has already spelled.
    pub fn pretty(&self) -> String {
        let suffix = self.quality.suffix().replace('b', "♭").replace('#', "♯");
        let mut s = format!("{}{}", self.root.pretty(), suffix);
        if let Some(b) = self.bass {
            s.push('/');
            s.push_str(&b.pretty());
        }
        s
    }

    /// Parse `"C"`, `"Am"`, `"F#m7b5"`, `"Bb7sus4"`, `"C/E"`, `"Cmin"`,
    /// `"CM7"`, `"C-7"`, `"Cø"`, `"C°"`.
    pub fn parse(s: &str) -> Result<Chord, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty chord symbol".into());
        }
        let (body, bass) = match s.split_once('/') {
            Some((b, t)) => (
                b,
                Some(Tpc::parse(t.trim()).ok_or_else(|| format!("bad bass note in {s:?}"))?),
            ),
            None => (s, None),
        };
        // Root = leading letter + accidentals. Greedy, so "Bb" is B-flat and
        // not B with a "b…" suffix.
        let mut split = body
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .ok_or_else(|| format!("bad chord symbol {s:?}"))?;
        for (i, c) in body.char_indices().skip(1) {
            if matches!(c, '#' | 'b' | '♯' | '♭' | 'x') {
                split = i + c.len_utf8();
            } else {
                break;
            }
        }
        let (root_s, suffix) = body.split_at(split);
        let root = Tpc::parse(root_s).ok_or_else(|| format!("bad root in chord {s:?}"))?;
        let quality = parse_quality(suffix)
            .ok_or_else(|| format!("unknown chord quality {suffix:?} in {s:?}"))?;
        Ok(match bass {
            Some(b) => Chord::with_bass(root, quality, b),
            None => Chord::new(root, quality),
        })
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.symbol())
    }
}

/// Aliases accepted on input but never emitted. Longest match wins, so
/// `"m7b5"` cannot be read as `"m7"` with junk on the end.
const QUALITY_ALIASES: &[(&str, ChordQuality)] = &[
    ("", ChordQuality::Maj),
    ("maj", ChordQuality::Maj),
    ("M", ChordQuality::Maj),
    ("min", ChordQuality::Min),
    ("-", ChordQuality::Min),
    ("o", ChordQuality::Dim),
    ("°", ChordQuality::Dim),
    ("+", ChordQuality::Aug),
    ("M7", ChordQuality::Maj7),
    ("maj7", ChordQuality::Maj7),
    ("Δ", ChordQuality::Maj7),
    ("-7", ChordQuality::Min7),
    ("min7", ChordQuality::Min7),
    ("ø", ChordQuality::HalfDim7),
    ("ø7", ChordQuality::HalfDim7),
    ("halfdim", ChordQuality::HalfDim7),
    ("o7", ChordQuality::Dim7),
    ("°7", ChordQuality::Dim7),
    ("m7-5", ChordQuality::HalfDim7),
    ("mmaj7", ChordQuality::MinMaj7),
    ("mM7", ChordQuality::MinMaj7),
    ("sus", ChordQuality::Sus4),
    ("add9", ChordQuality::Maj9),
];

fn parse_quality(suffix: &str) -> Option<ChordQuality> {
    let s = suffix.trim();
    // Canonical suffixes first (exact, case-sensitive: "M7" and "m7" differ),
    // then the alias table. Both are exact matches on the WHOLE suffix, so
    // trailing junk is an error rather than a silent truncation.
    if let Some(q) = ChordQuality::ALL.into_iter().find(|q| q.suffix() == s) {
        return Some(q);
    }
    if let Some((_, q)) = QUALITY_ALIASES.iter().find(|(a, _)| *a == s) {
        return Some(*q);
    }
    // Last resort: case-insensitive, for agents and typing humans.
    let lower = s.to_ascii_lowercase();
    ChordQuality::ALL
        .into_iter()
        .find(|q| q.suffix().to_ascii_lowercase() == lower)
        .or_else(|| {
            QUALITY_ALIASES
                .iter()
                .find(|(a, _)| a.to_ascii_lowercase() == lower)
                .map(|(_, q)| *q)
        })
}

/// The chord built on a 0-based scale degree by stacking scale thirds — the
/// key's own chord vocabulary, spelled and named by the scale rather than
/// assumed.
///
/// `seventh` adds the fourth stacked degree. Returns `None` only when the
/// stack produces a set outside [`ChordQuality::ALL`] (possible in the
/// exotic scales, e.g. degrees of the octatonic).
pub fn diatonic_chord(key: &Key, degree: usize, seventh: bool) -> Option<Chord> {
    let offs = key.scale.offsets();
    let n = offs.len();
    let root_off = offs[degree % n];
    let count = if seventh { 4 } else { 3 };
    let rel: Vec<i16> = (0..count)
        .map(|i| offs[(degree + i * 2) % n] - root_off)
        .collect();
    let quality = ChordQuality::from_offsets(&rel)?;
    Some(Chord::new(key.tonic.plus_fifths(root_off), quality))
}

/// Every diatonic chord of a key, degree order. Pentatonic and symmetric
/// scales yield gaps (a degree whose stack has no name); those are skipped,
/// so the result is not necessarily `degree_count()` long.
pub fn diatonic_chords(key: &Key, seventh: bool) -> Vec<(usize, Chord)> {
    (0..key.degree_count())
        .filter_map(|d| diatonic_chord(key, d, seventh).map(|c| (d, c)))
        .collect()
}

/// The mode a chord's own scale is, for a chord standing on scale degree
/// `degree` of `key` — the chord-scale, which is what a soloist plays over
/// it. Only meaningful for diatonic keys.
pub fn chord_scale(key: &Key, degree: usize) -> Option<Key> {
    if !key.scale.is_diatonic() {
        return None;
    }
    const MODES: [ScaleType; 7] = [
        ScaleType::Ionian,
        ScaleType::Dorian,
        ScaleType::Phrygian,
        ScaleType::Lydian,
        ScaleType::Mixolydian,
        ScaleType::Aeolian,
        ScaleType::Locrian,
    ];
    // The parent's own mode index, rotated by the degree.
    let base = MODES.iter().position(|m| *m == key.scale)?;
    Some(Key::new(key.degree(degree), MODES[(base + degree) % 7]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::scale::ScaleType;

    #[test]
    fn the_seven_diatonic_sevenths_of_c_major() {
        let key = Key::c_major();
        let got: Vec<String> =
            diatonic_chords(&key, true).iter().map(|(_, c)| c.symbol()).collect();
        assert_eq!(got, ["Cmaj7", "Dm7", "Em7", "Fmaj7", "G7", "Am7", "Bm7b5"]);
    }

    #[test]
    fn diatonic_triads_of_a_minor_and_e_harmonic_minor() {
        let got: Vec<String> = diatonic_chords(&Key::new(Tpc::A, ScaleType::Aeolian), false)
            .iter()
            .map(|(_, c)| c.symbol())
            .collect();
        assert_eq!(got, ["Am", "Bdim", "C", "Dm", "Em", "F", "G"]);
        // Harmonic minor's whole point: degree V becomes MAJOR, so the key
        // gets a real dominant, and degree vii becomes a diminished seventh.
        let hm: Vec<String> = diatonic_chords(&Key::new(Tpc::E, ScaleType::HarmonicMinor), false)
            .iter()
            .map(|(_, c)| c.symbol())
            .collect();
        assert_eq!(hm, ["Em", "F#dim", "Gaug", "Am", "B", "C", "D#dim"]);
    }

    #[test]
    fn diminished_seventh_spells_a_double_flat() {
        let c = Chord::new(Tpc::C, ChordQuality::Dim7);
        let names: Vec<String> = c.tones().iter().map(|t| t.name()).collect();
        assert_eq!(names, ["C", "Eb", "Gb", "Bbb"], "not A — that is the point of Tpc");
        assert_eq!(c.pitch_classes(), vec![0, 3, 6, 9]);
    }

    #[test]
    fn symbols_round_trip_for_every_quality() {
        for q in ChordQuality::ALL {
            for root in [Tpc::C, Tpc(6), Tpc(-2), Tpc(-5)] {
                let c = Chord::new(root, q);
                let back = Chord::parse(&c.symbol())
                    .unwrap_or_else(|e| panic!("{}: {e}", c.symbol()));
                assert_eq!(back, c, "{}", c.symbol());
            }
        }
        let slash = Chord::with_bass(Tpc::C, ChordQuality::Maj, Tpc::E);
        assert_eq!(slash.symbol(), "C/E");
        assert_eq!(Chord::parse("C/E").unwrap(), slash);
    }

    #[test]
    fn parser_accepts_the_symbols_people_actually_type() {
        for (input, want) in [
            ("C", Chord::new(Tpc::C, ChordQuality::Maj)),
            ("Cmaj", Chord::new(Tpc::C, ChordQuality::Maj)),
            ("Am", Chord::new(Tpc::A, ChordQuality::Min)),
            ("Amin", Chord::new(Tpc::A, ChordQuality::Min)),
            ("A-7", Chord::new(Tpc::A, ChordQuality::Min7)),
            ("CM7", Chord::new(Tpc::C, ChordQuality::Maj7)),
            ("Bb7sus4", Chord::new(Tpc(-2), ChordQuality::Dom7Sus4)),
            ("F#m7b5", Chord::new(Tpc(6), ChordQuality::HalfDim7)),
            ("Bø", Chord::new(Tpc::B, ChordQuality::HalfDim7)),
            ("B°7", Chord::new(Tpc::B, ChordQuality::Dim7)),
            ("Gsus", Chord::new(Tpc::G, ChordQuality::Sus4)),
            ("Db13", Chord::new(Tpc(-5), ChordQuality::Dom13)),
        ] {
            assert_eq!(Chord::parse(input).unwrap(), want, "{input}");
        }
        assert!(Chord::parse("Cmaj77").is_err(), "trailing junk is an error");
        assert!(Chord::parse("Hm").is_err());
        assert!(Chord::parse("").is_err());
    }

    #[test]
    fn tone_roles_are_labelled_by_degree() {
        let c7 = Chord::new(Tpc::C, ChordQuality::Dom7);
        assert_eq!(c7.tone_for_pc(0).unwrap().1, "1");
        assert_eq!(c7.tone_for_pc(4).unwrap().1, "3");
        assert_eq!(c7.tone_for_pc(7).unwrap().1, "5");
        assert_eq!(c7.tone_for_pc(10).unwrap().1, "♭7");
        assert_eq!(c7.tone_for_pc(10).unwrap().0.name(), "Bb");
        assert!(c7.tone_for_pc(2).is_none(), "the 9th is not a chord tone of C7");
    }

    #[test]
    fn guide_tones_are_the_third_and_seventh() {
        let names: Vec<String> = Chord::new(Tpc::C, ChordQuality::Dom7)
            .guide_tones()
            .iter()
            .map(|t| t.name())
            .collect();
        assert_eq!(names, ["E", "Bb"]);
        // A sus chord's fourth stands in for the missing third.
        let sus: Vec<String> = Chord::new(Tpc::C, ChordQuality::Sus4)
            .guide_tones()
            .iter()
            .map(|t| t.name())
            .collect();
        assert_eq!(sus, ["F"]);
    }

    #[test]
    fn dominant_shape_is_major_third_plus_minor_seventh() {
        assert!(ChordQuality::Dom7.is_dominant_shape());
        assert!(ChordQuality::Dom7Flat9.is_dominant_shape());
        assert!(ChordQuality::Dom13.is_dominant_shape());
        assert!(!ChordQuality::Maj7.is_dominant_shape());
        assert!(!ChordQuality::Min7.is_dominant_shape());
        assert!(!ChordQuality::HalfDim7.is_dominant_shape());
    }

    #[test]
    fn chord_scales_rotate_the_parent_mode() {
        let key = Key::c_major();
        assert_eq!(chord_scale(&key, 1).unwrap().canonical(), "D dorian");
        assert_eq!(chord_scale(&key, 4).unwrap().canonical(), "G mixolydian");
        assert_eq!(chord_scale(&key, 3).unwrap().canonical(), "F lydian");
        // From a modal parent, not just from major.
        let d_dorian = Key::new(Tpc::D, ScaleType::Dorian);
        assert_eq!(chord_scale(&d_dorian, 0).unwrap().canonical(), "D dorian");
        assert_eq!(chord_scale(&d_dorian, 4).unwrap().canonical(), "A aeolian");
        assert!(chord_scale(&Key::new(Tpc::C, ScaleType::Blues), 0).is_none());
    }

    #[test]
    fn pretty_symbols_use_typographic_accidentals() {
        assert_eq!(Chord::parse("F#m7b5").unwrap().pretty(), "F♯m7♭5");
        assert_eq!(Chord::parse("Bb7sus4").unwrap().pretty(), "B♭7sus4");
        assert_eq!(Chord::parse("C/E").unwrap().pretty(), "C/E");
    }
}
