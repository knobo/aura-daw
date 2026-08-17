//! The palette: which notes may I play right now, and why.
//!
//! This is the feature that makes the editor teach (product doc §4.3, ruling
//! H-9). Given the key and the chord sounding at a tick, every one of the
//! twelve pitch classes gets a role:
//!
//! | Role | Rule |
//! |---|---|
//! | chord tone | in the chord (root / 3rd / 5th / 7th by interval step) |
//! | extension  | in the key, and NOT a semitone above a chord tone → an available 9/11/13 |
//! | avoid      | in the key, **a semitone above a chord tone** |
//! | tension    | outside the key, a semitone BELOW a chord tone (a chromatic approach), or a blue note |
//! | outside    | everything else |
//!
//! The avoid rule is one line and it is not a heuristic: it is Berklee
//! chord-scale theory's *avoid note*, and it produces the textbook answer on
//! all seven diatonic sevenths of a major key — including the case that
//! proves it is doing real work, `Fmaj7`, where the ♯11 must come out
//! AVAILABLE (it is the best note on a IVmaj7) and does, because B is a
//! semitone *below* C rather than above any chord tone.
//!
//! The seven-chord table is a test in this file, not a comment.

use super::chord::Chord;
use super::scale::Key;
use super::tpc::{degree_label, interval_step, Tpc};

/// What a pitch class is, relative to the current chord and key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteRole {
    Root,
    Third,
    Fifth,
    Seventh,
    /// A chord tone above the seventh (the 9 of a `C9`), or an available
    /// extension from the key.
    Extension,
    /// In the key, but the extension/avoid rules had nothing to say — the
    /// fallback that keeps exotic scales honest rather than mislabelled.
    Scale,
    /// Outside the key, but it resolves: a semitone below a chord tone, or a
    /// blue note. Usable in passing, needs somewhere to go.
    Tension,
    /// In the key and a semitone above a chord tone. It fights the chord.
    Avoid,
    /// Outside everything.
    Outside,
}

impl NoteRole {
    /// Wire id — also the CSS class the piano roll tints with.
    pub fn id(self) -> &'static str {
        match self {
            NoteRole::Root => "root",
            NoteRole::Third => "third",
            NoteRole::Fifth => "fifth",
            NoteRole::Seventh => "seventh",
            NoteRole::Extension => "extension",
            NoteRole::Scale => "scale",
            NoteRole::Tension => "tension",
            NoteRole::Avoid => "avoid",
            NoteRole::Outside => "outside",
        }
    }

    /// Is this a note a beginner can play on a strong beat without thinking?
    /// The predicate behind "no wrong notes" input and behind the melody
    /// generator's strong-beat rule.
    pub fn is_safe(self) -> bool {
        matches!(
            self,
            NoteRole::Root
                | NoteRole::Third
                | NoteRole::Fifth
                | NoteRole::Seventh
                | NoteRole::Extension
                | NoteRole::Scale
        )
    }

    /// A chord tone proper (not an available extension from the key).
    pub fn is_chord_tone(self) -> bool {
        matches!(self, NoteRole::Root | NoteRole::Third | NoteRole::Fifth | NoteRole::Seventh)
    }
}

/// One pitch class, classified.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteClass {
    pub pitch_class: u8,
    /// Best spelling in this context (the key's own, else the chord's, else
    /// the simplest).
    pub tpc: Tpc,
    pub role: NoteRole,
    /// Degree label relative to the chord (or to the key when there is no
    /// chord): `"1"`, `"♭7"`, `"♯11"`.
    pub degree: String,
    /// The teaching sentence, for the roles where there is something to say.
    pub why: Option<String>,
}

/// The twelve classes, plus the context they were derived from.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub key: Key,
    pub chord: Option<Chord>,
    pub classes: Vec<NoteClass>,
}

impl Palette {
    pub fn class_of_pc(&self, pc: u8) -> &NoteClass {
        &self.classes[(pc % 12) as usize]
    }

    pub fn role_of_midi(&self, midi: u8) -> NoteRole {
        self.class_of_pc(midi % 12).role
    }

    /// Snap a MIDI key to the nearest safe note — "no wrong notes" input.
    /// Ties go UP, because a beginner sliding off a black key is more often
    /// reaching for the note above.
    pub fn nearest_safe_midi(&self, midi: u8) -> u8 {
        if self.role_of_midi(midi).is_safe() {
            return midi;
        }
        for d in 1..=6i16 {
            for cand in [midi as i16 + d, midi as i16 - d] {
                if (0..=127).contains(&cand) && self.role_of_midi(cand as u8).is_safe() {
                    return cand as u8;
                }
            }
        }
        midi
    }

    /// The chord tones of this palette inside a MIDI range, ascending — the
    /// candidate pool every generator draws its strong-beat notes from.
    pub fn chord_tone_keys(&self, lo: u8, hi: u8) -> Vec<u8> {
        (lo..=hi).filter(|k| self.role_of_midi(*k).is_chord_tone()).collect()
    }

    /// Safe notes in a MIDI range, ascending.
    pub fn safe_keys(&self, lo: u8, hi: u8) -> Vec<u8> {
        (lo..=hi).filter(|k| self.role_of_midi(*k).is_safe()).collect()
    }
}

/// Classify all twelve pitch classes against `key` and, when one is sounding,
/// `chord`. With no chord the palette degrades to the key's scale, which is
/// the honest answer between chord regions.
pub fn palette(key: &Key, chord: Option<&Chord>) -> Palette {
    let key_pcs = key.pitch_classes();
    let chord_pcs: Vec<u8> = chord.map(|c| c.pitch_classes()).unwrap_or_default();
    let classes = (0..12u8)
        .map(|pc| classify(pc, key, chord, &key_pcs, &chord_pcs))
        .collect();
    Palette { key: *key, chord: chord.copied(), classes }
}

fn classify(
    pc: u8,
    key: &Key,
    chord: Option<&Chord>,
    key_pcs: &[u8],
    chord_pcs: &[u8],
) -> NoteClass {
    let tpc = spell(pc, key, chord);
    let in_key = key_pcs.contains(&pc);
    // A semitone ABOVE a chord tone is what makes a note fight the chord; a
    // semitone BELOW one is a leading tone into it. The asymmetry is the whole
    // rule.
    let above = chord_pcs.contains(&((pc + 11) % 12));
    let below = chord_pcs.contains(&((pc + 1) % 12));
    let degree = match chord {
        Some(c) => degree_label(tpc.fifths_from(c.root)),
        None => degree_label(tpc.fifths_from(key.tonic)),
    };

    // 1. Chord tone: role from the interval's diatonic step, so a ♭3 is a
    //    third and a ♭♭7 is a seventh.
    if let Some(c) = chord {
        if let Some((tone, label)) = c.tone_for_pc(pc) {
            let fifths = tone.fifths_from(c.root);
            let role = match interval_step(fifths) {
                0 => NoteRole::Root,
                2 => NoteRole::Third,
                4 => NoteRole::Fifth,
                6 => NoteRole::Seventh,
                _ => NoteRole::Extension,
            };
            let why = match role {
                NoteRole::Root => Some(format!("The root of {} — the safest note there is.", c.pretty())),
                NoteRole::Third => Some(format!(
                    "The third of {}: the note that decides whether the chord is major or minor.",
                    c.pretty()
                )),
                NoteRole::Fifth => Some(format!(
                    "The fifth of {} — stable, but it says the least about the chord.",
                    c.pretty()
                )),
                NoteRole::Seventh => Some(format!(
                    "The seventh of {}: with the third, this is what gives the chord its \
                     character. Land on it and the harmony is unmistakable.",
                    c.pretty()
                )),
                _ => Some(format!("The {label} is part of {} itself.", c.pretty())),
            };
            return NoteClass { pitch_class: pc, tpc, role, degree: label, why };
        }
    }

    // 2. In the key: available colour, unless it sits a semitone above a
    //    chord tone.
    if in_key {
        if above {
            let clashes_with = chord
                .and_then(|c| c.tone_for_pc((pc + 11) % 12).map(|(t, l)| (t, l, c.pretty())));
            let why = clashes_with.map(|(t, l, sym)| {
                format!(
                    "An avoid note: it sits one semitone above {} (the {l} of {sym}), so the two \
                     notes grind instead of blending. Fine passing through on a weak beat — \
                     never held.",
                    t.pretty(),
                )
            });
            return NoteClass { pitch_class: pc, tpc, role: NoteRole::Avoid, degree, why };
        }
        if chord.is_some() {
            let why = Some(format!(
                "Available colour: the {degree}. It is in the key and does not clash with any \
                 chord tone, so it can be held."
            ));
            return NoteClass { pitch_class: pc, tpc, role: NoteRole::Extension, degree, why };
        }
        let why = Some(format!("Degree {degree} of {}.", key.label()));
        return NoteClass { pitch_class: pc, tpc, role: NoteRole::Scale, degree, why };
    }

    // 3. Outside the key. A semitone below a chord tone is a leading tone into
    //    it; the blue notes are the other chromatic notes that earn their
    //    place.
    if below {
        let target = chord.and_then(|c| c.tone_for_pc((pc + 1) % 12));
        let why = target.map(|(t, l)| {
            format!(
                "Chromatic, but it resolves: a semitone below {} (the {l}). Play it on the way \
                 in, not on the beat.",
                t.pretty()
            )
        });
        return NoteClass { pitch_class: pc, tpc, role: NoteRole::Tension, degree, why };
    }
    if is_blue_note(pc, key) {
        let why = Some(format!(
            "A blue note ({degree} of the key). Outside the scale on purpose — the whole point \
             is that it bends."
        ));
        return NoteClass { pitch_class: pc, tpc, role: NoteRole::Tension, degree, why };
    }
    NoteClass {
        pitch_class: pc,
        tpc,
        role: NoteRole::Outside,
        degree,
        why: Some(format!(
            "Outside {} and outside the chord. Not forbidden — but it needs a reason.",
            key.label()
        )),
    }
}

/// The ♭3, ♭5 and ♭7 of a major key: the notes that are wrong in theory and
/// right in practice everywhere blues touched.
fn is_blue_note(pc: u8, key: &Key) -> bool {
    if key.scale.is_minorish() {
        return false; // ♭3 and ♭7 are already in the scale — nothing to borrow
    }
    let t = key.tonic.pitch_class();
    [3u8, 6, 10].iter().any(|off| (t + off) % 12 == pc)
}

/// Best spelling for a pitch class in context: the chord's own tone if it is
/// one, else the key's note, else the simplest spelling leaning the way the
/// key signature leans. Beginners never see a `D♯` in a flat key this way.
fn spell(pc: u8, key: &Key, chord: Option<&Chord>) -> Tpc {
    // The CHORD wins for its own tones: the seventh of Cdim7 is B♭♭, even in
    // C major where the same pitch class is otherwise spelled A. Everything
    // else is spelled by the key, which is what a player reads.
    if let Some(t) = chord.and_then(|c| c.tones().into_iter().find(|t| t.pitch_class() == pc)) {
        return t;
    }
    if let Some(t) = key.spelled().into_iter().find(|t| t.pitch_class() == pc) {
        return t;
    }
    // In neither: take the simplest spelling, and break the E♭-vs-D♯ kind of
    // tie in the direction the key signature already leans, so a flat key's
    // chromatic notes come out flat. (Fewest accidentals FIRST — otherwise a
    // one-sided window prints F♭ in E♭ major, where every musician writes E.)
    let sharpish = key.key_signature_fifths() >= 0;
    (-11..=11)
        .map(Tpc)
        .filter(|t| t.pitch_class() == pc)
        .min_by_key(|t| {
            let lean = if sharpish { -t.0 } else { t.0 };
            (t.accidentals().abs(), lean)
        })
        .unwrap_or_else(|| Tpc::simplest_for_pc(pc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::chord::diatonic_chords;

    fn roles(key: &Key, symbol: &str) -> Vec<(String, &'static str)> {
        let chord = Chord::parse(symbol).unwrap();
        palette(key, Some(&chord))
            .classes
            .iter()
            .map(|c| (c.tpc.name(), c.role.id()))
            .collect()
    }

    fn names_with_role(key: &Key, symbol: &str, role: NoteRole) -> Vec<String> {
        let chord = Chord::parse(symbol).unwrap();
        palette(key, Some(&chord))
            .classes
            .iter()
            .filter(|c| c.role == role)
            .map(|c| c.tpc.name())
            .collect()
    }

    /// Ruling H-9: the product doc's table, as a test (note names come back in
    /// PITCH-CLASS order, which is why the lists are not in the doc's reading
    /// order). If this fails, the feature's credibility fails with it.
    #[test]
    fn the_seven_diatonic_sevenths_get_their_textbook_available_and_avoid_notes() {
        let key = Key::c_major();
        for (symbol, extensions, avoid) in [
            ("Cmaj7", vec!["D", "A"], vec!["F"]),
            ("Dm7", vec!["E", "G", "B"], vec![]),
            ("Em7", vec!["A"], vec!["C", "F"]),
            ("Fmaj7", vec!["D", "G", "B"], vec![]),
            ("G7", vec!["E", "A"], vec!["C"]),
            ("Am7", vec!["D", "B"], vec!["F"]),
            ("Bm7b5", vec!["E", "G"], vec!["C"]),
        ] {
            assert_eq!(
                names_with_role(&key, symbol, NoteRole::Extension),
                extensions,
                "{symbol} available extensions"
            );
            assert_eq!(names_with_role(&key, symbol, NoteRole::Avoid), avoid, "{symbol} avoid notes");
        }
    }

    #[test]
    fn the_sharp_eleven_on_four_maj_seven_is_available_not_avoided() {
        // The row that proves the rule does real work: B over Fmaj7 is the
        // ♯11, the characteristic lydian colour, and a naive
        // "non-chord-tone = risky" rule would flag it.
        let p = palette(&Key::c_major(), Some(&Chord::parse("Fmaj7").unwrap()));
        let b = p.class_of_pc(11);
        assert_eq!(b.role, NoteRole::Extension);
        assert_eq!(b.degree, "♯11");
        assert!(b.why.as_ref().unwrap().contains("held"));
    }

    #[test]
    fn chord_tones_are_labelled_by_role_not_by_semitone() {
        let p = palette(&Key::c_major(), Some(&Chord::parse("G7").unwrap()));
        assert_eq!(p.class_of_pc(7).role, NoteRole::Root);
        assert_eq!(p.class_of_pc(11).role, NoteRole::Third);
        assert_eq!(p.class_of_pc(2).role, NoteRole::Fifth);
        assert_eq!(p.class_of_pc(5).role, NoteRole::Seventh);
        assert_eq!(p.class_of_pc(5).degree, "♭7");
        assert_eq!(p.class_of_pc(5).tpc.name(), "F");
        // A flat third is still a THIRD.
        let m = palette(&Key::c_major(), Some(&Chord::parse("Cm").unwrap()));
        assert_eq!(m.class_of_pc(3).role, NoteRole::Third);
        assert_eq!(m.class_of_pc(3).degree, "♭3");
        // And a doubly-flattened seventh is still a seventh.
        let d = palette(&Key::c_major(), Some(&Chord::parse("Cdim7").unwrap()));
        assert_eq!(d.class_of_pc(9).role, NoteRole::Seventh);
        assert_eq!(d.class_of_pc(9).tpc.name(), "Bbb");
    }

    #[test]
    fn chromatic_notes_split_into_approach_tensions_and_outside() {
        let p = palette(&Key::c_major(), Some(&Chord::parse("C").unwrap()));
        // D# is outside the key and a semitone below E (the third): an
        // approach tone, tense but going somewhere.
        assert_eq!(p.class_of_pc(3).role, NoteRole::Tension);
        assert_eq!(p.class_of_pc(3).tpc.name(), "D#");
        assert!(p.class_of_pc(3).why.as_ref().unwrap().contains("resolves"));
        // B♭ is outside the key and not adjacent to any chord tone of C —
        // but it is the ♭7 blue note, which is a different reason to keep it.
        assert_eq!(p.class_of_pc(10).role, NoteRole::Tension);
        assert!(p.class_of_pc(10).why.as_ref().unwrap().contains("blue"));
        // C# is outside, not below a chord tone (D is not in a C major
        // triad), and not a blue note.
        assert_eq!(p.class_of_pc(1).role, NoteRole::Outside);
    }

    #[test]
    fn with_no_chord_the_palette_is_the_scale() {
        let p = palette(&Key::c_major(), None);
        let scale: Vec<u8> = p
            .classes
            .iter()
            .filter(|c| c.role == NoteRole::Scale)
            .map(|c| c.pitch_class)
            .collect();
        assert_eq!(scale, vec![0, 2, 4, 5, 7, 9, 11]);
        assert!(p.classes.iter().all(|c| c.role != NoteRole::Avoid), "nothing to clash with");
    }

    #[test]
    fn spelling_follows_the_key_not_the_pitch_class() {
        // Eb major: the chromatic notes come out flat, not sharp.
        let eb = Key::parse("Eb major").unwrap();
        let names: Vec<String> = palette(&eb, None).classes.iter().map(|c| c.tpc.name()).collect();
        assert!(names.contains(&"Eb".to_string()));
        assert!(names.contains(&"Ab".to_string()));
        assert!(!names.iter().any(|n| n.contains('#')), "no sharps in a flat key: {names:?}");
        // A major: sharps.
        let a = Key::parse("A major").unwrap();
        let names: Vec<String> = palette(&a, None).classes.iter().map(|c| c.tpc.name()).collect();
        assert!(!names.iter().any(|n| n.ends_with('b')), "no flats in a sharp key: {names:?}");
    }

    #[test]
    fn nearest_safe_snaps_off_avoid_and_outside_notes() {
        let p = palette(&Key::c_major(), Some(&Chord::parse("Cmaj7").unwrap()));
        // F4 (65) is the avoid note; the nearest safe notes are E4 (64) and
        // G4 (67) — ties go up, but 66 (F#) is outside, so 64 wins by
        // distance… no: |65-64| = 1, so E4.
        assert_eq!(p.nearest_safe_midi(65), 64);
        // C#4 (61) is outside; D4 (62) is the 9th, one step up.
        assert_eq!(p.nearest_safe_midi(61), 62);
        // A safe note is returned unchanged.
        assert_eq!(p.nearest_safe_midi(60), 60);
        assert_eq!(p.nearest_safe_midi(64), 64);
    }

    #[test]
    fn chord_tone_and_safe_key_pools_are_ascending_and_in_range() {
        let p = palette(&Key::c_major(), Some(&Chord::parse("C").unwrap()));
        let tones = p.chord_tone_keys(60, 72);
        assert_eq!(tones, vec![60, 64, 67, 72]);
        let safe = p.safe_keys(60, 72);
        assert!(safe.windows(2).all(|w| w[0] < w[1]));
        assert!(safe.iter().all(|k| (60..=72).contains(k)));
        assert!(!safe.contains(&65), "F is the avoid note over a C major chord");
    }

    #[test]
    fn every_diatonic_chord_in_every_diatonic_key_leaves_at_least_three_safe_notes() {
        // A sanity net over the whole space: if the rules ever conspire to
        // leave a beginner with nothing to play, this fails.
        for tonic in -5..=6 {
            for scale in crate::theory::scale::ScaleType::ALL.into_iter().filter(|s| s.is_diatonic()) {
                let key = Key::new(Tpc(tonic), scale);
                for (_, chord) in diatonic_chords(&key, true) {
                    let p = palette(&key, Some(&chord));
                    let safe = p.classes.iter().filter(|c| c.role.is_safe()).count();
                    assert!(safe >= 3, "{} over {}: only {safe} safe notes", chord.symbol(), key.label());
                }
            }
        }
    }
}
