//! Tonal pitch classes: the line of fifths.
//!
//! WHY not a plain pitch class 0..11 (ruling H-1): every label the Composer
//! prints depends on the difference between F♯ and G♭ — key signatures, chord
//! symbols, "raised 7th" vs "flat 1st", and any future notation surface. A
//! pitch class throws that away and it cannot be recovered from context
//! cheaply or reliably.
//!
//! The encoding is a signed position on the line of fifths (`C = 0`,
//! `G = +1`, `F = -1`, `F♯ = +6`, `B♭ = -2`), which makes every operation
//! arithmetic instead of a lookup table:
//!
//! * `pitch_class = (fifths * 7) mod 12`
//! * `letter      = "FCGDAEB"[(fifths + 1) mod 7]`
//! * `accidentals = (fifths + 1) div 7`
//! * transposition by an interval is ADDITION
//! * a diatonic key signature IS a fifths index
//!
//! Precedent: Humdrum, music21, MuseScore's `tpc`.

use std::fmt;

/// Letters in line-of-fifths order. `F` sits at index 0 because `F` is
/// fifths −1, so the index of `fifths` is `(fifths + 1).rem_euclid(7)`.
const LETTERS: [char; 7] = ['F', 'C', 'G', 'D', 'A', 'E', 'B'];

/// Semitone of each natural letter above C, indexed by DIATONIC step
/// (`C = 0 … B = 6`) — not by line-of-fifths order.
const NATURAL_SEMITONES: [i16; 7] = [0, 2, 4, 5, 7, 9, 11];

/// The ionian scale's fifths offset per diatonic step (`1 2 3 4 5 6 7`).
/// This is the "natural" interval for a step, which is what an alteration is
/// measured against ([`interval_alteration`]) — and, in `analysis`, what an
/// accidental on a Roman numeral is measured from.
pub(crate) const IONIAN_OFFSETS: [i16; 7] = [0, 2, 4, -1, 1, 3, 5];

/// The fifths offset of a 0-based MAJOR-scale degree. The reference an altered
/// Roman numeral is read against: `♭VII` is `ionian_offset(6) - 7`, in every
/// key and every mode.
pub fn ionian_offset(degree: usize) -> i16 {
    IONIAN_OFFSETS[degree % 7]
}

/// A tonal pitch class: a position on the line of fifths.
///
/// Enharmonics are DISTINCT values (`Tpc(6)` = F♯, `Tpc(-6)` = G♭) and that
/// is the whole point; use [`Tpc::pitch_class`] when you want them to
/// collapse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tpc(pub i16);

impl Tpc {
    pub const C: Tpc = Tpc(0);
    pub const G: Tpc = Tpc(1);
    pub const D: Tpc = Tpc(2);
    pub const A: Tpc = Tpc(3);
    pub const E: Tpc = Tpc(4);
    pub const B: Tpc = Tpc(5);
    pub const F: Tpc = Tpc(-1);

    /// Pitch class 0..=11 (C = 0). Enharmonics collapse here and only here.
    pub fn pitch_class(self) -> u8 {
        (self.0 as i32 * 7).rem_euclid(12) as u8
    }

    /// The note letter, ignoring accidentals.
    pub fn letter(self) -> char {
        LETTERS[(self.0 as i32 + 1).rem_euclid(7) as usize]
    }

    /// Accidental count: `+2` = double sharp, `-1` = flat, `0` = natural.
    pub fn accidentals(self) -> i8 {
        (self.0 as i32 + 1).div_euclid(7) as i8
    }

    /// Diatonic step index of the letter (`C = 0`, `D = 1`, … `B = 6`) —
    /// the index [`NATURAL_SEMITONES`] and octave arithmetic use.
    pub fn diatonic_step(self) -> usize {
        match self.letter() {
            'C' => 0,
            'D' => 1,
            'E' => 2,
            'F' => 3,
            'G' => 4,
            'A' => 5,
            _ => 6, // 'B'
        }
    }

    /// Spelled name with ASCII accidentals (`"F#"`, `"Bb"`, `"C##"`) — the
    /// wire/parse form. Use [`Tpc::pretty`] for display.
    pub fn name(self) -> String {
        let acc = self.accidentals();
        let mut s = String::with_capacity(1 + acc.unsigned_abs() as usize);
        s.push(self.letter());
        for _ in 0..acc.abs() {
            s.push(if acc > 0 { '#' } else { 'b' });
        }
        s
    }

    /// Display name with typographic accidentals (`"F♯"`, `"B♭"`).
    pub fn pretty(self) -> String {
        let acc = self.accidentals();
        let mut s = String::new();
        s.push(self.letter());
        for _ in 0..acc.abs() {
            s.push(if acc > 0 { '♯' } else { '♭' });
        }
        s
    }

    /// Parse `"C"`, `"F#"`, `"Bb"`, `"C##"`, `"E♭"` (case-insensitive letter,
    /// both ASCII and typographic accidentals). `None` on anything else —
    /// callers turn that into a user-facing error with their own context.
    pub fn parse(s: &str) -> Option<Tpc> {
        let mut chars = s.chars();
        let letter = chars.next()?.to_ascii_uppercase();
        let step = LETTERS.iter().position(|c| *c == letter)?;
        // `LETTERS` is fifths order, so its index maps straight back.
        let mut fifths = step as i16 - 1;
        let mut acc = 0i16;
        for c in chars {
            match c {
                '#' | '♯' => acc += 1,
                'b' | 'B' | '♭' => acc -= 1,
                'x' | '𝄪' => acc += 2,
                _ => return None,
            }
        }
        fifths += acc * 7;
        Some(Tpc(fifths))
    }

    /// Transpose along the line of fifths — the primitive every other
    /// transposition is built from (a major third is `+4`, a minor seventh
    /// is `-2`, a chromatic semitone up is `+7`).
    pub fn plus_fifths(self, n: i16) -> Tpc {
        Tpc(self.0 + n)
    }

    /// Fifths distance from `other` to `self`, i.e. the interval as a single
    /// signed number.
    pub fn fifths_from(self, other: Tpc) -> i16 {
        self.0 - other.0
    }

    /// The enharmonically simplest spelling of this pitch class: the
    /// representative with the fewest accidentals (ties → the sharp side,
    /// which is what `C♯` vs `D♭` conventionally resolves to in isolation).
    /// Used when a *pitch class* has to become a name at all (e.g. naming a
    /// note the user played on a keyboard, where nobody chose a spelling).
    pub fn simplest_for_pc(pc: u8) -> Tpc {
        // Fifths in −5..=6 covers every pitch class exactly once with at
        // most one accidental, and puts F♯ (not G♭) on the tritone — the
        // conventional reading of an unspelled black key in isolation.
        (-5..=6)
            .map(Tpc)
            .find(|t| t.pitch_class() == pc % 12)
            .unwrap_or(Tpc::C)
    }
}

impl fmt::Display for Tpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// A sounding pitch: a spelling plus an octave in scientific pitch notation
/// (`C4` = middle C = MIDI 60).
///
/// The octave belongs to the LETTER, not to the sounding pitch, which is why
/// `Cb4` sounds `B3` while staying spelled in octave 4 — the same convention
/// MusicXML and MuseScore use. Getting this wrong makes leading tones jump an
/// octave in generated melodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pitch {
    pub tpc: Tpc,
    pub octave: i8,
}

impl Pitch {
    pub fn new(tpc: Tpc, octave: i8) -> Self {
        Self { tpc, octave }
    }

    /// MIDI key number. `i16` rather than `u8` because intermediate results
    /// in a voicing search legitimately leave 0..127 before being rejected.
    pub fn midi(self) -> i16 {
        12 * (self.octave as i16 + 1)
            + NATURAL_SEMITONES[self.tpc.diatonic_step()]
            + self.tpc.accidentals() as i16
    }

    /// The lowest octave placing this spelling at or above `min_midi`.
    /// The search is bounded and cheap; voicing candidate enumeration calls
    /// it per chord tone.
    pub fn at_or_above(tpc: Tpc, min_midi: i16) -> Pitch {
        let mut p = Pitch::new(tpc, -1);
        while p.midi() < min_midi {
            p.octave += 1;
        }
        p
    }

    pub fn name(self) -> String {
        format!("{}{}", self.tpc.name(), self.octave)
    }
}

/// How far `fifths` is from the ionian ("natural") interval of the same
/// diatonic step, in chromatic semitones: `-1` for a minor third, `+1` for an
/// augmented fourth, `-2` for a diminished seventh.
///
/// One chromatic semitone is 7 steps along the line of fifths, which is why
/// the division is exact.
pub fn interval_alteration(fifths: i16) -> i16 {
    let step = interval_step(fifths);
    (fifths - IONIAN_OFFSETS[step]) / 7
}

/// Diatonic step of an interval given as a fifths offset: `0` = unison,
/// `2` = a third, `4` = a fifth. `(4 * fifths) mod 7` because four steps of a
/// fifth land on the next scale degree.
pub fn interval_step(fifths: i16) -> usize {
    (fifths as i32 * 4).rem_euclid(7) as usize
}

/// Semitones above the root for an interval given as a fifths offset,
/// folded into one octave.
pub fn interval_semitones(fifths: i16) -> u8 {
    (fifths as i32 * 7).rem_euclid(12) as u8
}

/// Jazz degree label for a chord-relative fifths offset: `"1"`, `"♭3"`,
/// `"♯11"`, `"♭♭7"`.
///
/// Steps 2/4/6 (seconds, fourths, sixths) print as **9/11/13** because that
/// is what they are called when they are colour on top of a chord, which is
/// the only context this function is used in (chord *symbols* spell sus2 and
/// 6 chords themselves — see `chord::ChordQuality::suffix`).
pub fn degree_label(fifths: i16) -> String {
    let step = interval_step(fifths);
    let alt = interval_alteration(fifths);
    let number = match step {
        0 => 1,
        1 => 9,
        2 => 3,
        3 => 11,
        4 => 5,
        5 => 13,
        _ => 7,
    };
    let mut s = String::new();
    for _ in 0..alt.abs() {
        s.push(if alt > 0 { '♯' } else { '♭' });
    }
    s.push_str(&number.to_string());
    s
}

/// Classical interval name for a fifths offset: `"P5"`, `"M3"`, `"m7"`,
/// `"A4"`, `"d5"`. Used in why-strings, where "the tritone" reads better as
/// `A4`/`d5` than as a semitone count.
pub fn interval_name(fifths: i16) -> String {
    let step = interval_step(fifths);
    let alt = interval_alteration(fifths);
    // Unisons, fourths and fifths are perfectable; the rest are major/minor.
    let perfectable = matches!(step, 0 | 3 | 4);
    let quality = match (perfectable, alt) {
        (true, 0) => "P".to_string(),
        (false, 0) => "M".to_string(),
        (false, -1) => "m".to_string(),
        (_, n) if n > 0 => "A".repeat(n as usize),
        (_, n) => {
            // A diminished major interval is doubly diminished, hence the
            // extra step for non-perfectable intervals.
            let d = if perfectable { -n } else { -n - 1 };
            "d".repeat(d.max(1) as usize)
        }
    };
    format!("{quality}{}", step + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enharmonics_are_distinct_values_with_one_pitch_class() {
        let fs = Tpc::parse("F#").unwrap();
        let gb = Tpc::parse("Gb").unwrap();
        assert_ne!(fs, gb, "F# and Gb must not be the same Tpc");
        assert_eq!(fs.pitch_class(), gb.pitch_class(), "…but they sound alike");
        assert_eq!((fs.0, gb.0), (6, -6));
        assert_eq!((fs.name(), gb.name()), ("F#".to_string(), "Gb".to_string()));
    }

    #[test]
    fn letters_and_accidentals_are_arithmetic() {
        for (fifths, name) in [
            (0, "C"),
            (1, "G"),
            (-1, "F"),
            (5, "B"),
            (6, "F#"),
            (7, "C#"),
            (-2, "Bb"),
            (-7, "Cb"),
            (14, "C##"),
        ] {
            assert_eq!(Tpc(fifths).name(), name, "fifths {fifths}");
            assert_eq!(Tpc::parse(name).unwrap(), Tpc(fifths), "parse {name}");
        }
    }

    #[test]
    fn pitch_classes_match_a_semitone_oracle() {
        // Independent oracle: the natural letter's semitone plus accidentals.
        for fifths in -20..=20 {
            let t = Tpc(fifths);
            let expect = (NATURAL_SEMITONES[t.diatonic_step()] + t.accidentals() as i16)
                .rem_euclid(12) as u8;
            assert_eq!(t.pitch_class(), expect, "fifths {fifths}");
        }
    }

    #[test]
    fn transposition_by_fifths_round_trips() {
        for fifths in -20..=20 {
            for n in -20..=20 {
                assert_eq!(Tpc(fifths).plus_fifths(n).plus_fifths(-n), Tpc(fifths));
            }
        }
    }

    #[test]
    fn spelled_octave_belongs_to_the_letter() {
        assert_eq!(Pitch::new(Tpc::C, 4).midi(), 60, "C4 is middle C");
        assert_eq!(Pitch::new(Tpc::parse("Cb").unwrap(), 4).midi(), 59, "Cb4 sounds B3");
        assert_eq!(Pitch::new(Tpc::B, 3).midi(), 59);
        assert_eq!(Pitch::new(Tpc::parse("B#").unwrap(), 3).midi(), 60, "B#3 sounds C4");
        assert_eq!(Pitch::new(Tpc::A, 4).midi(), 69, "A4 = 440 Hz key");
    }

    #[test]
    fn at_or_above_finds_the_lowest_octave() {
        let p = Pitch::at_or_above(Tpc::E, 60);
        assert_eq!(p.midi(), 64);
        assert_eq!(Pitch::at_or_above(Tpc::C, 60).midi(), 60, "inclusive");
    }

    #[test]
    fn degree_labels_read_like_a_chord_chart() {
        for (fifths, label) in [
            (0, "1"),
            (4, "3"),
            (-3, "♭3"),
            (1, "5"),
            (-6, "♭5"),
            (8, "♯5"),
            (5, "7"),
            (-2, "♭7"),
            (-9, "♭♭7"),
            (2, "9"),
            (-5, "♭9"),
            (9, "♯9"),
            (-1, "11"),
            (6, "♯11"),
            (3, "13"),
            (-4, "♭13"),
        ] {
            assert_eq!(degree_label(fifths), label, "fifths {fifths}");
        }
    }

    #[test]
    fn interval_names_are_classical() {
        for (fifths, name) in [
            (0, "P1"),
            (1, "P5"),
            (-1, "P4"),
            (4, "M3"),
            (-3, "m3"),
            (2, "M2"),
            (-2, "m7"),
            (5, "M7"),
            (6, "A4"),
            (-6, "d5"),
            (-9, "d7"),
        ] {
            assert_eq!(interval_name(fifths), name, "fifths {fifths}");
        }
    }

    #[test]
    fn simplest_spelling_of_a_pitch_class_stays_within_one_accidental() {
        for pc in 0..12u8 {
            let t = Tpc::simplest_for_pc(pc);
            assert_eq!(t.pitch_class(), pc);
            assert!(t.accidentals().abs() <= 1, "{pc} -> {}", t.name());
        }
        assert_eq!(Tpc::simplest_for_pc(0).name(), "C");
        assert_eq!(Tpc::simplest_for_pc(6).name(), "F#", "tie goes to the sharp side");
        assert_eq!(Tpc::simplest_for_pc(10).name(), "Bb");
    }
}
