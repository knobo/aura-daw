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
//!   is why it is atomic: a pad press must never rebuild the graph. A
//!   scene clock is deliberately many-to-one: every track the region
//!   names binds to the same clock, so N slots can share one playhead.
//!
//! Sized per-graph and built on the control thread, exactly like
//! `ParamTable` (`rt.rs`, round-2 §2.4) — a retired graph keeps reading the
//! table it was built with, so a renumbering cannot bleed into it. Nothing
//! here allocates, locks or blocks after construction.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};

/// The transport's clock. Its position is the callback's `base_pos`; its
/// `on` flag is the transport's play state (V-13).
pub const TRANSPORT_CLOCK: u32 = 0;

/// What one node reads for one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Playhead {
    pub pos: u64,
    /// This block does not continue the previous one — a live node owes an
    /// `all_notes_off` before it processes.
    pub discontinuity: bool,
    /// False = render nothing at all for this node this block.
    pub on: bool,
    /// True for [`TRANSPORT_CLOCK`]: the caller applies the arrangement's
    /// `LoopSpec` and automation ramps itself. False: this clock's own
    /// start/end govern its playback, and the arrangement's loop does not
    /// apply to it at all.
    pub is_transport: bool,
}

struct ClockState {
    on: AtomicBool,
    pos: AtomicU64,
    start: AtomicU64,
    end: AtomicU64,
    looping: AtomicBool,
    /// Pending: set by `fire` and by a loop wrap. Latched into `block_disc`
    /// by the next [`ClockTable::begin_block`] call, then left alone until
    /// the one after that.
    discont: AtomicBool,
    /// This block's discontinuity, as latched by the last `begin_block`.
    /// `playhead` reads this WITHOUT clearing it — see `begin_block`'s doc
    /// for why a per-reader `swap` here was a defect, not just a detail.
    block_disc: AtomicBool,
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
            block_disc: AtomicBool::new(false),
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

    /// Point a mixer slot at a clock. Last writer wins (V-14). Many slots
    /// may point at the same clock at once — a scene names every track its
    /// region covers, and they all share that one playhead.
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

    /// Latch every clock's pending discontinuity into this block's value.
    /// Call once per output block, before any `playhead()` call for that
    /// block.
    ///
    /// This is the generalisation of the single `SharedRt::launch_overlay()`
    /// call the old overlay made once per block: `engine.rs` swapped
    /// `launch_discont` exactly once and handed the same `LaunchPlayhead`
    /// down to every flagged track, so every reader agreed on whether the
    /// block was a jump. A scene clock is deliberately bound to MANY slots
    /// (every track its region names — V-4/V-14's many-to-one is the
    /// intended shape, not something to design away), so a version of this
    /// table that let `playhead()` itself consume the flag (via `swap`,
    /// which is what an earlier draft did) would give the discontinuity to
    /// only the first of N readers in the block; the remaining N-1 tracks
    /// would never get their `all_notes_off` and would hang a note on every
    /// fired scene but the first track in it. Latching once, up front, and
    /// letting `playhead()` only ever read the latch (never clear it) makes
    /// `playhead()` idempotent for any number of readers sharing a clock in
    /// the same block — exactly as idempotent as the old single overlay
    /// read was, generalized from one playhead to N.
    pub fn begin_block(&self) {
        for c in &self.clocks {
            let pending = c.discont.swap(false, Relaxed);
            c.block_disc.store(pending, Relaxed);
        }
    }

    /// What `slot` reads this block. `base_pos` is the transport's, used
    /// only when the slot is on [`TRANSPORT_CLOCK`].
    #[inline]
    pub fn playhead(&self, slot: usize, base_pos: u64, disc: bool) -> Playhead {
        if slot >= self.slot_clock.len() {
            return Playhead { pos: base_pos, discontinuity: disc, on: false, is_transport: true };
        }
        let idx = self.clock_of(slot);
        let Some(c) = self.clocks.get(idx as usize) else {
            return Playhead { pos: base_pos, discontinuity: disc, on: false, is_transport: true };
        };
        let on = c.on.load(Relaxed);
        if idx == TRANSPORT_CLOCK {
            return Playhead { pos: base_pos, discontinuity: disc, on, is_transport: true };
        }
        Playhead {
            pos: c.pos.load(Relaxed),
            // Read, never cleared here — see `begin_block`. Any number of
            // slots sharing this clock see the same value this block.
            discontinuity: c.block_disc.load(Relaxed),
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

    fn table() -> ClockTable {
        ClockTable::with_slots_and_clocks(4, 3)
    }

    #[test]
    fn a_slot_defaults_to_the_transport_clock() {
        let t = table();
        t.set_transport_playing(true);
        let ph = t.playhead(0, 5_000, false);
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

        assert!(!t.playhead(0, 5_000, false).on);
        let ph = t.playhead(2, 5_000, false);
        assert!(ph.on);
        assert_eq!(ph.pos, 0);
        assert!(!ph.is_transport);
    }

    #[test]
    fn a_fired_clock_starts_at_its_start_and_reports_a_discontinuity() {
        let t = table();
        t.fire(1, 400, 900, false);
        t.bind_slot(1, 1);
        t.begin_block();
        let ph = t.playhead(1, 0, false);
        assert_eq!(ph.pos, 400);
        assert!(ph.discontinuity, "the press is a jump");
    }

    /// V-4/V-14: a scene clock is many-to-one, so the discontinuity it
    /// carries must reach every slot bound to it in the block it fired,
    /// not just whichever slot happens to read first (that was the bug a
    /// per-reader `swap` in `playhead()` would have caused: a hanging note
    /// on every track of a fired scene but one).
    #[test]
    fn discontinuity_is_latched_once_per_block_for_every_reader_sharing_a_clock() {
        let t = table();
        t.fire(1, 0, 1_000, false);
        t.bind_slot(1, 1);
        t.bind_slot(2, 1); // a scene names two tracks
        t.begin_block();
        assert!(t.playhead(1, 0, false).discontinuity, "first reader");
        assert!(t.playhead(2, 0, false).discontinuity, "second reader, same block");

        t.begin_block(); // the next block: nothing newly fired
        assert!(!t.playhead(1, 0, false).discontinuity);
        assert!(!t.playhead(2, 0, false).discontinuity);
    }

    #[test]
    fn a_fire_between_blocks_is_seen_starting_the_begin_block_that_latches_it() {
        let t = table();
        t.bind_slot(1, 1);
        t.begin_block();
        assert!(!t.playhead(1, 0, false).discontinuity, "nothing fired yet");

        t.fire(1, 0, 1_000, false); // lands between two blocks
        assert!(
            !t.playhead(1, 0, false).discontinuity,
            "not visible until the next begin_block latches it"
        );
        t.begin_block();
        assert!(t.playhead(1, 0, false).discontinuity, "latched now");
    }

    #[test]
    fn advance_moves_running_clocks_and_stops_one_at_its_end() {
        let t = table();
        t.fire(1, 0, 100, false);
        t.bind_slot(1, 1);
        t.advance(64);
        assert_eq!(t.playhead(1, 0, false).pos, 64);
        t.advance(64);
        assert!(!t.playhead(1, 0, false).on, "past its end");
        assert!(!t.any_running());
    }

    #[test]
    fn a_looping_clock_wraps_to_its_start_instead_of_ending() {
        let t = table();
        t.fire(1, 0, 100, true);
        t.bind_slot(1, 1);
        t.advance(96);
        t.advance(32);
        t.begin_block();
        let ph = t.playhead(1, 0, false);
        assert!(ph.on, "a loop does not end");
        assert_eq!(ph.pos, 28, "wrapped: 128 - 100");
        assert!(ph.discontinuity, "the wrap is a jump the live node must hear");
    }

    /// The wrap arithmetic is a single `%`, not a subtract-once-and-check,
    /// so it must collapse any number of whole periods in one `advance`
    /// call, not just one.
    #[test]
    fn advance_collapses_a_multi_period_loop_overshoot_in_one_call() {
        let t = table();
        t.fire(1, 0, 100, true);
        t.bind_slot(1, 1);
        t.advance(340);
        assert_eq!(t.playhead(1, 0, false).pos, 40, "340 % 100");
    }

    #[test]
    fn advance_never_moves_the_transport_clock() {
        let t = table();
        t.set_transport_playing(true);
        t.advance(64);
        assert_eq!(
            t.playhead(0, 5_000, false).pos,
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
        let ph = t.playhead(99, 1_234, false);
        assert!(!ph.on, "a slot outside the table renders nothing");
    }

    /// What the offline bounce builds: no non-transport clocks at all.
    /// Every clock-mutating call must no-op cleanly rather than panic or
    /// silently create state that doesn't exist.
    #[test]
    fn a_transport_only_table_no_ops_every_clock_operation() {
        let t = ClockTable::with_slots_and_clocks(2, 1);
        t.fire(1, 0, 100, true); // clock 1 doesn't exist — dropped
        t.bind_slot(0, 1); // dropped: slot 0 stays on the transport
        assert!(!t.any_running());
        assert_eq!(t.clock_of(0), TRANSPORT_CLOCK);

        t.set_transport_playing(true);
        let ph = t.playhead(0, 7_000, false);
        assert!(ph.is_transport);
        assert_eq!(ph.pos, 7_000);
        assert!(ph.on);
    }
}
