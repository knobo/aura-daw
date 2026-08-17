//! Pitch Coach scoring: what a take did against a reference melody.
//!
//! Pure and tauri-free — [`score`] takes note spans and pitch frames and
//! returns numbers. The command layer (Task 14) reads the clip, builds the
//! reference with [`crate::midi::schedule::clip_notes`] and stamps the
//! report's `reference_track_id`.
//!
//! Two things here are deliberate and easy to "fix" wrongly:
//!
//! * **Vibrato is singing, not error.** Hit detection runs on a 120 ms
//!   moving mean of the sung pitch, so a trained voice is not marked down
//!   for a wobble it produced on purpose. The vibrato metrics themselves
//!   read the UNSMOOTHED series — smoothing exists precisely to hide the
//!   thing they measure.
//! * **Silence is not a wrong note.** A note nobody sang reports
//!   `coverage ≈ 0`, not a low `hit_fraction`; the two need different
//!   advice, and merging them would tell someone who stayed quiet that they
//!   were flat.

use crate::audio::pitch::PitchFrame;
use crate::ids::NoteId;
use crate::midi::schedule::AbsNote;

/// The first 70 ms of a note is free: a consonant, a breath or a scoop into
/// the note is not a pitch error.
pub const ONSET_GRACE_MS: f32 = 70.0;
/// Width of the moving mean that hit detection scores against — wide enough
/// to average over one cycle of a 5–7 Hz vibrato's worth of wobble.
pub const VIBRATO_MEAN_MS: f32 = 120.0;
/// Stability and vibrato are properties of the held part of a note, so they
/// skip this much of its head.
pub const SUSTAIN_SKIP_MS: f32 = 80.0;
/// Consecutive in-tolerance frames needed before a hit counts…
pub const HIT_ENTER_FRAMES: usize = 2;
/// …and consecutive out-of-tolerance frames needed to end one. Both are
/// debouncing: the frames that trigger a transition belong to the state they
/// trigger, so a flawless run still scores 100 %.
pub const HIT_LEAVE_FRAMES: usize = 3;
/// How far before a note's start the onset search may walk to find the
/// beginning of the voiced run the singer entered on. Bounded so a legato
/// phrase does not report every note as entering when the phrase did; also
/// bounded by the previous reference note's end.
pub const ONSET_LOOKBACK_MS: f32 = 250.0;
/// Peak-to-peak wobble below which a "vibrato rate" is noise, not vibrato.
const VIBRATO_FLOOR_CENTS: f32 = 20.0;
/// Notes that start this close together are one chord. Grouping by ONSET is
/// what keeps a legato melody a melody: grouping by "overlaps the cluster so
/// far" is transitive, and one note overhanging the next chains the whole
/// phrase into a single target (see
/// `reference_melody_keeps_a_legato_phrase_separate`).
const CHORD_ONSET_MS: f32 = 30.0;
/// Share of the shorter note that two notes must share before the melody
/// line is a guess. A legato overhang of a few ticks is not a chord; a drone
/// held under the tune covers its whole length and is.
const AMBIGUOUS_OVERLAP: f32 = 0.5;

/// How one reference note was sung.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteScore {
    /// Identity from the source clip. Repeats of a looped clip's content
    /// share it — `start_sample` is what makes a row unique.
    pub note_id: NoteId,
    pub start_sample: u64,
    pub end_sample: u64,
    pub key: u8,
    /// Fraction of the scored (voiced, past the grace window) frames that
    /// were within tolerance, after debouncing.
    pub hit_fraction: f32,
    /// Fraction of the note's frames that were voiced at all — "did you
    /// sing this note", asked separately from "was it in tune". Measured
    /// over the frames that EXIST in the window: a note the take never
    /// reached at all reports 0, but a note the take only half covers is
    /// scored on the half it has.
    pub coverage: f32,
    /// Signed cents, so "you are consistently 30 cents flat" survives; a
    /// mean of absolute values would throw away the direction, which is the
    /// only part a singer can act on.
    pub mean_cents: f32,
    pub median_cents: f32,
    /// Positive = entered late.
    pub onset_offset_ms: f32,
    /// Standard deviation of the smoothed cents over the sustain: drift,
    /// with vibrato averaged out.
    pub stability_cents: f32,
    pub vibrato_rate_hz: f32,
    /// Peak-to-peak, the way vibrato width is usually quoted.
    pub vibrato_extent_cents: f32,
    /// This note came out of a chord and the melody line was guessed.
    pub ambiguous: bool,
}

/// A take, scored.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitchScoreReport {
    pub notes: Vec<NoteScore>,
    /// How many of `notes` the aggregates were computed from — the ones that
    /// are not `ambiguous`. **Zero means the summary numbers mean nothing**
    /// (every reference note came out of a chord), which is a different
    /// story from a score of zero and has to be shown as one.
    pub scored_notes: usize,
    /// Share of the scored notes' duration that landed inside the tolerance,
    /// counting silence as outside it. Meaningless when `scored_notes == 0`.
    pub in_tolerance_pct: f32,
    pub mean_abs_cents: f32,
    pub median_signed_cents: f32,
    pub mean_onset_offset_ms: f32,
    pub rating: String,
    pub tolerance_cents: f32,
    /// Stamped by the command layer; [`score`] leaves it empty.
    pub reference_track_id: String,
}

/// Reduce a reference track to a single melody line: overlapping notes
/// collapse to their top note, which is where a lead line lives, and the
/// survivor is flagged so the session statistics can ignore a guess.
///
/// Chord resolution is Rust-side on purpose (ADR 0006): the panel draws
/// every note of the reference track, and only the backend decides which
/// one the singer is being scored against.
///
/// Clustering is by ONSET ([`CHORD_ONSET_MS`]), not by overlap. Overlap is
/// transitive and a melody drawn legato — every note overhanging the next by
/// a tick — would collapse into one target covering the whole phrase, which
/// leaves a report with a single row and nothing scoreable. A note that
/// another note nonetheless covers ([`AMBIGUOUS_OVERLAP`] of the shorter
/// one) is still flagged: a drone held under the tune makes the lead line a
/// guess whether or not it shares an onset with it.
pub fn reference_melody(mut notes: Vec<AbsNote>, rate: u32) -> Vec<(AbsNote, bool)> {
    notes.sort_by_key(|n| (n.start, n.key));
    let eps = ms_to_samples(CHORD_ONSET_MS, rate);
    let mut groups: Vec<Vec<AbsNote>> = Vec::new();
    for n in notes.iter().copied() {
        match groups.last_mut() {
            Some(g) if n.start.saturating_sub(g[0].start) <= eps => g.push(n),
            _ => groups.push(vec![n]),
        }
    }
    groups
        .iter()
        .map(|g| {
            let (top, chord) = top_of(g);
            // `notes` here, not the surviving targets: the note that makes a
            // melody ambiguous is usually the one chord resolution just
            // discarded.
            // `*o != top`, not a `note_id` comparison: every repeat of a
            // looped clip's content carries the same `note_id`, so a note
            // would find itself in the next pass.
            let covered = notes.iter().any(|o| *o != top && substantially_overlaps(&top, o));
            (top, chord || covered)
        })
        .collect()
}

/// The highest note of a cluster, flagged ambiguous when the cluster held
/// more than one.
fn top_of(cluster: &[AbsNote]) -> (AbsNote, bool) {
    let top = *cluster.iter().max_by_key(|n| n.key).expect("cluster is non-empty");
    (top, cluster.len() > 1)
}

/// Do these two notes share [`AMBIGUOUS_OVERLAP`] of the shorter one?
fn substantially_overlaps(a: &AbsNote, b: &AbsNote) -> bool {
    let overlap = a.end.min(b.end).saturating_sub(a.start.max(b.start));
    let shorter = (a.end - a.start).min(b.end - b.start);
    shorter > 0 && overlap as f32 >= AMBIGUOUS_OVERLAP * shorter as f32
}

/// Score `frames` (sorted by `sample`, project-domain positions) against a
/// resolved reference melody.
///
/// `reference_track_id` is left empty — the caller knows it, this function
/// does not.
pub fn score(
    reference: &[(AbsNote, bool)],
    frames: &[PitchFrame],
    rate: u32,
    tolerance_cents: f32,
) -> PitchScoreReport {
    let mut notes = Vec::with_capacity(reference.len());
    // Session statistics ignore ambiguous notes: a chord we resolved by
    // guessing must not be allowed to claim the singer was flat.
    let mut all_cents: Vec<f32> = Vec::new();
    let mut prev_end = 0u64;
    for (note, ambiguous) in reference {
        let (scored, cents) = score_one(note, *ambiguous, frames, rate, tolerance_cents, prev_end);
        if !*ambiguous {
            all_cents.extend(cents);
        }
        notes.push(scored);
        prev_end = prev_end.max(note.end);
    }

    let clean: Vec<&NoteScore> = notes.iter().filter(|n| !n.ambiguous).collect();

    let weight: f32 = clean.iter().map(|n| (n.end_sample - n.start_sample) as f32).sum();
    // `hit_fraction * coverage`, not `hit_fraction` alone. `hit_fraction` is
    // measured over the frames the singer VOICED, so one in-tune blip per
    // note would otherwise claim the note's whole duration and report
    // "Locked in" for a take that was mostly silence. The headline claims to
    // be how much of the take landed inside the tolerance; that has to
    // include the part that was not sung at all.
    let in_tolerance_pct = if weight > 0.0 {
        100.0
            * clean
                .iter()
                .map(|n| n.hit_fraction * n.coverage * (n.end_sample - n.start_sample) as f32)
                .sum::<f32>()
            / weight
    } else {
        // Nothing scoreable is NOT the worst possible score: a reference
        // track that is all chords would otherwise report 0 % "Finding it"
        // for a flawless take. `scored_notes == 0` is what the report shows
        // instead of a number.
        0.0
    };
    let mean_abs_cents = if all_cents.is_empty() {
        0.0
    } else {
        all_cents.iter().map(|c| c.abs()).sum::<f32>() / all_cents.len() as f32
    };
    let median_signed_cents = median(&mut all_cents);
    let sung: Vec<&&NoteScore> = clean.iter().filter(|n| n.coverage > 0.0).collect();
    let mean_onset_offset_ms = if sung.is_empty() {
        0.0
    } else {
        sung.iter().map(|n| n.onset_offset_ms).sum::<f32>() / sung.len() as f32
    };

    PitchScoreReport {
        scored_notes: clean.len(),
        notes,
        in_tolerance_pct,
        mean_abs_cents,
        median_signed_cents,
        mean_onset_offset_ms,
        rating: rating_for(in_tolerance_pct).to_string(),
        tolerance_cents,
        reference_track_id: String::new(),
    }
}

/// Five words, because a number alone tells a beginner nothing.
fn rating_for(pct: f32) -> &'static str {
    if pct < 40.0 {
        "Finding it"
    } else if pct < 60.0 {
        "Getting there"
    } else if pct < 80.0 {
        "Solid"
    } else if pct < 93.0 {
        "Sharp"
    } else {
        "Locked in"
    }
}

/// Signed, octave-folded distance from a sung `midi` to a reference `key`,
/// in cents. Octave folding forgives the slip that a monophonic detector
/// makes most often, and that a singer makes on purpose when a melody sits
/// outside their range.
///
/// This is the same rule as `centsToTarget` in `src/lib/pitch/lane.ts` —
/// the lane and the report must never disagree about what "30 cents flat"
/// means. Change one, change the other.
pub fn cents_to_key(midi: f32, key: u8) -> f32 {
    let mut diff = midi - key as f32;
    diff -= 12.0 * (diff / 12.0).round();
    diff * 100.0
}

/// Undo the ±600 cent fold *in place*, so a series that steps across the
/// fold boundary stays continuous. Each value is moved by whole octaves until
/// it is within half an octave of the one before it.
fn unwrap_octaves(cents: &mut [f32]) {
    for i in 1..cents.len() {
        let step = cents[i] - cents[i - 1];
        cents[i] -= 1200.0 * (step / 1200.0).round();
    }
}

fn ms_to_samples(ms: f32, rate: u32) -> u64 {
    (ms * rate as f32 / 1000.0).max(0.0) as u64
}

fn samples_to_ms(samples: i64, rate: u32) -> f32 {
    samples as f32 * 1000.0 / rate as f32
}

/// The frames inside `[start, end)`. `frames` must be sorted by `sample`.
fn window(frames: &[PitchFrame], start: u64, end: u64) -> &[PitchFrame] {
    let lo = frames.partition_point(|f| f.sample < start);
    let hi = frames.partition_point(|f| f.sample < end);
    &frames[lo..hi]
}

/// Centred moving mean of `midi` over [`VIBRATO_MEAN_MS`], in TIME rather
/// than in frames, so a different hop or project rate does not silently
/// change the width of the window.
fn moving_mean_midi(voiced: &[&PitchFrame], rate: u32) -> Vec<f32> {
    let half = ms_to_samples(VIBRATO_MEAN_MS / 2.0, rate);
    let mut out = Vec::with_capacity(voiced.len());
    let mut lo = 0usize;
    let mut hi = 0usize;
    let mut sum = 0.0f64;
    for f in voiced {
        let from = f.sample.saturating_sub(half);
        let to = f.sample.saturating_add(half);
        while hi < voiced.len() && voiced[hi].sample <= to {
            sum += voiced[hi].midi as f64;
            hi += 1;
        }
        while lo < hi && voiced[lo].sample < from {
            sum -= voiced[lo].midi as f64;
            lo += 1;
        }
        let n = (hi - lo).max(1);
        out.push((sum / n as f64) as f32);
    }
    out
}

/// Score one note, returning its row and the cents series it was judged on
/// — the session aggregate reuses that series rather than recomputing it,
/// so a summary can never disagree with the rows above it.
fn score_one(
    note: &AbsNote,
    ambiguous: bool,
    frames: &[PitchFrame],
    rate: u32,
    tolerance_cents: f32,
    prev_end: u64,
) -> (NoteScore, Vec<f32>) {
    let win = window(frames, note.start, note.end);
    let voiced: Vec<&PitchFrame> = win.iter().filter(|f| f.voiced).collect();
    let coverage = if win.is_empty() { 0.0 } else { voiced.len() as f32 / win.len() as f32 };

    let smoothed = moving_mean_midi(&voiced, rate);
    let grace_end = note.start + ms_to_samples(ONSET_GRACE_MS, rate);
    let sustain_start = note.start + ms_to_samples(SUSTAIN_SKIP_MS, rate);

    let mut scored: Vec<f32> = Vec::new();
    let mut sustain_smoothed: Vec<f32> = Vec::new();
    let mut sustain_raw: Vec<(u64, f32)> = Vec::new();
    for (f, m) in voiced.iter().zip(&smoothed) {
        let cents = cents_to_key(*m, note.key);
        if f.sample >= grace_end {
            scored.push(cents);
        }
        if f.sample >= sustain_start {
            sustain_smoothed.push(cents);
            sustain_raw.push((f.sample, cents_to_key(f.midi, note.key)));
        }
    }
    // Wobble is measured on an UNWRAPPED series. `cents_to_key` folds every
    // frame into ±600 independently, so a singer sitting about a tritone from
    // the target flips between +600 and -600 while their voice barely moves —
    // which reads as 1200 cents of vibrato and a rate invented out of the
    // fold's own crossings. The reported error stays folded (that is the
    // octave forgiveness); only the steadiness measures are unwrapped.
    unwrap_octaves(&mut sustain_smoothed);
    let mut sustain_unwrapped: Vec<f32> = sustain_raw.iter().map(|(_, c)| *c).collect();
    unwrap_octaves(&mut sustain_unwrapped);
    for (slot, c) in sustain_raw.iter_mut().zip(&sustain_unwrapped) {
        slot.1 = *c;
    }

    let hit_fraction = if scored.is_empty() {
        0.0
    } else {
        let hits = debounce(&scored.iter().map(|c| c.abs() <= tolerance_cents).collect::<Vec<_>>());
        hits.iter().filter(|h| **h).count() as f32 / hits.len() as f32
    };
    let mean_cents = if scored.is_empty() {
        0.0
    } else {
        scored.iter().sum::<f32>() / scored.len() as f32
    };
    let median_cents = median(&mut scored.clone());
    let (vibrato_rate_hz, vibrato_extent_cents) = vibrato(&sustain_raw, rate);

    let row = NoteScore {
        note_id: note.note_id,
        start_sample: note.start,
        end_sample: note.end,
        key: note.key,
        hit_fraction,
        coverage,
        mean_cents,
        median_cents,
        onset_offset_ms: onset_offset_ms(note, frames, rate, prev_end),
        stability_cents: std_dev(&sustain_smoothed),
        vibrato_rate_hz,
        vibrato_extent_cents,
        ambiguous,
    };
    (row, scored)
}

/// How late (positive) or early (negative) the singer entered, in ms.
///
/// The first voiced frame *inside* the note is not the answer: someone who
/// came in early is already voiced when the note starts, and would score a
/// perfect 0. So the search walks back to the start of the voiced run the
/// note was entered on — bounded by the previous reference note's end and
/// by [`ONSET_LOOKBACK_MS`], or a legato phrase would report its own first
/// onset for every note in it.
fn onset_offset_ms(note: &AbsNote, frames: &[PitchFrame], rate: u32, prev_end: u64) -> f32 {
    let lo = frames.partition_point(|f| f.sample < note.start);
    let hi = frames.partition_point(|f| f.sample < note.end);
    let Some(first) = (lo..hi).find(|i| frames[*i].voiced) else {
        // Nothing was sung here; `coverage` carries that story, and a
        // fabricated offset would pollute the session mean.
        return 0.0;
    };
    let floor = prev_end.max(note.start.saturating_sub(ms_to_samples(ONSET_LOOKBACK_MS, rate)));
    let mut i = first;
    while i > 0 && frames[i - 1].voiced && frames[i - 1].sample >= floor {
        i -= 1;
    }
    samples_to_ms(frames[i].sample as i64 - note.start as i64, rate)
}

/// Debounce a boolean series with [`HIT_ENTER_FRAMES`] /
/// [`HIT_LEAVE_FRAMES`] hysteresis. The frames that trigger a transition
/// count as part of the state they trigger — an implementation that made
/// them cost the singer would cap a flawless take below 100 %.
fn debounce(raw: &[bool]) -> Vec<bool> {
    let mut out = raw.to_vec();
    let mut i = 0usize;
    while i < out.len() {
        let mut j = i;
        while j < out.len() && out[j] == out[i] {
            j += 1;
        }
        let run = j - i;
        let is_edge = i == 0 || j == out.len();
        // A run too short to be believed is absorbed into its neighbours —
        // except at the ends, where there is only one neighbour and nothing
        // to interpolate from.
        let too_short = if out[i] { run < HIT_ENTER_FRAMES } else { run < HIT_LEAVE_FRAMES };
        if too_short && !is_edge {
            let flip = !out[i];
            for slot in out.iter_mut().take(j).skip(i) {
                *slot = flip;
            }
        }
        i = j;
    }
    out
}

/// Vibrato rate and peak-to-peak extent from the UNSMOOTHED cents series,
/// by counting sign changes about its own mean.
///
/// A cheap estimator is the right one here: this is feedback for a singer,
/// not a measurement. Below [`VIBRATO_FLOOR_CENTS`] of peak-to-peak wobble
/// there is no vibrato to report a rate for, and a rate derived from noise
/// crossings would be worse than none.
fn vibrato(sustain: &[(u64, f32)], rate: u32) -> (f32, f32) {
    if sustain.len() < 4 {
        return (0.0, 0.0);
    }
    let lo = sustain.iter().map(|(_, c)| *c).fold(f32::INFINITY, f32::min);
    let hi = sustain.iter().map(|(_, c)| *c).fold(f32::NEG_INFINITY, f32::max);
    let extent = hi - lo;
    let seconds = samples_to_ms(sustain[sustain.len() - 1].0 as i64 - sustain[0].0 as i64, rate)
        / 1000.0;
    if extent < VIBRATO_FLOOR_CENTS || seconds < 0.1 {
        return (0.0, extent.max(0.0));
    }
    let mean = sustain.iter().map(|(_, c)| *c).sum::<f32>() / sustain.len() as f32;
    let mut crossings = 0usize;
    let mut last_sign = 0i8;
    for (_, c) in sustain {
        let sign = if *c > mean {
            1
        } else if *c < mean {
            -1
        } else {
            0
        };
        if sign != 0 {
            if last_sign != 0 && sign != last_sign {
                crossings += 1;
            }
            last_sign = sign;
        }
    }
    (crossings as f32 / (2.0 * seconds), extent)
}

fn median(values: &mut [f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn std_dev(values: &[f32]) -> f32 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / values.len() as f32;
    var.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::pitch::PitchFrame;

    const RATE: u32 = 48_000;
    const HOP: u64 = 480;

    fn note(id: u32, key: u8, start_ms: u64, len_ms: u64) -> AbsNote {
        AbsNote {
            start: start_ms * RATE as u64 / 1000,
            end: (start_ms + len_ms) * RATE as u64 / 1000,
            key,
            clip_id_hash: 0,
            note_id: NoteId(id),
            repeat_index: 0,
        }
    }

    /// Frames covering `[from_ms, to_ms)` at `midi`, one per 10 ms hop.
    fn sung(from_ms: u64, to_ms: u64, midi: f32) -> Vec<PitchFrame> {
        let a = from_ms * RATE as u64 / 1000;
        let b = to_ms * RATE as u64 / 1000;
        (a..b)
            .step_by(HOP as usize)
            .map(|s| PitchFrame {
                sample: s,
                midi,
                hz: crate::audio::yin::midi_to_hz(midi),
                clarity: 0.9,
                rms: 0.3,
                voiced: true,
            })
            .collect()
    }

    #[test]
    fn a_perfect_run_scores_full_marks() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let rep = score(&r, &sung(0, 1000, 60.0), RATE, 50.0);
        assert!(rep.in_tolerance_pct > 99.0, "got {}", rep.in_tolerance_pct);
        assert!(rep.notes[0].mean_cents.abs() < 1.0);
        assert!(rep.notes[0].coverage > 0.95);
        assert_eq!(rep.rating, "Locked in");
    }

    /// The diagnostic a game cannot give: not "you missed" but "you are
    /// consistently 30 cents flat".
    #[test]
    fn a_consistently_flat_run_reports_a_signed_mean() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let rep = score(&r, &sung(0, 1000, 60.0 - 0.30), RATE, 50.0);
        assert!(
            (rep.notes[0].mean_cents + 30.0).abs() < 3.0,
            "got {}",
            rep.notes[0].mean_cents
        );
        assert!(
            rep.notes[0].hit_fraction > 0.9,
            "30 cents flat is still inside a 50 cent window"
        );
        assert!(rep.median_signed_cents < -20.0, "the session summary must keep the sign");
    }

    #[test]
    fn a_semitone_flat_run_is_not_a_hit() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let rep = score(&r, &sung(0, 1000, 59.0), RATE, 50.0);
        assert!(rep.notes[0].hit_fraction < 0.1, "got {}", rep.notes[0].hit_fraction);
        assert_eq!(rep.rating, "Finding it");
    }

    /// Vibrato is singing, not error. Scoring against a 120 ms moving mean
    /// is what stops a trained voice being marked down.
    #[test]
    fn vibrato_within_tolerance_still_counts_as_a_hit() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let frames: Vec<PitchFrame> = sung(0, 1000, 60.0)
            .into_iter()
            .enumerate()
            .map(|(i, mut f)| {
                let t = i as f32 * 0.010;
                // 5.5 Hz, 90 cents peak-to-peak
                f.midi = 60.0 + 0.45 * (2.0 * std::f32::consts::PI * 5.5 * t).sin();
                f.hz = crate::audio::yin::midi_to_hz(f.midi);
                f
            })
            .collect();
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(
            rep.notes[0].hit_fraction > 0.9,
            "vibrato scored as error: {}",
            rep.notes[0].hit_fraction
        );
        assert!(
            rep.notes[0].vibrato_rate_hz > 4.0 && rep.notes[0].vibrato_rate_hz < 7.0,
            "vibrato rate {} should land near 5.5 Hz",
            rep.notes[0].vibrato_rate_hz
        );
        assert!(rep.notes[0].vibrato_extent_cents > 50.0);
        assert!(
            rep.notes[0].stability_cents < 30.0,
            "vibrato must not read as drift: {}",
            rep.notes[0].stability_cents
        );
    }

    /// A consonant is not a mistake: the first 70 ms of a note is free.
    #[test]
    fn an_unvoiced_onset_does_not_penalise_the_note() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let mut frames = sung(0, 1000, 60.0);
        for f in frames.iter_mut().take(6) {
            f.voiced = false;
        }
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(
            rep.notes[0].hit_fraction > 0.95,
            "60 ms of consonant must be free: {}",
            rep.notes[0].hit_fraction
        );
    }

    #[test]
    fn a_late_entry_is_reported_as_positive_onset_offset() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let rep = score(&r, &sung(120, 1000, 60.0), RATE, 50.0);
        assert!(
            (rep.notes[0].onset_offset_ms - 120.0).abs() < 25.0,
            "got {}",
            rep.notes[0].onset_offset_ms
        );
    }

    #[test]
    fn an_early_entry_is_reported_as_negative_onset_offset() {
        let r = reference_melody(vec![note(1, 60, 500, 1000)], RATE);
        let rep = score(&r, &sung(400, 1500, 60.0), RATE, 50.0);
        assert!(rep.notes[0].onset_offset_ms < -50.0, "got {}", rep.notes[0].onset_offset_ms);
    }

    /// A legato phrase is voiced straight through. Without a bound, every
    /// note after the first would claim the phrase's own entry as its
    /// onset — a whole verse reported as "seconds early".
    #[test]
    fn a_legato_phrase_does_not_credit_every_note_with_the_first_entry() {
        let r = reference_melody(vec![note(1, 60, 0, 500), note(2, 62, 500, 500)], RATE);
        let mut frames = sung(0, 500, 60.0);
        frames.extend(sung(500, 1000, 62.0));
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(
            rep.notes[1].onset_offset_ms.abs() < 15.0,
            "second note's onset walked back into the first: {}",
            rep.notes[1].onset_offset_ms
        );
    }

    /// Silence must read as "you did not sing", not as "you sang badly" —
    /// the two need different advice.
    #[test]
    fn silence_reports_low_coverage_not_a_low_hit_fraction() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let mut frames = sung(0, 1000, 60.0);
        for f in frames.iter_mut() {
            f.voiced = false;
        }
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(rep.notes[0].coverage < 0.05, "coverage {}", rep.notes[0].coverage);
        assert_eq!(rep.notes[0].onset_offset_ms, 0.0, "no entry to be early or late about");
    }

    #[test]
    fn an_octave_slip_is_forgiven_by_folding() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let rep = score(&r, &sung(0, 1000, 72.0), RATE, 50.0);
        assert!(rep.notes[0].hit_fraction > 0.9, "octave folding must absorb this");
    }

    #[test]
    fn a_chord_resolves_to_the_top_note_and_is_flagged() {
        let r = reference_melody(vec![note(1, 60, 0, 1000), note(2, 64, 0, 1000)], RATE);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0.key, 64, "lead lines take the top note, never the root");
        assert!(r[0].1, "an overlap must be flagged ambiguous");
    }

    /// Chord guesswork must not be allowed to claim the singer was flat.
    #[test]
    fn ambiguous_notes_are_excluded_from_the_session_statistics() {
        let r = reference_melody(vec![
            note(1, 60, 0, 500),
            note(2, 64, 0, 500), // overlap -> ambiguous
            note(3, 62, 500, 500), // clean
        ], RATE);
        let mut frames = sung(0, 500, 64.0 - 0.9); // badly off on the ambiguous one
        frames.extend(sung(500, 1000, 62.0)); // perfect on the clean one
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(
            rep.mean_abs_cents < 20.0,
            "the ambiguous note must not drag the summary: {}",
            rep.mean_abs_cents
        );
        assert_eq!(rep.notes.len(), 2, "but it is still reported per note");
    }

    #[test]
    fn tolerance_tier_changes_what_counts_as_a_hit() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let frames = sung(0, 1000, 60.0 + 0.40); // 40 cents sharp
        assert!(
            score(&r, &frames, RATE, 50.0).notes[0].hit_fraction > 0.9,
            "inside standard"
        );
        assert!(
            score(&r, &frames, RATE, 20.0).notes[0].hit_fraction < 0.1,
            "outside pro"
        );
    }

    /// The bug this guards: clustering by "overlaps the cluster so far" is
    /// transitive, so a phrase drawn legato — every note overhanging the next
    /// by a hair — collapsed into ONE target spanning the whole phrase, and
    /// the report came back with a single row.
    #[test]
    fn reference_melody_keeps_a_legato_phrase_separate() {
        let overhang = 5; // ms of overlap between consecutive notes
        let r = reference_melody(
            (0..6)
                .map(|i| note(i, 60 + i as u8, i as u64 * 500, 500 + overhang))
                .collect(),
            RATE,
        );
        assert_eq!(r.len(), 6, "a legato phrase is six targets, not one");
        assert!(
            r.iter().all(|(_, ambiguous)| !ambiguous),
            "a few ms of overhang is not a chord"
        );
    }

    /// A drone held under the tune makes every note it covers a guess, even
    /// though it shares an onset with none of them.
    #[test]
    fn a_note_covered_by_a_longer_one_is_ambiguous() {
        let r = reference_melody(
            vec![
                note(1, 48, 0, 2000), // the drone
                note(2, 60, 0, 500),
                note(3, 62, 500, 500),
                note(4, 64, 1000, 500),
            ],
            RATE,
        );
        assert!(
            r.iter().all(|(_, ambiguous)| *ambiguous),
            "every note under a drone is a guessed melody line"
        );
    }

    /// Nothing scoreable is not a score of zero. Before `scored_notes`
    /// existed, an all-chords reference reported 0 % "Finding it" for a take
    /// sung perfectly.
    #[test]
    fn an_all_ambiguous_reference_reports_no_scored_notes() {
        let r = reference_melody(vec![note(1, 60, 0, 1000), note(2, 64, 0, 1000)], RATE);
        let rep = score(&r, &sung(0, 1000, 64.0), RATE, 50.0);
        assert_eq!(rep.scored_notes, 0, "the summary has nothing to stand on");
        assert!(rep.notes[0].hit_fraction > 0.9, "the row itself still scores");
    }

    #[test]
    fn scored_notes_counts_the_unambiguous_rows() {
        let r = reference_melody(vec![note(1, 60, 0, 500), note(2, 62, 500, 500)], RATE);
        let rep = score(&r, &sung(0, 1000, 60.0), RATE, 50.0);
        assert_eq!(rep.scored_notes, 2);
    }

    /// One in-tune blip per note used to report "Locked in": `hit_fraction`
    /// is measured over the frames that were VOICED, and the headline gave
    /// each note its full duration regardless.
    #[test]
    fn a_note_barely_sung_cannot_claim_its_whole_duration() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let mut frames = sung(0, 1000, 60.0);
        // Sung for 150 ms of a 1 s note, in tune throughout.
        for f in frames.iter_mut().skip(15) {
            f.voiced = false;
        }
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(
            rep.notes[0].hit_fraction > 0.5,
            "what was sung was in tune: {}",
            rep.notes[0].hit_fraction
        );
        assert!(rep.notes[0].coverage < 0.2, "and almost none of it was sung");
        assert!(
            rep.in_tolerance_pct < 25.0,
            "the headline must not read as a take that was sung: {}",
            rep.in_tolerance_pct
        );
    }

    /// A singer parked about a tritone away crosses the ±600 cent fold, and
    /// the folded series flips 1200 cents while their voice barely moves.
    #[test]
    fn a_singer_near_the_octave_fold_does_not_report_fake_vibrato() {
        let r = reference_melody(vec![note(1, 60, 0, 1000)], RATE);
        let frames: Vec<PitchFrame> = sung(0, 1000, 60.0)
            .into_iter()
            .enumerate()
            .map(|(i, mut f)| {
                // Wobbling ~10 cents either side of exactly six semitones up.
                let t = i as f32 * 0.010;
                f.midi = 66.0 + 0.10 * (2.0 * std::f32::consts::PI * 5.0 * t).sin();
                f.hz = crate::audio::yin::midi_to_hz(f.midi);
                f
            })
            .collect();
        let rep = score(&r, &frames, RATE, 50.0);
        assert!(
            rep.notes[0].vibrato_extent_cents < 100.0,
            "a 20 cent wobble read as {} cents of vibrato",
            rep.notes[0].vibrato_extent_cents
        );
        assert!(
            rep.notes[0].stability_cents < 50.0,
            "the fold read as drift: {}",
            rep.notes[0].stability_cents
        );
    }

    #[test]
    fn unwrap_octaves_makes_a_folded_series_continuous() {
        let mut s = vec![590.0, -595.0, 598.0, -590.0];
        unwrap_octaves(&mut s);
        for w in s.windows(2) {
            assert!((w[1] - w[0]).abs() < 600.0, "{s:?} still jumps an octave");
        }
    }

    /// A single stray frame either way is detector noise, not a miss and
    /// not a save.
    #[test]
    fn debounce_absorbs_single_frame_flicker_without_costing_the_edges() {
        assert_eq!(debounce(&[true; 5]), vec![true; 5], "a clean run stays 100 %");
        assert_eq!(
            debounce(&[true, true, true, false, true, true, true]),
            vec![true; 7],
            "one bad frame inside a hit is absorbed"
        );
        assert_eq!(
            debounce(&[false, false, false, true, false, false, false]),
            vec![false; 7],
            "one good frame inside a miss does not rescue it"
        );
    }
}

