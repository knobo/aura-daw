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

/// `ClockState::carried_from`'s "nothing was carried into this clock":
/// [`ClockTable::reconcile_adoption`] leaves such a clock alone.
const NO_CARRY: u32 = u32::MAX;

/// `ClockTable::carry_generation`'s "this table carried nothing from
/// anywhere", so no retired table can ever match it.
const NO_GENERATION: u64 = u64::MAX;

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
    /// Monotonic count of CONTROL-SIDE writes to this clock: `fire` and
    /// `stop`, the two calls that express a user intent about it. NOT
    /// bumped by `advance`, which is the RT thread continuing what a fire
    /// already asked for.
    ///
    /// That distinction is the whole point. Between `carry_over`'s snapshot
    /// and [`ClockTable::reconcile_adoption`], the retired graph keeps
    /// advancing its own table while control-side writes land on the fresh
    /// one; comparing this counter against [`Self::carried_writes`] is what
    /// tells "the retired graph advanced past the snapshot" (take its
    /// position) from "a pad was fired into the new table during the window"
    /// (the new write is newer, keep it).
    writes: AtomicU64,
    /// `writes` as it stood when `carry_over` snapshotted this clock's
    /// source. Equal to `writes` = nothing has been fired or stopped here
    /// since.
    carried_writes: AtomicU64,
    /// Which clock of the PREVIOUS graph's table this one was carried from,
    /// or [`NO_CARRY`]. Recorded at build time because adoption runs on the
    /// audio thread, where the binding-id → clock map that produced the
    /// pairing is not reachable (it lives behind a mutex, keyed by String).
    carried_from: AtomicU32,
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
            writes: AtomicU64::new(0),
            carried_writes: AtomicU64::new(0),
            carried_from: AtomicU32::new(NO_CARRY),
        }
    }

    /// Copy the whole RUNNING state of `src` into `self`. Deliberately not
    /// `writes`' bookkeeping (the caller owns that) and not `block_disc`:
    /// the latched flag belongs to the block the source table rendered, and
    /// re-delivering it would be a second, spurious `all_notes_off`.
    fn take_state_from(&self, src: &ClockState) {
        self.start.store(src.start.load(Relaxed), Relaxed);
        self.end.store(src.end.load(Relaxed), Relaxed);
        self.pos.store(src.pos.load(Relaxed), Relaxed);
        self.looping.store(src.looping.load(Relaxed), Relaxed);
        self.discont.store(src.discont.load(Relaxed), Relaxed);
        self.on.store(src.on.load(Relaxed), Relaxed);
    }
}

pub struct ClockTable {
    clocks: Vec<ClockState>,
    slot_clock: Vec<AtomicU32>,
    /// The `GraphTables::generation` of the table `carry_over` read from,
    /// or [`NO_GENERATION`]. [`ClockTable::reconcile_adoption`] refuses to
    /// run unless the graph being retired is that exact table: graphs are
    /// normally adopted in publish order, so the one being replaced IS the
    /// one carried from — but a full graph queue ([I1]) drops a snapshot
    /// and retries, and then it is not, and reconciling against a stranger
    /// would copy one binding's playhead onto another's.
    carry_generation: AtomicU64,
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
            carry_generation: AtomicU64::new(NO_GENERATION),
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
        // BUMPED FIRST, before the state it stands for, and that order is
        // load-bearing. Relaxed buys no happens-before, so the RT thread's
        // `reconcile_adoption` may read this counter at any point in this
        // sequence; bumping first makes the only possible misreading the
        // SAFE one. If it reads the counter before this line, none of the
        // stores below have run either, so its copy lands first and this
        // fire then overwrites it — the press wins. Were the bump last, a
        // reader could see the fire's `pos` already stored yet the counter
        // not yet moved, conclude "nothing was fired here" and copy the
        // retired graph's position straight over the user's press.
        c.writes.fetch_add(1, Relaxed);
        c.start.store(start, Relaxed);
        c.end.store(end.max(start.saturating_add(1)), Relaxed);
        c.pos.store(start, Relaxed);
        c.looping.store(looping, Relaxed);
        c.discont.store(true, Relaxed);
        c.on.store(true, Relaxed);
    }

    /// Stop a RUNNING clock, leaving one pending discontinuity behind.
    /// Returns whether this call is the one that stopped it.
    ///
    /// That flag is the old `SharedRt::end_launch`/`launch_ended` flush
    /// frame, generalized: cutting a sounding clip mid-note has to reach the
    /// live nodes bound to this clock as an `all_notes_off`, or the voice
    /// hangs with nothing left to release it (the control plane releases the
    /// slot a poll later, and a released slot reads the transport, which
    /// knows nothing about the note). The next
    /// [`ClockTable::begin_block`] latches the flag and every slot still
    /// bound here sees the jump exactly once — and because a stopped clock
    /// leaves `any_running` false, the engine keeps rendering for one more
    /// block on [`ClockTable::flush_pending`] so that `begin_block` actually
    /// happens (`engine::OutputCb::render`).
    ///
    /// The `swap` is the guard the deleted `end_launch` had
    /// (`if !self.launch_on.swap(false) { return false }`), and it is not
    /// decoration: `launch_stop` is documented as safe to press with nothing
    /// launched, and the frontend's stop-all does press it unconditionally.
    /// A clock that was already off must fabricate NO discontinuity — the
    /// slots may still be bound to it, and an `all_notes_off` they never
    /// earned cuts arrangement notes mid-song. It is one atomic, so it is
    /// also the answer to "was something sounding?": an `is_on`-then-`stop`
    /// pair can report true for a clock `advance` turned off in between.
    pub fn stop(&self, clock: u32) -> bool {
        let Some(c) = self.clocks.get(clock as usize) else { return false };
        if clock == TRANSPORT_CLOCK {
            return false;
        }
        // Before the `swap`, and unconditionally even for the idle no-op —
        // same argument as `fire`'s (see there). A stop the reconciliation
        // failed to notice would be UNDONE by copying the retired graph's
        // still-running state back over it; a stop it notices needlessly
        // only costs that clock the exactness this counter buys, which is
        // one block of stale position. Counting the no-op press is the
        // cheaper side of that trade.
        c.writes.fetch_add(1, Relaxed);
        if !c.on.swap(false, Relaxed) {
            return false;
        }
        c.discont.store(true, Relaxed);
        true
    }

    /// Any clock carrying a discontinuity no `begin_block` has latched yet.
    ///
    /// This is what makes the flush frame survive a stopped transport. A
    /// clock that ends or is cut leaves `any_running` false, so without this
    /// the engine would render nothing that block, `begin_block` would never
    /// run, and the `all_notes_off` the flag exists to carry would be
    /// dropped — leaving the voice frozen in the live node, to resurrect the
    /// next time that node is rendered. The overlay this replaces bought the
    /// same extra block with `LaunchPlayhead { ended: true }`, which made
    /// `overlay_on` true for exactly one more block.
    pub fn flush_pending(&self) -> bool {
        self.clocks.iter().skip(1).any(|c| c.discont.load(Relaxed))
    }

    /// Does THIS clock still owe its nodes a discontinuity no `begin_block`
    /// has latched? The per-clock half of [`ClockTable::flush_pending`].
    ///
    /// The control plane holds a cut scene's slots bound until this goes
    /// false (`GraphTables::release_finished_scenes`), which is what replaced
    /// "release a poll after the cut" — the drive poll is 8 ms and a block
    /// can be 10.7 ms, so a poll was never a guarantee that a block had run
    /// in between.
    ///
    /// What it proves is bounded, and the callers say so: `begin_block` clears
    /// this at the START of a block, so a false answer means that block has
    /// BEGUN, not that any particular node has read `block_disc` yet. A drive
    /// poll landing inside one callback can still release a slot before that
    /// slot's own `playhead()` call. Sub-millisecond, and strictly narrower
    /// than releasing in the same breath as the cut; closing it entirely needs
    /// a blocks-rendered counter the release could wait on, which is booked as
    /// a follow-up rather than built here.
    pub fn flush_pending_for(&self, clock: u32) -> bool {
        self.clocks
            .get(clock as usize)
            .is_some_and(|c| c.discont.load(Relaxed))
    }

    /// Mark a clock "stopped, owing its nodes one discontinuity" without it
    /// ever having run — the state `stop` leaves behind, minted directly.
    ///
    /// This is deliberately the thing `stop`'s `swap` guard exists to
    /// PREVENT (a flush fabricated for a clock that was not running, which
    /// would `all_notes_off` tracks that never went exclusive). It is
    /// legitimate here for one reason: `engine::rebuild` calls it only on a
    /// clock it has just created, and binds to it only slots it has just
    /// verified were sounding on a scene the new document no longer has.
    /// Those nodes are owed exactly one `all_notes_off` and there is no
    /// surviving clock left to carry it.
    pub fn owe_flush(&self, clock: u32) {
        let Some(c) = self.clocks.get(clock as usize) else { return };
        if clock == TRANSPORT_CLOCK {
            return;
        }
        c.discont.store(true, Relaxed);
    }

    /// Is the transport clock running (V-13)? Read by
    /// `mixer::node_playhead` for a slot whose own clock has stopped: such a
    /// slot rejoins the arrangement, so what governs it is the transport's
    /// play state, not the dead clock's.
    #[inline]
    pub fn transport_on(&self) -> bool {
        self.clocks.first().is_some_and(|c| c.on.load(Relaxed))
    }

    /// Which published table `carry_over` is about to read from
    /// (`GraphTables::generation`). Recorded so
    /// [`ClockTable::reconcile_adoption`] can refuse to reconcile against a
    /// graph this table was not built from.
    pub fn set_carry_source(&self, generation: u64) {
        self.carry_generation.store(generation, Relaxed);
    }

    /// Which generation `set_carry_source` recorded — test-only, so a
    /// `rebuild` test can pin that the fresh table is actually able to
    /// reconcile at adoption rather than silently falling back.
    #[cfg(test)]
    pub fn carry_source_for_tests(&self) -> u64 {
        self.carry_generation.load(Relaxed)
    }

    /// Copy one clock's whole running state out of the PREVIOUS graph's
    /// table (`engine::rebuild`, control thread, before publication).
    ///
    /// `src_clock` and `dst_clock` are separate because a clock index means
    /// "the i-th scene binding in document order", and that is only stable
    /// while the document is: adding a binding ahead of a sounding one
    /// shifts it. `engine::rebuild` pairs the two ends BY BINDING ID, so
    /// what is carried is the scene, not the index.
    ///
    /// The table is per-graph (V-4, the `ParamTable` discipline), but a
    /// launched scene is not: the overlay this replaces lived on `SharedRt`
    /// and therefore survived every rebuild, so a clip edit made while a pad
    /// was sounding did not rewind it. Carrying the state forward is what
    /// keeps that true now that the playhead is versioned with the snapshot.
    ///
    /// The PENDING discontinuity is carried, the latched one is not: if the
    /// retired graph already latched and delivered the jump, re-delivering it
    /// would be a second, spurious `all_notes_off`.
    ///
    /// This is a SNAPSHOT, taken while the retired graph is still rendering.
    /// It is [`ClockTable::reconcile_adoption`] that makes it exact — this
    /// call's job is to leave behind the two facts that reconciliation needs
    /// (where the state came from, and what the fire/stop counter read at
    /// snapshot time) and to be a correct-enough fallback for the case
    /// reconciliation refuses.
    pub fn carry_over(&self, prev: &Self, src_clock: u32, dst_clock: u32) {
        let (Some(dst), Some(src)) = (
            self.clocks.get(dst_clock as usize),
            prev.clocks.get(src_clock as usize),
        ) else {
            return;
        };
        dst.take_state_from(src);
        dst.carried_from.store(src_clock, Relaxed);
        let writes = src.writes.load(Relaxed);
        dst.writes.store(writes, Relaxed);
        dst.carried_writes.store(writes, Relaxed);
    }

    /// AUDIO THREAD, once, at the moment this table's graph replaces
    /// `retired`'s (`engine::OutputCb::render`). Re-takes each carried
    /// clock's position from the graph that has been rendering all along.
    ///
    /// `carry_over` runs at BUILD time, but the retired graph keeps
    /// advancing its own table for every block between that snapshot and
    /// this adoption. Without this call the fresh table therefore starts
    /// roughly one callback block BEHIND — a backwards jump carrying no
    /// discontinuity flag, which is a click for audio and a re-emitted
    /// note-on for MIDI, every time a scene is sounding while the user
    /// edits anything.
    ///
    /// The one thing the copy must not do is clobber a pad pressed DURING
    /// that window: such a fire lands on this (already published) table,
    /// while the retired one still holds the older scene. `writes` vs
    /// `carried_writes` is exactly that question, and answering it costs
    /// one relaxed load per clock — RT-safe by the same standard as every
    /// other read in this file: no allocation, no lock, no syscall, and a
    /// bounded loop over a vector sized at graph build.
    pub fn reconcile_adoption(&self, retired: &Self, retired_generation: u64) {
        // Adoption is normally publish-ordered, so `retired` IS the table
        // `carry_over` read — but a full graph queue ([I1]) retries a
        // rebuild against a snapshot that was never adopted, and then the
        // indices in `carried_from` describe a table nobody is holding.
        if self.carry_generation.load(Relaxed) != retired_generation {
            return;
        }
        for dst in self.clocks.iter().skip(1) {
            let src_idx = dst.carried_from.load(Relaxed);
            if src_idx == NO_CARRY {
                continue;
            }
            if dst.writes.load(Relaxed) != dst.carried_writes.load(Relaxed) {
                // Fired or stopped since the snapshot: the control thread's
                // write is the newer statement of intent, and the retired
                // graph knows nothing about it.
                continue;
            }
            let Some(src) = retired.clocks.get(src_idx as usize) else { continue };
            dst.take_state_from(src);
        }
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
                // Same flush frame `stop` leaves behind, for the same
                // reason — reaching the end must all-notes-off the nodes
                // bound here, which is what `advance_launch` setting
                // `launch_ended` used to buy.
                c.discont.store(true, Relaxed);
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

    /// Where a clock stands, addressed by CLOCK index — `playhead` answers
    /// per SLOT, and the reconciliation tests below are about the clocks
    /// themselves, before anything is bound to them.
    fn pos_of(t: &ClockTable, clock: u32) -> u64 {
        t.clocks[clock as usize].pos.load(Relaxed)
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

    /// The `launch_ended` flush frame, re-expressed: a clip cut mid-note
    /// owes its live nodes an `all_notes_off`, and the only carrier for that
    /// is a discontinuity on the clock they are still bound to.
    #[test]
    fn stopping_a_clock_leaves_one_discontinuity_for_the_nodes_on_it() {
        let t = table();
        t.fire(1, 0, 10_000, false);
        t.bind_slot(1, 1);
        t.begin_block();
        assert!(t.playhead(1, 0, false).discontinuity, "the fire");

        t.stop(1);
        t.begin_block();
        let ph = t.playhead(1, 0, false);
        assert!(!ph.on, "stopped");
        assert!(ph.discontinuity, "the cut is a jump the live node must hear");

        t.begin_block();
        assert!(!t.playhead(1, 0, false).discontinuity, "exactly once");
    }

    /// Reaching the marked end is the same event as being cut, so it carries
    /// the same flush — `advance_launch` setting `launch_ended` is where this
    /// requirement comes from.
    #[test]
    fn a_clock_reaching_its_end_leaves_the_same_discontinuity() {
        let t = table();
        t.fire(1, 0, 100, false);
        t.bind_slot(1, 1);
        t.advance(128);
        t.begin_block();
        let ph = t.playhead(1, 0, false);
        assert!(!ph.on);
        assert!(ph.discontinuity, "the end all-notes-offs the node");
    }

    /// The deleted `ending_an_idle_overlay_is_a_no_op`, re-homed for real.
    /// `launch_stop` is documented as safe to press with nothing launched,
    /// and the frontend's stop-all does press it unconditionally — including
    /// in the window between a scene ending and the drive thread releasing
    /// its slots. A fabricated flush there sends `all_notes_off` to tracks
    /// that never went exclusive, cutting arrangement notes mid-song.
    #[test]
    fn stopping_a_clock_that_was_not_running_fabricates_no_flush() {
        let t = table();
        t.bind_slot(1, 1);
        assert!(!t.stop(1), "nothing was sounding");
        t.begin_block();
        assert!(
            !t.playhead(1, 0, false).discontinuity,
            "and no all-notes-off was invented for it"
        );

        // The same clock, having actually run and ended on its own: the
        // second stop is the idle one, and must add nothing either.
        t.fire(1, 0, 100, false);
        t.advance(128);
        assert!(!t.stop(1), "`advance` already stopped it");
        t.begin_block();
        assert!(t.playhead(1, 0, false).discontinuity, "the END's flush, once");
        t.begin_block();
        assert!(!t.playhead(1, 0, false).discontinuity);
    }

    #[test]
    fn stop_reports_whether_it_was_the_one_that_stopped_the_clock() {
        let t = table();
        t.fire(1, 0, 1_000, false);
        assert!(t.stop(1), "this call cut it");
        assert!(!t.stop(1), "the second press is a no-op");
    }

    /// What keeps the flush frame alive across a stopped transport: a clock
    /// that ended is no longer `any_running`, so `flush_pending` is the only
    /// thing left telling the engine to render the block that latches it.
    #[test]
    fn flush_pending_outlives_the_clock_that_stopped() {
        let t = table();
        t.fire(1, 0, 100, false);
        t.bind_slot(1, 1);
        assert!(t.any_running());

        t.advance(128);
        assert!(!t.any_running(), "ended");
        assert!(t.flush_pending(), "but it still owes its nodes a jump");

        t.begin_block();
        assert!(!t.flush_pending(), "latched — and only one block's worth");
        assert!(t.playhead(1, 0, false).discontinuity);
    }

    #[test]
    fn transport_on_reports_clock_zero() {
        let t = table();
        assert!(!t.transport_on());
        t.set_transport_playing(true);
        assert!(t.transport_on());
    }

    /// A rebuild must not rewind a sounding pad: the overlay lived on
    /// `SharedRt` and survived rebuilds, so the per-graph table has to carry
    /// the state across by hand (`engine::rebuild`).
    #[test]
    fn carry_over_moves_a_running_clock_into_the_next_graphs_table() {
        let prev = table();
        prev.fire(1, 400, 900, true);
        prev.advance(64);
        prev.begin_block(); // the retired graph already consumed the fire

        let next = table();
        next.carry_over(&prev, 1, 1);
        next.bind_slot(1, 1);
        next.begin_block();
        let ph = next.playhead(1, 0, false);
        assert!(ph.on, "still sounding across the rebuild");
        assert_eq!(ph.pos, 464, "and at the same position");
        assert!(
            !ph.discontinuity,
            "the retired graph already delivered the fire's jump"
        );
    }

    #[test]
    fn carry_over_of_an_unconsumed_fire_still_delivers_its_discontinuity() {
        let prev = table();
        prev.fire(1, 400, 900, false); // no begin_block: nobody saw it yet
        let next = table();
        next.carry_over(&prev, 1, 1);
        next.bind_slot(1, 1);
        next.begin_block();
        assert!(next.playhead(1, 0, false).discontinuity);
    }

    /// A clock may move between one graph and the next, because an index is
    /// "the i-th scene binding in document order" and adding a binding ahead
    /// of a sounding one shifts it. `engine::rebuild` pairs the ends by
    /// BINDING ID, so `carry_over` has to take two indices, not one.
    #[test]
    fn carry_over_can_move_a_clock_to_a_different_index() {
        let prev = table();
        prev.fire(1, 400, 900, false);
        prev.advance(64);

        let next = table();
        next.carry_over(&prev, 1, 2); // the same scene, renumbered
        next.bind_slot(1, 2);
        let ph = next.playhead(1, 0, false);
        assert!(ph.on);
        assert_eq!(ph.pos, 464);
        assert!(!next.is_on(1), "and nothing landed on the index it left");
    }

    /// The residual defect Task 7 booked forward: `carry_over` snapshots at
    /// BUILD time, and the retired graph keeps advancing its own table for
    /// every block until the fresh one is adopted. Without reconciliation
    /// the scene restarts behind where it actually is — a backwards jump
    /// carrying no discontinuity, which is a click for audio and a
    /// re-emitted note-on for MIDI.
    #[test]
    fn a_rebuild_during_a_sounding_scene_does_not_move_its_playhead_backwards() {
        let prev = table();
        prev.fire(1, 0, 100_000, false);
        prev.advance(64);

        // The rebuild snapshots here...
        let next = table();
        next.set_carry_source(7);
        next.carry_over(&prev, 1, 1);
        assert_eq!(pos_of(&next, 1), 64, "the snapshot, as taken");

        // ...and the retired graph renders two more blocks before the fresh
        // one reaches the callback.
        prev.advance(64);
        prev.advance(64);

        next.reconcile_adoption(&prev, 7);
        assert_eq!(
            pos_of(&next, 1),
            192,
            "adoption takes the position the graph that was rendering actually reached"
        );
        assert!(next.is_on(1));
    }

    /// The other half of the same trade, and the reason a plain "copy again
    /// at adoption" is wrong: a pad pressed DURING the window fires into the
    /// already-published new table, and the retired graph knows nothing
    /// about it. Reconciliation must leave that clock alone.
    #[test]
    fn a_fire_inside_the_rebuild_window_is_not_clobbered_by_reconciliation() {
        let prev = table();
        prev.fire(1, 0, 100_000, false);
        prev.advance(64);

        let next = table();
        next.set_carry_source(7);
        next.carry_over(&prev, 1, 1);

        // The user presses the pad again while the fresh graph is queued.
        next.fire(1, 50_000, 60_000, false);
        prev.advance(64); // the retired graph, still rendering the old take

        next.reconcile_adoption(&prev, 7);
        assert_eq!(
            pos_of(&next, 1),
            50_000,
            "the press is newer than anything the retired graph knew"
        );
        next.bind_slot(1, 1);
        next.begin_block();
        assert!(
            next.playhead(1, 0, false).discontinuity,
            "and it still carries its own jump"
        );
    }

    /// A stop is a control-side intent exactly like a fire, so it counts the
    /// same way: reconciliation must not resurrect a scene the user cut
    /// while the graph was in the queue.
    #[test]
    fn a_stop_inside_the_rebuild_window_is_not_undone_by_reconciliation() {
        let prev = table();
        prev.fire(1, 0, 100_000, false);
        prev.advance(64);

        let next = table();
        next.set_carry_source(7);
        next.carry_over(&prev, 1, 1);
        assert!(next.stop(1), "Escape, while the fresh graph is queued");
        prev.advance(64);

        next.reconcile_adoption(&prev, 7);
        assert!(!next.is_on(1), "the cut stands");
    }

    /// `carry_over`'s known double-delivery — a fire snapshotted before the
    /// retired graph latched it leaves the flag pending in both tables —
    /// closes on its own once adoption re-reads the source: by then the
    /// retired graph has consumed it, and the fresh table takes that.
    #[test]
    fn reconciliation_drops_a_discontinuity_the_retired_graph_already_delivered() {
        let prev = table();
        prev.fire(1, 0, 100_000, false);

        let next = table();
        next.set_carry_source(7);
        next.carry_over(&prev, 1, 1); // the fire is still pending in both

        prev.begin_block(); // the retired graph delivers it
        prev.advance(64);

        next.reconcile_adoption(&prev, 7);
        next.bind_slot(1, 1);
        next.begin_block();
        assert!(
            !next.playhead(1, 0, false).discontinuity,
            "delivered once, by the graph that was actually rendering"
        );
    }

    /// The graph queue can be full ([I1]), in which case a rebuild is
    /// retried against a snapshot that was never adopted and `carried_from`
    /// describes a table nobody holds. Reconciling anyway would copy one
    /// binding's playhead onto another's, so the generation has to match.
    #[test]
    fn reconciliation_refuses_a_graph_this_table_was_not_carried_from() {
        let prev = table();
        prev.fire(1, 0, 100_000, false);
        prev.advance(64);

        let next = table();
        next.set_carry_source(7);
        next.carry_over(&prev, 1, 1);
        prev.advance(64);

        next.reconcile_adoption(&prev, 9); // a different graph is being retired
        assert_eq!(
            pos_of(&next, 1),
            64,
            "the build-time snapshot stands — one block of rewind beats the wrong clock"
        );
    }

    /// A clock nobody carried into — a binding added by this very rebuild,
    /// or a player's lane (Task 9) — must take nothing from whatever sits at
    /// its index in the retired table. Indices are per-document, and the
    /// document is what just changed.
    #[test]
    fn reconciliation_leaves_a_clock_nothing_was_carried_into_alone() {
        let prev = table();
        prev.fire(1, 0, 100_000, false);
        prev.fire(2, 0, 100_000, false);
        prev.advance(64);

        let next = table();
        next.set_carry_source(7);
        next.carry_over(&prev, 2, 2); // only this one is the same scene

        next.reconcile_adoption(&prev, 7);
        assert!(next.is_on(2), "the carried scene keeps sounding");
        assert!(!next.is_on(1), "and nothing leaked in at the index beside it");
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
