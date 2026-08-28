//! `ClockTable`: N independent playheads, one per graph (Plan V, V-4).
//!
//! This replaces the launch overlay's single atomic set
//! (`launch_on`/`launch_pos`/`launch_start`/`launch_end` + `FLAG_LAUNCH`),
//! which could only ever express ONE sounding thing: two pads could not
//! sound at once and a retrigger rewound whatever was playing.
//!
//! Two vectors and nothing else:
//!
//! * `clocks` — the playheads. Index [`TRANSPORT_CLOCK`] is the transport:
//!   its position comes from the callback (`base_pos`), never from here,
//!   and its `on` flag is the transport's play state (V-13). Every other
//!   index is a player's or a scene's, and owns its own position.
//! * `slot_clock` — which clock each MIXER SLOT reads. A player's entry is
//!   written once at graph build; a scene's is written at FIRE time, which
//!   is why it is atomic: a pad press must never rebuild the graph.
//!
//! Sized per-graph and built on the control thread, exactly like
//! `ParamTable` (`rt.rs`, round-2 §2.4) — a retired graph keeps reading the
//! table it was built with, so a renumbering cannot bleed into it. Nothing
//! here allocates, locks or blocks after construction.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

use crate::audio::transport::LoopSpec;

/// The transport's clock. Its position is the callback's `base_pos`; its
/// `on` flag is the transport's play state (V-13).
pub const TRANSPORT_CLOCK: u32 = 0;

/// What one node reads for one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Playhead {
    pub pos: u64,
    /// The clock's own loop is a start/end pair, not a `LoopSpec`; the
    /// arrangement's `LoopSpec` applies only to the transport clock.
    pub looping: bool,
    /// This block does not continue the previous one — a live node owes an
    /// `all_notes_off` before it processes.
    pub discontinuity: bool,
    /// False = render nothing at all for this node this block.
    pub on: bool,
    /// True for [`TRANSPORT_CLOCK`], so the caller knows the arrangement's
    /// `LoopSpec` and automation ramps apply.
    pub is_transport: bool,
}

struct ClockState {
    on: AtomicBool,
    pos: AtomicU64,
    start: AtomicU64,
    end: AtomicU64,
    looping: AtomicBool,
    /// Set by `fire` and by a loop wrap; consumed by the first `playhead`
    /// that reads it, exactly as the overlay's `launch_discont` was.
    discont: AtomicBool,
}

impl ClockState {
    fn idle() -> Self {
        Self {
            on: AtomicBool::new(false),
            pos: AtomicU64::new(0),
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
            looping: AtomicBool::new(false),
            discont: AtomicBool::new(false),
        }
    }
}

pub struct ClockTable {
    clocks: Vec<ClockState>,
    slot_clock: Vec<AtomicU32>,
}

impl Default for ClockTable {
    /// One transport clock and no slots — what an empty or headless graph
    /// needs, and what every test that does not care about clocks gets.
    fn default() -> Self {
        Self::with_slots_and_clocks(0, 1)
    }
}

impl ClockTable {
    /// A table for `n_slots` mixer slots and `n_clocks` clocks (at least
    /// one, which is the transport's). Every slot starts on the transport.
    pub fn with_slots_and_clocks(n_slots: usize, n_clocks: usize) -> Self {
        Self {
            clocks: (0..n_clocks.max(1)).map(|_| ClockState::idle()).collect(),
            slot_clock: (0..n_slots).map(|_| AtomicU32::new(TRANSPORT_CLOCK)).collect(),
        }
    }

    pub fn clocks(&self) -> usize {
        self.clocks.len()
    }

    pub fn slots(&self) -> usize {
        self.slot_clock.len()
    }

    #[inline]
    pub fn clock_of(&self, slot: usize) -> u32 {
        self.slot_clock.get(slot).map_or(TRANSPORT_CLOCK, |c| c.load(Relaxed))
    }

    /// V-13. Called by the control plane on play/stop.
    pub fn set_transport_playing(&self, playing: bool) {
        if let Some(c) = self.clocks.first() {
            c.on.store(playing, Relaxed);
        }
    }

    /// Start a clock at `start`, running to `end`. Retrigger rewinds THIS
    /// clock and nothing else — which is the whole difference from the
    /// overlay it replaces.
    pub fn fire(&self, clock: u32, start: u64, end: u64, looping: bool) {
        let Some(c) = self.clocks.get(clock as usize) else { return };
        if clock == TRANSPORT_CLOCK {
            return; // the transport is driven by the callback, not fired
        }
        c.start.store(start, Relaxed);
        c.end.store(end.max(start.saturating_add(1)), Relaxed);
        c.pos.store(start, Relaxed);
        c.looping.store(looping, Relaxed);
        c.discont.store(true, Relaxed);
        c.on.store(true, Relaxed);
    }

    pub fn stop(&self, clock: u32) {
        let Some(c) = self.clocks.get(clock as usize) else { return };
        if clock == TRANSPORT_CLOCK {
            return;
        }
        c.on.store(false, Relaxed);
    }

    pub fn is_on(&self, clock: u32) -> bool {
        self.clocks.get(clock as usize).is_some_and(|c| c.on.load(Relaxed))
    }

    /// Any non-transport clock running: what tells the output callback to
    /// render a graph even though the transport is stopped.
    pub fn any_running(&self) -> bool {
        self.clocks.iter().skip(1).any(|c| c.on.load(Relaxed))
    }

    /// Point a mixer slot at a clock. Last writer wins (V-14).
    pub fn bind_slot(&self, slot: usize, clock: u32) {
        if clock as usize >= self.clocks.len() {
            return;
        }
        if let Some(c) = self.slot_clock.get(slot) {
            c.store(clock, Relaxed);
        }
    }

    /// Return a slot to the transport, but ONLY if it still reads `clock`
    /// (V-14): a scene that ends must not steal back a track another scene
    /// has since claimed. Returns whether the release happened.
    pub fn release_slot_if(&self, slot: usize, clock: u32) -> bool {
        let Some(c) = self.slot_clock.get(slot) else { return false };
        c.compare_exchange(clock, TRANSPORT_CLOCK, Relaxed, Relaxed).is_ok()
    }

    /// What `slot` reads this block. `base_pos` and `lp` are the
    /// transport's, used only when the slot is on [`TRANSPORT_CLOCK`].
    #[inline]
    pub fn playhead(&self, slot: usize, base_pos: u64, _lp: &LoopSpec, disc: bool) -> Playhead {
        if slot >= self.slot_clock.len() {
            return Playhead {
                pos: base_pos,
                looping: false,
                discontinuity: disc,
                on: false,
                is_transport: true,
            };
        }
        let idx = self.clock_of(slot);
        let Some(c) = self.clocks.get(idx as usize) else {
            return Playhead {
                pos: base_pos,
                looping: false,
                discontinuity: disc,
                on: false,
                is_transport: true,
            };
        };
        let on = c.on.load(Relaxed);
        if idx == TRANSPORT_CLOCK {
            return Playhead {
                pos: base_pos,
                looping: false,
                discontinuity: disc,
                on,
                is_transport: true,
            };
        }
        Playhead {
            pos: c.pos.load(Relaxed),
            looping: c.looping.load(Relaxed),
            discontinuity: c.discont.swap(false, Relaxed),
            on,
            is_transport: false,
        }
    }

    /// Advance every running non-transport clock by `frames`. The transport
    /// is advanced by the callback that owns `SharedRt::position`, never
    /// here — two writers on one playhead is the bug this whole table
    /// exists to make impossible.
    ///
    /// Called once per output block, after the render.
    pub fn advance(&self, frames: u64) {
        for c in self.clocks.iter().skip(1) {
            if !c.on.load(Relaxed) {
                continue;
            }
            let start = c.start.load(Relaxed);
            let end = c.end.load(Relaxed);
            let next = c.pos.load(Relaxed).saturating_add(frames);
            if next < end {
                c.pos.store(next, Relaxed);
                continue;
            }
            if c.looping.load(Relaxed) {
                let span = end.saturating_sub(start).max(1);
                c.pos.store(start + (next - start) % span, Relaxed);
                c.discont.store(true, Relaxed);
            } else {
                c.pos.store(end, Relaxed);
                c.on.store(false, Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::transport::LoopSpec;

    fn table() -> ClockTable {
        ClockTable::with_slots_and_clocks(4, 3)
    }

    #[test]
    fn a_slot_defaults_to_the_transport_clock() {
        let t = table();
        t.set_transport_playing(true);
        let ph = t.playhead(0, 5_000, &LoopSpec::OFF, false);
        assert_eq!(t.clock_of(0), TRANSPORT_CLOCK);
        assert_eq!(ph.pos, 5_000);
        assert!(ph.on);
        assert!(ph.is_transport);
    }

    /// V-13: clock 0's `on` flag IS the transport's play state, so "only
    /// launched nodes render while stopped" needs no separate concept.
    #[test]
    fn a_stopped_transport_silences_transport_slots_but_not_a_running_clock() {
        let t = table();
        t.set_transport_playing(false);
        t.fire(1, 0, 1_000, false);
        t.bind_slot(2, 1);

        assert!(!t.playhead(0, 5_000, &LoopSpec::OFF, false).on);
        let ph = t.playhead(2, 5_000, &LoopSpec::OFF, false);
        assert!(ph.on);
        assert_eq!(ph.pos, 0);
        assert!(!ph.is_transport);
    }

    #[test]
    fn a_fired_clock_starts_at_its_start_and_reports_a_discontinuity_once() {
        let t = table();
        t.fire(1, 400, 900, false);
        t.bind_slot(1, 1);
        let first = t.playhead(1, 0, &LoopSpec::OFF, false);
        assert_eq!(first.pos, 400);
        assert!(first.discontinuity, "the press is a jump");
        let second = t.playhead(1, 0, &LoopSpec::OFF, false);
        assert!(!second.discontinuity, "consumed exactly once");
    }

    #[test]
    fn advance_moves_running_clocks_and_stops_one_at_its_end() {
        let t = table();
        t.fire(1, 0, 100, false);
        t.bind_slot(1, 1);
        t.advance(64);
        assert_eq!(t.playhead(1, 0, &LoopSpec::OFF, false).pos, 64);
        t.advance(64);
        assert!(!t.playhead(1, 0, &LoopSpec::OFF, false).on, "past its end");
        assert!(!t.any_running());
    }

    #[test]
    fn a_looping_clock_wraps_to_its_start_instead_of_ending() {
        let t = table();
        t.fire(1, 0, 100, true);
        t.bind_slot(1, 1);
        t.advance(96);
        t.advance(32);
        let ph = t.playhead(1, 0, &LoopSpec::OFF, false);
        assert!(ph.on, "a loop does not end");
        assert_eq!(ph.pos, 28, "wrapped: 128 - 100");
        assert!(ph.discontinuity, "the wrap is a jump the live node must hear");
    }

    #[test]
    fn advance_never_moves_the_transport_clock() {
        let t = table();
        t.set_transport_playing(true);
        t.advance(64);
        assert_eq!(
            t.playhead(0, 5_000, &LoopSpec::OFF, false).pos,
            5_000,
            "the callback owns the transport position, not this table"
        );
    }

    /// V-14: two scenes may name the same track now that scenes are not
    /// singular, so a release must not steal a slot someone else has claimed.
    #[test]
    fn release_only_frees_a_slot_still_bound_to_the_releasing_clock() {
        let t = table();
        t.bind_slot(3, 1);
        t.bind_slot(3, 2); // a second scene claims it
        assert!(!t.release_slot_if(3, 1), "clock 1 no longer owns it");
        assert_eq!(t.clock_of(3), 2);
        assert!(t.release_slot_if(3, 2));
        assert_eq!(t.clock_of(3), TRANSPORT_CLOCK);
    }

    #[test]
    fn out_of_range_indices_are_dropped_not_panics() {
        let t = table();
        t.fire(99, 0, 10, false);
        t.bind_slot(99, 1);
        t.stop(99);
        assert!(!t.release_slot_if(99, 1));
        let ph = t.playhead(99, 1_234, &LoopSpec::OFF, false);
        assert!(!ph.on, "a slot outside the table renders nothing");
    }
}
