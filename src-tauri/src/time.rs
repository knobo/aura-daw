//! Compile-time-distinct time domains (round-2 §3.1, ADR 0002). MIDI/
//! musical positions are `Ticks`; audio clip anchors and engine positions
//! are `Samples`. No `Ord` crosses domains and there is no ambient global
//! tempo map — conversion is a named call on `midi::tempo::TempoMap`
//! (O-4). This module owns the newtypes and the supertick constant only;
//! the bijection itself lives in `midi::tempo` (Task 3).

use serde::{Deserialize, Serialize};

/// Musical position/duration, ticks at the project PPQ.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Ticks(pub u64);

/// Audio-engine position/duration, samples at the project sample rate.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Samples(pub u64);

impl Ticks {
    pub fn checked_add(self, rhs: Ticks) -> Option<Ticks> {
        self.0.checked_add(rhs.0).map(Ticks)
    }
    pub fn checked_sub(self, rhs: Ticks) -> Option<Ticks> {
        self.0.checked_sub(rhs.0).map(Ticks)
    }
}
impl Samples {
    pub fn checked_add(self, rhs: Samples) -> Option<Samples> {
        self.0.checked_add(rhs.0).map(Samples)
    }
    pub fn checked_sub(self, rhs: Samples) -> Option<Samples> {
        self.0.checked_sub(rhs.0).map(Samples)
    }
}

/// Ardour's superclock constant: 2^10 * 3^4 * 5^3 * 7^2 per second,
/// divisible by every common sample rate and PPQ (round-2 §3.3).
pub const SUPERTICKS_PER_SECOND: u64 = 508_032_000;

/// Cross-domain comparison, explicit and never `Ord` (round-2 §3.1):
/// comparing a `Ticks` and a `Samples` value needs a tempo map to make
/// them commensurable, so every call site names one. Implemented in this
/// module too (Task 3 needs `TempoMap::tick_to_samples_v3`, which doesn't
/// exist yet — the trait is declared now, WITHOUT impls, so it compiles
/// standalone; Task 3 adds the two impls once the bijection exists).
pub trait CmpIn<Other> {
    fn cmp_in(&self, other: &Other, map: &crate::midi::tempo::TempoMap) -> std::cmp::Ordering;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_and_samples_are_distinct_types_with_checked_arithmetic() {
        let a = Ticks(100);
        let b = Ticks(40);
        assert_eq!(a.checked_sub(b), Some(Ticks(60)));
        assert_eq!(b.checked_sub(a), None, "underflow is None, never a wrap");
        assert_eq!(a.checked_add(b), Some(Ticks(140)));
        assert_eq!(Ticks(u64::MAX).checked_add(Ticks(1)), None);

        let s = Samples(48_000);
        assert_eq!(s.checked_add(Samples(2_000)), Some(Samples(50_000)));

        // Serialize transparently (a bare number, round-2 §3.6).
        assert_eq!(serde_json::to_string(&a).unwrap(), "100");
        let back: Ticks = serde_json::from_str("100").unwrap();
        assert_eq!(back, a);
    }
}
