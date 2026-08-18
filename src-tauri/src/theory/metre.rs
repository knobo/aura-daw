//! Metre: the weight hierarchy, Euclidean onsets, and measurable syncopation.
//!
//! This is the most formalised corner of music theory, which is why "a drum
//! generator based on music theory" turns out to be a real claim rather than a
//! slogan (product doc §4.7).
//!
//! **One weight function, five uses (ruling H-10).** A bar subdivides by 2s
//! and 3s into a tree (Lerdahl & Jackendoff, *A Generative Theory of Tonal
//! Music*); a position's weight is the number of tree levels it sits on. In
//! 4/4 at sixteenths that is `4 0 1 0 | 2 0 1 0 | 3 0 1 0 | 2 0 1 0`. Kick
//! placement, the backbeat (beats 2 and 4 *are* the weight-2 positions), hat
//! accents, ghost-note placement and every velocity in the app come out of
//! this one array. A generator that invents its own accent rule is a bug.
//!
//! **Syncopation is a number** (Longuet-Higgins & Lee 1984), so the slider is
//! not a vibe: a note on a weak position followed by nothing on a stronger one
//! scores the weight difference, and the generator can aim at a target.
//!
//! **Euclidean onsets** (Bjorklund's algorithm; Toussaint 2005, *The Euclidean
//! Algorithm Generates Traditional Musical Rhythms*) distribute k onsets as
//! evenly as possible over n steps. `E(3,8)` is the tresillo, `E(5,8)` the
//! cinquillo, and rotations of one pattern are a whole related family — son
//! and rumba clave are rotations of each other. Two integers and a rotation,
//! instead of a pattern library.

/// A time signature. Kept separate from `crate::midi::MeterEvent` (which is
/// document state at a tick) so this module stays free of project types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meter {
    pub num: u8,
    pub den: u8,
}

impl Meter {
    pub const FOUR_FOUR: Meter = Meter { num: 4, den: 4 };

    pub fn new(num: u8, den: u8) -> Self {
        Self { num: num.max(1), den: den.max(1) }
    }

    /// Compound metre: the notated beat groups in threes (6/8, 9/8, 12/8), so
    /// the *felt* beat is a dotted note and the tree has a level the numerator
    /// alone does not show.
    pub fn is_compound(&self) -> bool {
        self.den >= 8 && self.num % 3 == 0 && self.num > 3
    }

    /// Quarter notes per bar — the bridge to ticks (PPQ is per quarter).
    pub fn quarters_per_bar(&self) -> f64 {
        self.num as f64 * 4.0 / self.den as f64
    }
}

/// A quantisation grid over one bar: the metre, the project PPQ, and how many
/// steps each notated beat is divided into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub meter: Meter,
    pub ppq: u32,
    /// Steps per denominator unit. `4` over a 4/4 bar is sixteenths; `2` over
    /// a 6/8 bar is also sixteenths.
    pub steps_per_beat: u32,
}

impl Grid {
    pub fn new(meter: Meter, ppq: u32, steps_per_beat: u32) -> Self {
        Self { meter, ppq: ppq.max(1), steps_per_beat: steps_per_beat.max(1) }
    }

    pub fn steps_per_bar(&self) -> usize {
        self.meter.num as usize * self.steps_per_beat as usize
    }

    /// Ticks per grid step. Integer by construction for every PPQ that divides
    /// cleanly (960 covers every subdivision up to 10ths and their dotted and
    /// triplet forms); anything else truncates, which is why the accessor
    /// exists rather than the arithmetic being inlined at call sites.
    pub fn ticks_per_step(&self) -> u32 {
        (self.ppq * 4 / self.meter.den as u32 / self.steps_per_beat).max(1)
    }

    pub fn ticks_per_bar(&self) -> u64 {
        self.ticks_per_step() as u64 * self.steps_per_bar() as u64
    }

    pub fn tick_of_step(&self, step: usize) -> u64 {
        step as u64 * self.ticks_per_step() as u64
    }

    /// The metrical tree's periods, coarsest first, ending above 1. Each entry
    /// is a level: a step whose index is divisible by the period sits on it.
    pub fn levels(&self) -> Vec<usize> {
        let total = self.steps_per_bar();
        let mut divisors: Vec<usize> = Vec::new();
        if self.meter.is_compound() {
            // The felt beat is three notated ones: split the bar into groups
            // FIRST (12/8 → 2 halves → 4 groups of 3), so the dotted-beat
            // level exists in the tree.
            divisors.extend(factor_2s_then_3s(self.meter.num as usize / 3));
            divisors.push(3);
        } else {
            divisors.extend(factor_2s_then_3s(self.meter.num as usize));
        }
        divisors.extend(factor_2s_then_3s(self.steps_per_beat as usize));
        let mut periods = Vec::with_capacity(divisors.len() + 1);
        let mut p = total;
        periods.push(p);
        for d in divisors {
            if d <= 1 {
                continue;
            }
            p /= d;
            if p > 1 {
                periods.push(p);
            }
        }
        periods
    }

    /// Metrical weight per step of one bar: how many tree levels the step sits
    /// on. Higher = stronger.
    pub fn weights(&self) -> Vec<u8> {
        let periods = self.levels();
        (0..self.steps_per_bar())
            .map(|s| periods.iter().filter(|p| s % **p == 0).count() as u8)
            .collect()
    }

    /// The strongest weight this grid produces — the denominator for anything
    /// that scales with weight (velocity, choice probability).
    pub fn max_weight(&self) -> u8 {
        self.levels().len() as u8
    }

    /// The steps a backbeat lands on: the strong beats that are NOT the
    /// downbeat. In 4/4 sixteenths that is steps 4 and 12 — beats 2 and 4,
    /// derived rather than hard-coded.
    pub fn backbeat_steps(&self) -> Vec<usize> {
        let w = self.weights();
        let beat = self.steps_per_beat as usize;
        let beat_weight = w.get(beat).copied().unwrap_or(0);
        (0..self.steps_per_bar())
            .filter(|s| *s > 0 && s % beat == 0 && w[*s] <= beat_weight)
            .collect()
    }

    /// Steps that are notated beats (weight at or above the beat level).
    pub fn beat_steps(&self) -> Vec<usize> {
        (0..self.steps_per_bar())
            .step_by(self.steps_per_beat as usize)
            .collect()
    }
}

/// 4s and 8s before 3s: `4 → [2,2]`, `6 → [2,3]`, `12 → [2,2,3]`, `5 → [5]`.
/// A prime numerator stays one level, which is the honest reading of 5/4 or
/// 7/8 without a grouping annotation.
fn factor_2s_then_3s(mut n: usize) -> Vec<usize> {
    let mut out = Vec::new();
    while n % 2 == 0 && n > 1 {
        out.push(2);
        n /= 2;
    }
    while n % 3 == 0 && n > 1 {
        out.push(3);
        n /= 3;
    }
    if n > 1 {
        out.push(n);
    }
    out
}

/// Bjorklund's algorithm: `k` onsets distributed as evenly as possible over
/// `n` steps. Implemented as the paper describes (pair sequences off against
/// each other until at most one remainder is left) rather than with a
/// closed-form approximation — the approximations agree with it on `E(3,8)`
/// and disagree on `E(5,8)`, and the whole point of the function is that it
/// reproduces the traditional rhythms exactly.
pub fn euclid(k: usize, n: usize) -> Vec<bool> {
    if n == 0 {
        return Vec::new();
    }
    if k == 0 {
        return vec![false; n];
    }
    if k >= n {
        return vec![true; n];
    }
    let mut a: Vec<Vec<bool>> = vec![vec![true]; k];
    let mut b: Vec<Vec<bool>> = vec![vec![false]; n - k];
    while b.len() > 1 {
        let m = a.len().min(b.len());
        let mut next_a: Vec<Vec<bool>> = Vec::with_capacity(m);
        for i in 0..m {
            let mut seq = a[i].clone();
            seq.extend_from_slice(&b[i]);
            next_a.push(seq);
        }
        let next_b: Vec<Vec<bool>> =
            if a.len() > m { a[m..].to_vec() } else { b[m..].to_vec() };
        a = next_a;
        b = next_b;
    }
    a.into_iter().chain(b).flatten().collect()
}

/// Rotate a cyclic pattern left by `by` steps (`result[i] = p[(i + by) mod n]`),
/// so a positive rotation moves the pattern earlier. Son clave and rumba clave
/// are rotations of each other; so are most of a groove's variations.
pub fn rotate(pattern: &[bool], by: isize) -> Vec<bool> {
    let n = pattern.len();
    if n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| pattern[((i as isize + by).rem_euclid(n as isize)) as usize])
        .collect()
}

/// Longuet-Higgins & Lee syncopation: for every onset, if the strongest REST
/// before the next onset sits on a stronger metrical position than the onset
/// itself, add the difference. Cyclic — a bar loops, so the last onset is
/// measured against the wrap.
///
/// Straight patterns score 0. Displacing an onset off a strong position and
/// leaving that position empty is exactly what raises the number, which is
/// what makes it a usable dial.
pub fn syncopation(pattern: &[bool], weights: &[u8]) -> i32 {
    let n = pattern.len().min(weights.len());
    if n == 0 {
        return 0;
    }
    let mut total = 0i32;
    for i in 0..n {
        if !pattern[i] {
            continue;
        }
        // Scan forward (cyclically) to the next onset, tracking the strongest
        // rest on the way.
        let mut strongest_rest = 0u8;
        let mut found_rest = false;
        for d in 1..=n {
            let j = (i + d) % n;
            if pattern[j] {
                break;
            }
            if weights[j] > strongest_rest {
                strongest_rest = weights[j];
                found_rest = true;
            }
        }
        if found_rest && strongest_rest > weights[i] {
            total += (strongest_rest - weights[i]) as i32;
        }
    }
    total
}

/// Velocity from metrical weight (ruling H-10): `floor` at `base`, reaching
/// `base + accent` on the strongest position. Every generator's velocities go
/// through here, which is why a groove and a comp part accent together.
pub fn velocity_for_weight(weight: u8, max_weight: u8, base: u8, accent: u8) -> u8 {
    if max_weight == 0 {
        return base;
    }
    let scaled = accent as u32 * weight as u32 / max_weight as u32;
    (base as u32 + scaled).clamp(1, 127) as u8
}

/// Swing: the tick offset to add to a step. Every ODD step of a pair is pushed
/// later by `amount` of the way toward the next step (0.0 = straight,
/// 1/3 ≈ triplet swing, 0.5 = a dotted feel).
pub fn swing_offset(step: usize, ticks_per_step: u32, amount: f32) -> i64 {
    if step % 2 == 0 || amount <= 0.0 {
        return 0;
    }
    (ticks_per_step as f32 * amount.clamp(0.0, 0.9)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_four_sixteenths_give_the_textbook_weight_array() {
        let g = Grid::new(Meter::FOUR_FOUR, 960, 4);
        assert_eq!(g.steps_per_bar(), 16);
        assert_eq!(g.ticks_per_step(), 240);
        assert_eq!(g.ticks_per_bar(), 3840);
        assert_eq!(
            g.weights(),
            vec![4, 0, 1, 0, 2, 0, 1, 0, 3, 0, 1, 0, 2, 0, 1, 0],
            "bar > half > beat > eighth > sixteenth"
        );
        assert_eq!(g.max_weight(), 4);
    }

    #[test]
    fn three_four_has_no_half_bar_level() {
        let g = Grid::new(Meter::new(3, 4), 960, 4);
        assert_eq!(g.weights(), vec![3, 0, 1, 0, 2, 0, 1, 0, 2, 0, 1, 0]);
        assert_eq!(g.backbeat_steps(), vec![4, 8], "beats 2 and 3 are equally strong");
    }

    #[test]
    fn six_eight_groups_by_three() {
        let g = Grid::new(Meter::new(6, 8), 960, 2);
        assert!(g.meter.is_compound());
        assert_eq!(g.steps_per_bar(), 12);
        assert_eq!(g.ticks_per_step(), 240, "sixteenths at ppq 960");
        // Strong at 1 and 4 (steps 0 and 6) — the two dotted-quarter beats.
        assert_eq!(g.weights(), vec![3, 0, 1, 0, 1, 0, 2, 0, 1, 0, 1, 0]);
    }

    #[test]
    fn twelve_eight_keeps_its_half_bar_level() {
        let g = Grid::new(Meter::new(12, 8), 960, 1);
        assert_eq!(g.steps_per_bar(), 12);
        // bar(12) > half(6) > dotted beat(3): 12/8 is four groups of three.
        assert_eq!(g.weights(), vec![3, 0, 0, 1, 0, 0, 2, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn an_odd_metre_stays_one_level() {
        let g = Grid::new(Meter::new(5, 4), 960, 2);
        assert_eq!(g.weights(), vec![2, 0, 1, 0, 1, 0, 1, 0, 1, 0]);
    }

    #[test]
    fn backbeat_is_derived_not_hard_coded() {
        let g = Grid::new(Meter::FOUR_FOUR, 960, 4);
        assert_eq!(g.backbeat_steps(), vec![4, 12], "beats 2 and 4");
        assert_eq!(g.beat_steps(), vec![0, 4, 8, 12]);
    }

    #[test]
    fn euclid_reproduces_the_traditional_rhythms() {
        fn s(p: &[bool]) -> String {
            p.iter().map(|b| if *b { 'x' } else { '.' }).collect()
        }
        assert_eq!(s(&euclid(3, 8)), "x..x..x.", "tresillo");
        assert_eq!(s(&euclid(5, 8)), "x.xx.xx.", "cinquillo");
        assert_eq!(s(&euclid(2, 5)), "x.x..");
        assert_eq!(s(&euclid(4, 16)), "x...x...x...x...", "four on the floor");
        assert_eq!(s(&euclid(7, 12)), "x.xx.x.xx.x.", "West African bell pattern");
        assert_eq!(s(&euclid(0, 4)), "....");
        assert_eq!(s(&euclid(4, 4)), "xxxx");
        assert_eq!(s(&euclid(9, 4)), "xxxx", "more onsets than steps saturates");
        assert!(euclid(3, 0).is_empty());
        // Onsets are conserved and evenly spread: no two gaps differ by more
        // than one step, which is the defining property.
        for n in 2..=16 {
            for k in 1..n {
                let p = euclid(k, n);
                assert_eq!(p.iter().filter(|b| **b).count(), k, "E({k},{n}) onset count");
                let idx: Vec<usize> = p.iter().enumerate().filter(|(_, b)| **b).map(|(i, _)| i).collect();
                let gaps: Vec<usize> = (0..idx.len())
                    .map(|i| (idx[(i + 1) % idx.len()] + n - idx[i]) % n)
                    .map(|g| if g == 0 { n } else { g })
                    .collect();
                let (lo, hi) = (gaps.iter().min().unwrap(), gaps.iter().max().unwrap());
                assert!(hi - lo <= 1, "E({k},{n}) gaps {gaps:?} are not maximally even");
            }
        }
    }

    #[test]
    fn rotation_is_cyclic_and_length_preserving() {
        let p = euclid(3, 8);
        assert_eq!(rotate(&p, 0), p);
        assert_eq!(rotate(&p, 8), p, "a full turn is identity");
        assert_eq!(rotate(&rotate(&p, 3), -3), p);
        assert_eq!(rotate(&p, 3).len(), 8);
        assert_eq!(rotate(&p, 3).iter().filter(|b| **b).count(), 3);
        // Rotating the tresillo by three lands the second onset on the
        // downbeat.
        assert!(rotate(&p, 3)[0]);
    }

    #[test]
    fn syncopation_is_zero_for_straight_and_positive_for_displaced() {
        let g = Grid::new(Meter::FOUR_FOUR, 960, 2); // eighths
        let w = g.weights();
        let four_on_the_floor = vec![true, false, false, false, true, false, false, false];
        assert_eq!(syncopation(&four_on_the_floor, &w), 0, "on the beat is never syncopated");
        let all = vec![true; 8];
        assert_eq!(syncopation(&all, &w), 0, "no rests, no syncopation");
        let tresillo = euclid(3, 8);
        assert!(syncopation(&tresillo, &w) > 0, "the tresillo is syncopated by construction");
        // The extreme case: play ONLY the weak eighths and leave every strong
        // position empty.
        let offbeats = vec![false, true, false, true, false, true, false, true];
        assert!(
            syncopation(&offbeats, &w) > syncopation(&tresillo, &w),
            "all-offbeat is more syncopated than the tresillo"
        );
        assert_eq!(syncopation(&[], &w), 0);
    }

    #[test]
    fn velocity_tracks_weight() {
        let g = Grid::new(Meter::FOUR_FOUR, 960, 4);
        let w = g.weights();
        let max = g.max_weight();
        let downbeat = velocity_for_weight(w[0], max, 60, 60);
        let sixteenth = velocity_for_weight(w[1], max, 60, 60);
        let beat = velocity_for_weight(w[4], max, 60, 60);
        assert_eq!(downbeat, 120);
        assert_eq!(sixteenth, 60);
        assert_eq!(beat, 90);
        assert!(downbeat > beat && beat > sixteenth);
        assert_eq!(velocity_for_weight(4, 4, 100, 60), 127, "clamped, never wrapped");
    }

    #[test]
    fn swing_pushes_only_the_odd_steps() {
        assert_eq!(swing_offset(0, 240, 0.5), 0);
        assert_eq!(swing_offset(1, 240, 0.0), 0, "no swing, no offset");
        assert_eq!(swing_offset(1, 240, 1.0 / 3.0), 80, "triplet swing");
        assert_eq!(swing_offset(2, 240, 0.5), 0);
        assert_eq!(swing_offset(3, 240, 0.5), 120);
    }
}
