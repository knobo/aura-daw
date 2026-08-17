//! Scales, modes and keys — stored as fifths offsets (ruling H-2).
//!
//! WHY offsets on the line of fifths rather than semitone sets: a major scale
//! is EXACTLY the seven contiguous fifths from −1 to +5 relative to its
//! tonic, so every diatonic mode is a 7-slot window with the tonic in a
//! different slot. That single fact gives correct spelling for free
//! (`aeolian = [0, +2, −3, −1, +1, −4, −2]` spells A B C D E F G, never
//! A B B♯ D E E♯ F♯), makes the key signature a subtraction
//! ([`Key::key_signature_fifths`]), and makes "which chords belong to this
//! key" and "how far away is that key" the same lookup — which is what turns
//! the circle of fifths from a diagram into an editor (`circle.rs`).

use super::tpc::Tpc;

/// The scale/mode vocabulary. Additive: a new variant needs an offsets row
/// and an id, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScaleType {
    Ionian,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Aeolian,
    Locrian,
    HarmonicMinor,
    MelodicMinor,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    WholeTone,
    Octatonic,
}

impl ScaleType {
    /// Every scale type, in menu order (diatonic modes first).
    pub const ALL: [ScaleType; 14] = [
        ScaleType::Ionian,
        ScaleType::Dorian,
        ScaleType::Phrygian,
        ScaleType::Lydian,
        ScaleType::Mixolydian,
        ScaleType::Aeolian,
        ScaleType::Locrian,
        ScaleType::HarmonicMinor,
        ScaleType::MelodicMinor,
        ScaleType::MajorPentatonic,
        ScaleType::MinorPentatonic,
        ScaleType::Blues,
        ScaleType::WholeTone,
        ScaleType::Octatonic,
    ];

    /// Fifths offsets from the tonic, in scale-degree order. Verified by the
    /// semitone tests below — the derived semitone set is the readable form,
    /// this is the spelling-correct one.
    pub fn offsets(self) -> &'static [i16] {
        match self {
            ScaleType::Ionian => &[0, 2, 4, -1, 1, 3, 5],
            ScaleType::Dorian => &[0, 2, -3, -1, 1, 3, -2],
            ScaleType::Phrygian => &[0, -5, -3, -1, 1, -4, -2],
            ScaleType::Lydian => &[0, 2, 4, 6, 1, 3, 5],
            ScaleType::Mixolydian => &[0, 2, 4, -1, 1, 3, -2],
            ScaleType::Aeolian => &[0, 2, -3, -1, 1, -4, -2],
            ScaleType::Locrian => &[0, -5, -3, -1, -6, -4, -2],
            // Aeolian with a raised 7th (the leading tone that gives the
            // minor key a real dominant).
            ScaleType::HarmonicMinor => &[0, 2, -3, -1, 1, -4, 5],
            // Ascending melodic minor: raised 6th AND 7th.
            ScaleType::MelodicMinor => &[0, 2, -3, -1, 1, 3, 5],
            ScaleType::MajorPentatonic => &[0, 2, 4, 1, 3],
            ScaleType::MinorPentatonic => &[0, -3, -1, 1, -2],
            // Minor pentatonic + the ♭5 blue note.
            ScaleType::Blues => &[0, -3, -1, -6, 1, -2],
            ScaleType::WholeTone => &[0, 2, 4, 6, -4, -2],
            // Whole-half diminished.
            ScaleType::Octatonic => &[0, 2, -3, -1, -6, -4, 3, 5],
        }
    }

    /// A contiguous 7-window on the line of fifths — true for the diatonic
    /// modes and nothing else. Gates the key-signature and circle-window
    /// machinery, which is only meaningful for these.
    pub fn is_diatonic(self) -> bool {
        matches!(
            self,
            ScaleType::Ionian
                | ScaleType::Dorian
                | ScaleType::Phrygian
                | ScaleType::Lydian
                | ScaleType::Mixolydian
                | ScaleType::Aeolian
                | ScaleType::Locrian
        )
    }

    /// True when the scale's third is minor — the one bit of "is this a
    /// major or a minor key" that generators and labels actually need.
    pub fn is_minorish(self) -> bool {
        self.offsets().iter().any(|o| *o == -3)
    }

    /// Stable wire id (`"harmonicMinor"`). The serde form of a [`Key`] is
    /// `"<tonic> <id>"`.
    pub fn id(self) -> &'static str {
        match self {
            ScaleType::Ionian => "ionian",
            ScaleType::Dorian => "dorian",
            ScaleType::Phrygian => "phrygian",
            ScaleType::Lydian => "lydian",
            ScaleType::Mixolydian => "mixolydian",
            ScaleType::Aeolian => "aeolian",
            ScaleType::Locrian => "locrian",
            ScaleType::HarmonicMinor => "harmonicMinor",
            ScaleType::MelodicMinor => "melodicMinor",
            ScaleType::MajorPentatonic => "majorPentatonic",
            ScaleType::MinorPentatonic => "minorPentatonic",
            ScaleType::Blues => "blues",
            ScaleType::WholeTone => "wholeTone",
            ScaleType::Octatonic => "octatonic",
        }
    }

    /// What a human calls it (`"major"`, `"minor"`, `"dorian"`) — ionian and
    /// aeolian get their everyday names because that is what a beginner is
    /// looking for in a menu.
    pub fn label(self) -> &'static str {
        match self {
            ScaleType::Ionian => "major",
            ScaleType::Aeolian => "minor",
            ScaleType::HarmonicMinor => "harmonic minor",
            ScaleType::MelodicMinor => "melodic minor",
            ScaleType::MajorPentatonic => "major pentatonic",
            ScaleType::MinorPentatonic => "minor pentatonic",
            ScaleType::WholeTone => "whole tone",
            other => other.id(),
        }
    }

    /// Parse an id or a friendly alias.
    pub fn parse(s: &str) -> Option<ScaleType> {
        let k = s.trim().to_ascii_lowercase().replace([' ', '-', '_'], "");
        ScaleType::ALL
            .into_iter()
            .find(|t| t.id().to_ascii_lowercase() == k || t.label().replace(' ', "") == k)
            .or(match k.as_str() {
                "maj" | "major" => Some(ScaleType::Ionian),
                "min" | "minor" | "naturalminor" => Some(ScaleType::Aeolian),
                "harmmin" => Some(ScaleType::HarmonicMinor),
                "melmin" => Some(ScaleType::MelodicMinor),
                "majpent" => Some(ScaleType::MajorPentatonic),
                "minpent" => Some(ScaleType::MinorPentatonic),
                "dim" | "diminished" => Some(ScaleType::Octatonic),
                _ => None,
            })
    }
}

/// A key: a tonic spelling plus a scale type. Both halves matter — `D dorian`
/// and `C ionian` share every pitch class and imply completely different
/// chord functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub tonic: Tpc,
    pub scale: ScaleType,
}

impl Key {
    pub fn new(tonic: Tpc, scale: ScaleType) -> Self {
        Self { tonic, scale }
    }

    /// C major, the default a fresh harmony document starts from.
    pub fn c_major() -> Self {
        Self::new(Tpc::C, ScaleType::Ionian)
    }

    /// The scale's spelled notes, in degree order starting on the tonic.
    pub fn spelled(&self) -> Vec<Tpc> {
        self.scale.offsets().iter().map(|o| self.tonic.plus_fifths(*o)).collect()
    }

    /// Pitch classes of the scale, in degree order (may be unsorted —
    /// degree order is the useful one).
    pub fn pitch_classes(&self) -> Vec<u8> {
        self.spelled().iter().map(|t| t.pitch_class()).collect()
    }

    pub fn contains_pc(&self, pc: u8) -> bool {
        self.pitch_classes().contains(&(pc % 12))
    }

    /// 0-based scale degree of a pitch class, if it is in the scale.
    pub fn degree_of_pc(&self, pc: u8) -> Option<usize> {
        self.pitch_classes().iter().position(|p| *p == pc % 12)
    }

    /// The spelled note for a 0-based degree, wrapping past the octave.
    pub fn degree(&self, degree: usize) -> Tpc {
        let offs = self.scale.offsets();
        self.tonic.plus_fifths(offs[degree % offs.len()])
    }

    pub fn degree_count(&self) -> usize {
        self.scale.offsets().len()
    }

    /// The key signature as a fifths index (`0` = C major/A minor, `+6` = F♯
    /// major, `−3` = E♭ major/C minor): the fifths position of the RELATIVE
    /// MAJOR tonic.
    ///
    /// For a diatonic mode this is exact and falls straight out of the window
    /// (`tonic + min_offset + 1`, because the ionian window starts one fifth
    /// below its tonic). A non-diatonic scale has no key signature of its
    /// own, so it reports its DIATONIC PARENT's — the one a score would
    /// actually be written in. Symmetric scales (whole tone, octatonic)
    /// have no parent at all and report their tonic's, which is a
    /// convention, not a fact.
    pub fn key_signature_fifths(&self) -> i16 {
        if self.scale.is_diatonic() {
            let min = self.scale.offsets().iter().copied().min().unwrap_or(0);
            return self.tonic.0 + min + 1;
        }
        match self.scale {
            // Written in the natural minor of the same tonic.
            ScaleType::HarmonicMinor
            | ScaleType::MelodicMinor
            | ScaleType::MinorPentatonic
            | ScaleType::Blues => self.tonic.0 - 3,
            ScaleType::MajorPentatonic => self.tonic.0,
            _ => self.tonic.0,
        }
    }

    /// Display name: `"C major"`, `"A minor"`, `"D dorian"`.
    pub fn label(&self) -> String {
        format!("{} {}", self.tonic.pretty(), self.scale.label())
    }

    /// Wire form: `"C ionian"` — unambiguous, and it keeps `Tpc`'s integer
    /// encoding an implementation detail of this module.
    pub fn canonical(&self) -> String {
        format!("{} {}", self.tonic.name(), self.scale.id())
    }

    /// Parse the wire form, and also the friendly forms a human or an agent
    /// is likely to send (`"Bb major"`, `"f# minor"`, `"C"` = C major).
    pub fn parse(s: &str) -> Result<Key, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty key".into());
        }
        // The tonic is the leading letter plus any accidentals; the rest is
        // the scale. Split there rather than on whitespace so `"Ebminor"`
        // parses too.
        let mut split = 1;
        for (i, c) in s.char_indices().skip(1) {
            if matches!(c, '#' | 'b' | '♯' | '♭' | 'x') {
                split = i + c.len_utf8();
            } else {
                break;
            }
        }
        let (tonic_s, rest) = s.split_at(split);
        let tonic = Tpc::parse(tonic_s).ok_or_else(|| format!("bad tonic in key {s:?}"))?;
        let rest = rest.trim();
        let scale = if rest.is_empty() {
            ScaleType::Ionian
        } else {
            ScaleType::parse(rest).ok_or_else(|| format!("unknown scale {rest:?} in key {s:?}"))?
        };
        Ok(Key::new(tonic, scale))
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Semitone sets derived from the fifths offsets — the readable check
    /// that the offset tables are right.
    #[test]
    fn offsets_derive_the_textbook_semitone_sets() {
        for (scale, semis) in [
            (ScaleType::Ionian, vec![0, 2, 4, 5, 7, 9, 11]),
            (ScaleType::Dorian, vec![0, 2, 3, 5, 7, 9, 10]),
            (ScaleType::Phrygian, vec![0, 1, 3, 5, 7, 8, 10]),
            (ScaleType::Lydian, vec![0, 2, 4, 6, 7, 9, 11]),
            (ScaleType::Mixolydian, vec![0, 2, 4, 5, 7, 9, 10]),
            (ScaleType::Aeolian, vec![0, 2, 3, 5, 7, 8, 10]),
            (ScaleType::Locrian, vec![0, 1, 3, 5, 6, 8, 10]),
            (ScaleType::HarmonicMinor, vec![0, 2, 3, 5, 7, 8, 11]),
            (ScaleType::MelodicMinor, vec![0, 2, 3, 5, 7, 9, 11]),
            (ScaleType::MajorPentatonic, vec![0, 2, 4, 7, 9]),
            (ScaleType::MinorPentatonic, vec![0, 3, 5, 7, 10]),
            (ScaleType::Blues, vec![0, 3, 5, 6, 7, 10]),
            (ScaleType::WholeTone, vec![0, 2, 4, 6, 8, 10]),
            (ScaleType::Octatonic, vec![0, 2, 3, 5, 6, 8, 9, 11]),
        ] {
            let got = Key::new(Tpc::C, scale).pitch_classes();
            assert_eq!(got, semis, "{}", scale.id());
        }
    }

    #[test]
    fn a_minor_spells_the_white_keys() {
        let names: Vec<String> =
            Key::new(Tpc::A, ScaleType::Aeolian).spelled().iter().map(|t| t.name()).collect();
        assert_eq!(names, ["A", "B", "C", "D", "E", "F", "G"]);
    }

    #[test]
    fn f_sharp_major_spells_e_sharp_not_f() {
        let names: Vec<String> = Key::new(Tpc(6), ScaleType::Ionian)
            .spelled()
            .iter()
            .map(|t| t.name())
            .collect();
        assert_eq!(names, ["F#", "G#", "A#", "B", "C#", "D#", "E#"]);
    }

    #[test]
    fn key_signatures_come_out_of_the_window() {
        assert_eq!(Key::c_major().key_signature_fifths(), 0);
        assert_eq!(Key::new(Tpc::A, ScaleType::Aeolian).key_signature_fifths(), 0, "relative minor");
        assert_eq!(Key::new(Tpc(6), ScaleType::Ionian).key_signature_fifths(), 6, "F# major");
        assert_eq!(Key::new(Tpc(-2), ScaleType::Ionian).key_signature_fifths(), -2, "Bb major");
        assert_eq!(Key::new(Tpc::D, ScaleType::Dorian).key_signature_fifths(), 0, "D dorian");
        assert_eq!(Key::new(Tpc::E, ScaleType::Phrygian).key_signature_fifths(), 0, "E phrygian");
        assert_eq!(Key::new(Tpc::F, ScaleType::Lydian).key_signature_fifths(), 0, "F lydian");
        assert_eq!(Key::new(Tpc::G, ScaleType::Mixolydian).key_signature_fifths(), 0);
        assert_eq!(Key::new(Tpc::B, ScaleType::Locrian).key_signature_fifths(), 0);
    }

    #[test]
    fn key_wire_form_round_trips_and_accepts_friendly_input() {
        for k in [
            Key::c_major(),
            Key::new(Tpc(-2), ScaleType::Aeolian),
            Key::new(Tpc(6), ScaleType::Dorian),
            Key::new(Tpc::E, ScaleType::HarmonicMinor),
        ] {
            assert_eq!(Key::parse(&k.canonical()).unwrap(), k, "{}", k.canonical());
        }
        assert_eq!(Key::parse("C").unwrap(), Key::c_major(), "bare tonic = major");
        assert_eq!(Key::parse("Bb major").unwrap(), Key::new(Tpc(-2), ScaleType::Ionian));
        assert_eq!(Key::parse("f# minor").unwrap(), Key::new(Tpc(6), ScaleType::Aeolian));
        assert_eq!(Key::parse("Ebminor").unwrap(), Key::new(Tpc(-3), ScaleType::Aeolian));
        assert!(Key::parse("H major").is_err(), "no German note names (yet)");
        assert!(Key::parse("C klezmer").is_err());
    }

    #[test]
    fn minorish_tracks_the_third() {
        assert!(!ScaleType::Ionian.is_minorish());
        assert!(ScaleType::Aeolian.is_minorish());
        assert!(ScaleType::Dorian.is_minorish());
        assert!(ScaleType::HarmonicMinor.is_minorish());
        assert!(!ScaleType::Lydian.is_minorish());
    }

    #[test]
    fn only_the_seven_modes_are_diatonic() {
        assert_eq!(ScaleType::ALL.iter().filter(|s| s.is_diatonic()).count(), 7);
    }
}
