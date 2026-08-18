//! Voice leading: the difference between a chord generator and music.
//!
//! Beginner chords sound wrong for mechanical reasons theory has known for
//! four hundred years — every chord in root position, all voices leaping in
//! parallel, tendency tones left hanging. So this module does not "play the
//! chord": it enumerates candidate voicings and runs a **dynamic-programming
//! pass over the whole progression**, minimising movement while resolving the
//! notes that want to move (product doc §4.5).
//!
//! DP rather than greedy on purpose: a locally smooth choice can strand the
//! next chord in a corner of the register, and the cheapest *pair* of moves is
//! routinely not the cheapest first move. The state space is tiny (a few dozen
//! candidates per chord), so the exact answer costs nothing.

use crate::midi::MidiNote;

use super::analysis::analyze;
use super::chord::Chord;
use super::harmony::HarmonyDoc;
use super::metre::{velocity_for_weight, Grid};
use super::tpc::{interval_step, Pitch};
use super::Annotation;

/// How to spread a chord's notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoicingStyle {
    /// Stacked as tightly as the chord allows — the default keyboard voicing.
    Close,
    /// The second voice from the top dropped an octave: the standard guitar and
    /// big-band voicing, because it opens the middle without moving the melody.
    Drop2,
    /// Root, third (or the sus'd fourth) and seventh — no fifth. The fifth
    /// says the least about a chord, so dropping it is the cheapest way to
    /// sound like a jazz pianist rather than a hymn book.
    Shell,
    /// Bass an octave down, upper voices close: wide, cinematic.
    Spread,
}

impl VoicingStyle {
    pub fn parse(s: &str) -> Option<VoicingStyle> {
        match s.trim().to_ascii_lowercase().as_str() {
            "close" => Some(VoicingStyle::Close),
            "drop2" | "drop-2" => Some(VoicingStyle::Drop2),
            "shell" => Some(VoicingStyle::Shell),
            "spread" => Some(VoicingStyle::Spread),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            VoicingStyle::Close => "close",
            VoicingStyle::Drop2 => "drop2",
            VoicingStyle::Shell => "shell",
            VoicingStyle::Spread => "spread",
        }
    }
}

/// How to place a voiced chord in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordRhythm {
    /// One sustained block per chord — the chord-track sound.
    Block,
    /// The voicing's notes in sequence on the grid, cycling up. Costs nothing
    /// extra and makes an eight-bar sketch sound played rather than pasted.
    Arpeggio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoicingOptions {
    pub lo: u8,
    pub hi: u8,
    pub voices: usize,
    pub style: VoicingStyle,
    /// Penalise parallel fifths and octaves in the outer voices. Style-gated
    /// because pop, gospel and rock want them — this is a taste flag, not a
    /// correctness one.
    pub avoid_parallels: bool,
}

impl Default for VoicingOptions {
    fn default() -> Self {
        // C3..C6: the range a keyboard comp part actually lives in, low enough
        // to carry the harmony and high enough to stay out of the bass part's
        // way.
        Self { lo: 48, hi: 84, voices: 4, style: VoicingStyle::Close, avoid_parallels: false }
    }
}

/// Candidate voicings of one chord inside the register, ascending, deduplicated
/// and in a deterministic order.
pub fn candidates(chord: &Chord, opts: &VoicingOptions) -> Vec<Vec<u8>> {
    let tones = select_tones(chord, opts);
    if tones.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    // A slash chord names its own bass, so its inversion is not ours to
    // choose: `C/E` with G in the bass is a different chord.
    let inversions = if chord.bass.is_some() { 1 } else { tones.len() };
    for inversion in 0..inversions {
        let order: Vec<_> = tones[inversion..].iter().chain(&tones[..inversion]).copied().collect();
        for bass_shift in 0..4i16 {
            let mut voicing: Vec<i16> = Vec::with_capacity(order.len());
            let bass = Pitch::at_or_above(order[0], opts.lo as i16).midi() + 12 * bass_shift;
            voicing.push(bass);
            for t in &order[1..] {
                let prev = *voicing.last().expect("bass pushed first");
                let mut p = Pitch::at_or_above(*t, prev + 1).midi();
                // Never stack a unison: the same pitch class an octave up is
                // the nearest legal placement.
                while p <= prev {
                    p += 12;
                }
                voicing.push(p);
            }
            if let Some(v) = apply_style(voicing, opts) {
                if v.iter().all(|k| *k >= opts.lo as i16 && *k <= opts.hi as i16)
                    && v.last().copied().unwrap_or(0) - v[0] <= span_limit(opts.style)
                {
                    let keys: Vec<u8> = v.iter().map(|k| *k as u8).collect();
                    if !out.contains(&keys) {
                        out.push(keys);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

fn span_limit(style: VoicingStyle) -> i16 {
    match style {
        VoicingStyle::Spread => 34,
        VoicingStyle::Drop2 => 26,
        _ => 22,
    }
}

/// Which chord tones to use, in stacking order.
fn select_tones(chord: &Chord, opts: &VoicingOptions) -> Vec<super::Tpc> {
    let mut tones = chord.tones();
    if let Some(bass) = chord.bass {
        // A slash bass leads, whether or not it is a chord tone.
        tones.retain(|t| t.pitch_class() != bass.pitch_class());
        tones.insert(0, bass);
    }
    if opts.style == VoicingStyle::Shell {
        // Root, the third/sus, and the seventh — drop the fifth. Only when
        // something survives as a real chord: a plain triad keeps its fifth,
        // because root-and-third alone is an interval, not a voicing.
        let kept: Vec<_> = tones
            .iter()
            .copied()
            .filter(|t| interval_step(t.fifths_from(chord.root)) != 4)
            .collect();
        if kept.len() >= 3 {
            tones = kept;
        }
    }
    // A shell voicing's tone count IS the voicing — padding it back up to the
    // requested voice count with a doubled root would put the fifth's weight
    // back and undo the point.
    let want = if opts.style == VoicingStyle::Shell {
        tones.len().clamp(2, opts.voices.max(2))
    } else {
        opts.voices.clamp(2, 6)
    };
    while tones.len() > want {
        // Drop the fifth first, then the highest extension: both say less than
        // the third and the seventh.
        let fifth = tones
            .iter()
            .position(|t| interval_step(t.fifths_from(chord.root)) == 4 && tones.len() > 3);
        match fifth {
            Some(i) => {
                tones.remove(i);
            }
            None => {
                tones.pop();
            }
        }
    }
    while tones.len() < want {
        // Double the root at the top rather than inventing a tone.
        tones.push(chord.root);
    }
    tones
}

fn apply_style(mut v: Vec<i16>, opts: &VoicingOptions) -> Option<Vec<i16>> {
    match opts.style {
        VoicingStyle::Close | VoicingStyle::Shell => {}
        VoicingStyle::Drop2 => {
            if v.len() < 3 {
                return None;
            }
            let i = v.len() - 2;
            v[i] -= 12;
            v.sort_unstable();
        }
        VoicingStyle::Spread => {
            v[0] -= 12;
        }
    }
    // Duplicated pitches after a transform mean the style did not fit this
    // chord; reject rather than emit a thinner voicing than asked for.
    if v.windows(2).any(|w| w[0] >= w[1]) {
        return None;
    }
    Some(v)
}

/// Cost of moving from `prev` (voiced under `prev_chord`) to `cand`.
fn transition_cost(prev: &[u8], cand: &[u8], prev_chord: &Chord, opts: &VoicingOptions) -> f32 {
    let n = prev.len().min(cand.len());
    let mut cost = 0.0f32;
    for i in 0..n {
        let d = (cand[i] as i16 - prev[i] as i16).unsigned_abs() as f32;
        // The OUTER voices are the ones the ear tracks: the top reads as a
        // melody and the bottom as the bass line, so their motion costs more
        // than an inner voice's. Without this the DP happily drops the bass an
        // octave to save two inner semitones, which is the most audible flaw a
        // comp part can have.
        let w = match i {
            _ if i + 1 == n => 1.5,
            0 => 1.3,
            _ => 1.0,
        };
        cost += d * w;
    }
    if prev == cand {
        // Not forbidden — a repeated chord is legal — but a voicing that
        // moves at all reads as playing rather than holding.
        cost += 2.0;
    }
    // Tendency tone: a dominant seventh's ♭7 wants to fall a semitone. Reward
    // the candidate that lets it.
    if prev_chord.quality.is_dominant_shape() {
        let seventh_pc = prev_chord.root.plus_fifths(-2).pitch_class();
        if let Some(i) = prev.iter().position(|k| k % 12 == seventh_pc) {
            if i < n && prev[i] as i16 - cand[i] as i16 == 1 {
                cost -= 6.0;
            }
        }
    }
    if opts.avoid_parallels && n >= 2 {
        let outer_prev = prev[n - 1] as i16 - prev[0] as i16;
        let outer_cand = cand[n - 1] as i16 - cand[0] as i16;
        let same_interval = outer_prev % 12 == outer_cand % 12;
        let perfect = matches!(outer_cand.rem_euclid(12), 0 | 7);
        let same_direction = (cand[0] as i16 - prev[0] as i16).signum()
            == (cand[n - 1] as i16 - prev[n - 1] as i16).signum()
            && cand[0] != prev[0];
        if same_interval && perfect && same_direction {
            cost += 12.0;
        }
    }
    cost
}

/// Cost of a voicing on its own: how far it sits from the middle of the
/// register, a spacing penalty for gaps above the bass, and a mild preference
/// for having the chord's ROOT in the bass.
///
/// The root preference is deliberately mild: inversions are how voice leading
/// stays smooth, so banning them would defeat the point. But with it absent
/// entirely the DP happily opens a progression on a second-inversion tonic
/// (`C` voiced `G3 C4 C5 E5`), which is an unstable chord to start on and
/// sounds like a mistake to anyone who can hear it. `first` makes the
/// preference firm for the opening chord, where there is no previous voicing
/// to be smooth with anyway.
fn root_position_penalty(v: &[u8], chord: &Chord, first: bool) -> f32 {
    if chord.bass.is_some() {
        return 0.0; // a slash chord's bass is the composer's choice, not ours
    }
    let bass_pc = v.first().map(|k| k % 12);
    if bass_pc == Some(chord.root.pitch_class()) {
        0.0
    } else if first {
        14.0
    } else {
        0.5
    }
}

fn static_cost(v: &[u8], opts: &VoicingOptions) -> f32 {
    let centre = (opts.lo as f32 + opts.hi as f32) / 2.0;
    let mean = v.iter().map(|k| *k as f32).sum::<f32>() / v.len().max(1) as f32;
    let mut cost = (mean - centre).abs() * 0.3;
    // Wide at the bottom, close at the top is the classical spacing rule; a
    // gap between the UPPER voices is what sounds hollow.
    for w in v.windows(2).skip(1) {
        let gap = w[1] as i16 - w[0] as i16;
        if gap > 12 {
            cost += (gap - 12) as f32 * 1.5;
        }
    }
    cost
}

/// Voice-lead a whole progression: one voicing per chord, chosen together.
///
/// Returns an empty vec for an empty input, and falls back to whatever
/// candidates exist for a chord the register cannot hold (rather than dropping
/// the chord — a silent bar is worse than a cramped one).
pub fn voice_lead(chords: &[Chord], opts: &VoicingOptions) -> Vec<Vec<u8>> {
    if chords.is_empty() {
        return Vec::new();
    }
    let all: Vec<Vec<Vec<u8>>> = chords
        .iter()
        .map(|c| {
            let mut cands = candidates(c, opts);
            if cands.is_empty() {
                // Widen once: a register too tight for the requested voice
                // count still has to produce sound.
                let mut relaxed = *opts;
                relaxed.voices = 3;
                relaxed.lo = opts.lo.saturating_sub(12);
                relaxed.hi = (opts.hi as u16 + 12).min(127) as u8;
                cands = candidates(c, &relaxed);
            }
            cands
        })
        .collect();
    if all.iter().any(|c| c.is_empty()) {
        return Vec::new();
    }

    // DP: best[i][j] = cheapest path ending on candidate j of chord i.
    let mut best: Vec<Vec<f32>> = Vec::with_capacity(all.len());
    let mut from: Vec<Vec<usize>> = Vec::with_capacity(all.len());
    best.push(
        all[0]
            .iter()
            .map(|v| static_cost(v, opts) + root_position_penalty(v, &chords[0], true))
            .collect(),
    );
    from.push(vec![0; all[0].len()]);
    for i in 1..all.len() {
        let mut row = Vec::with_capacity(all[i].len());
        let mut back = Vec::with_capacity(all[i].len());
        for cand in &all[i] {
            let mut best_cost = f32::MAX;
            let mut best_k = 0usize;
            for (k, prev) in all[i - 1].iter().enumerate() {
                let c = best[i - 1][k]
                    + transition_cost(prev, cand, &chords[i - 1], opts);
                if c < best_cost {
                    best_cost = c;
                    best_k = k;
                }
            }
            row.push(
                best_cost + static_cost(cand, opts) + root_position_penalty(cand, &chords[i], false),
            );
            back.push(best_k);
        }
        best.push(row);
        from.push(back);
    }

    // Backtrack from the cheapest final state.
    let last = best.len() - 1;
    let mut j = (0..best[last].len())
        .min_by(|a, b| best[last][*a].partial_cmp(&best[last][*b]).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(0);
    let mut out = vec![Vec::new(); all.len()];
    for i in (0..all.len()).rev() {
        out[i] = all[i][j].clone();
        j = from[i][j];
    }
    out
}

/// Total semitone movement across a voiced progression — the number
/// `voice_lead` minimises, exposed so a test (and a curious user) can check
/// that it beats stacking every chord in root position.
pub fn total_movement(voicings: &[Vec<u8>]) -> u32 {
    voicings
        .windows(2)
        .map(|w| {
            let n = w[0].len().min(w[1].len());
            (0..n).map(|i| (w[1][i] as i16 - w[0][i] as i16).unsigned_abs() as u32).sum::<u32>()
        })
        .sum()
}

/// Render the harmony document's chords as MIDI notes on a clip starting at
/// `start_tick`.
///
/// Ruling H-6: the result is ordinary notes for an ordinary `MidiClip`. The
/// annotations ride alongside for display and are not written anywhere.
pub fn chord_notes(
    harmony: &HarmonyDoc,
    grid: &Grid,
    start_tick: u64,
    opts: &VoicingOptions,
    rhythm: ChordRhythm,
) -> (Vec<MidiNote>, Vec<Annotation>) {
    let spans: Vec<_> = harmony.chords.iter().filter(|c| c.end_tick() > start_tick).collect();
    if spans.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let chords: Vec<Chord> = spans.iter().map(|s| s.chord).collect();
    let voicings = voice_lead(&chords, opts);
    if voicings.len() != spans.len() {
        return (Vec::new(), Vec::new());
    }
    let weights = grid.weights();
    let max_weight = grid.max_weight();
    let mut notes = Vec::new();
    let mut annotations = Vec::new();
    for (span, voicing) in spans.iter().zip(&voicings) {
        let rel = span.tick.saturating_sub(start_tick);
        let key = harmony.key_at(span.tick);
        let a = analyze(&span.chord, &key);
        // The chord's own downbeat drives its velocity, so a chord landing on
        // a weak beat is played softer — the same weight function the drums
        // use (ruling H-10).
        let step = (rel / grid.ticks_per_step() as u64) as usize % weights.len().max(1);
        let velocity = velocity_for_weight(weights[step], max_weight, 62, 40);
        match rhythm {
            ChordRhythm::Block => {
                for key_num in voicing {
                    notes.push(MidiNote {
                        tick: rel as u32,
                        length_ticks: span.length_ticks.max(1) as u32,
                        key: *key_num,
                        velocity,
                        channel: 0,
                        note_id: crate::ids::NoteId(0),
                    });
                }
            }
            ChordRhythm::Arpeggio => {
                let step_ticks = grid.ticks_per_step() as u64;
                let steps = (span.length_ticks / step_ticks.max(1)).max(1);
                for i in 0..steps {
                    let key_num = voicing[(i as usize) % voicing.len()];
                    let s = ((rel + i * step_ticks) / step_ticks) as usize % weights.len().max(1);
                    notes.push(MidiNote {
                        tick: (rel + i * step_ticks) as u32,
                        length_ticks: step_ticks as u32,
                        key: key_num,
                        velocity: velocity_for_weight(weights[s], max_weight, 58, 40),
                        channel: 0,
                        note_id: crate::ids::NoteId(0),
                    });
                }
            }
        }
        let tones = voicing
            .iter()
            .map(|k| super::Tpc::simplest_for_pc(k % 12).pretty())
            .collect::<Vec<_>>()
            .join(" ");
        annotations.push(Annotation::new(
            rel,
            span.length_ticks,
            format!("{} · {}", span.chord.pretty(), a.roman),
            format!("{} Voiced {tones} — {}.", a.why, movement_note(opts.style)),
        ));
    }
    notes.sort_by_key(|n| (n.tick, n.key));
    (notes, annotations)
}

fn movement_note(style: VoicingStyle) -> &'static str {
    match style {
        VoicingStyle::Close => {
            "the voices move as little as possible from the chord before, which is what stops a \
             progression sounding like separate chords"
        }
        VoicingStyle::Drop2 => {
            "the second voice from the top is dropped an octave, opening the middle without \
             moving the top line"
        }
        VoicingStyle::Shell => {
            "root, third and seventh only — the fifth is the tone that says least, so leaving it \
             out sounds more played, not thinner"
        }
        VoicingStyle::Spread => "the bass is an octave below the rest, for width",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::harmony::ChordSpan;
    use crate::theory::metre::Meter;
    use crate::theory::scale::Key;

    fn chords(symbols: &[&str]) -> Vec<Chord> {
        symbols.iter().map(|s| Chord::parse(s).unwrap()).collect()
    }

    /// Every chord stacked in root position from the bottom of the register —
    /// the naive rendering voice leading has to beat.
    fn root_position(chords: &[Chord], opts: &VoicingOptions) -> Vec<Vec<u8>> {
        chords
            .iter()
            .map(|c| {
                let mut v = Vec::new();
                let mut prev = opts.lo as i16 - 1;
                for t in select_tones(c, opts) {
                    let mut p = Pitch::at_or_above(t, prev + 1).midi();
                    while p <= prev {
                        p += 12;
                    }
                    v.push(p as u8);
                    prev = p;
                }
                v
            })
            .collect()
    }

    #[test]
    fn voice_leading_moves_less_than_root_position_stacking() {
        let opts = VoicingOptions::default();
        let prog = chords(&["C", "Am", "F", "G7", "C"]);
        let led = voice_lead(&prog, &opts);
        let naive = root_position(&prog, &opts);
        assert_eq!(led.len(), prog.len());
        assert!(
            total_movement(&led) < total_movement(&naive),
            "voice-led {} vs root-position {}",
            total_movement(&led),
            total_movement(&naive)
        );
        // And it is not a trivial win: real progressions should move only a
        // few semitones per chord.
        assert!(
            (total_movement(&led) as f32) < total_movement(&naive) as f32 / 1.5,
            "the win should be substantial, not marginal: led {} vs naive {}",
            total_movement(&led),
            total_movement(&naive)
        );
        // A per-chord average is the wrong guard here: the DP minimises a
        // WEIGHTED cost (outer voices count more, register and root position
        // enter it), so a path with a slightly larger raw semitone sum can be
        // the better-sounding one. What must hold is that no single voice
        // lurches: nothing moves more than an octave between two chords.
        for w in led.windows(2) {
            for i in 0..w[0].len().min(w[1].len()) {
                let d = (w[1][i] as i16 - w[0][i] as i16).abs();
                assert!(d <= 12, "voice {i} leapt {d} semitones: {:?} -> {:?}", w[0], w[1]);
            }
        }
    }

    #[test]
    fn a_progression_opens_in_root_position() {
        // Inversions are how voice leading stays smooth, so they are welcome
        // everywhere EXCEPT the opening chord, where there is no previous
        // voicing to be smooth with and a second-inversion tonic just sounds
        // like a mistake. (Found by reading a dumped sketch: the DP used to
        // open `C` as `G3 C4 C5 E5`.)
        let opts = VoicingOptions::default();
        for symbols in [
            vec!["C", "Am", "F", "G7", "C"],
            vec!["Am", "F", "C", "G"],
            vec!["Fmaj7", "Bb13", "Eb"],
            vec!["Dm7", "G7", "Cmaj7"],
        ] {
            let prog = chords(&symbols);
            let led = voice_lead(&prog, &opts);
            assert_eq!(
                led[0][0] % 12,
                prog[0].root.pitch_class(),
                "{} should open in root position, got {:?}",
                symbols[0],
                led[0]
            );
        }
        // …but a slash chord's bass is the composer's choice, not ours.
        let slash = voice_lead(&chords(&["C/E", "F"]), &opts);
        assert_eq!(slash[0][0] % 12, 4, "C/E keeps E in the bass");
    }

    #[test]
    fn every_voice_stays_inside_the_register() {
        let opts = VoicingOptions { lo: 55, hi: 79, voices: 4, ..Default::default() };
        for v in voice_lead(&chords(&["Cmaj7", "F#m7b5", "Bb13", "Eb", "Am7"]), &opts) {
            assert!(!v.is_empty());
            assert!(v.iter().all(|k| (55..=79).contains(k)), "{v:?}");
            assert!(v.windows(2).all(|w| w[0] < w[1]), "ascending, no unisons: {v:?}");
        }
    }

    #[test]
    fn the_dominant_seventh_falls_a_semitone_into_the_tonic() {
        // The single most-taught voice-leading rule: the ♭7 of V7 resolves
        // down to the 3rd of I.
        let opts = VoicingOptions::default();
        let led = voice_lead(&chords(&["G7", "C"]), &opts);
        let f_pc = 5u8; // the ♭7 of G7
        let e_pc = 4u8; // the 3rd of C
        let i = led[0]
            .iter()
            .position(|k| k % 12 == f_pc)
            .expect("G7 contains its seventh");
        assert_eq!(
            led[1][i] % 12,
            e_pc,
            "the voice holding F must land on E: {:?} -> {:?}",
            led[0],
            led[1]
        );
        assert_eq!(led[0][i] as i16 - led[1][i] as i16, 1, "and by ONE semitone, downward");
    }

    #[test]
    fn styles_change_the_shape_not_the_harmony() {
        let prog = chords(&["Cmaj7", "Dm7", "G7"]);
        for style in [VoicingStyle::Close, VoicingStyle::Drop2, VoicingStyle::Shell, VoicingStyle::Spread] {
            let opts = VoicingOptions { style, ..Default::default() };
            let led = voice_lead(&prog, &opts);
            assert_eq!(led.len(), 3, "{}", style.id());
            for (v, c) in led.iter().zip(&prog) {
                let pcs = c.pitch_classes();
                assert!(
                    v.iter().all(|k| pcs.contains(&(k % 12))),
                    "{}: {v:?} contains a note that is not in {}",
                    style.id(),
                    c.symbol()
                );
                assert!(v.windows(2).all(|w| w[0] < w[1]));
            }
        }
        // Shell drops the fifth; close keeps it.
        let shell = voice_lead(&prog, &VoicingOptions { style: VoicingStyle::Shell, ..Default::default() });
        assert!(
            !shell[0].iter().any(|k| k % 12 == 7),
            "no fifth in a shell voicing of Cmaj7: {:?}",
            shell[0]
        );
        assert_eq!(shell[0].len(), 3);
    }

    #[test]
    fn parallel_octaves_are_penalised_when_asked() {
        // Two root-position triads a step apart are the textbook parallel-
        // octave trap. With the flag on, the chosen pair must not move the
        // outer voices in parallel into an octave or a fifth.
        let prog = chords(&["C", "D"]);
        let opts = VoicingOptions { voices: 3, avoid_parallels: true, ..Default::default() };
        let led = voice_lead(&prog, &opts);
        let n = led[0].len();
        let outer0 = (led[0][n - 1] - led[0][0]) % 12;
        let outer1 = (led[1][n - 1] - led[1][0]) % 12;
        let parallel = outer0 == outer1
            && matches!(outer1, 0 | 7)
            && (led[1][0] as i16 - led[0][0] as i16).signum()
                == (led[1][n - 1] as i16 - led[0][n - 1] as i16).signum()
            && led[1][0] != led[0][0];
        assert!(!parallel, "{:?} -> {:?} moves in parallel", led[0], led[1]);
    }

    #[test]
    fn slash_chords_put_their_bass_at_the_bottom() {
        let opts = VoicingOptions { voices: 3, ..Default::default() };
        for v in voice_lead(&chords(&["C/E", "C/G"]), &opts) {
            assert!(!v.is_empty());
        }
        let e_bass = voice_lead(&chords(&["C/E"]), &opts);
        assert_eq!(e_bass[0][0] % 12, 4, "E in the bass: {:?}", e_bass[0]);
    }

    #[test]
    fn no_voice_lurches_in_any_schema_or_key() {
        // A sweep over the shipped schemas in a major and a minor key: the
        // properties that reading one dumped sketch taught us to check, applied
        // to every progression the panel can produce.
        use crate::theory::progression::{generate, Plan, SCHEMAS};
        use crate::theory::scale::ScaleType;
        for schema in SCHEMAS {
            for key in [Key::c_major(), Key::new(crate::theory::Tpc::A, ScaleType::Aeolian)] {
                let prog = generate(&key, &Plan::Schema(schema.id.into()), 8, 3).unwrap();
                let chords: Vec<Chord> = prog.slots.iter().map(|s| s.chord).collect();
                let led = voice_lead(&chords, &VoicingOptions::default());
                assert_eq!(led.len(), chords.len(), "{} in {}", schema.id, key.label());
                assert_eq!(
                    led[0][0] % 12,
                    chords[0].root.pitch_class(),
                    "{} in {} should open in root position: {:?}",
                    schema.id,
                    key.label(),
                    led[0]
                );
                for w in led.windows(2) {
                    for i in 0..w[0].len().min(w[1].len()) {
                        let d = (w[1][i] as i16 - w[0][i] as i16).abs();
                        assert!(
                            d <= 12,
                            "{} in {}: voice {i} leapt {d}: {:?} -> {:?}",
                            schema.id,
                            key.label(),
                            w[0],
                            w[1]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn voicing_is_deterministic() {
        let opts = VoicingOptions::default();
        let prog = chords(&["Am7", "D7", "Gmaj7", "Cmaj7"]);
        assert_eq!(voice_lead(&prog, &opts), voice_lead(&prog, &opts));
    }

    #[test]
    fn block_chords_fill_their_span_and_explain_themselves() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let harmony = HarmonyDoc::from_chords(Key::c_major(), 0, 3840, &chords(&["C", "G7"]));
        let (notes, annotations) =
            chord_notes(&harmony, &grid, 0, &VoicingOptions::default(), ChordRhythm::Block);
        assert_eq!(annotations.len(), 2);
        assert!(annotations[1].label.contains("V7"), "{}", annotations[1].label);
        assert!(annotations[0].why.len() > 40);
        assert_eq!(notes.iter().filter(|n| n.tick == 0).count(), 4, "four voices at bar 1");
        assert!(notes.iter().all(|n| n.length_ticks == 3840), "block chords sustain");
        assert!(notes.iter().all(|n| n.velocity > 0 && n.velocity <= 127));
        assert!(notes.windows(2).all(|w| (w[0].tick, w[0].key) <= (w[1].tick, w[1].key)), "sorted");
    }

    #[test]
    fn arpeggios_land_on_the_grid_and_cycle_the_voicing() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 2); // eighths
        let harmony = HarmonyDoc::from_chords(Key::c_major(), 0, 3840, &chords(&["C"]));
        let (notes, _) =
            chord_notes(&harmony, &grid, 0, &VoicingOptions::default(), ChordRhythm::Arpeggio);
        assert_eq!(notes.len(), 8, "one bar of eighths");
        assert!(notes.iter().all(|n| n.tick % 480 == 0), "on the grid");
        assert!(notes.iter().all(|n| n.length_ticks == 480));
        // Velocity follows metrical weight: the downbeat is the loudest.
        assert!(notes[0].velocity > notes[1].velocity);
    }

    #[test]
    fn a_clip_starting_later_gets_clip_relative_ticks() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let harmony = HarmonyDoc::from_chords(Key::c_major(), 7680, 3840, &chords(&["C", "F"]));
        let (notes, ann) =
            chord_notes(&harmony, &grid, 7680, &VoicingOptions::default(), ChordRhythm::Block);
        assert_eq!(notes.iter().map(|n| n.tick).min(), Some(0), "relative to the clip, not the song");
        assert_eq!(ann[1].tick, 3840);
    }

    #[test]
    fn an_empty_harmony_produces_nothing_rather_than_panicking() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let (notes, ann) = chord_notes(
            &HarmonyDoc::default(),
            &grid,
            0,
            &VoicingOptions::default(),
            ChordRhythm::Block,
        );
        assert!(notes.is_empty() && ann.is_empty());
        assert!(voice_lead(&[], &VoicingOptions::default()).is_empty());
    }

    #[test]
    fn a_span_before_the_clip_start_is_skipped_not_wrapped() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let harmony = HarmonyDoc {
            keys: vec![super::super::harmony::KeySpan { tick: 0, key: Key::c_major() }],
            chords: vec![
                ChordSpan::new(0, 3840, Chord::parse("C").unwrap()),
                ChordSpan::new(3840, 3840, Chord::parse("G7").unwrap()),
            ],
        };
        let (_, ann) =
            chord_notes(&harmony, &grid, 3840, &VoicingOptions::default(), ChordRhythm::Block);
        assert_eq!(ann.len(), 1, "only the chord at or after the clip start");
        assert!(ann[0].label.contains("G7"));
    }
}
