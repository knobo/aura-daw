//! The one seeded PRNG the generators share (ruling H-4: determinism is a
//! contract).
//!
//! SplitMix64 — Vigna's, the algorithm `std`'s own `SipHasher`-free
//! `SmallRng` seeding uses and the one Java's `SplittableRandom` ships. It is
//! nine lines, has no state beyond a `u64`, and passes BigCrush; nothing here
//! needs cryptographic quality, it needs to be *the same next year*.
//!
//! Deliberately NOT `rand::thread_rng()` (which the crate does depend on, for
//! MCP tokens): a generator seeded from the ambient thread cannot be tested,
//! cannot be reproduced from a stored seed, and cannot let a user say "I liked
//! take 3".

/// A reproducible stream. `Clone` on purpose: a generator that wants to try
/// two continuations from the same point clones rather than re-seeds.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Any seed is legal, including 0 — SplitMix64's increment makes the
        // zero state a normal one, unlike xorshift's.
        Self { state: seed }
    }

    /// A derived stream, so one part's choices cannot shift another's: the
    /// melody's rhythm and its pitches draw from `seed ^ tag`-derived
    /// streams, and adding a part later does not renumber the others.
    pub fn stream(seed: u64, tag: &str) -> Self {
        let mut h = 0xcbf2_9ce4_8422_2325u64; // FNV-1a offset basis
        for b in tag.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self::new(seed ^ h)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (Lemire's multiply-shift; `n == 0` yields 0 rather
    /// than panicking, because callers reach it with empty candidate lists at
    /// the edges and a panic in a generator is a dead UI).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }

    /// Uniform in `0.0..1.0`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// `true` with probability `p`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    /// Uniform inclusive integer range.
    pub fn range(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        lo + self.below((hi - lo + 1) as usize) as i32
    }

    /// Pick one element. `None` only for an empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        items.get(self.below(items.len()))
    }

    /// Pick an index by weight (weights need not sum to anything; negatives
    /// are treated as zero). Falls back to the last index when every weight is
    /// zero, which is a deliberate "something rather than nothing".
    pub fn weighted(&mut self, weights: &[f32]) -> usize {
        let total: f32 = weights.iter().map(|w| w.max(0.0)).sum();
        if total <= 0.0 {
            return weights.len().saturating_sub(1);
        }
        let mut r = self.unit() * total;
        for (i, w) in weights.iter().enumerate() {
            r -= w.max(0.0);
            if r <= 0.0 {
                return i;
            }
        }
        weights.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitmix64_matches_the_reference_vector() {
        // Reference: SplitMix64 seeded with 0 (Vigna's published sequence).
        let mut r = Rng::new(0);
        assert_eq!(r.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(r.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(r.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn the_same_seed_is_the_same_stream() {
        let a: Vec<u64> = (0..8).map(|_| Rng::new(42).next_u64()).collect();
        assert!(a.windows(2).all(|w| w[0] == w[1]), "a fresh Rng(42) always starts the same");
        let mut x = Rng::new(7);
        let mut y = Rng::new(7);
        assert_eq!(
            (0..64).map(|_| x.next_u64()).collect::<Vec<_>>(),
            (0..64).map(|_| y.next_u64()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn derived_streams_differ_but_stay_reproducible() {
        let a = Rng::stream(1, "melody").next_u64();
        let b = Rng::stream(1, "drums").next_u64();
        assert_ne!(a, b, "one seed, independent parts");
        assert_eq!(a, Rng::stream(1, "melody").next_u64());
    }

    #[test]
    fn below_stays_in_range_and_covers_it() {
        let mut r = Rng::new(9);
        let mut seen = [false; 5];
        for _ in 0..500 {
            let v = r.below(5);
            assert!(v < 5);
            seen[v] = true;
        }
        assert!(seen.iter().all(|s| *s), "every bucket reachable");
        assert_eq!(Rng::new(0).below(0), 0, "no panic on an empty range");
        assert_eq!(Rng::new(0).below(1), 0);
    }

    #[test]
    fn weighted_respects_zero_weights() {
        let mut r = Rng::new(3);
        for _ in 0..200 {
            assert_eq!(r.weighted(&[0.0, 1.0, 0.0]), 1, "only the non-zero option");
        }
        // All-zero weights must still return an index, not panic.
        assert!(r.weighted(&[0.0, 0.0]) < 2);
    }

    #[test]
    fn unit_and_chance_are_bounded() {
        let mut r = Rng::new(11);
        for _ in 0..1000 {
            let u = r.unit();
            assert!((0.0..1.0).contains(&u), "{u}");
        }
        let mut r = Rng::new(11);
        assert!(!r.chance(0.0));
        let mut r = Rng::new(11);
        assert!(r.chance(1.0));
    }

    #[test]
    fn range_is_inclusive_and_tolerates_inversion() {
        let mut r = Rng::new(5);
        for _ in 0..200 {
            let v = r.range(-3, 3);
            assert!((-3..=3).contains(&v));
        }
        assert_eq!(r.range(4, 4), 4);
        assert_eq!(r.range(9, 2), 9, "an inverted range yields its low end");
    }
}
