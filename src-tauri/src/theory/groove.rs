//! Drums from metrical theory (product doc §4.7).
//!
//! "A drum generator based on music theory" sounds like the least plausible
//! part of the brief and turns out to have the most rigorous answer, because
//! rhythm is the best-formalised corner of music theory:
//!
//! * **Placement** comes from the metrical weight tree (`metre::Grid::weights`)
//!   — the kick takes strong positions, the snare takes the *secondary* strong
//!   ones (which is what a backbeat IS, derived rather than hard-coded), ghost
//!   notes take weight-zero positions.
//! * **Density** distributes onsets with Bjorklund's algorithm, so the spacing
//!   is maximally even rather than arbitrary.
//! * **Syncopation** is a measured quantity (Longuet-Higgins & Lee): the dial
//!   displaces kicks off strong positions and the result can be *checked*.
//! * **Velocity** is the same weight function again (ruling H-10), which is why
//!   the drums accent in agreement with every other generated part.
//! * **Fills** land at hypermetric boundaries because phrase structure is the
//!   same tree one level up.
//! * **Genre is a parameter vector**, not a pattern library — six numbers, so
//!   genres can be interpolated, tweaked and explained.

use crate::midi::MidiNote;

use super::metre::{self, swing_offset, velocity_for_weight, Grid, Meter};
use super::rng::Rng;
use super::Annotation;

/// A drum voice, with its General MIDI key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Kick,
    Snare,
    Rimshot,
    Clap,
    ClosedHat,
    OpenHat,
    Ride,
    Crash,
    TomHi,
    TomMid,
    TomLo,
    Shaker,
}

impl Role {
    /// GM percussion key numbers (channel 10, 1-based — channel 9 zero-based).
    pub fn gm_key(self) -> u8 {
        match self {
            Role::Kick => 36,
            Role::Snare => 38,
            Role::Rimshot => 37,
            Role::Clap => 39,
            Role::ClosedHat => 42,
            Role::OpenHat => 46,
            Role::Ride => 51,
            Role::Crash => 49,
            Role::TomHi => 50,
            Role::TomMid => 47,
            Role::TomLo => 43,
            Role::Shaker => 70,
        }
    }

    fn base_velocity(self) -> u8 {
        match self {
            Role::Kick => 78,
            Role::Snare => 74,
            Role::Crash => 84,
            Role::Rimshot | Role::Clap => 68,
            Role::ClosedHat | Role::Shaker => 48,
            Role::OpenHat | Role::Ride => 56,
            Role::TomHi | Role::TomMid | Role::TomLo => 70,
        }
    }
}

/// The GM drum channel (0-based 9 = "channel 10" on the box).
pub const DRUM_CHANNEL: u8 = 9;

/// A genre, which is to say a point in the parameter space below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Genre {
    Rock,
    Funk,
    HipHop,
    House,
    Jazz,
    Latin,
    Ballad,
    Metal,
}

impl Genre {
    pub const ALL: [Genre; 8] = [
        Genre::Rock,
        Genre::Funk,
        Genre::HipHop,
        Genre::House,
        Genre::Jazz,
        Genre::Latin,
        Genre::Ballad,
        Genre::Metal,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Genre::Rock => "rock",
            Genre::Funk => "funk",
            Genre::HipHop => "hipHop",
            Genre::House => "house",
            Genre::Jazz => "jazz",
            Genre::Latin => "latin",
            Genre::Ballad => "ballad",
            Genre::Metal => "metal",
        }
    }

    pub fn parse(s: &str) -> Option<Genre> {
        let k = s.trim().to_ascii_lowercase().replace(['-', ' ', '_'], "");
        Genre::ALL.into_iter().find(|g| g.id().to_ascii_lowercase() == k)
    }

    /// The vector. Every field is a number a user could reasonably move, which
    /// is the point: there is no hidden pattern behind a genre name.
    fn vector(self) -> GenreVector {
        match self {
            // subdivision, swing, sync, kick, hat, ghosts, cymbal
            Genre::Rock => GenreVector::new(4, 0.0, 15, 45, HatVoice::ClosedHat, 2, false),
            // Funk's hats are sixteenths — that constant density under a
            // sparse, displaced kick IS the genre.
            Genre::Funk => GenreVector::new(4, 0.12, 70, 70, HatVoice::ClosedHat, 1, false),
            Genre::HipHop => GenreVector::new(4, 0.18, 55, 40, HatVoice::ClosedHat, 4, false),
            Genre::House => GenreVector::new(4, 0.0, 10, 100, HatVoice::OpenOffbeat, 2, false),
            // A jazz ride plays swung EIGHTHS (hat_every 1 on an eighth
            // grid); on quarters there would be nothing for the swing to
            // displace.
            Genre::Jazz => GenreVector::new(2, 0.33, 45, 25, HatVoice::Ride, 1, true),
            Genre::Latin => GenreVector::new(4, 0.0, 60, 55, HatVoice::Shaker, 2, false),
            Genre::Ballad => GenreVector::new(2, 0.0, 5, 30, HatVoice::ClosedHat, 2, false),
            Genre::Metal => GenreVector::new(4, 0.0, 20, 85, HatVoice::ClosedHat, 2, false),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HatVoice {
    ClosedHat,
    /// Closed hats plus an open hat on every off-beat — the house signature.
    OpenOffbeat,
    /// Swung ride instead of hats.
    Ride,
    Shaker,
}

#[derive(Debug, Clone, Copy)]
struct GenreVector {
    steps_per_beat: u32,
    swing: f32,
    /// 0..100, the genre's own syncopation lean before the user's dial.
    syncopation: u8,
    /// 0..100, the genre's own kick density before the user's dial.
    kick_density: u8,
    hat: HatVoice,
    /// Hat onset every `hat_every` steps.
    hat_every: usize,
    ghosts: bool,
}

impl GenreVector {
    const fn new(
        steps_per_beat: u32,
        swing: f32,
        syncopation: u8,
        kick_density: u8,
        hat: HatVoice,
        hat_every: usize,
        ghosts: bool,
    ) -> Self {
        Self { steps_per_beat, swing, syncopation, kick_density, hat, hat_every, ghosts }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrooveOptions {
    pub genre: Genre,
    /// `None` = the genre's own value; `Some(0..100)` overrides.
    pub syncopation: Option<u8>,
    pub density: Option<u8>,
    pub swing: Option<u8>,
    /// 0..100 timing/velocity jitter. Bounded and seeded, so a "humanised"
    /// groove is still reproducible.
    pub humanize: u8,
    /// A fill lands at the end of every `fill_every` bars. `0` = never.
    pub fill_every: usize,
    pub seed: u64,
}

impl Default for GrooveOptions {
    fn default() -> Self {
        Self {
            genre: Genre::Rock,
            syncopation: None,
            density: None,
            swing: None,
            humanize: 20,
            fill_every: 4,
            seed: 0,
        }
    }
}

/// Generate a groove of `bars` bars.
///
/// The grid is built from the genre's subdivision rather than taken from the
/// caller: a jazz groove is an eighth-note grid and a funk groove a sixteenth,
/// and that choice is part of what the genre IS.
pub fn groove_notes(
    meter: Meter,
    ppq: u32,
    start_tick: u64,
    bars: usize,
    opts: &GrooveOptions,
) -> (Vec<MidiNote>, Vec<Annotation>) {
    if bars == 0 {
        return (Vec::new(), Vec::new());
    }
    let v = opts.genre.vector();
    let grid = Grid::new(meter, ppq, v.steps_per_beat);
    let steps = grid.steps_per_bar();
    let weights = grid.weights();
    let max_weight = grid.max_weight();
    let step_ticks = grid.ticks_per_step() as u64;
    let bar_ticks = grid.ticks_per_bar();
    let sync = opts.syncopation.unwrap_or(v.syncopation).min(100);
    let density = opts.density.unwrap_or(v.kick_density).min(100);
    let swing = opts.swing.map(|s| s.min(100) as f32 / 100.0 * 0.5).unwrap_or(v.swing);
    let mut rng = Rng::stream(opts.seed, "groove");

    let beats = grid.beat_steps();
    let backbeat = grid.backbeat_steps();

    // ── kick: Euclidean over the BEATS, then displaced by the dial ──
    let k = ((density as f32 / 100.0) * (beats.len() as f32 - 1.0)).round() as usize + 1;
    let chosen = metre::euclid(k.min(beats.len()), beats.len());
    // Rotate so the downbeat always carries a kick: a groove whose bar does
    // not start is a different feature, not a default.
    let first_on = chosen.iter().position(|b| *b).unwrap_or(0);
    let chosen = metre::rotate(&chosen, first_on as isize);
    let mut kick_steps: Vec<usize> =
        beats.iter().zip(&chosen).filter(|(_, on)| **on).map(|(s, _)| *s).collect();
    // Displacement: push the LAST n kicks one grid step late. Deterministic
    // (not sampled), so the measured syncopation rises monotonically with the
    // dial instead of merely tending to.
    let movable = kick_steps.len().saturating_sub(1);
    let n_move = ((sync as f32 / 100.0) * movable as f32).round() as usize;
    for i in 0..n_move {
        let idx = kick_steps.len() - 1 - i;
        if kick_steps[idx] + 1 < steps {
            kick_steps[idx] += 1;
        }
    }
    // Funk and hip-hop add a sixteenth pickup into the backbeat — the "and"
    // that makes a groove feel like it leans forward.
    if matches!(opts.genre, Genre::Funk | Genre::HipHop) && density > 30 {
        if let Some(b) = backbeat.first() {
            let pickup = b.saturating_sub(1);
            if !kick_steps.contains(&pickup) {
                kick_steps.push(pickup);
            }
        }
    }
    kick_steps.sort_unstable();
    kick_steps.dedup();

    let mut notes: Vec<MidiNote> = Vec::new();
    let mut annotations: Vec<Annotation> = Vec::new();

    for bar in 0..bars {
        let bar_start = start_tick + bar as u64 * bar_ticks;
        let is_fill_bar = opts.fill_every > 0 && (bar + 1) % opts.fill_every == 0;
        // The fill takes the last beat; the rest of the bar plays normally.
        let fill_from = if is_fill_bar {
            steps.saturating_sub(v.steps_per_beat as usize)
        } else {
            steps
        };

        for step in 0..steps {
            if step >= fill_from {
                continue;
            }
            let w = weights[step];
            let swung = swing_offset(step, step_ticks as u32, swing);

            if kick_steps.contains(&step) {
                push(&mut notes, &mut rng, opts, bar_start, step, step_ticks, 0, Role::Kick, w, max_weight);
            }
            if backbeat.contains(&step) {
                let role = if opts.genre == Genre::HipHop { Role::Clap } else { Role::Snare };
                push(&mut notes, &mut rng, opts, bar_start, step, step_ticks, 0, role, w, max_weight);
            }
            // Ghost notes: weight-zero positions, played quietly. This is the
            // difference between a drum machine and a drummer.
            if v.ghosts && w == 0 && rng.chance(0.18) {
                push_soft(&mut notes, &mut rng, opts, bar_start, step, step_ticks, swung, Role::Snare, 32);
            }
            // Hats / ride / shaker.
            if step % v.hat_every == 0 {
                let role = match v.hat {
                    HatVoice::ClosedHat | HatVoice::OpenOffbeat => Role::ClosedHat,
                    HatVoice::Ride => Role::Ride,
                    HatVoice::Shaker => Role::Shaker,
                };
                push(&mut notes, &mut rng, opts, bar_start, step, step_ticks, swung, role, w, max_weight);
                if v.hat == HatVoice::OpenOffbeat && !beats.contains(&step) {
                    push(&mut notes, &mut rng, opts, bar_start, step, step_ticks, swung, Role::OpenHat, w, max_weight);
                }
            }
        }

        // Crash on the first bar and at every section boundary — the same
        // hierarchy one level up.
        if bar == 0 || (opts.fill_every > 0 && bar % opts.fill_every == 0) {
            push(&mut notes, &mut rng, opts, bar_start, 0, step_ticks, 0, Role::Crash, max_weight, max_weight);
        }

        if is_fill_bar {
            let fill_roles = [Role::Snare, Role::TomHi, Role::TomMid, Role::TomLo];
            for (i, step) in (fill_from..steps).enumerate() {
                let role = fill_roles[i % fill_roles.len()];
                push(&mut notes, &mut rng, opts, bar_start, step, step_ticks, 0, role, weights[step].max(1), max_weight);
            }
            annotations.push(Annotation::new(
                bar_start.saturating_sub(start_tick),
                bar_ticks,
                format!("fill · bar {}", bar + 1),
                "A fill on the last beat of the phrase. Fills land here because phrase structure \
                 is the metrical hierarchy one level up: the bar that ends a four-bar group is \
                 the strong beat of a larger bar."
                    .to_string(),
            ));
        }
    }

    notes.sort_by_key(|n| (n.tick, n.key));

    // The explanations. Each one names the mechanism AND the number it
    // produced, so a user can check it rather than trust it.
    let kick_pattern: Vec<bool> = (0..steps).map(|s| kick_steps.contains(&s)).collect();
    let measured = metre::syncopation(&kick_pattern, &weights);
    annotations.insert(
        0,
        Annotation::new(
            0,
            bar_ticks,
            format!("kick · {} onsets", kick_steps.len()),
            format!(
                "Placed by Bjorklund's algorithm — {} onsets spread as evenly as possible over \
                 the bar's {} beats — then {} of them pushed a step late by the syncopation dial \
                 ({}%). Measured syncopation (Longuet-Higgins & Lee): {}.",
                k.min(beats.len()),
                beats.len(),
                n_move,
                sync,
                measured,
            ),
        ),
    );
    annotations.insert(
        1,
        Annotation::new(
            0,
            bar_ticks,
            format!("snare · steps {backbeat:?}"),
            format!(
                "The backbeat is not a constant: these are the positions the metrical tree of \
                 {}/{} marks as strong but not strongest — in 4/4 that is beats 2 and 4, and in \
                 3/4 it is beats 2 and 3. Every velocity in this groove comes from the same \
                 weight array, which is why it accents in agreement with the other parts.",
                meter.num, meter.den,
            ),
        ),
    );
    if swing > 0.0 {
        annotations.push(Annotation::new(
            0,
            bar_ticks,
            format!("swing · {:.0}%", swing * 100.0),
            "Every second grid step is pushed later by this fraction of a step. A third is \
             triplet swing; the small values are what separates a groove that breathes from one \
             that marches."
                .to_string(),
        ));
    }
    (notes, annotations)
}

#[allow(clippy::too_many_arguments)]
fn push(
    notes: &mut Vec<MidiNote>,
    rng: &mut Rng,
    opts: &GrooveOptions,
    bar_start: u64,
    step: usize,
    step_ticks: u64,
    swung: i64,
    role: Role,
    weight: u8,
    max_weight: u8,
) {
    // Velocity: the role's floor, accented by metrical weight (ruling H-10).
    let base = velocity_for_weight(weight, max_weight, role.base_velocity(), 30);
    push_soft(notes, rng, opts, bar_start, step, step_ticks, swung, role, base);
}

#[allow(clippy::too_many_arguments)]
fn push_soft(
    notes: &mut Vec<MidiNote>,
    rng: &mut Rng,
    opts: &GrooveOptions,
    bar_start: u64,
    step: usize,
    step_ticks: u64,
    swung: i64,
    role: Role,
    velocity: u8,
) {
    let h = opts.humanize.min(100) as f32 / 100.0;
    // Bounded jitter: at most a tenth of a step and ±12 velocity, so
    // "humanised" never means "out of time".
    let jitter_ticks = (rng.range(-100, 100) as f32 / 100.0 * h * step_ticks as f32 * 0.10) as i64;
    let jitter_vel = (rng.range(-12, 12) as f32 * h) as i16;
    let tick = (bar_start as i64 + step as i64 * step_ticks as i64 + swung + jitter_ticks).max(0);
    notes.push(MidiNote {
        tick: tick as u32,
        // Drum hits are short: the sample decides the length, not the note.
        length_ticks: (step_ticks / 2).max(1) as u32,
        key: role.gm_key(),
        velocity: (velocity as i16 + jitter_vel).clamp(1, 127) as u8,
        channel: DRUM_CHANNEL,
        note_id: crate::ids::NoteId(0),
    });
}

/// Shift every note back so the earliest lands at 0 — used when a groove is
/// rendered into a clip that starts at `start_tick`, since humanising can pull
/// the first hit slightly negative.
pub fn to_clip_relative(notes: &mut [MidiNote], start_tick: u64) {
    for n in notes.iter_mut() {
        n.tick = (n.tick as u64).saturating_sub(start_tick) as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(opts: &GrooveOptions, bars: usize) -> Vec<MidiNote> {
        groove_notes(Meter::FOUR_FOUR, 960, 0, bars, opts).0
    }

    /// Which grid steps of `bar` a role plays, by SNAPPING each hit to its
    /// nearest step first. Humanising moves a hit either side of the grid — a
    /// downbeat jittered two ticks early is still the downbeat, and bucketing
    /// by raw tick would file it in the previous bar.
    fn steps_of(notes: &[MidiNote], role: Role, step_ticks: u64, bar_ticks: u64, bar: u64) -> Vec<usize> {
        let steps_per_bar = (bar_ticks / step_ticks) as usize;
        let mut v: Vec<usize> = notes
            .iter()
            .filter(|n| n.key == role.gm_key())
            .map(|n| (n.tick as f64 / step_ticks as f64).round() as usize)
            .filter(|s| *s / steps_per_bar == bar as usize)
            .map(|s| s % steps_per_bar)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn the_bar_always_starts_with_a_kick() {
        for genre in Genre::ALL {
            for seed in 0..6u64 {
                let opts = GrooveOptions { genre, seed, ..Default::default() };
                let notes = gen(&opts, 2);
                let grid = Grid::new(Meter::FOUR_FOUR, 960, genre.vector().steps_per_beat);
                for bar in 0..2 {
                    let kicks =
                        steps_of(&notes, Role::Kick, grid.ticks_per_step() as u64, grid.ticks_per_bar(), bar);
                    assert!(
                        kicks.contains(&0),
                        "{} seed {seed} bar {bar}: no downbeat kick ({kicks:?})",
                        genre.id()
                    );
                }
            }
        }
    }

    #[test]
    fn the_snare_lands_on_the_derived_backbeat() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let opts = GrooveOptions { genre: Genre::Rock, humanize: 0, ..Default::default() };
        let notes = gen(&opts, 1);
        let snares = steps_of(&notes, Role::Snare, grid.ticks_per_step() as u64, grid.ticks_per_bar(), 0);
        assert_eq!(snares, grid.backbeat_steps(), "beats 2 and 4, derived from the metre");
        // 3/4: the backbeat is beats 2 AND 3, and the groove follows without a
        // special case.
        let three = Grid::new(Meter::new(3, 4), 960, 4);
        let (notes, _) = groove_notes(Meter::new(3, 4), 960, 0, 1, &opts);
        let snares = steps_of(&notes, Role::Snare, three.ticks_per_step() as u64, three.ticks_per_bar(), 0);
        assert_eq!(snares, three.backbeat_steps());
    }

    #[test]
    fn the_syncopation_dial_raises_measured_syncopation_monotonically() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let weights = grid.weights();
        let mut last = -1i32;
        for dial in [0u8, 25, 50, 75, 100] {
            let opts = GrooveOptions {
                genre: Genre::Rock,
                syncopation: Some(dial),
                density: Some(100),
                humanize: 0,
                fill_every: 0,
                ..Default::default()
            };
            let notes = gen(&opts, 1);
            let kicks = steps_of(&notes, Role::Kick, grid.ticks_per_step() as u64, grid.ticks_per_bar(), 0);
            let pattern: Vec<bool> = (0..16).map(|s| kicks.contains(&s)).collect();
            let measured = metre::syncopation(&pattern, &weights);
            assert!(
                measured >= last,
                "dial {dial}: measured {measured} < previous {last} ({kicks:?})"
            );
            last = measured;
        }
        assert!(last > 0, "the top of the dial must actually be syncopated");
    }

    #[test]
    fn density_controls_the_number_of_kicks() {
        let grid = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let opts = |d: u8| GrooveOptions {
            genre: Genre::Rock,
            density: Some(d),
            syncopation: Some(0),
            humanize: 0,
            fill_every: 0,
            ..Default::default()
        };
        let count = |d: u8| {
            steps_of(&gen(&opts(d), 1), Role::Kick, grid.ticks_per_step() as u64, grid.ticks_per_bar(), 0)
                .len()
        };
        assert_eq!(count(0), 1, "one kick a bar at the bottom");
        assert_eq!(count(100), 4, "four on the floor at the top");
        assert!(count(50) > count(0) && count(50) < count(100));
    }

    #[test]
    fn velocities_follow_metrical_weight() {
        let opts = GrooveOptions { genre: Genre::Rock, humanize: 0, fill_every: 0, ..Default::default() };
        let notes = gen(&opts, 1);
        let hats: Vec<&MidiNote> = notes.iter().filter(|n| n.key == Role::ClosedHat.gm_key()).collect();
        assert!(hats.len() >= 4);
        // The hat on the downbeat is louder than the hat on a weak eighth.
        let downbeat = hats.iter().find(|n| n.tick == 0).expect("a hat on 1");
        let weak = hats
            .iter()
            .find(|n| n.tick == 480)
            .expect("a hat on the second eighth");
        assert!(
            downbeat.velocity > weak.velocity,
            "downbeat {} vs weak {}",
            downbeat.velocity,
            weak.velocity
        );
        assert!(notes.iter().all(|n| n.velocity >= 1 && n.velocity <= 127));
    }

    #[test]
    fn fills_land_at_the_end_of_every_phrase() {
        let opts = GrooveOptions { genre: Genre::Rock, fill_every: 4, humanize: 0, ..Default::default() };
        let (notes, ann) = groove_notes(Meter::FOUR_FOUR, 960, 0, 8, &opts);
        let fill_labels: Vec<&str> = ann.iter().filter(|a| a.label.starts_with("fill")).map(|a| a.label.as_str()).collect();
        assert_eq!(fill_labels.len(), 2, "bars 4 and 8: {fill_labels:?}");
        assert!(fill_labels[0].contains("bar 4"));
        assert!(fill_labels[1].contains("bar 8"));
        // Toms only appear in fills.
        let toms: Vec<u64> = notes
            .iter()
            .filter(|n| [Role::TomHi, Role::TomMid, Role::TomLo].iter().any(|r| r.gm_key() == n.key))
            .map(|n| n.tick as u64 / 3840)
            .collect();
        assert!(toms.iter().all(|bar| *bar == 3 || *bar == 7), "{toms:?}");
        assert!(!toms.is_empty());
        // fill_every: 0 means never.
        let (_, ann) = groove_notes(
            Meter::FOUR_FOUR,
            960,
            0,
            8,
            &GrooveOptions { fill_every: 0, ..opts },
        );
        assert!(!ann.iter().any(|a| a.label.starts_with("fill")));
    }

    #[test]
    fn every_hit_is_on_the_drum_channel_with_a_gm_key() {
        let known: Vec<u8> = [
            Role::Kick, Role::Snare, Role::Rimshot, Role::Clap, Role::ClosedHat, Role::OpenHat,
            Role::Ride, Role::Crash, Role::TomHi, Role::TomMid, Role::TomLo, Role::Shaker,
        ]
        .iter()
        .map(|r| r.gm_key())
        .collect();
        for genre in Genre::ALL {
            let notes = gen(&GrooveOptions { genre, seed: 3, ..Default::default() }, 4);
            assert!(!notes.is_empty(), "{}", genre.id());
            assert!(notes.iter().all(|n| n.channel == DRUM_CHANNEL), "{}", genre.id());
            assert!(notes.iter().all(|n| known.contains(&n.key)), "{}", genre.id());
            assert!(notes.iter().all(|n| n.length_ticks > 0));
        }
    }

    #[test]
    fn genres_differ_in_what_they_play_not_in_a_hidden_pattern_bank() {
        let fingerprint = |g: Genre| {
            let notes = gen(&GrooveOptions { genre: g, humanize: 0, seed: 1, ..Default::default() }, 1);
            let mut keys: Vec<u8> = notes.iter().map(|n| n.key).collect();
            keys.sort_unstable();
            keys.dedup();
            (keys, notes.len())
        };
        let rock = fingerprint(Genre::Rock);
        let jazz = fingerprint(Genre::Jazz);
        let house = fingerprint(Genre::House);
        assert!(jazz.0.contains(&Role::Ride.gm_key()), "jazz rides");
        assert!(!jazz.0.contains(&Role::ClosedHat.gm_key()), "…instead of hats");
        assert!(house.0.contains(&Role::OpenHat.gm_key()), "house opens its hats off the beat");
        assert!(rock.0.contains(&Role::ClosedHat.gm_key()));
        assert!(fingerprint(Genre::HipHop).0.contains(&Role::Clap.gm_key()), "hip-hop claps");
        let _ = (rock.1, jazz.1, house.1);
        // …and in how much they play: the kick density is a genre parameter,
        // not a stored pattern (house is four-on-the-floor, a ballad is not).
        let kicks = |g: Genre| {
            let grid = Grid::new(Meter::FOUR_FOUR, 960, g.vector().steps_per_beat);
            steps_of(
                &gen(&GrooveOptions { genre: g, humanize: 0, fill_every: 0, seed: 1, ..Default::default() }, 1),
                Role::Kick,
                grid.ticks_per_step() as u64,
                grid.ticks_per_bar(),
                0,
            )
            .len()
        };
        assert_eq!(kicks(Genre::House), 4, "four on the floor");
        assert!(kicks(Genre::Ballad) < kicks(Genre::Metal));
    }

    #[test]
    fn swing_delays_only_the_odd_steps() {
        let opts = GrooveOptions {
            genre: Genre::Rock,
            swing: Some(66),
            humanize: 0,
            fill_every: 0,
            ..Default::default()
        };
        let notes = gen(&opts, 1);
        let hats: Vec<u32> = notes
            .iter()
            .filter(|n| n.key == Role::ClosedHat.gm_key())
            .map(|n| n.tick)
            .collect();
        assert!(hats.contains(&0), "the downbeat is never swung");
        // The second eighth (step 2 of a sixteenth grid) is even, so it stays
        // put; a hat on an odd step would be late. Rock hats sit every second
        // step, so none of them swing — which is correct and worth pinning.
        assert!(hats.iter().all(|t| t % 240 == 0), "{hats:?}");
        // Jazz rides on an eighth grid, where every second onset IS odd.
        let jazz = gen(
            &GrooveOptions { genre: Genre::Jazz, humanize: 0, fill_every: 0, ..Default::default() },
            1,
        );
        let rides: Vec<u32> = jazz.iter().filter(|n| n.key == Role::Ride.gm_key()).map(|n| n.tick).collect();
        assert!(rides.iter().any(|t| t % 480 != 0), "a swung ride is off the grid: {rides:?}");
    }

    #[test]
    fn humanising_is_bounded_and_seeded() {
        let a = gen(&GrooveOptions { humanize: 100, seed: 42, ..Default::default() }, 2);
        let b = gen(&GrooveOptions { humanize: 100, seed: 42, ..Default::default() }, 2);
        assert_eq!(a, b, "same seed, same groove");
        let straight = gen(&GrooveOptions { humanize: 0, seed: 42, ..Default::default() }, 2);
        assert_ne!(a, straight);
        // Nothing moves more than a tenth of a step (24 ticks at a 240-tick
        // sixteenth), so "humanised" never means "out of time".
        for n in &a {
            let nearest = ((n.tick as f64 / 240.0).round() * 240.0) as i64;
            assert!(
                (n.tick as i64 - nearest).abs() <= 24,
                "a hit at {} is {} ticks off the grid",
                n.tick,
                (n.tick as i64 - nearest).abs()
            );
        }
    }

    #[test]
    fn a_compound_metre_grooves_without_a_special_case() {
        let (notes, ann) = groove_notes(Meter::new(6, 8), 960, 0, 2, &GrooveOptions::default());
        assert!(!notes.is_empty());
        let grid = Grid::new(Meter::new(6, 8), 960, 4);
        let kicks = steps_of(&notes, Role::Kick, grid.ticks_per_step() as u64, grid.ticks_per_bar(), 0);
        assert!(kicks.contains(&0));
        assert!(ann[1].why.contains("6/8"));
    }

    #[test]
    fn zero_bars_is_silence_not_a_panic() {
        assert!(groove_notes(Meter::FOUR_FOUR, 960, 0, 0, &GrooveOptions::default()).0.is_empty());
    }

    #[test]
    fn annotations_explain_the_mechanism_with_its_numbers() {
        let (_, ann) = groove_notes(Meter::FOUR_FOUR, 960, 0, 4, &GrooveOptions::default());
        assert!(ann[0].label.starts_with("kick"));
        assert!(ann[0].why.contains("Bjorklund"), "{}", ann[0].why);
        assert!(ann[0].why.contains("Longuet-Higgins"), "{}", ann[0].why);
        assert!(ann[1].why.contains("beats 2 and 4"), "{}", ann[1].why);
        assert!(ann.iter().all(|a| a.why.len() > 40));
    }
}
