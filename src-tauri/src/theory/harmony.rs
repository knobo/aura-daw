//! The harmony document: `tick → key` and `tick → chord`.
//!
//! Ruling H-3: this is a session-level MAP PAIR, exactly parallel to the tempo
//! map and the meter map — which is the strongest analogy the repo already
//! has, and the reason there is no new `TrackKind` and no new lane (H-5). A
//! song has one harmonic context at a time; polytonality is out of scope and
//! says so in the product doc's non-goals.
//!
//! Positions are integer ticks at the project PPQ (debt D-02). The wire form
//! spells keys and chords as STRINGS (`"C ionian"`, `"Cmaj7"`), which keeps
//! `Tpc`'s integer encoding an implementation detail of `theory/`, makes a
//! journal line and a `project.json` readable by a human, and makes the whole
//! document diffable.

use serde::{Deserialize, Serialize};

use super::analysis::{analyze, Analysis};
use super::chord::Chord;
use super::palette::{palette, Palette};
use super::scale::Key;

/// A key change at a tick. The span runs until the next `KeySpan`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySpan {
    pub tick: u64,
    pub key: Key,
}

/// One chord, with an explicit length: gaps are legal and meaningful (a bar
/// with no chord is a bar with no harmonic claim, and the palette falls back
/// to the key there).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChordSpan {
    pub tick: u64,
    pub length_ticks: u64,
    pub chord: Chord,
}

impl ChordSpan {
    pub fn new(tick: u64, length_ticks: u64, chord: Chord) -> Self {
        Self { tick, length_ticks, chord }
    }

    pub fn end_tick(&self) -> u64 {
        self.tick + self.length_ticks
    }

    pub fn contains(&self, tick: u64) -> bool {
        tick >= self.tick && tick < self.end_tick()
    }
}

/// The document. Empty is a valid, meaningful state: no harmonic claim yet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarmonyDoc {
    #[serde(default)]
    pub keys: Vec<KeySpan>,
    #[serde(default)]
    pub chords: Vec<ChordSpan>,
}

impl HarmonyDoc {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.chords.is_empty()
    }

    /// One key for the whole song, no chords yet — what "set the key" writes
    /// before anything else exists.
    pub fn in_key(key: Key) -> Self {
        Self { keys: vec![KeySpan { tick: 0, key }], chords: Vec::new() }
    }

    /// A run of equal-length chords from `start`, in one key.
    pub fn from_chords(key: Key, start: u64, each_ticks: u64, chords: &[Chord]) -> Self {
        Self {
            keys: vec![KeySpan { tick: 0, key }],
            chords: chords
                .iter()
                .enumerate()
                .map(|(i, c)| ChordSpan::new(start + i as u64 * each_ticks, each_ticks, *c))
                .collect(),
        }
    }

    /// The key in force at `tick`. Defaults to C major for an empty document
    /// rather than returning an `Option`: every consumer needs *a* key, and
    /// "no key chosen yet" behaves like C major everywhere in the app.
    pub fn key_at(&self, tick: u64) -> Key {
        self.keys
            .iter()
            .rev()
            .find(|k| k.tick <= tick)
            .or(self.keys.first())
            .map(|k| k.key)
            .unwrap_or_else(Key::c_major)
    }

    /// The chord sounding at `tick`, if any.
    pub fn chord_at(&self, tick: u64) -> Option<&ChordSpan> {
        self.chords.iter().find(|c| c.contains(tick))
    }

    /// The chord at `tick`, or the nearest one starting after it — what a
    /// panel's "current chord" readout wants while the playhead sits in a gap
    /// before the progression starts.
    pub fn chord_at_or_next(&self, tick: u64) -> Option<&ChordSpan> {
        self.chord_at(tick).or_else(|| self.chords.iter().find(|c| c.tick >= tick))
    }

    /// The palette at `tick` — the single call the piano roll and the "no wrong
    /// notes" seam both go through.
    pub fn palette_at(&self, tick: u64) -> Palette {
        let key = self.key_at(tick);
        palette(&key, self.chord_at(tick).map(|s| &s.chord))
    }

    /// Functional analysis of the chord at `tick`, in the key at `tick`.
    pub fn analysis_at(&self, tick: u64) -> Option<Analysis> {
        let key = self.key_at(tick);
        self.chord_at(tick).map(|s| analyze(&s.chord, &key))
    }

    /// One past the last tick this document says anything about.
    pub fn end_tick(&self) -> u64 {
        self.chords.iter().map(|c| c.end_tick()).max().unwrap_or(0)
    }

    /// Sorted, non-overlapping, no zero lengths, at most one key span per tick,
    /// and a key at tick 0 whenever anything is present.
    ///
    /// Called from the op's apply arm BEFORE any mutation (round-2 §4: a failed
    /// apply must leave the session untouched), which is why it returns a
    /// human-readable reason rather than a bool.
    pub fn validate(&self) -> Result<(), String> {
        if !self.keys.windows(2).all(|w| w[0].tick < w[1].tick) {
            return Err("harmony: key spans must be sorted by tick and distinct".into());
        }
        if !self.keys.is_empty() && self.keys[0].tick != 0 {
            return Err("harmony: the first key span must be at tick 0".into());
        }
        if self.keys.is_empty() && !self.chords.is_empty() {
            return Err("harmony: chords without a key span".into());
        }
        for c in &self.chords {
            if c.length_ticks == 0 {
                return Err(format!("harmony: chord at tick {} has zero length", c.tick));
            }
        }
        for w in self.chords.windows(2) {
            if w[1].tick < w[0].end_tick() {
                return Err(format!(
                    "harmony: chord at tick {} overlaps the one at tick {} (ends at {})",
                    w[1].tick, w[0].tick, w[0].end_tick()
                ));
            }
        }
        Ok(())
    }

    /// Sort and repair a document the caller assembled loosely: order both
    /// lists, drop zero-length chords, truncate overlaps, and guarantee a key
    /// at tick 0. Used by the commands so a UI gesture never has to be
    /// careful about ordering — but NOT by the op, which validates instead
    /// (silently repairing document state would hide bugs).
    pub fn normalized(mut self) -> Self {
        self.keys.sort_by_key(|k| k.tick);
        self.keys.dedup_by_key(|k| k.tick);
        if self.keys.is_empty() {
            if self.chords.is_empty() {
                return self;
            }
            self.keys.push(KeySpan { tick: 0, key: Key::c_major() });
        } else if self.keys[0].tick != 0 {
            let first = self.keys[0].key;
            self.keys.insert(0, KeySpan { tick: 0, key: first });
        }
        self.chords.sort_by_key(|c| c.tick);
        self.chords.retain(|c| c.length_ticks > 0);
        for i in 0..self.chords.len().saturating_sub(1) {
            let next = self.chords[i + 1].tick;
            if self.chords[i].end_tick() > next {
                self.chords[i].length_ticks = next.saturating_sub(self.chords[i].tick);
            }
        }
        self.chords.retain(|c| c.length_ticks > 0);
        self
    }

    /// Append a chord after the last one (or at `default_start` when empty).
    /// The circle widget's click lands here.
    pub fn appended(mut self, chord: Chord, length_ticks: u64, default_start: u64) -> Self {
        let tick = self.chords.last().map(|c| c.end_tick()).unwrap_or(default_start);
        self.chords.push(ChordSpan::new(tick, length_ticks.max(1), chord));
        self
    }

    /// `"C | Am | F | G7"` — for logs, test assertions and the journal's eyes.
    pub fn progression_string(&self) -> String {
        self.chords
            .iter()
            .map(|c| c.chord.symbol())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

// ---------------------------------------------------------------------------
// String wire forms for the theory types that ride in the document
// ---------------------------------------------------------------------------
//
// WHY hand-written rather than derived: the document is a PERSISTED format
// (project.json and, through `Op::HarmonySet`, journal.ndjson). Serialising
// `Tpc(-2)` as an integer would make every future spelling change a migration
// and every journal line unreadable; `"Bb"` is stable, diffable and
// human-checkable. Parse failures are real errors — a corrupt symbol is not
// silently rounded to something playable.

impl Serialize for super::tpc::Tpc {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

impl<'de> Deserialize<'de> for super::tpc::Tpc {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        super::tpc::Tpc::parse(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("bad note name {s:?}")))
    }
}

impl Serialize for Key {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.canonical())
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Key::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Chord {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.symbol())
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Chord::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::palette::NoteRole;
    use crate::theory::scale::ScaleType;
    use crate::theory::tpc::Tpc;

    fn doc() -> HarmonyDoc {
        // | C | Am | F | G7 | at one bar each, ppq 960, 4/4.
        HarmonyDoc::from_chords(
            Key::c_major(),
            0,
            3840,
            &["C", "Am", "F", "G7"].map(|s| Chord::parse(s).unwrap()),
        )
    }

    #[test]
    fn the_wire_form_is_readable_and_round_trips() {
        let d = doc();
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["keys"][0]["key"], "C ionian");
        assert_eq!(v["chords"][3]["chord"], "G7");
        assert_eq!(v["chords"][3]["tick"], 11520);
        assert_eq!(v["chords"][3]["lengthTicks"], 3840);
        let back: HarmonyDoc = serde_json::from_value(v).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn spellings_survive_the_wire() {
        // The point of string wire forms: an enharmonic distinction must not
        // be lost or normalised in a persisted format.
        let d = HarmonyDoc::from_chords(
            Key::parse("Gb major").unwrap(),
            0,
            960,
            &[Chord::parse("Gbmaj7").unwrap(), Chord::parse("Cbmaj7").unwrap()],
        );
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("Gb ionian"), "{json}");
        assert!(json.contains("Cbmaj7"), "{json}");
        let back: HarmonyDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(back.chords[1].chord.root.name(), "Cb", "not B");
        assert_eq!(back, d);
    }

    #[test]
    fn a_bad_symbol_is_an_error_not_a_guess() {
        let bad = r#"{"keys":[{"tick":0,"key":"C ionian"}],"chords":[{"tick":0,"lengthTicks":960,"chord":"Cmaj77"}]}"#;
        let err = serde_json::from_str::<HarmonyDoc>(bad).unwrap_err().to_string();
        assert!(err.contains("Cmaj77"), "{err}");
        let bad_key = r#"{"keys":[{"tick":0,"key":"H klezmer"}]}"#;
        assert!(serde_json::from_str::<HarmonyDoc>(bad_key).is_err());
    }

    #[test]
    fn lookups_find_the_span_in_force() {
        let d = doc();
        assert_eq!(d.chord_at(0).unwrap().chord.symbol(), "C");
        assert_eq!(d.chord_at(3839).unwrap().chord.symbol(), "C", "end is exclusive");
        assert_eq!(d.chord_at(3840).unwrap().chord.symbol(), "Am");
        assert_eq!(d.chord_at(11520).unwrap().chord.symbol(), "G7");
        assert!(d.chord_at(15360).is_none(), "past the end");
        assert_eq!(d.end_tick(), 15360);
        assert_eq!(d.progression_string(), "C | Am | F | G7");
    }

    #[test]
    fn key_changes_take_effect_at_their_tick_and_default_to_c_major() {
        let mut d = doc();
        d.keys.push(KeySpan { tick: 7680, key: Key::new(Tpc::A, ScaleType::Aeolian) });
        assert_eq!(d.key_at(0).canonical(), "C ionian");
        assert_eq!(d.key_at(7679).canonical(), "C ionian");
        assert_eq!(d.key_at(7680).canonical(), "A aeolian", "at the tick, not after it");
        assert_eq!(d.key_at(999_999).canonical(), "A aeolian");
        assert_eq!(HarmonyDoc::default().key_at(0).canonical(), "C ionian", "empty behaves as C major");
    }

    #[test]
    fn the_palette_follows_the_chord_and_falls_back_to_the_key_in_a_gap() {
        let mut d = doc();
        // Over C: F is the avoid note. Over F (bar 3): B is available.
        assert_eq!(d.palette_at(0).class_of_pc(5).role, NoteRole::Avoid);
        assert_eq!(d.palette_at(7680).class_of_pc(11).role, NoteRole::Extension);
        // Drop bar 2 to make a gap: no chord means no avoid notes at all.
        d.chords.remove(1);
        let gap = d.palette_at(3840 + 100);
        assert!(gap.chord.is_none());
        assert!(gap.classes.iter().all(|c| c.role != NoteRole::Avoid));
        assert_eq!(gap.class_of_pc(5).role, NoteRole::Scale, "F is just a scale note now");
    }

    #[test]
    fn analysis_reads_the_chord_in_the_key_at_that_tick() {
        let mut d = doc();
        assert_eq!(d.analysis_at(11520).unwrap().roman, "V7", "G7 in C major");
        assert_eq!(d.analysis_at(3840).unwrap().roman, "vi", "Am in C major");
        // The SAME chord under a key change is a different chord functionally
        // — which is the whole reason key spans exist and are not a global
        // setting.
        d.keys.push(KeySpan { tick: 3840, key: Key::new(Tpc::A, ScaleType::Aeolian) });
        assert_eq!(d.analysis_at(3840).unwrap().roman, "i", "…and home in A minor");
        assert_eq!(d.analysis_at(11520).unwrap().roman, "V7/III", "G7 in A minor pulls to C");
        assert!(d.analysis_at(15360).is_none());
    }

    #[test]
    fn validate_rejects_exactly_the_states_that_would_corrupt_a_lookup() {
        assert!(HarmonyDoc::default().validate().is_ok(), "empty is valid");
        assert!(doc().validate().is_ok());

        let mut unsorted = doc();
        unsorted.chords.swap(0, 1);
        assert!(unsorted.validate().unwrap_err().contains("overlaps"));

        let mut overlapping = doc();
        overlapping.chords[0].length_ticks = 4000;
        assert!(overlapping.validate().unwrap_err().contains("overlaps"));

        let mut zero = doc();
        zero.chords[2].length_ticks = 0;
        assert!(zero.validate().unwrap_err().contains("zero length"));

        let mut late_key = doc();
        late_key.keys[0].tick = 480;
        assert!(late_key.validate().unwrap_err().contains("tick 0"));

        let mut dup_key = doc();
        dup_key.keys.push(KeySpan { tick: 0, key: Key::c_major() });
        assert!(dup_key.validate().unwrap_err().contains("distinct"));

        let orphan = HarmonyDoc { keys: vec![], chords: doc().chords };
        assert!(orphan.validate().unwrap_err().contains("without a key"));
    }

    #[test]
    fn normalized_repairs_a_loosely_assembled_document() {
        let d = HarmonyDoc {
            keys: vec![KeySpan { tick: 1920, key: Key::c_major() }],
            chords: vec![
                ChordSpan::new(3840, 3840, Chord::parse("Am").unwrap()),
                ChordSpan::new(0, 5000, Chord::parse("C").unwrap()), // overlaps
                ChordSpan::new(7680, 0, Chord::parse("F").unwrap()), // zero length
            ],
        }
        .normalized();
        d.validate().expect("normalized output is always valid");
        assert_eq!(d.keys[0].tick, 0, "a key is guaranteed at tick 0");
        assert_eq!(d.keys.len(), 2);
        assert_eq!(d.progression_string(), "C | Am");
        assert_eq!(d.chords[0].length_ticks, 3840, "the overlap was truncated, not dropped");
    }

    #[test]
    fn append_lands_after_the_last_chord() {
        let d = HarmonyDoc::in_key(Key::c_major())
            .appended(Chord::parse("C").unwrap(), 1920, 0)
            .appended(Chord::parse("G7").unwrap(), 1920, 0);
        assert_eq!(d.progression_string(), "C | G7");
        assert_eq!(d.chords[1].tick, 1920);
        d.validate().unwrap();
        // Into an empty document, `default_start` decides where it lands.
        let at_bar_two = HarmonyDoc::in_key(Key::c_major())
            .appended(Chord::parse("F").unwrap(), 1920, 3840);
        assert_eq!(at_bar_two.chords[0].tick, 3840);
    }
}
