//! The Composer's stateful seam (Plan H1 Task 6).
//!
//! `theory/` is pure; this file is the ONE place where it meets `Session`, ops
//! and commands. Five additive commands:
//!
//! | Command | Does |
//! |---|---|
//! | `harmony_get` | the harmony document + the circle, as the panel draws it |
//! | `harmony_set` | replace the document — batch-shaped, ONE op, one undo |
//! | `composer_palette` | which notes may I play at this tick, and why |
//! | `composer_suggest` | ranked next chords, each with its reasoning |
//! | `composer_generate` | write parts as ordinary MIDI clips, in ONE transaction |
//!
//! Two disciplines matter here and are load-bearing:
//!
//! * **Generation happens OUTSIDE the transaction** (the `hum.rs`
//!   prepare-outside pattern). Voice leading is a DP over the whole
//!   progression; running it under the session lock would hold the lock across
//!   work that has nothing to do with the document.
//! * **One transaction for the whole idea** (ruling H-7). Chords, bass, melody
//!   and drums land together, so one Ctrl+Z removes the sketch rather than one
//!   quarter of it, and a failure applies nothing.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::midi::{MidiClip, MidiNote};
use crate::theory::{
    self, analysis, bass, circle, groove, melody, progression, voicing, Annotation, Chord,
    ChordSpan, Grid, HarmonyDoc, Key, KeySpan, Meter,
};

use super::op::{Op, TxMeta};
use super::ops;
use super::session::Tx;
use super::ControlPlane;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// What the panel needs to draw itself: the document, the circle of fifths for
/// the current key, the per-chord analysis, and the static menus (schemas,
/// genres, styles). All backend-derived — the widget contains no theory.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarmonyView {
    pub harmony: HarmonyDoc,
    /// The key in force at `atTicks` (the panel's cursor), as a label and in
    /// canonical form.
    pub key: String,
    pub key_label: String,
    pub key_signature: i16,
    /// Twelve wedges in clockwise order, spelled for this key.
    pub wedges: Vec<circle::Wedge>,
    /// One row per chord region: the numeral, the function, and the why.
    pub spans: Vec<SpanView>,
    /// Keys one easy move away, each with the reason it is easy.
    pub neighbours: Vec<NeighbourView>,
    /// Chords this key can borrow, with why.
    pub borrowed: Vec<BorrowedView>,
    pub schemas: Vec<progression::Schema>,
    pub genres: Vec<&'static str>,
    pub voicing_styles: Vec<&'static str>,
    pub bass_styles: Vec<&'static str>,
    pub ppq: u32,
    pub ticks_per_bar: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanView {
    pub tick: u64,
    pub length_ticks: u64,
    pub symbol: String,
    pub pretty: String,
    pub roman: String,
    pub function: &'static str,
    pub borrowed: bool,
    pub why: String,
    /// Spelled chord tones, for the panel's chord readout.
    pub tones: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeighbourView {
    pub key: String,
    pub label: String,
    pub steps: i16,
    pub why: String,
    /// Chords common to both keys — the pivots that make the move smooth.
    pub pivots: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BorrowedView {
    pub symbol: String,
    pub pretty: String,
    pub roman: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteView {
    pub tick: u64,
    pub key: String,
    pub key_label: String,
    pub chord: Option<String>,
    pub chord_pretty: Option<String>,
    pub roman: Option<String>,
    pub classes: Vec<theory::palette::NoteClass>,
}

/// Argument of `composer_generate`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateRequest {
    /// Any of `"chords"`, `"bass"`, `"melody"`, `"drums"`. Empty = all four.
    #[serde(default)]
    pub parts: Vec<String>,
    /// `"C major"`, `"f# minor"`, `"D dorian"`. None = the document's key.
    pub key: Option<String>,
    /// A progression plan (`"axis"`, `"circle"`, `"functional:70"`). None =
    /// keep the harmony document as it is and write parts against it.
    pub plan: Option<String>,
    pub bars: Option<u32>,
    /// Where the sketch starts, in ticks. None = 0.
    pub at_ticks: Option<u64>,
    /// None = derived from the request so a repeat call is still reproducible.
    pub seed: Option<u64>,
    pub genre: Option<String>,
    pub voicing: Option<String>,
    /// `"block"` or `"arpeggio"`.
    pub rhythm: Option<String>,
    pub bass_style: Option<String>,
    /// 0..100, melody notes per bar / kick density.
    pub density: Option<u8>,
    pub syncopation: Option<u8>,
    /// Reuse a track per part instead of creating one: `{"melody": "<trackId>"}`.
    #[serde(default)]
    pub track_ids: BTreeMap<String, String>,
    /// Write the generated progression into the harmony document. Defaults to
    /// true when `plan` is given (the progression IS the point), false
    /// otherwise (the caller is arranging an existing harmony).
    pub write_harmony: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedClip {
    pub part: String,
    pub clip_id: String,
    pub track_id: String,
    pub track_name: String,
    /// True when this call created the track.
    pub created_track: bool,
    pub note_count: usize,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateReply {
    pub clips: Vec<GeneratedClip>,
    pub harmony: HarmonyView,
    /// The progression that was generated, when a plan was given.
    pub progression: Option<ProgressionView>,
    /// The seed that produced this. Stored by the panel so "I liked take 3" is
    /// recoverable (ruling H-4).
    pub seed: u64,
    pub bars: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionView {
    pub key: String,
    pub key_label: String,
    pub why: String,
    pub slots: Vec<progression::Slot>,
}

// ---------------------------------------------------------------------------
// Pure view assembly (no lock, no I/O — testable on a bare document)
// ---------------------------------------------------------------------------

/// Build the panel's view of a harmony document. Pure: takes the document, not
/// the session.
pub fn harmony_view(harmony: &HarmonyDoc, at_ticks: u64, ppq: u32, meter: Meter) -> HarmonyView {
    let key = harmony.key_at(at_ticks);
    let spans = harmony
        .chords
        .iter()
        .map(|s| {
            let k = harmony.key_at(s.tick);
            let a = analysis::analyze(&s.chord, &k);
            SpanView {
                tick: s.tick,
                length_ticks: s.length_ticks,
                symbol: s.chord.symbol(),
                pretty: s.chord.pretty(),
                roman: a.roman,
                function: a.function.id(),
                borrowed: a.borrowed,
                why: a.why,
                tones: s.chord.tones().iter().map(|t| t.pretty()).collect(),
            }
        })
        .collect();
    let neighbours = circle::neighbours(&key)
        .into_iter()
        .map(|(k, why)| NeighbourView {
            key: k.canonical(),
            label: k.label(),
            steps: circle::distance(&key, &k),
            why,
            pivots: circle::pivot_chords(&key, &k)
                .into_iter()
                .take(3)
                .map(|(c, _)| c.pretty())
                .collect(),
        })
        .collect();
    let borrowed = circle::borrowed_chords(&key)
        .into_iter()
        .map(|(c, why)| BorrowedView {
            symbol: c.symbol(),
            pretty: c.pretty(),
            roman: analysis::analyze(&c, &key).roman,
            why,
        })
        .collect();
    HarmonyView {
        harmony: harmony.clone(),
        key: key.canonical(),
        key_label: key.label(),
        key_signature: key.key_signature_fifths(),
        wedges: circle::wedges(&key),
        spans,
        neighbours,
        borrowed,
        schemas: progression::SCHEMAS.to_vec(),
        genres: groove::Genre::ALL.iter().map(|g| g.id()).collect(),
        voicing_styles: vec!["close", "drop2", "shell", "spread"],
        bass_styles: vec!["root", "rootFifth", "walking", "pedal"],
        ppq,
        ticks_per_bar: Grid::new(meter, ppq, 4).ticks_per_bar(),
    }
}

/// The palette at a tick, as the piano roll tints with it.
pub fn palette_view(harmony: &HarmonyDoc, tick: u64) -> PaletteView {
    let key = harmony.key_at(tick);
    let pal = harmony.palette_at(tick);
    let span = harmony.chord_at(tick);
    PaletteView {
        tick,
        key: key.canonical(),
        key_label: key.label(),
        chord: span.map(|s| s.chord.symbol()),
        chord_pretty: span.map(|s| s.chord.pretty()),
        roman: span.map(|s| analysis::analyze(&s.chord, &key).roman),
        classes: pal.classes,
    }
}

/// One generated part, before it becomes a clip.
#[derive(Debug)]
struct Part {
    name: &'static str,
    track_name: String,
    notes: Vec<MidiNote>,
    annotations: Vec<Annotation>,
}

/// Everything a generate request produces, computed with NO lock held.
#[derive(Debug)]
struct Prepared {
    harmony: HarmonyDoc,
    write_harmony: bool,
    progression: Option<ProgressionView>,
    parts: Vec<Part>,
    seed: u64,
    bars: u32,
    at_ticks: u64,
    length_ticks: u64,
}

/// Derive a seed from the request when the caller did not pick one, so that
/// "generate again with the same settings" is reproducible while "generate
/// again" (a fresh call with a fresh seed from the UI) is not. Hashing the
/// request rather than reading a clock keeps `theory/` honest about ruling H-4.
fn derived_seed(req: &GenerateRequest) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(req.plan.as_deref().unwrap_or("").as_bytes());
    mix(req.key.as_deref().unwrap_or("").as_bytes());
    mix(req.genre.as_deref().unwrap_or("").as_bytes());
    mix(&req.bars.unwrap_or(8).to_le_bytes());
    mix(&req.at_ticks.unwrap_or(0).to_le_bytes());
    h
}

/// The pure half of `composer_generate`: validate, generate, and lay everything
/// out — no session, no lock, no I/O. Unit-tested directly.
fn prepare(
    req: &GenerateRequest,
    current: &HarmonyDoc,
    ppq: u32,
    meter: Meter,
) -> Result<Prepared, String> {
    let bars = req.bars.unwrap_or(8).clamp(1, 256);
    let at_ticks = req.at_ticks.unwrap_or(0);
    let seed = req.seed.unwrap_or_else(|| derived_seed(req));
    let grid = Grid::new(meter, ppq, 4);
    let bar_ticks = grid.ticks_per_bar();

    let requested_key = match &req.key {
        Some(s) => Some(Key::parse(s)?),
        None => None,
    };
    let base_key = requested_key.unwrap_or_else(|| current.key_at(at_ticks));

    // ── the harmony ──
    let (harmony, prog_view, write_harmony) = match &req.plan {
        Some(plan_str) => {
            let plan = progression::Plan::parse(plan_str)?;
            let prog = progression::generate(&base_key, &plan, bars as usize, seed)?;
            let mut chords: Vec<ChordSpan> = Vec::with_capacity(prog.slots.len());
            let mut tick = at_ticks;
            for slot in &prog.slots {
                let len = slot.bars.max(1) as u64 * bar_ticks;
                chords.push(ChordSpan::new(tick, len, slot.chord));
                tick += len;
            }
            // Splice into whatever is already there: spans this sketch covers
            // are replaced, spans outside it are untouched. A generate call is
            // an edit, not a reset.
            let end = tick;
            let mut keys: Vec<KeySpan> = current
                .keys
                .iter()
                .copied()
                .filter(|k| k.tick < at_ticks || k.tick >= end)
                .collect();
            keys.push(KeySpan { tick: at_ticks, key: prog.key });
            let mut kept: Vec<ChordSpan> = current
                .chords
                .iter()
                .copied()
                .filter(|c| c.end_tick() <= at_ticks || c.tick >= end)
                .collect();
            kept.extend(chords);
            let doc = HarmonyDoc { keys, chords: kept }.normalized();
            let view = ProgressionView {
                key: prog.key.canonical(),
                key_label: prog.key.label(),
                why: prog.why.clone(),
                slots: prog.slots.clone(),
            };
            (doc, Some(view), req.write_harmony.unwrap_or(true))
        }
        None => {
            let mut doc = current.clone();
            if let Some(k) = requested_key {
                doc.keys = vec![KeySpan { tick: 0, key: k }];
                doc = doc.normalized();
            }
            let write = req.write_harmony.unwrap_or(requested_key.is_some());
            (doc, None, write)
        }
    };
    harmony.validate()?;

    // How long the sketch actually is: the harmony decides, not the request,
    // so a truncated schema does not leave four empty bars of drums.
    let covered_end = harmony
        .chords
        .iter()
        .filter(|c| c.tick >= at_ticks)
        .map(|c| c.end_tick())
        .max()
        .unwrap_or(at_ticks + bars as u64 * bar_ticks);
    let length_ticks = covered_end.saturating_sub(at_ticks).max(bar_ticks);
    let effective_bars = ((length_ticks + bar_ticks - 1) / bar_ticks).max(1) as u32;

    // ── the parts ──
    let wanted: Vec<String> = if req.parts.is_empty() {
        vec!["chords".into(), "bass".into(), "melody".into(), "drums".into()]
    } else {
        req.parts.clone()
    };
    let mut parts: Vec<Part> = Vec::new();
    for name in &wanted {
        match name.as_str() {
            "chords" => {
                let opts = voicing::VoicingOptions {
                    style: req
                        .voicing
                        .as_deref()
                        .map(|s| {
                            voicing::VoicingStyle::parse(s)
                                .ok_or_else(|| format!("unknown voicing style {s:?}"))
                        })
                        .transpose()?
                        .unwrap_or(voicing::VoicingStyle::Close),
                    ..Default::default()
                };
                let rhythm = match req.rhythm.as_deref() {
                    None | Some("block") => voicing::ChordRhythm::Block,
                    Some("arpeggio") | Some("arp") => voicing::ChordRhythm::Arpeggio,
                    Some(other) => return Err(format!("unknown chord rhythm {other:?}")),
                };
                let (notes, annotations) =
                    voicing::chord_notes(&harmony, &grid, at_ticks, &opts, rhythm);
                parts.push(Part { name: "chords", track_name: "Composer Chords".into(), notes, annotations });
            }
            "bass" => {
                let style = req
                    .bass_style
                    .as_deref()
                    .map(|s| {
                        bass::BassStyle::parse(s).ok_or_else(|| format!("unknown bass style {s:?}"))
                    })
                    .transpose()?
                    .unwrap_or(bass::BassStyle::Root);
                let (notes, annotations) = bass::bass_notes(
                    &harmony,
                    &grid,
                    at_ticks,
                    &bass::BassOptions { style, ..Default::default() },
                );
                parts.push(Part { name: "bass", track_name: "Composer Bass".into(), notes, annotations });
            }
            "melody" => {
                let opts = melody::MelodyOptions {
                    density: req.density.unwrap_or(45),
                    syncopation: req.syncopation.unwrap_or(20),
                    seed,
                    ..Default::default()
                };
                let (notes, annotations) =
                    melody::melody_notes(&harmony, &grid, at_ticks, effective_bars as usize, &opts);
                parts.push(Part { name: "melody", track_name: "Composer Melody".into(), notes, annotations });
            }
            "drums" => {
                let genre = req
                    .genre
                    .as_deref()
                    .map(|g| groove::Genre::parse(g).ok_or_else(|| format!("unknown genre {g:?}")))
                    .transpose()?
                    .unwrap_or(groove::Genre::Rock);
                let opts = groove::GrooveOptions {
                    genre,
                    density: req.density,
                    syncopation: req.syncopation,
                    seed,
                    ..Default::default()
                };
                let (mut notes, mut annotations) =
                    groove::groove_notes(meter, ppq, at_ticks, effective_bars as usize, &opts);
                groove::to_clip_relative(&mut notes, at_ticks);
                // Honest about the one thing a generated drum clip cannot do
                // for itself: there is no bundled kit to point the track at.
                annotations.push(Annotation::new(
                    0,
                    bar_ticks,
                    "General MIDI",
                    "These are GM drum keys on channel 10 (kick 36, snare 38, hats 42/46). Point \
                     the track at a drum kit — a sampler instrument or a plugin — to hear it as \
                     drums rather than as pitches."
                        .to_string(),
                ));
                parts.push(Part { name: "drums", track_name: "Composer Drums".into(), notes, annotations });
            }
            other => return Err(format!("unknown part {other:?} (chords|bass|melody|drums)")),
        }
    }
    // Refuse politely rather than half-succeeding. The melody generator can
    // work from a key alone (a scale is enough for a tune), so "did any part
    // produce notes" is NOT the right question — a request for chords over an
    // empty harmony would silently come back as a melody and nothing else.
    if harmony.chords.iter().all(|c| c.end_tick() <= at_ticks) {
        return Err(
            "nothing to write: the harmony has no chords from here on — pick a plan, or set a \
             progression first"
                .into(),
        );
    }
    if parts.iter().all(|p| p.notes.is_empty()) {
        return Err("nothing to write: every requested part came back empty".into());
    }
    for p in &parts {
        for n in &p.notes {
            n.validate()?;
        }
    }
    Ok(Prepared {
        harmony,
        write_harmony,
        progression: prog_view,
        parts,
        seed,
        bars: effective_bars,
        at_ticks,
        length_ticks,
    })
}

/// The document-mutation half: ONE transaction for the whole sketch (H-7).
fn commit_sketch(
    tx: &mut Tx<'_>,
    prepared: &Prepared,
    track_ids: &BTreeMap<String, String>,
) -> Result<Vec<GeneratedClip>, String> {
    if prepared.write_harmony {
        tx.apply(Op::HarmonySet {
            keys: prepared.harmony.keys.clone(),
            chords: prepared.harmony.chords.clone(),
        })?;
    }
    let mut out = Vec::with_capacity(prepared.parts.len());
    for part in &prepared.parts {
        if part.notes.is_empty() {
            continue;
        }
        // Resolve or create the target track, inside the same transaction
        // (ruling H-8, the `hum.rs` precedent).
        let (track_id, track_name, created) = match track_ids.get(part.name) {
            Some(id) => {
                let track = tx
                    .store()
                    .tracks
                    .iter()
                    .find(|t| t.id == *id)
                    .ok_or_else(|| format!("unknown track: {id}"))?;
                if track.kind != "midi" {
                    return Err(format!(
                        "track {id} is kind \"{}\" — generated parts need a midi track",
                        track.kind
                    ));
                }
                (crate::ids::TrackId::from(id.clone()), track.name.clone(), false)
            }
            None => {
                let track = ops::add_track_tx(tx, Some(part.track_name.clone()), Some("midi".into()))?;
                (track.id.clone(), track.name.clone(), true)
            }
        };
        let lane_id = crate::ids::LaneId::default_for_track(track_id.as_str());
        let mut clip = MidiClip {
            id: crate::ids::ClipId::mint(),
            track_id: track_id.clone(),
            name: part.track_name.clone(),
            timeline_start_ticks: prepared.at_ticks,
            length_ticks: prepared.length_ticks,
            notes: part.notes.clone(),
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id,
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        };
        clip.ensure_note_ids()?;
        let index = tx.midi().clips.len();
        tx.apply(Op::MidiClipAdd { clip: clip.clone(), index })?;
        out.push(GeneratedClip {
            part: part.name.to_string(),
            clip_id: clip.id.to_string(),
            track_id: track_id.to_string(),
            track_name,
            created_track: created,
            note_count: part.notes.len(),
            annotations: part.annotations.clone(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// ControlPlane front doors
// ---------------------------------------------------------------------------

impl ControlPlane {
    fn harmony_context(&self) -> (HarmonyDoc, u32, Meter) {
        let session = self.session.lock();
        let m = session.midi.meter_events.first().copied();
        (
            session.midi.harmony.clone(),
            session.midi.ppq,
            m.map(|e| Meter::new(e.num, e.den)).unwrap_or(Meter::FOUR_FOUR),
        )
    }

    pub fn harmony_view(&self, at_ticks: u64) -> HarmonyView {
        let (harmony, ppq, meter) = self.harmony_context();
        harmony_view(&harmony, at_ticks, ppq, meter)
    }

    /// Replace the harmony document — one op, one undo entry (ruling H-4).
    /// The incoming document is NORMALISED first (sorted, overlaps truncated),
    /// so a UI gesture never has to be careful about ordering; the op itself
    /// still validates, because silently repairing document state inside the
    /// mutation channel would hide bugs.
    pub fn harmony_set(
        &self,
        keys: Vec<KeySpan>,
        chords: Vec<ChordSpan>,
        at_ticks: u64,
        meta: TxMeta,
    ) -> Result<HarmonyView, String> {
        let doc = HarmonyDoc { keys, chords }.normalized();
        doc.validate()?;
        self.commit(meta, |tx| {
            tx.apply(Op::HarmonySet {
                keys: doc.keys.clone(),
                chords: doc.chords.clone(),
            })?;
            Ok(())
        })?;
        Ok(self.harmony_view(at_ticks))
    }

    pub fn composer_palette(&self, tick: u64) -> PaletteView {
        let (harmony, _, _) = self.harmony_context();
        palette_view(&harmony, tick)
    }

    pub fn composer_suggest(&self, at_ticks: u64, limit: usize) -> Vec<progression::Suggestion> {
        let (harmony, _, _) = self.harmony_context();
        let key = harmony.key_at(at_ticks);
        // The chords already placed BEFORE this point are the context; the
        // suggestion is what comes next, not what could replace what is there.
        let so_far: Vec<Chord> = harmony
            .chords
            .iter()
            .filter(|c| c.tick < at_ticks)
            .map(|c| c.chord)
            .collect();
        progression::suggest_next(&key, &so_far, limit.clamp(1, 24))
    }

    /// Generate a sketch. Everything is computed with no lock held; the
    /// mutation is ONE transaction (H-7).
    pub fn composer_generate(
        &self,
        req: GenerateRequest,
        meta: TxMeta,
    ) -> Result<GenerateReply, String> {
        let (current, ppq, meter) = self.harmony_context();
        let prepared = prepare(&req, &current, ppq, meter)?;
        let mut clips = Vec::new();
        self.commit(meta, |tx| {
            clips = commit_sketch(tx, &prepared, &req.track_ids)?;
            Ok(())
        })?;
        log::info!(
            "composer_generate: {} clip(s), seed {}, {} bars at tick {}",
            clips.len(),
            prepared.seed,
            prepared.bars,
            prepared.at_ticks
        );
        Ok(GenerateReply {
            clips,
            harmony: self.harmony_view(prepared.at_ticks),
            progression: prepared.progression,
            seed: prepared.seed,
            bars: prepared.bars,
        })
    }
}

// ---------------------------------------------------------------------------
// Commands (additive names, registered in lib.rs)
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn harmony_get(
    at_ticks: Option<u64>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<HarmonyView, String> {
    Ok(control.harmony_view(at_ticks.unwrap_or(0)))
}

/// Batch-shaped (D-03): one invoke carries the document's full span sets, which
/// is one op and one undo step for a whole editing gesture.
#[tauri::command]
pub fn harmony_set(
    keys: Vec<KeySpan>,
    chords: Vec<ChordSpan>,
    at_ticks: Option<u64>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<HarmonyView, String> {
    control.harmony_set(keys, chords, at_ticks.unwrap_or(0), TxMeta::user("harmony set"))
}

#[tauri::command]
pub fn composer_palette(
    tick: u64,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<PaletteView, String> {
    Ok(control.composer_palette(tick))
}

#[tauri::command]
pub fn composer_suggest(
    at_ticks: Option<u64>,
    limit: Option<usize>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<Vec<progression::Suggestion>, String> {
    Ok(control.composer_suggest(at_ticks.unwrap_or(0), limit.unwrap_or(6)))
}

#[tauri::command]
pub fn composer_generate(
    request: GenerateRequest,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<GenerateReply, String> {
    control.composer_generate(request, TxMeta::user("composer generate"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::ScaleType;

    fn doc() -> HarmonyDoc {
        HarmonyDoc::from_chords(
            Key::c_major(),
            0,
            3840,
            &["C", "Am", "F", "G7"].map(|s| Chord::parse(s).unwrap()),
        )
    }

    #[test]
    fn the_view_ships_everything_the_widget_needs_and_no_theory() {
        let v = harmony_view(&doc(), 0, 960, Meter::FOUR_FOUR);
        assert_eq!(v.key, "C ionian");
        assert_eq!(v.key_label, "C major");
        assert_eq!(v.key_signature, 0);
        assert_eq!(v.wedges.len(), 12);
        assert_eq!(v.wedges.iter().filter(|w| w.in_key).count(), 7);
        assert_eq!(v.spans.len(), 4);
        assert_eq!(v.spans[3].roman, "V7");
        assert_eq!(v.spans[3].function, "dominant");
        assert_eq!(v.spans[3].tones, ["G", "B", "D", "F"], "the ♭7 of G is F");
        assert_eq!(v.neighbours.len(), 4);
        assert!(v.neighbours.iter().any(|n| n.key == "G ionian" && n.steps == 1));
        assert!(!v.neighbours[0].pivots.is_empty(), "a neighbour offers pivots");
        assert!(!v.borrowed.is_empty());
        assert_eq!(v.borrowed[0].roman, "♭VII");
        assert_eq!(v.ticks_per_bar, 3840);
        assert!(!v.schemas.is_empty() && !v.genres.is_empty());
    }

    #[test]
    fn the_view_follows_a_key_change_at_the_cursor() {
        let mut d = doc();
        d.keys.push(KeySpan { tick: 7680, key: Key::new(theory::Tpc::A, ScaleType::Aeolian) });
        assert_eq!(harmony_view(&d, 0, 960, Meter::FOUR_FOUR).key_label, "C major");
        assert_eq!(harmony_view(&d, 7680, 960, Meter::FOUR_FOUR).key_label, "A minor");
    }

    #[test]
    fn the_palette_view_serialises_twelve_classes_with_their_whys() {
        let v = palette_view(&doc(), 0);
        assert_eq!(v.classes.len(), 12);
        assert_eq!(v.chord.as_deref(), Some("C"));
        assert_eq!(v.roman.as_deref(), Some("I"));
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(json["classes"][0]["role"], "root");
        assert_eq!(json["classes"][5]["role"], "avoid", "F over a C chord");
        assert!(json["classes"][5]["why"].as_str().unwrap().contains("semitone above"));
        assert_eq!(json["classes"][0]["pitchClass"], 0);
    }

    #[test]
    fn a_plan_generates_a_progression_and_splices_it_into_the_document() {
        let req = GenerateRequest {
            plan: Some("axis".into()),
            bars: Some(4),
            parts: vec!["chords".into()],
            ..Default::default()
        };
        let p = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        assert_eq!(p.harmony.progression_string(), "C | G | Am | F");
        assert!(p.write_harmony, "a plan writes the harmony by default");
        assert_eq!(p.bars, 4);
        assert_eq!(p.length_ticks, 4 * 3840);
        assert_eq!(p.parts.len(), 1);
        assert!(!p.parts[0].notes.is_empty());
        let view = p.progression.unwrap();
        assert_eq!(view.slots.len(), 4);
        assert!(view.why.contains("axis"));
    }

    #[test]
    fn generating_at_a_later_bar_keeps_the_earlier_harmony() {
        let req = GenerateRequest {
            plan: Some("ii-v-i".into()),
            bars: Some(4),
            at_ticks: Some(4 * 3840),
            parts: vec!["chords".into()],
            ..Default::default()
        };
        let p = prepare(&req, &doc(), 960, Meter::FOUR_FOUR).unwrap();
        // The first four bars are untouched; the new chords follow them.
        assert!(p.harmony.progression_string().starts_with("C | Am | F | G7 | "));
        assert_eq!(p.harmony.chord_at(0).unwrap().chord.symbol(), "C");
        assert_eq!(p.harmony.chord_at(4 * 3840).unwrap().chord.symbol(), "Dm7");
        p.harmony.validate().unwrap();
        // And the clip starts at the sketch, not at the song.
        assert_eq!(p.at_ticks, 4 * 3840);
        assert_eq!(p.parts[0].notes.iter().map(|n| n.tick).min(), Some(0));
    }

    #[test]
    fn generating_over_an_existing_range_replaces_only_that_range() {
        let req = GenerateRequest {
            plan: Some("ii-v-i".into()),
            bars: Some(2),
            at_ticks: Some(3840),
            parts: vec!["chords".into()],
            ..Default::default()
        };
        let p = prepare(&req, &doc(), 960, Meter::FOUR_FOUR).unwrap();
        p.harmony.validate().unwrap();
        assert_eq!(p.harmony.chord_at(0).unwrap().chord.symbol(), "C", "bar 1 survives");
        assert_eq!(p.harmony.chord_at(3840).unwrap().chord.symbol(), "Dm7", "bar 2 replaced");
        assert_eq!(p.harmony.chord_at(3 * 3840).unwrap().chord.symbol(), "G7", "bar 4 survives");
    }

    #[test]
    fn all_four_parts_generate_against_an_existing_harmony() {
        let req = GenerateRequest::default();
        let p = prepare(&req, &doc(), 960, Meter::FOUR_FOUR).unwrap();
        assert_eq!(p.parts.len(), 4);
        let names: Vec<&str> = p.parts.iter().map(|x| x.name).collect();
        assert_eq!(names, ["chords", "bass", "melody", "drums"]);
        assert!(p.parts.iter().all(|x| !x.notes.is_empty()), "every part writes notes");
        assert!(p.parts.iter().all(|x| !x.annotations.is_empty()), "…and explains itself");
        assert!(!p.write_harmony, "no plan and no key: the harmony is left alone");
        // The drums land on the GM drum channel; everything else on 0.
        let drums = p.parts.iter().find(|x| x.name == "drums").unwrap();
        assert!(drums.notes.iter().all(|n| n.channel == 9));
        assert!(drums.annotations.iter().any(|a| a.label == "General MIDI"));
        let melody = p.parts.iter().find(|x| x.name == "melody").unwrap();
        assert!(melody.notes.iter().all(|n| n.channel == 0));
    }

    #[test]
    fn every_generated_note_passes_the_wire_validator() {
        for plan in ["axis", "12-bar-blues", "circle", "functional:80", "andalusian"] {
            for seed in 0..6u64 {
                let req = GenerateRequest {
                    plan: Some(plan.into()),
                    bars: Some(8),
                    seed: Some(seed),
                    ..Default::default()
                };
                let p = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR)
                    .unwrap_or_else(|e| panic!("{plan} seed {seed}: {e}"));
                for part in &p.parts {
                    for n in &part.notes {
                        n.validate().unwrap_or_else(|e| panic!("{plan}/{}: {e}", part.name));
                        assert!(n.key <= 127);
                        assert!((n.tick as u64) < p.length_ticks + 3840, "{} out of range", n.tick);
                    }
                }
            }
        }
    }

    #[test]
    fn the_same_seed_is_the_same_sketch() {
        let req = GenerateRequest {
            plan: Some("functional:60".into()),
            seed: Some(7),
            bars: Some(8),
            ..Default::default()
        };
        let a = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        let b = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        assert_eq!(a.harmony, b.harmony);
        for (x, y) in a.parts.iter().zip(&b.parts) {
            assert_eq!(x.notes, y.notes, "{}", x.name);
        }
        // No seed given: the seed is DERIVED from the request, so the same
        // request is still reproducible (and a different request is not).
        let no_seed = GenerateRequest { seed: None, ..req.clone() };
        let c = prepare(&no_seed, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        let d = prepare(&no_seed, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        assert_eq!(c.seed, d.seed);
        assert_ne!(
            c.seed,
            prepare(
                &GenerateRequest { plan: Some("axis".into()), ..no_seed.clone() },
                &HarmonyDoc::default(),
                960,
                Meter::FOUR_FOUR
            )
            .unwrap()
            .seed
        );
    }

    #[test]
    fn a_bad_request_is_a_structured_error_and_generates_nothing() {
        let base = GenerateRequest { plan: Some("axis".into()), ..Default::default() };
        for (req, needle) in [
            (GenerateRequest { plan: Some("nope".into()), ..base.clone() }, "plan"),
            (GenerateRequest { key: Some("H major".into()), ..base.clone() }, "tonic"),
            (GenerateRequest { parts: vec!["kazoo".into()], ..base.clone() }, "unknown part"),
            (GenerateRequest { genre: Some("polka".into()), ..base.clone() }, "genre"),
            (GenerateRequest { voicing: Some("wide".into()), ..base.clone() }, "voicing"),
            (GenerateRequest { rhythm: Some("shuffle".into()), ..base.clone() }, "rhythm"),
            (GenerateRequest { bass_style: Some("slap".into()), ..base.clone() }, "bass style"),
        ] {
            let err = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR)
                .map(|_| ())
                .expect_err("should reject");
            assert!(err.contains(needle), "{err:?} should mention {needle:?}");
        }
        // No plan and an empty document: refused plainly, and NOT half-served
        // as a melody over no harmony.
        let err = prepare(&GenerateRequest::default(), &HarmonyDoc::default(), 960, Meter::FOUR_FOUR)
            .map(|_| ())
            .expect_err("nothing to write");
        assert!(err.contains("no chords"), "{err}");
        // Same for a document whose chords all end before the sketch starts.
        let err = prepare(
            &GenerateRequest { at_ticks: Some(100 * 3840), ..Default::default() },
            &doc(),
            960,
            Meter::FOUR_FOUR,
        )
        .map(|_| ())
        .expect_err("past the end of the harmony");
        assert!(err.contains("no chords"), "{err}");
    }

    #[test]
    fn a_key_without_a_plan_rewrites_the_key_and_keeps_the_chords() {
        let req = GenerateRequest {
            key: Some("A minor".into()),
            parts: vec!["chords".into()],
            ..Default::default()
        };
        let p = prepare(&req, &doc(), 960, Meter::FOUR_FOUR).unwrap();
        assert_eq!(p.harmony.key_at(0).canonical(), "A aeolian");
        assert_eq!(p.harmony.progression_string(), "C | Am | F | G7", "the chords are untouched");
        assert!(p.write_harmony, "a key change is a document edit");
    }

    #[test]
    fn a_truncated_schema_does_not_leave_empty_bars_of_drums() {
        // The 12-bar blues asked for in 3 bars covers 3 bars, so the groove and
        // the melody must be 3 bars too — the harmony decides the length.
        let req = GenerateRequest {
            plan: Some("12-bar-blues".into()),
            bars: Some(3),
            ..Default::default()
        };
        let p = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        assert_eq!(p.bars, 3);
        assert_eq!(p.length_ticks, 3 * 3840);
        for part in &p.parts {
            let last = part.notes.iter().map(|n| n.tick as u64).max().unwrap_or(0);
            assert!(last < p.length_ticks, "{} writes past the sketch ({last})", part.name);
        }
    }

    #[test]
    fn the_meter_reaches_the_generators() {
        let req = GenerateRequest { plan: Some("axis".into()), bars: Some(2), ..Default::default() };
        let three_four = prepare(&req, &HarmonyDoc::default(), 960, Meter::new(3, 4)).unwrap();
        assert_eq!(three_four.length_ticks, 2 * 2880, "two bars of 3/4");
        let drums = three_four.parts.iter().find(|p| p.name == "drums").unwrap();
        // The 3/4 backbeat is beats 2 AND 3, derived — so a snare lands on the
        // third beat, which never happens in 4/4.
        assert!(
            drums.notes.iter().any(|n| n.key == 38 && (n.tick as i64 - 1920).abs() < 60),
            "no snare near beat 3 of a 3/4 bar"
        );
    }
    // ---- the transaction (session-level, no engine needed) ----------------

    use crate::audio::types::Store;
    use crate::control::session::Session;
    use crate::midi::MidiStore;
    use parking_lot::Mutex;

    fn session() -> Mutex<Session> {
        Mutex::new(Session::new(Store::default(), MidiStore::default()))
    }

    #[test]
    fn harmony_set_is_one_op_whose_inverse_restores_the_previous_document() {
        let session = session();
        let d = doc();
        let committed = Session::transact(&session, TxMeta::system("t"), |tx| {
            tx.apply(Op::HarmonySet { keys: d.keys.clone(), chords: d.chords.clone() })
        })
        .unwrap();
        assert_eq!(committed.ops.len(), 1, "one op for a whole document edit");
        assert_eq!(session.lock().midi.harmony, d);
        // …and it rides the published snapshot, so a reader never re-locks.
        assert_eq!(*committed.snapshot.midi.harmony, d);

        Session::transact(&session, TxMeta::system("undo"), |tx| {
            for inv in committed.inverses.clone() {
                tx.apply(inv)?;
            }
            Ok(())
        })
        .unwrap();
        assert!(session.lock().midi.harmony.is_empty(), "undo emptied it again");
    }

    #[test]
    fn an_invalid_document_is_rejected_without_touching_the_session() {
        let session = session();
        Session::transact(&session, TxMeta::system("first"), |tx| {
            let d = doc();
            tx.apply(Op::HarmonySet { keys: d.keys, chords: d.chords })
        })
        .unwrap();
        let before = session.lock().midi.harmony.clone();
        let err = Session::transact(&session, TxMeta::system("bad"), |tx| {
            tx.apply(Op::HarmonySet {
                keys: vec![KeySpan { tick: 480, key: Key::c_major() }],
                chords: vec![],
            })
        })
        .map(|_| ())
        .expect_err("a key span that is not at tick 0 is invalid");
        assert!(err.contains("tick 0"), "{err}");
        assert_eq!(session.lock().midi.harmony, before, "rolled back");
    }

    #[test]
    fn a_generate_lands_every_part_in_one_transaction() {
        let session = session();
        let req = GenerateRequest { plan: Some("axis".into()), bars: Some(4), ..Default::default() };
        let prepared = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        let mut clips = Vec::new();
        let committed = Session::transact(&session, TxMeta::user("generate"), |tx| {
            clips = commit_sketch(tx, &prepared, &BTreeMap::new())?;
            Ok(())
        })
        .unwrap();
        assert_eq!(clips.len(), 4, "chords, bass, melody, drums");
        assert!(clips.iter().all(|c| c.created_track));
        // ONE batch: the harmony write, four track adds, four clip adds.
        let harmony_ops = committed.ops.iter().filter(|o| matches!(o, Op::HarmonySet { .. })).count();
        let track_adds = committed.ops.iter().filter(|o| matches!(o, Op::TrackAdd { .. })).count();
        let clip_adds = committed.ops.iter().filter(|o| matches!(o, Op::MidiClipAdd { .. })).count();
        assert_eq!((harmony_ops, track_adds, clip_adds), (1, 4, 4), "{:?}", committed.ops.len());
        {
            let s = session.lock();
            assert_eq!(s.midi.clips.len(), 4);
            assert_eq!(s.store.tracks.len(), 4);
            assert_eq!(s.midi.harmony.progression_string(), "C | G | Am | F");
            // Every clip is an ORDINARY clip: real note ids, a content id, the
            // right lane, placed on the timeline (ruling H-6).
            for c in &s.midi.clips {
                assert!(!c.notes.is_empty());
                assert!(c.notes.iter().all(|n| n.note_id.0 >= 1), "note ids minted");
                assert!(!c.content_id.0.is_empty());
                assert_eq!(c.lane_id, crate::ids::LaneId::default_for_track(c.track_id.as_str()));
                assert_eq!(c.timeline_start_ticks, 0);
                assert_eq!(c.length_ticks, 4 * 3840);
            }
        }
        // One undo removes the whole idea (ruling H-7).
        Session::transact(&session, TxMeta::user("undo"), |tx| {
            for inv in committed.inverses.clone() {
                tx.apply(inv)?;
            }
            Ok(())
        })
        .unwrap();
        let s = session.lock();
        assert!(s.midi.clips.is_empty(), "no clips left");
        assert!(s.store.tracks.is_empty(), "no tracks left");
        assert!(s.midi.harmony.is_empty(), "no harmony left");
    }

    #[test]
    fn generating_onto_an_unknown_or_wrong_track_applies_nothing() {
        let session = session();
        // An audio track is not a legal target for a MIDI part.
        let audio_id = {
            let mut s = session.lock();
            ops::add_track(&mut s.store, Some("A".into()), Some("audio".into())).unwrap().id.to_string()
        };
        let req = GenerateRequest { plan: Some("axis".into()), bars: Some(2), ..Default::default() };
        let prepared = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        for (target, needle) in [
            (audio_id.clone(), "midi track"),
            ("no-such-track".to_string(), "unknown track"),
        ] {
            let mut ids = BTreeMap::new();
            ids.insert("melody".to_string(), target);
            let err = Session::transact(&session, TxMeta::user("g"), |tx| {
                commit_sketch(tx, &prepared, &ids).map(|_| ())
            })
            .map(|_| ())
            .expect_err("should reject");
            assert!(err.contains(needle), "{err:?} should mention {needle:?}");
            let s = session.lock();
            assert!(s.midi.clips.is_empty(), "a rejected sketch writes NOTHING");
            assert!(s.midi.harmony.is_empty(), "…including the harmony");
            assert_eq!(s.store.tracks.len(), 1, "…and creates no tracks");
        }
    }

    #[test]
    fn an_existing_track_is_reused_rather_than_duplicated() {
        let session = session();
        let track_id = {
            let mut s = session.lock();
            ops::add_track(&mut s.store, Some("My Lead".into()), Some("midi".into()))
                .unwrap()
                .id
                .to_string()
        };
        let req = GenerateRequest {
            plan: Some("axis".into()),
            bars: Some(2),
            parts: vec!["melody".into()],
            ..Default::default()
        };
        let prepared = prepare(&req, &HarmonyDoc::default(), 960, Meter::FOUR_FOUR).unwrap();
        let mut ids = BTreeMap::new();
        ids.insert("melody".to_string(), track_id.clone());
        let mut clips = Vec::new();
        Session::transact(&session, TxMeta::user("g"), |tx| {
            clips = commit_sketch(tx, &prepared, &ids)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(clips.len(), 1);
        assert!(!clips[0].created_track);
        assert_eq!(clips[0].track_id, track_id);
        assert_eq!(clips[0].track_name, "My Lead");
        assert_eq!(session.lock().store.tracks.len(), 1, "no second track");
    }

    #[test]
    fn the_harmony_survives_a_save_and_load_and_leaves_a_clean_project_alone() {
        let dir = std::env::temp_dir().join(format!("aura-composer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (_p, project_dir) = crate::audio::project::create(&dir, "Composer", 48_000, 120.0).unwrap();

        // A project that never used the Composer must not grow a `harmony` key.
        let mut store = MidiStore::default();
        crate::midi::persist::save_into_project(&project_dir, &store).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(project_dir.join("project.json")).unwrap()).unwrap();
        assert!(raw.get("harmony").is_none(), "no key for an empty document");

        // With a document, it round-trips — spelling included.
        store.harmony = HarmonyDoc::from_chords(
            Key::parse("Gb major").unwrap(),
            0,
            3840,
            &["Gbmaj7", "Cbmaj7"].map(|s| Chord::parse(s).unwrap()),
        );
        crate::midi::persist::save_into_project(&project_dir, &store).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(project_dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["harmony"]["keys"][0]["key"], "Gb ionian");
        let loaded = crate::midi::persist::load_from_project(&project_dir).unwrap().unwrap();
        assert_eq!(loaded.harmony, store.harmony);
        assert_eq!(loaded.harmony.chords[1].chord.root.name(), "Cb", "not B");

        // Emptying it again removes the key rather than writing `null`.
        store.harmony = HarmonyDoc::default();
        crate::midi::persist::save_into_project(&project_dir, &store).unwrap();
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(project_dir.join("project.json")).unwrap()).unwrap();
        assert!(raw.get("harmony").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rotten_harmony_block_loses_the_harmony_not_the_project() {
        let dir = std::env::temp_dir().join(format!("aura-composer-bad-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (_p, project_dir) = crate::audio::project::create(&dir, "Composer", 48_000, 120.0).unwrap();
        let store = MidiStore::default();
        crate::midi::persist::save_into_project(&project_dir, &store).unwrap();
        let file = project_dir.join("project.json");
        let mut raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        raw["harmony"] = serde_json::json!({
            "keys": [{"tick": 0, "key": "C ionian"}],
            "chords": [{"tick": 0, "lengthTicks": 960, "chord": "Cmaj77"}]
        });
        std::fs::write(&file, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
        let loaded = crate::midi::persist::load_from_project(&project_dir)
            .expect("the project still opens")
            .unwrap();
        assert!(loaded.harmony.is_empty(), "the harmony is dropped, the song is not");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
