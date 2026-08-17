//! Functional analysis: chord ↔ Roman numeral, harmonic function, borrowed
//! chords, secondary dominants — and the why-string that goes with each.
//!
//! Both directions matter. Analysis (`chord + key → numeral`) drives the
//! labels in the progression strip and the ranking in `progression::
//! suggest_next`; synthesis (`numeral + key → chord`) is the language the
//! stored progression schemas are written in (`"I-V-vi-IV"`), so a schema is
//! one string rather than twelve transposed tables.

use super::chord::{diatonic_chord, Chord, ChordQuality};
use super::scale::Key;
use super::tpc::{interval_name, Tpc};

/// Harmonic function — the three-way classification everything downstream
/// reasons with (the functional automaton, the cadence rules, the ranking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Function {
    /// Home. Rest, arrival, where phrases end.
    Tonic,
    /// Departure. Prepares the dominant (the classic `ii`/`IV`).
    Predominant,
    /// Tension that wants to resolve down a fifth.
    Dominant,
    /// Outside the key's functional system — a colour chord that borrows.
    Chromatic,
}

impl Function {
    pub fn id(self) -> &'static str {
        match self {
            Function::Tonic => "tonic",
            Function::Predominant => "predominant",
            Function::Dominant => "dominant",
            Function::Chromatic => "chromatic",
        }
    }
}

/// What the analysis found. `why` is the teaching payload (product doc §3:
/// never a theory word without what it does).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub roman: String,
    pub function: Function,
    /// 0-based scale degree of the root, when the root is IN the key.
    pub degree: Option<usize>,
    /// Root is in the key but the quality is not the key's own chord there,
    /// or the root is outside the key entirely.
    pub borrowed: bool,
    /// `Some(degree)` when this is a dominant OF another degree (`V7/ii`).
    pub applied_to: Option<usize>,
    pub why: String,
}

/// Roman numerals I..VII, upper case; lowercased for minor/diminished.
const NUMERALS: [&str; 7] = ["I", "II", "III", "IV", "V", "VI", "VII"];

/// Generic scale degree of a root in a key, ignoring chromatic alteration:
/// B♭ and B are both "the seventh degree" of C. Derived from the LETTER, which
/// is exactly why the spelling has to be right (`tpc.rs`).
fn generic_degree(root: Tpc, key: &Key) -> usize {
    (root.diatonic_step() + 7 - key.tonic.diatonic_step()) % 7
}

/// Chromatic alteration of a root against the key's own note on that degree,
/// in semitones (`−1` for ♭VII in major).
fn degree_alteration(root: Tpc, key: &Key, degree: usize) -> i16 {
    let natural = key.degree(degree);
    (root.0 - natural.0) / 7
}

/// The numeral suffix for a quality, in the case-carrying convention this
/// module uses: the numeral's case already says major/minor, so the suffix
/// only has to add what the case cannot.
fn numeral_suffix(q: ChordQuality) -> &'static str {
    match q {
        ChordQuality::Maj | ChordQuality::Min => "",
        ChordQuality::Dim => "°",
        ChordQuality::Aug => "+",
        ChordQuality::Five => "5",
        ChordQuality::Sus2 => "sus2",
        ChordQuality::Sus4 => "sus4",
        ChordQuality::Maj6 | ChordQuality::Min6 => "6",
        ChordQuality::Maj7 => "maj7",
        ChordQuality::Dom7 | ChordQuality::Min7 => "7",
        ChordQuality::MinMaj7 => "maj7",
        ChordQuality::HalfDim7 => "ø7",
        ChordQuality::Dim7 => "°7",
        ChordQuality::Aug7 => "+7",
        ChordQuality::Dom7Sus4 => "7sus4",
        ChordQuality::Maj9 | ChordQuality::Dom9 | ChordQuality::Min9 => "9",
        ChordQuality::Dom7Flat9 => "7♭9",
        ChordQuality::Dom7Sharp9 => "7♯9",
        ChordQuality::Dom13 => "13",
        ChordQuality::Min11 => "11",
        ChordQuality::Maj7Sharp11 => "maj7♯11",
    }
}

fn is_lowercase_numeral(q: ChordQuality) -> bool {
    matches!(
        q,
        ChordQuality::Min
            | ChordQuality::Min6
            | ChordQuality::Min7
            | ChordQuality::Min9
            | ChordQuality::Min11
            | ChordQuality::MinMaj7
            | ChordQuality::Dim
            | ChordQuality::Dim7
            | ChordQuality::HalfDim7
    )
}

/// Figured-bass inversion figure for a slash chord: `"6"` for a triad with
/// its third in the bass, `"65"`/`"43"`/`"42"` for sevenths. Empty when the
/// bass is the root or is not a chord tone at all (a `C/D` gets no figure —
/// it is a pedal, not an inversion).
fn inversion_figure(chord: &Chord) -> &'static str {
    let Some(bass) = chord.bass else { return "" };
    let tones = chord.tones();
    let Some(idx) = tones.iter().position(|t| t.pitch_class() == bass.pitch_class()) else {
        return "";
    };
    let seventh = chord.quality.has_seventh();
    match (idx, seventh) {
        (1, false) => "6",
        (2, false) => "64",
        (1, true) => "65",
        (2, true) => "43",
        (3, true) => "42",
        _ => "",
    }
}

/// Analyse a chord in a key.
pub fn analyze(chord: &Chord, key: &Key) -> Analysis {
    let degree = generic_degree(chord.root, key);
    let alt = degree_alteration(chord.root, key, degree);
    let in_key_root = alt == 0 && degree < key.degree_count();
    let native = if in_key_root {
        diatonic_chord(key, degree, chord.quality.has_seventh())
    } else {
        None
    };
    let is_native = native.map(|c| c.quality == chord.quality).unwrap_or(false);

    let mut numeral = NUMERALS[degree].to_string();
    if is_lowercase_numeral(chord.quality) {
        numeral = numeral.to_ascii_lowercase();
    }
    let prefix = match alt {
        0 => String::new(),
        n if n > 0 => "♯".repeat(n as usize),
        n => "♭".repeat((-n) as usize),
    };
    let figure = inversion_figure(chord);
    // Figured bass replaces the `7`: a first-inversion dominant seventh is
    // `V65`, never `V765` — the figure already says there is a seventh.
    let suffix = match (figure.is_empty(), numeral_suffix(chord.quality)) {
        (false, s) => s.strip_suffix('7').unwrap_or(s),
        (true, s) => s,
    };
    let mut roman = format!("{prefix}{numeral}{suffix}{figure}");

    // Secondary dominant: a dominant-shape chord a fifth above some degree of
    // the key that is NOT the key's own V. `V7/ii` reads better than `♭VI7`
    // and is what the chord is actually doing.
    let applied_to = if chord.quality.is_dominant_shape() && degree != 4 {
        (0..key.degree_count()).find(|d| {
            key.degree(*d).plus_fifths(1).pitch_class() == chord.root.pitch_class()
        })
    } else {
        None
    };
    if let Some(target) = applied_to {
        let mut target_numeral = NUMERALS[target].to_string();
        if diatonic_chord(key, target, false)
            .map(|c| is_lowercase_numeral(c.quality))
            .unwrap_or(false)
        {
            target_numeral = target_numeral.to_ascii_lowercase();
        }
        roman = format!("V{}/{}", numeral_suffix(chord.quality), target_numeral);
    }

    let function = if applied_to.is_some() {
        Function::Dominant
    } else if !in_key_root {
        Function::Chromatic
    } else if chord.quality.is_dominant_shape() {
        Function::Dominant
    } else {
        function_of_degree(degree)
    };

    let why = why_for(chord, key, degree, alt, is_native, applied_to, function);
    Analysis { roman, function, degree: in_key_root.then_some(degree), borrowed: !is_native, applied_to, why }
}

/// Degree → function, the standard mapping. Works unchanged in minor keys:
/// `i III VI` are tonic, `iv ii` predominant, `v/V vii` dominant. A foreign
/// quality on a native root (modal mixture) still functions where its degree
/// sits — it is flagged through `Analysis::borrowed`, not by moving it.
fn function_of_degree(degree: usize) -> Function {
    match degree {
        0 | 2 | 5 => Function::Tonic,
        1 | 3 => Function::Predominant,
        4 | 6 => Function::Dominant,
        _ => Function::Chromatic,
    }
}

/// The teaching sentence. Concrete: it names the actual notes that do the
/// work, not the category.
fn why_for(
    chord: &Chord,
    key: &Key,
    degree: usize,
    alt: i16,
    is_native: bool,
    applied_to: Option<usize>,
    function: Function,
) -> String {
    if let Some(target) = applied_to {
        let t = diatonic_chord(key, target, false)
            .map(|c| c.pretty())
            .unwrap_or_else(|| key.degree(target).pretty());
        return format!(
            "A secondary dominant: it borrows {}'s own V, so it pulls to {} instead of home. \
             Its {} is the note that does not belong to {} — that is what makes the pull audible.",
            t,
            t,
            chord.root.plus_fifths(4).pretty(),
            key.label(),
        );
    }
    if alt != 0 {
        let dir = if alt < 0 { "counter-clockwise" } else { "clockwise" };
        return format!(
            "A borrowed chord: {} is not in {}, so it comes from a neighbouring key one step \
             {} on the circle. Borrowed chords are colour — they work best going somewhere, \
             not sitting still.",
            chord.root.pretty(),
            key.label(),
            dir,
        );
    }
    if !is_native {
        return format!(
            "Modal mixture: {} sits on the {} degree of {}, but with the other quality. \
             It still functions as {}, with a note swapped for its chromatic neighbour.",
            chord.pretty(),
            ordinal(degree),
            key.label(),
            function.id(),
        );
    }
    match function {
        Function::Tonic => match degree {
            0 => format!(
                "Home. {} is where the phrase can end and sound finished.",
                chord.pretty()
            ),
            5 => format!(
                "The relative minor — the same key signature as {}, so it feels like home with \
                 the lights down. A very common way to leave the tonic without leaving the key.",
                key.label()
            ),
            _ => "A tonic substitute: it shares two of its three notes with the tonic chord, \
                 so it rests without repeating it."
                .to_string(),
        },
        Function::Predominant => format!(
            "Departure: it moves away from home and sets up the dominant. {} → V → I is the \
             oldest sentence in tonal music.",
            chord.pretty()
        ),
        Function::Dominant => {
            let third = chord.root.plus_fifths(4);
            format!(
                "The dominant: maximum tension. Its {} is the leading tone and wants to rise a \
                 semitone to {}; that pull is what makes the next chord sound like an arrival.",
                third.pretty(),
                key.tonic.pretty(),
            )
        }
        Function::Chromatic => format!("{} is outside {}'s functional chords.", chord.pretty(), key.label()),
    }
}

fn ordinal(degree: usize) -> &'static str {
    ["first", "second", "third", "fourth", "fifth", "sixth", "seventh"][degree.min(6)]
}

/// Synthesis: turn a Roman numeral into a chord in a key. The schema
/// language: `"I"`, `"vi"`, `"V7"`, `"bVII"`, `"ii7"`, `"V7/ii"`, `"IVmaj7"`,
/// `"viio7"`, `"i"`, `"Isus4"`.
///
/// Two conventions, both load-bearing and both the ones a chord chart uses:
///
/// * **Case decides quality**, but a numeral whose case AGREES with the key's
///   own chord there inherits that chord (so `"vii"` in C major is `Bdim`, not
///   `Bm`, and `"ii7"` is `Dm7`). A case that DISAGREES wins: `"V"` in A minor
///   is `E` major — the borrowed dominant, which is the whole reason a minor
///   key can cadence — and `"IV"` in A minor is `D` major, the dorian colour.
/// * **An accidental is measured from the MAJOR scale of the tonic**, never
///   from the key's own degree. `♭VII` means a triad on the ♭7 of the major
///   scale, which in a minor key is already the natural seventh degree — that
///   is why the Andalusian cadence in A minor is `Am ♭VII ♭VI V` = `Am G F E`
///   and not `Am G♭ F♭ E`.
pub fn parse_roman(key: &Key, s: &str) -> Result<Chord, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty roman numeral".into());
    }
    // Applied dominant: split on '/' first so "V7/ii" recurses on its target.
    if let Some((head, tail)) = s.split_once('/') {
        let target = parse_roman(key, tail)?;
        let quality = if head.contains('7') { ChordQuality::Dom7 } else { ChordQuality::Maj };
        return Ok(Chord::new(target.root.plus_fifths(1), quality));
    }
    let mut rest = s;
    let mut alt = 0i16;
    while let Some(c) = rest.chars().next() {
        match c {
            'b' | '♭' => alt -= 1,
            '#' | '♯' => alt += 1,
            _ => break,
        }
        rest = &rest[c.len_utf8()..];
    }
    // Longest numeral first so "III" is not read as "II" + junk.
    let mut degree = None;
    let mut suffix = "";
    for len in [3usize, 2, 1] {
        if rest.len() < len {
            continue;
        }
        let (head, tail) = rest.split_at(len);
        let upper = head.to_ascii_uppercase();
        if let Some(d) = NUMERALS.iter().position(|n| *n == upper) {
            degree = Some((d, head.chars().next().is_some_and(|c| c.is_lowercase())));
            suffix = tail;
            break;
        }
    }
    let (degree, lowercase) = degree.ok_or_else(|| format!("not a roman numeral: {s:?}"))?;
    let seventh = suffix.contains('7');
    let native = diatonic_chord(key, degree, seventh);
    let case_quality = if lowercase { ChordQuality::Min } else { ChordQuality::Maj };
    let quality = if suffix.is_empty() {
        match native {
            // The key's own chord, but only when the case agrees with it —
            // see this function's doc for why the disagreement matters.
            Some(c) if alt == 0 && is_lowercase_numeral(c.quality) == lowercase => c.quality,
            _ => case_quality,
        }
    } else {
        quality_from_numeral_suffix(suffix, lowercase)
            .ok_or_else(|| format!("unknown numeral suffix {suffix:?} in {s:?}"))?
    };
    let root = if alt == 0 {
        key.degree(degree)
    } else {
        // Measured from the major scale of the tonic (see the doc above).
        key.tonic.plus_fifths(crate::theory::tpc::ionian_offset(degree) + alt * 7)
    };
    Ok(Chord::new(root, quality))
}

fn quality_from_numeral_suffix(suffix: &str, lowercase: bool) -> Option<ChordQuality> {
    let s = suffix.trim();
    Some(match s {
        "" => {
            if lowercase {
                ChordQuality::Min
            } else {
                ChordQuality::Maj
            }
        }
        "7" => {
            if lowercase {
                ChordQuality::Min7
            } else {
                ChordQuality::Dom7
            }
        }
        "maj7" | "M7" => {
            if lowercase {
                ChordQuality::MinMaj7
            } else {
                ChordQuality::Maj7
            }
        }
        "6" => {
            if lowercase {
                ChordQuality::Min6
            } else {
                ChordQuality::Maj6
            }
        }
        "9" => {
            if lowercase {
                ChordQuality::Min9
            } else {
                ChordQuality::Dom9
            }
        }
        "11" => ChordQuality::Min11,
        "13" => ChordQuality::Dom13,
        "o" | "°" | "dim" => ChordQuality::Dim,
        "o7" | "°7" | "dim7" => ChordQuality::Dim7,
        "ø7" | "ø" | "m7b5" => ChordQuality::HalfDim7,
        "+" | "aug" => ChordQuality::Aug,
        "sus4" | "sus" => ChordQuality::Sus4,
        "sus2" => ChordQuality::Sus2,
        "7sus4" => ChordQuality::Dom7Sus4,
        "5" => ChordQuality::Five,
        _ => return None,
    })
}

/// The tritone of a dominant-shape chord, as a why-string fragment: which two
/// notes resolve where. Used by the melody generator's cadence handling and by
/// the suggestion list.
pub fn tritone_resolution(chord: &Chord) -> Option<String> {
    if !chord.quality.is_dominant_shape() {
        return None;
    }
    let third = chord.root.plus_fifths(4);
    let seventh = chord.root.plus_fifths(-2);
    Some(format!(
        "{} rises to {} and {} falls to {} ({} resolving inward)",
        third.pretty(),
        // A semitone UP is −5 fifths, a semitone DOWN is +5: the two voices
        // of the tritone move toward each other by a half step each.
        third.plus_fifths(-5).pretty(),
        seventh.pretty(),
        seventh.plus_fifths(5).pretty(),
        interval_name(seventh.fifths_from(third)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::scale::ScaleType;

    #[test]
    fn the_diatonic_sevenths_analyse_as_themselves() {
        let key = Key::c_major();
        for (symbol, roman, function) in [
            ("Cmaj7", "Imaj7", Function::Tonic),
            ("Dm7", "ii7", Function::Predominant),
            ("Em7", "iii7", Function::Tonic),
            ("Fmaj7", "IVmaj7", Function::Predominant),
            ("G7", "V7", Function::Dominant),
            ("Am7", "vi7", Function::Tonic),
            ("Bm7b5", "viiø7", Function::Dominant),
        ] {
            let a = analyze(&Chord::parse(symbol).unwrap(), &key);
            assert_eq!(a.roman, roman, "{symbol}");
            assert_eq!(a.function, function, "{symbol}");
            assert!(!a.borrowed, "{symbol} is native to C major");
        }
    }

    #[test]
    fn borrowed_flat_seven_is_named_and_explained() {
        let a = analyze(&Chord::parse("Bb").unwrap(), &Key::c_major());
        assert_eq!(a.roman, "♭VII");
        assert!(a.borrowed);
        assert_eq!(a.function, Function::Chromatic);
        assert!(a.why.contains("circle"), "why should place it on the circle: {}", a.why);
    }

    #[test]
    fn secondary_dominant_is_named_after_its_target() {
        let a = analyze(&Chord::parse("A7").unwrap(), &Key::c_major());
        assert_eq!(a.roman, "V7/ii", "A7 pulls to Dm, not to C");
        assert_eq!(a.applied_to, Some(1));
        assert_eq!(a.function, Function::Dominant);
        assert!(a.why.contains("secondary dominant") || a.why.contains("secondary"));
        // D7 in C major is V/V.
        assert_eq!(analyze(&Chord::parse("D7").unwrap(), &Key::c_major()).roman, "V7/V");
        // The key's own V7 stays V7 and is never "applied".
        let v = analyze(&Chord::parse("G7").unwrap(), &Key::c_major());
        assert_eq!(v.applied_to, None);
    }

    #[test]
    fn modal_mixture_is_flagged_without_being_called_chromatic() {
        // Fm in C major: right root (IV), minor quality — the "iv" of the
        // parallel minor.
        let a = analyze(&Chord::parse("Fm").unwrap(), &Key::c_major());
        assert_eq!(a.roman, "iv");
        assert!(a.borrowed);
        assert_eq!(a.function, Function::Predominant, "it still prepares the dominant");
        assert!(a.why.contains("Modal mixture"));
    }

    #[test]
    fn inversions_get_figured_bass() {
        assert_eq!(analyze(&Chord::parse("C/E").unwrap(), &Key::c_major()).roman, "I6");
        assert_eq!(analyze(&Chord::parse("C/G").unwrap(), &Key::c_major()).roman, "I64");
        // Figured bass replaces the 7 — a first-inversion dominant seventh is
        // V65, not V765.
        assert_eq!(analyze(&Chord::parse("G7/B").unwrap(), &Key::c_major()).roman, "V65");
        assert_eq!(analyze(&Chord::parse("G7/D").unwrap(), &Key::c_major()).roman, "V43");
        assert_eq!(analyze(&Chord::parse("G7/F").unwrap(), &Key::c_major()).roman, "V42");
        // A bass that is not a chord tone is a pedal, not an inversion.
        assert_eq!(analyze(&Chord::parse("C/D").unwrap(), &Key::c_major()).roman, "I");
    }

    #[test]
    fn minor_key_numerals_read_correctly() {
        let am = Key::new(Tpc::A, ScaleType::Aeolian);
        assert_eq!(analyze(&Chord::parse("Am").unwrap(), &am).roman, "i");
        assert_eq!(analyze(&Chord::parse("Dm").unwrap(), &am).roman, "iv");
        assert_eq!(analyze(&Chord::parse("F").unwrap(), &am).roman, "VI");
        assert_eq!(analyze(&Chord::parse("E7").unwrap(), &am).roman, "V7", "the borrowed dominant");
        assert_eq!(analyze(&Chord::parse("E7").unwrap(), &am).function, Function::Dominant);
    }

    #[test]
    fn roman_numerals_round_trip_through_synthesis() {
        let key = Key::c_major();
        for (numeral, symbol) in [
            ("I", "C"),
            ("ii", "Dm"),
            ("iii", "Em"),
            ("IV", "F"),
            ("V", "G"),
            ("vi", "Am"),
            ("vii", "Bdim"),
            ("V7", "G7"),
            ("ii7", "Dm7"),
            ("IVmaj7", "Fmaj7"),
            ("bVII", "Bb"),
            ("bIII", "Eb"),
            ("#IV", "F#"),
            ("Isus4", "Csus4"),
            ("viio7", "Bdim7"),
            ("V7/ii", "A7"),
            ("V7/V", "D7"),
        ] {
            let c = parse_roman(&key, numeral).unwrap_or_else(|e| panic!("{numeral}: {e}"));
            assert_eq!(c.symbol(), symbol, "{numeral}");
        }
        assert!(parse_roman(&key, "VIII").is_err());
        assert!(parse_roman(&key, "I?").is_err());
    }

    #[test]
    fn case_decides_quality_but_agreement_inherits_the_keys_own_chord() {
        let c = Key::c_major();
        // Agreement: the key's chord, including the ones the case cannot spell
        // (`vii` is diminished, not minor).
        assert_eq!(parse_roman(&c, "vii").unwrap().symbol(), "Bdim");
        assert_eq!(parse_roman(&c, "vi").unwrap().symbol(), "Am");
        // Disagreement: the case wins, which is how a minor key gets a real
        // dominant and a dorian IV.
        let am = Key::new(Tpc::A, ScaleType::Aeolian);
        assert_eq!(parse_roman(&am, "V").unwrap().symbol(), "E", "the borrowed dominant");
        assert_eq!(parse_roman(&am, "v").unwrap().symbol(), "Em", "…and the powerless one");
        assert_eq!(parse_roman(&am, "IV").unwrap().symbol(), "D", "dorian colour");
        assert_eq!(parse_roman(&am, "iv").unwrap().symbol(), "Dm");
    }

    #[test]
    fn altered_degrees_are_measured_from_the_major_scale() {
        // The trap: in A minor the seventh degree is ALREADY G, and ♭VII must
        // still mean G — not G♭. Same for ♭VI (F, not F♭).
        let am = Key::new(Tpc::A, ScaleType::Aeolian);
        let got: Vec<String> = ["i", "bVII", "bVI", "V"]
            .iter()
            .map(|n| parse_roman(&am, n).unwrap().symbol())
            .collect();
        assert_eq!(got, ["Am", "G", "F", "E"], "the Andalusian cadence");
        // And unchanged in major, where the ♭7 really is an alteration.
        let c = Key::c_major();
        assert_eq!(parse_roman(&c, "bVII").unwrap().symbol(), "Bb");
        assert_eq!(parse_roman(&c, "bVI").unwrap().symbol(), "Ab");
        assert_eq!(parse_roman(&c, "bIII").unwrap().symbol(), "Eb");
        // A phrygian key: ♭II is its own second degree.
        let ep = Key::new(Tpc::E, ScaleType::Phrygian);
        assert_eq!(parse_roman(&ep, "bII").unwrap().symbol(), "F");
    }

    #[test]
    fn tritone_resolution_names_both_voices() {
        let s = tritone_resolution(&Chord::parse("G7").unwrap()).unwrap();
        assert!(s.contains('B') && s.contains('C') && s.contains('F') && s.contains('E'), "{s}");
        assert!(tritone_resolution(&Chord::parse("Cmaj7").unwrap()).is_none());
    }
}
