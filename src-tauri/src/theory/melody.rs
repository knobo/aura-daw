//! Melody: a motif, developed — not a note-by-note random walk.
//!
//! The reason generated melodies sound generated is that they are sampled one
//! note at a time. Composers do not work that way: they state a motif and then
//! *develop* it, and both halves are algorithmic (product doc §4.6).
//!
//! Three mechanisms, in the order they run:
//!
//! 1. **A rhythm from the metrical grid** (`metre.rs`), so onsets land where
//!    the metre says they can and the syncopation dial displaces them on
//!    purpose rather than by accident.
//! 2. **Development operations** — repetition, sequence, inversion,
//!    augmentation, fragmentation. Each is one line and each is a real
//!    technique; together they are what makes bar 3 sound like a consequence
//!    of bar 1 instead of a second guess.
//! 3. **A figuration fix-up pass** that enforces the rules a beginner cannot
//!    see: chord tones on strong beats, non-chord tones only on weak ones and
//!    only when they resolve by step, leaps answered by a step in the opposite
//!    direction, and a cadence that lands on a stable tone so the phrase ends
//!    rather than stops.
//!
//! Pitches live as INDICES INTO THE KEY'S LADDER (its scale notes across the
//! register), not as semitones. That is what makes a transposed repetition
//! stay diatonic for free, and it is why "sequence up a third" is `+2` here.

use crate::midi::MidiNote;

use super::harmony::HarmonyDoc;
use super::metre::{velocity_for_weight, Grid};
use super::palette::NoteRole;
use super::rng::Rng;
use super::Annotation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MelodyOptions {
    pub lo: u8,
    pub hi: u8,
    /// 0..100 → notes per bar, from sparse (2) to busy (half the grid).
    pub density: u8,
    /// 0..100 → how often the line leaps instead of stepping. Kept low by
    /// default because singable lines are 70–80 % stepwise (Huron).
    pub leapiness: u8,
    /// 0..100 → how far onsets are pulled off the strong positions.
    pub syncopation: u8,
    /// Bars per phrase. The antecedent ends halfway on an unstable tone, the
    /// consequent ends on a stable one — which is what makes a melody sound
    /// finished.
    pub phrase_bars: usize,
    pub seed: u64,
}

impl Default for MelodyOptions {
    fn default() -> Self {
        // C4..C6: a singable soprano range that also sits above a default
        // keyboard comp (`voicing::VoicingOptions::default`).
        Self { lo: 60, hi: 84, density: 45, leapiness: 20, syncopation: 20, phrase_bars: 4, seed: 0 }
    }
}

/// A development operation — the vocabulary that turns one bar into a phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Development {
    Repeat,
    /// Transposed repetition by `n` scale steps. The single most effective
    /// operation there is: it keeps the shape and moves the level.
    Sequence(i32),
    /// Mirrored around its first note.
    Invert,
    /// Same pitches, twice the note values (half as many fit).
    Augment,
    /// Only the opening gesture, which leaves room for the cadence.
    Fragment,
}

impl Development {
    fn label(self) -> &'static str {
        match self {
            Development::Repeat => "repetition",
            Development::Sequence(_) => "sequence",
            Development::Invert => "inversion",
            Development::Augment => "augmentation",
            Development::Fragment => "fragmentation",
        }
    }

    fn why(self) -> &'static str {
        match self {
            Development::Repeat => {
                "The motif again, unchanged. Repetition is not laziness — it is how a listener \
                 learns which shape is the subject of the piece."
            }
            Development::Sequence(_) => {
                "The same shape, starting on a different scale degree. Because the steps are \
                 counted in the scale rather than in semitones, the copy is automatically in \
                 key and automatically fits the new chord."
            }
            Development::Invert => {
                "The motif upside down: every rise becomes a fall. It is recognisably the same \
                 idea and it answers rather than repeats."
            }
            Development::Augment => {
                "The same notes at twice the length. Slowing a motif down makes it sound like \
                 a conclusion, which is why it turns up near endings."
            }
            Development::Fragment => {
                "Just the opening gesture. Breaking a motif into its first few notes is what \
                 creates space for a cadence to land in."
            }
        }
    }
}

/// One figure: onsets within a bar, each a `(grid step, ladder index)` pair.
#[derive(Debug, Clone, PartialEq)]
struct Figure {
    onsets: Vec<(usize, i32)>,
}

impl Figure {
    fn developed(&self, op: Development, steps_per_bar: usize) -> Figure {
        let mut f = self.clone();
        match op {
            Development::Repeat => {}
            Development::Sequence(n) => {
                for (_, idx) in f.onsets.iter_mut() {
                    *idx += n;
                }
            }
            Development::Invert => {
                let pivot = f.onsets.first().map(|(_, i)| *i).unwrap_or(0);
                for (_, idx) in f.onsets.iter_mut() {
                    *idx = 2 * pivot - *idx;
                }
            }
            Development::Augment => {
                let first = f.onsets.first().map(|(s, _)| *s).unwrap_or(0);
                f.onsets = f
                    .onsets
                    .iter()
                    .map(|(s, i)| (first + (s - first) * 2, *i))
                    .filter(|(s, _)| *s < steps_per_bar)
                    .collect();
            }
            Development::Fragment => {
                let keep = (f.onsets.len() / 2).max(1);
                f.onsets.truncate(keep);
            }
        }
        if f.onsets.is_empty() {
            f.onsets.push((0, self.onsets.first().map(|(_, i)| *i).unwrap_or(0)));
        }
        f
    }
}

/// The key's scale notes as MIDI keys across the register, ascending. The
/// "ladder" every pitch decision is expressed in.
fn ladder(harmony: &HarmonyDoc, tick: u64, lo: u8, hi: u8) -> Vec<u8> {
    let key = harmony.key_at(tick);
    let pcs = key.pitch_classes();
    (lo..=hi).filter(|k| pcs.contains(&(k % 12))).collect()
}

/// Choose the motif's onsets: always the downbeat, then positions picked by
/// metrical weight — inverted as the syncopation dial rises, which is what
/// "syncopated" means (`metre::syncopation` measures the result).
fn motif_rhythm(grid: &Grid, opts: &MelodyOptions, rng: &mut Rng) -> Vec<usize> {
    let steps = grid.steps_per_bar();
    let weights = grid.weights();
    let max_w = grid.max_weight() as f32;
    let sync = opts.syncopation.min(100) as f32 / 100.0;
    let want = (2.0 + (opts.density.min(100) as f32 / 100.0) * (steps as f32 / 2.0 - 2.0))
        .round()
        .clamp(1.0, steps as f32) as usize;

    let mut chosen = vec![0usize];
    let mut pool: Vec<usize> = (1..steps).collect();
    while chosen.len() < want && !pool.is_empty() {
        let scores: Vec<f32> = pool
            .iter()
            .map(|s| {
                let w = weights[*s] as f32;
                // Straight: prefer strong positions. Syncopated: prefer the
                // weak ones, which is exactly the displacement the LHL measure
                // scores.
                let straight = w + 0.4;
                let off = (max_w - w) + 0.4;
                straight * (1.0 - sync) + off * sync
            })
            .collect();
        let i = rng.weighted(&scores);
        chosen.push(pool.remove(i));
    }
    chosen.sort_unstable();
    chosen
}

/// The motif's pitch contour, in ladder indices.
fn motif_pitches(start: i32, count: usize, opts: &MelodyOptions, rng: &mut Rng) -> Vec<i32> {
    let leap_p = opts.leapiness.min(100) as f32 / 100.0;
    let mut out = vec![start];
    let mut last_leap = 0i32;
    for _ in 1..count {
        let prev = *out.last().expect("seeded with start");
        let mut d = if last_leap != 0 {
            // A leap is answered by a step in the OPPOSITE direction: the
            // oldest melodic rule there is, and the one that makes a wide
            // interval sound intended rather than accidental.
            -last_leap.signum()
        } else if rng.chance(leap_p) {
            let size = rng.range(2, 4);
            if rng.chance(0.5) {
                size
            } else {
                -size
            }
        } else if rng.chance(0.5) {
            1
        } else {
            -1
        };
        if d == 0 {
            d = 1;
        }
        last_leap = if d.abs() >= 2 { d } else { 0 };
        out.push(prev + d);
    }
    out
}

/// Generate a melody over the harmony document.
///
/// Ruling H-6: the output is ordinary `MidiNote`s for an ordinary clip, plus
/// annotations that exist for display only.
pub fn melody_notes(
    harmony: &HarmonyDoc,
    grid: &Grid,
    start_tick: u64,
    bars: usize,
    opts: &MelodyOptions,
) -> (Vec<MidiNote>, Vec<Annotation>) {
    let ladder = ladder(harmony, start_tick, opts.lo, opts.hi);
    if bars == 0 || ladder.len() < 3 {
        return (Vec::new(), Vec::new());
    }
    let mut rng = Rng::stream(opts.seed, "melody");
    let steps_per_bar = grid.steps_per_bar();
    let step_ticks = grid.ticks_per_step() as u64;
    let bar_ticks = grid.ticks_per_bar();
    let weights = grid.weights();
    let max_weight = grid.max_weight();
    let beat_weight = weights.get(grid.steps_per_beat as usize).copied().unwrap_or(1);

    // The motif: start on a chord tone in the middle of the register, so the
    // line has room to rise and fall around it.
    let rhythm = motif_rhythm(grid, opts, &mut rng);
    let centre = (ladder.len() / 2) as i32;
    let motif = Figure {
        onsets: rhythm
            .iter()
            .copied()
            .zip(motif_pitches(centre, rhythm.len(), opts, &mut rng))
            .collect(),
    };

    // The climax belongs late — about two thirds through the phrase (the
    // melodic arch). Bars before it lean up, bars after it lean down.
    let peak_bar = ((bars as f32) * 0.66).round().max(1.0) as usize;

    let mut placed: Vec<(u64, u8, u8)> = Vec::new(); // (absolute tick, key, velocity)
    let mut annotations: Vec<Annotation> = Vec::new();
    let mut last_op: Option<Development> = None;
    for bar in 0..bars {
        let bar_start = start_tick + bar as u64 * bar_ticks;
        let op = if bar == 0 {
            Development::Repeat
        } else {
            let mut chosen = pick_development(bar, bars, peak_bar, &mut rng);
            // Never state the motif unchanged twice running. Repetition is how
            // a listener learns the shape (which is why it is weighted high),
            // but three identical bars is the most boring way a generated
            // phrase can fail, and the dump of a real sketch produced exactly
            // that.
            if chosen == Development::Repeat && last_op == Some(Development::Repeat) {
                chosen = pick_development(bar, bars, peak_bar, &mut rng);
                if chosen == Development::Repeat {
                    chosen = Development::Sequence(if rng.chance(0.5) { 1 } else { -1 });
                }
            }
            chosen
        };
        last_op = Some(op);
        let lean = if bar < peak_bar { 1 } else { -1 };
        let figure = motif.developed(op, steps_per_bar);
        for (step, idx) in &figure.onsets {
            let tick = bar_start + *step as u64 * step_ticks;
            let pal = harmony.palette_at(tick);
            let strong = weights[*step % weights.len()] >= beat_weight;
            // Ladder index → MIDI, biased by the arch and clamped into range.
            let biased = idx + if bar == 0 { 0 } else { lean * (bar as i32 % 2) };
            let mut key = ladder[biased.clamp(0, ladder.len() as i32 - 1) as usize];
            // The fix-up pass. A strong beat takes a chord tone; a weak beat
            // may be colour but never an avoid note.
            if strong {
                let tones = pal.chord_tone_keys(opts.lo, opts.hi);
                if !tones.is_empty() && !pal.role_of_midi(key).is_chord_tone() {
                    key = *tones
                        .iter()
                        .min_by_key(|t| (**t as i16 - key as i16).abs())
                        .expect("non-empty");
                }
            } else if !pal.role_of_midi(key).is_safe() {
                key = pal.nearest_safe_midi(key);
            }
            placed.push((tick, key, velocity_for_weight(weights[*step % weights.len()], max_weight, 68, 42)));
        }
        annotations.push(Annotation::new(
            bar_start.saturating_sub(start_tick),
            bar_ticks,
            format!("bar {} · {}", bar + 1, op.label()),
            if bar == 0 {
                "The motif: the shape everything after this is a version of. Its strong beats are \
                 chord tones, so it states the harmony as well as the tune."
                    .to_string()
            } else {
                op.why().to_string()
            },
        ));
    }

    // Leap repair: nothing wider than an octave survives, because nothing
    // wider than an octave is singable in a phrase like this.
    placed.sort_by_key(|(t, _, _)| *t);
    for i in 1..placed.len() {
        while (placed[i].1 as i16 - placed[i - 1].1 as i16).abs() > 12 {
            let step: i16 = if placed[i].1 > placed[i - 1].1 { -12 } else { 12 };
            let next = placed[i].1 as i16 + step;
            if !(opts.lo as i16..=opts.hi as i16).contains(&next) {
                break;
            }
            placed[i].1 = next as u8;
        }
    }

    // Every cadence adjustment below has to respect the SAME strong-beat rule
    // the generation pass enforced — a repair that lands an extension on beat
    // one has just broken the thing it was repairing.
    let is_strong = |tick: u64| -> bool {
        let step = ((tick.saturating_sub(start_tick)) / step_ticks) as usize % weights.len();
        weights[step] >= beat_weight
    };

    // The cadences. The antecedent (halfway) ends UNRESOLVED — on the fifth or
    // the second — and the consequent ends on the tonic of the final chord, on
    // the strongest onset of the last bar. This is the difference between a
    // phrase that finishes and one that stops.
    let half_bar = (opts.phrase_bars.max(2) / 2).min(bars);
    if bars > half_bar && half_bar > 0 {
        let split = start_tick + half_bar as u64 * bar_ticks;
        if let Some(last_before) = placed.iter_mut().filter(|(t, _, _)| *t < split).next_back() {
            let pal = harmony.palette_at(last_before.0);
            let strong = is_strong(last_before.0);
            let unstable: Vec<u8> = pal
                .safe_keys(opts.lo, opts.hi)
                .into_iter()
                .filter(|k| match pal.role_of_midi(*k) {
                    // On a strong beat only the fifth qualifies: it is
                    // unresolved AND a chord tone, which is exactly what a
                    // half cadence wants.
                    NoteRole::Fifth => true,
                    NoteRole::Extension => !strong,
                    _ => false,
                })
                .collect();
            if let Some(k) = nearest(&unstable, last_before.1) {
                last_before.1 = k;
            }
        }
    }
    if let Some(&(last_tick, _, _)) = placed.last() {
        let pal = harmony.palette_at(last_tick);
        let stable: Vec<u8> = pal
            .safe_keys(opts.lo, opts.hi)
            .into_iter()
            .filter(|k| matches!(pal.role_of_midi(*k), NoteRole::Root | NoteRole::Third))
            .collect();
        if let Some(entry) = placed.last_mut() {
            if let Some(k) = nearest(&stable, entry.1) {
                entry.1 = k;
            }
        }
        // Approach the final note by step wherever the previous note allows,
        // because an arrival that is leapt into does not sound like an arrival.
        let n = placed.len();
        if n >= 2 {
            let target = placed[n - 1].1;
            let prev = placed[n - 2].1;
            if (target as i16 - prev as i16).abs() > 2 {
                let pal_prev = harmony.palette_at(placed[n - 2].0);
                let strong = is_strong(placed[n - 2].0);
                let near: Vec<u8> = pal_prev
                    .safe_keys(opts.lo, opts.hi)
                    .into_iter()
                    .filter(|k| {
                        (*k as i16 - target as i16).abs() <= 2
                            && *k != target
                            && (!strong || pal_prev.role_of_midi(*k).is_chord_tone())
                    })
                    .collect();
                if let Some(k) = nearest(&near, prev) {
                    placed[n - 2].1 = k;
                }
            }
        }
        annotations.push(Annotation::new(
            last_tick.saturating_sub(start_tick),
            bar_ticks,
            "cadence",
            format!(
                "The last note lands on a note of {}, approached by step. Ending on a chord tone \
                 of the final chord on a strong beat is what makes a melody sound finished \
                 instead of interrupted.",
                harmony
                    .chord_at(last_tick)
                    .map(|c| c.chord.pretty())
                    .unwrap_or_else(|| harmony.key_at(last_tick).label()),
            ),
        ));
    }

    // Legato: every note lasts until the next onset (the last one to the end of
    // its bar), which is how a sung line behaves.
    let mut notes = Vec::with_capacity(placed.len());
    for i in 0..placed.len() {
        let (tick, key, velocity) = placed[i];
        let end = placed
            .get(i + 1)
            .map(|(t, _, _)| *t)
            .unwrap_or_else(|| start_tick + bars as u64 * bar_ticks);
        notes.push(MidiNote {
            tick: tick.saturating_sub(start_tick) as u32,
            length_ticks: end.saturating_sub(tick).max(step_ticks) as u32,
            key,
            velocity,
            channel: 0,
            note_id: crate::ids::NoteId(0),
        });
    }
    (notes, annotations)
}

fn nearest(candidates: &[u8], to: u8) -> Option<u8> {
    candidates.iter().copied().min_by_key(|k| (*k as i16 - to as i16).abs())
}

/// Which development a bar gets. Repetition and sequence dominate (they are
/// what makes a phrase cohere); augmentation shows up near the end, where
/// slowing down reads as a conclusion.
fn pick_development(bar: usize, bars: usize, peak_bar: usize, rng: &mut Rng) -> Development {
    let near_end = bar + 1 >= bars;
    let weights = if near_end {
        [0.15f32, 0.25, 0.10, 0.30, 0.20]
    } else if bar == peak_bar {
        [0.10, 0.45, 0.30, 0.05, 0.10]
    } else {
        [0.30, 0.40, 0.15, 0.05, 0.10]
    };
    match rng.weighted(&weights) {
        0 => Development::Repeat,
        1 => {
            // Steps and thirds: a sequence by a second or a third is the pair
            // that stays recognisable. A fourth already sounds like a new idea.
            let n = [1i32, -1, 2, -2][rng.below(4)];
            Development::Sequence(n)
        }
        2 => Development::Invert,
        3 => Development::Augment,
        _ => Development::Fragment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theory::chord::Chord;
    use crate::theory::metre::Meter;
    use crate::theory::scale::{Key, ScaleType};
    use crate::theory::tpc::Tpc;

    fn grid() -> Grid {
        Grid::new(Meter::FOUR_FOUR, 960, 4)
    }

    fn harmony() -> HarmonyDoc {
        HarmonyDoc::from_chords(
            Key::c_major(),
            0,
            3840,
            &["C", "Am", "F", "G7"].map(|s| Chord::parse(s).unwrap()),
        )
    }

    fn gen(seed: u64) -> (Vec<MidiNote>, Vec<Annotation>) {
        melody_notes(&harmony(), &grid(), 0, 4, &MelodyOptions { seed, ..Default::default() })
    }

    #[test]
    fn strong_beats_take_chord_tones_and_weak_beats_are_never_avoid_notes() {
        let g = grid();
        let h = harmony();
        let weights = g.weights();
        let beat_weight = weights[g.steps_per_beat as usize];
        for seed in 0..30u64 {
            let (notes, _) = gen(seed);
            assert!(!notes.is_empty(), "seed {seed}");
            for n in &notes {
                let tick = n.tick as u64;
                let step = (tick / g.ticks_per_step() as u64) as usize % weights.len();
                let pal = h.palette_at(tick);
                let role = pal.role_of_midi(n.key);
                if weights[step] >= beat_weight {
                    assert!(
                        role.is_chord_tone(),
                        "seed {seed}: key {} on a strong beat (tick {tick}) is {}",
                        n.key,
                        role.id()
                    );
                } else {
                    assert!(
                        role.is_safe(),
                        "seed {seed}: key {} on a weak beat (tick {tick}) is {}",
                        n.key,
                        role.id()
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_leaps_more_than_an_octave() {
        for seed in 0..30u64 {
            let (notes, _) = gen(seed);
            for w in notes.windows(2) {
                let leap = (w[1].key as i16 - w[0].key as i16).abs();
                assert!(leap <= 12, "seed {seed}: a leap of {leap} semitones");
            }
        }
    }

    #[test]
    fn the_line_stays_inside_its_register() {
        let opts = MelodyOptions { lo: 62, hi: 74, seed: 3, ..Default::default() };
        let (notes, _) = melody_notes(&harmony(), &grid(), 0, 4, &opts);
        assert!(!notes.is_empty());
        assert!(notes.iter().all(|n| (62..=74).contains(&n.key)), "{:?}", notes.iter().map(|n| n.key).collect::<Vec<_>>());
    }

    #[test]
    fn the_phrase_ends_on_a_stable_tone_of_the_final_chord() {
        for seed in 0..30u64 {
            let (notes, _) = gen(seed);
            let last = notes.last().expect("notes");
            let pal = harmony().palette_at(last.tick as u64);
            assert!(
                matches!(pal.role_of_midi(last.key), NoteRole::Root | NoteRole::Third),
                "seed {seed}: ends on {} ({}) over {}",
                last.key,
                pal.role_of_midi(last.key).id(),
                pal.chord.map(|c| c.symbol()).unwrap_or_default()
            );
        }
    }

    #[test]
    fn the_final_arrival_is_approached_by_step() {
        let mut stepwise = 0;
        for seed in 0..30u64 {
            let (notes, _) = gen(seed);
            if notes.len() < 2 {
                continue;
            }
            let d = (notes[notes.len() - 1].key as i16 - notes[notes.len() - 2].key as i16).abs();
            if d <= 2 {
                stepwise += 1;
            }
        }
        assert!(stepwise >= 25, "only {stepwise}/30 phrases stepped into their final note");
    }

    #[test]
    fn the_motif_recurs_rather_than_the_bars_being_unrelated() {
        // Repeat / Sequence / Invert all preserve the motif's RHYTHM, so a
        // developed phrase shares its onset pattern across bars. If every bar
        // had a different rhythm, nothing would be recognisable as a motif.
        let g = grid();
        let bar_ticks = g.ticks_per_bar();
        let mut shared_total = 0;
        for seed in 0..20u64 {
            let (notes, _) = gen(seed);
            let mut per_bar: Vec<Vec<u64>> = vec![Vec::new(); 4];
            for n in &notes {
                let bar = (n.tick as u64 / bar_ticks) as usize;
                if bar < 4 {
                    per_bar[bar].push(n.tick as u64 % bar_ticks);
                }
            }
            let motif = per_bar[0].clone();
            shared_total += per_bar.iter().filter(|b| **b == motif).count();
        }
        assert!(
            shared_total >= 40,
            "the motif's rhythm recurred only {shared_total} times across 20 phrases of 4 bars"
        );
    }

    #[test]
    fn the_motif_is_never_restated_unchanged_twice_running() {
        // Repetition is weighted high on purpose (it is how a listener learns
        // the shape), but three identical bars is the most boring way a
        // generated phrase can fail — and a dumped sketch produced exactly
        // that before this rule existed.
        for seed in 0..40u64 {
            let (_, ann) = gen(seed);
            let ops: Vec<String> = ann
                .iter()
                .filter(|a| a.label.starts_with("bar "))
                .map(|a| a.label.split('·').nth(1).unwrap_or("").trim().to_string())
                .collect();
            for w in ops.windows(2) {
                assert!(
                    !(w[0] == "repetition" && w[1] == "repetition"),
                    "seed {seed}: {ops:?}"
                );
            }
        }
    }

    #[test]
    fn density_controls_how_many_notes_there_are() {
        let sparse = melody_notes(
            &harmony(),
            &grid(),
            0,
            4,
            &MelodyOptions { density: 0, seed: 7, ..Default::default() },
        )
        .0
        .len();
        let busy = melody_notes(
            &harmony(),
            &grid(),
            0,
            4,
            &MelodyOptions { density: 100, seed: 7, ..Default::default() },
        )
        .0
        .len();
        assert!(busy > sparse * 2, "sparse {sparse}, busy {busy}");
        assert!(sparse >= 4, "even the sparsest phrase has a note per bar");
    }

    #[test]
    fn the_syncopation_dial_moves_onsets_off_the_beat() {
        let g = grid();
        let straight_off = offbeat_fraction(0);
        let synced_off = offbeat_fraction(100);
        assert!(
            synced_off > straight_off,
            "straight {straight_off:.2} vs syncopated {synced_off:.2}"
        );

        fn offbeat_fraction(sync: u8) -> f32 {
            let g = grid();
            let mut off = 0;
            let mut total = 0;
            for seed in 0..20u64 {
                let (notes, _) = melody_notes(
                    &harmony(),
                    &g,
                    0,
                    4,
                    &MelodyOptions { syncopation: sync, seed, ..Default::default() },
                );
                for n in &notes {
                    total += 1;
                    if (n.tick as u64) % (g.ticks_per_step() as u64 * 2) != 0 {
                        off += 1;
                    }
                }
            }
            off as f32 / total.max(1) as f32
        }
        let _ = g;
    }

    #[test]
    fn generation_is_deterministic_and_seed_sensitive() {
        assert_eq!(gen(99).0, gen(99).0);
        assert_ne!(
            gen(99).0.iter().map(|n| n.key).collect::<Vec<_>>(),
            gen(100).0.iter().map(|n| n.key).collect::<Vec<_>>()
        );
    }

    #[test]
    fn notes_are_legato_sorted_and_clip_relative() {
        let h = HarmonyDoc::from_chords(
            Key::c_major(),
            7680,
            3840,
            &["C", "G7"].map(|s| Chord::parse(s).unwrap()),
        );
        let (notes, ann) =
            melody_notes(&h, &grid(), 7680, 2, &MelodyOptions { seed: 4, ..Default::default() });
        assert!(!notes.is_empty());
        assert_eq!(notes.iter().map(|n| n.tick).min(), Some(0), "relative to the clip");
        assert!(notes.windows(2).all(|w| w[0].tick <= w[1].tick), "sorted");
        for w in notes.windows(2) {
            assert_eq!(w[0].tick + w[0].length_ticks, w[1].tick, "legato: no gaps, no overlaps");
        }
        assert!(notes.iter().all(|n| n.length_ticks > 0));
        assert!(ann.iter().any(|a| a.label == "cadence"));
    }

    #[test]
    fn every_bar_gets_an_annotation_that_names_a_real_technique() {
        let (_, ann) = gen(11);
        assert_eq!(ann.len(), 5, "four bars plus the cadence");
        for a in &ann {
            assert!(!a.why.is_empty());
            assert!(a.why.len() > 40, "{}", a.why);
        }
        assert!(ann[0].label.contains("bar 1"));
        assert!(ann[0].why.contains("motif"));
    }

    #[test]
    fn a_modal_key_stays_in_its_own_scale() {
        // D dorian: B natural is in the scale, B♭ is not. A melody in it must
        // never print a B♭ (which is what a "minor key" shortcut would do).
        let h = HarmonyDoc::from_chords(
            Key::new(Tpc::D, ScaleType::Dorian),
            0,
            3840,
            &["Dm7", "G7"].map(|s| Chord::parse(s).unwrap()),
        );
        for seed in 0..15u64 {
            let (notes, _) =
                melody_notes(&h, &grid(), 0, 2, &MelodyOptions { seed, ..Default::default() });
            assert!(
                notes.iter().all(|n| n.key % 12 != 10),
                "seed {seed}: a B♭ in D dorian"
            );
        }
    }

    #[test]
    fn an_empty_harmony_or_zero_bars_produces_nothing() {
        assert!(melody_notes(&harmony(), &grid(), 0, 0, &MelodyOptions::default()).0.is_empty());
        // No chords at all: the key alone still gives a ladder, so a melody is
        // possible — it just has no chord tones to snap to.
        let key_only = HarmonyDoc::in_key(Key::c_major());
        let (notes, _) = melody_notes(&key_only, &grid(), 0, 2, &MelodyOptions::default());
        assert!(!notes.is_empty(), "a key without chords is still a melody");
        assert!(notes.iter().all(|n| [0u8, 2, 4, 5, 7, 9, 11].contains(&(n.key % 12))));
    }
}
