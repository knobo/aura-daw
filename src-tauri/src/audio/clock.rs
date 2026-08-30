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

/// `ClockState::pending_at`'s "no quantized fire is waiting on this clock"
/// (V-21). A real target is a transport sample position, and `u64::MAX` is
/// 132 000 years at 44.1 kHz, so it can never collide with one.
const NO_PENDING: u64 = u64::MAX;

/// `ClockState::choke_group`'s "this clock is in no group": it chokes
/// nothing and nothing chokes it. Every scene clock has this, and so does
/// every player whose `chokeGroup` is `None` — which is every player
/// migrated from V2.
const NO_CHOKE: u32 = u32::MAX;

/// Source of [`ClockState::seq`]. Process-wide on purpose — see that field.
/// A `u64` at one press per nanosecond runs for 584 years.
static FIRE_SEQ: AtomicU64 = AtomicU64::new(1);

/// What one node reads for one block.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Linear gain the fire that started this clock carries (V-18), 1.0
    /// for anything not fired with a velocity. Multiplied into the slot's
    /// fader, which is what makes it reach a RAW player too: `raw` empties
    /// the compiled node, so a value living in `ParamTable` could not.
    pub gain: f32,
    /// This clock's slots never fall back to the arrangement when it stops
    /// (V-2). Only the PLAYER range sets it — see
    /// [`ClockTable::with_slots_clocks_and_players`].
    ///
    /// A scene's track is a timeline track that a pad borrowed, so when the
    /// scene ends it rejoins the song and `mixer::node_playhead` gives it
    /// `base_pos`. A PLAYER is not in the song at all: its row's one clip
    /// sits at position 0 (the ephemeral placement), so handing it the
    /// transport's position would sound the pad's sample at bar 1 of the
    /// arrangement, every time the song rolled past it. An idle player
    /// renders nothing instead.
    pub exclusive: bool,
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
    /// V-18: the fire's velocity gain, as `f32` bits. Read once per block
    /// per slot by [`ClockTable::playhead`].
    gain: AtomicU32,
    /// V-21: the TRANSPORT position this clock's armed-but-not-started fire
    /// is waiting for, or [`NO_PENDING`]. `start`/`end`/`looping`/`gain`
    /// already hold that fire's parameters — a pending fire is a fire whose
    /// `on` has not been set yet.
    pending_at: AtomicU64,
    /// V-19: which press this clock is serving, from [`FIRE_SEQ`]. 0 means
    /// "never fired". Ordering voices by it is what makes stealing
    /// oldest-first, and taking it from a PROCESS-wide counter rather than a
    /// per-table one is what keeps the order meaningful across a rebuild:
    /// `take_state_from` carries it, and a fresh table's own counter would
    /// have started again at 0 and made every carried voice look older than
    /// every new one.
    seq: AtomicU64,
    /// V-20: the pad's choke group as `u32`, or [`NO_CHOKE`]. Written at
    /// GRAPH BUILD from the document, not at fire time, because the choke
    /// has to be resolvable from the audio thread when a quantized fire
    /// finally starts.
    choke_group: AtomicU32,
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
            gain: AtomicU32::new(1.0f32.to_bits()),
            seq: AtomicU64::new(0),
            pending_at: AtomicU64::new(NO_PENDING),
            choke_group: AtomicU32::new(NO_CHOKE),
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
        // V3. The velocity gain and a not-yet-started quantized fire belong
        // to the PRESS, and an edit made while a pad sounds must not turn it
        // up or swallow a fire that is still waiting for its beat. The choke
        // group deliberately does NOT travel: it is a document property, and
        // the fresh table read it from the document it was built from.
        self.gain.store(src.gain.load(Relaxed), Relaxed);
        self.pending_at.store(src.pending_at.load(Relaxed), Relaxed);
        self.seq.store(src.seq.load(Relaxed), Relaxed);
    }
}

pub struct ClockTable {
    clocks: Vec<ClockState>,
    slot_clock: Vec<AtomicU32>,
    /// How many clocks after [`TRANSPORT_CLOCK`] belong to PLAYERS, i.e. the
    /// size of the reserved range `1 ..= n_player_clocks` (`engine::rebuild`
    /// allocates it; scenes start above it). Immutable after construction,
    /// so reading it costs an integer compare and no atomic.
    ///
    /// It exists because [`Playhead::exclusive`] has to be answerable on the
    /// audio thread from the clock index alone — the table is the only thing
    /// the RT side holds that knows which clock a slot reads.
    n_player_clocks: u32,
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
        Self::with_slots_clocks_and_players(n_slots, n_clocks, 0)
    }

    /// [`ClockTable::with_slots_and_clocks`] plus the size of the reserved
    /// PLAYER range: clocks `1 ..= n_players` are players', and their slots
    /// report [`Playhead::exclusive`]. Clamped to what actually exists, so a
    /// caller cannot mark clocks the table does not have.
    pub fn with_slots_clocks_and_players(
        n_slots: usize,
        n_clocks: usize,
        n_players: usize,
    ) -> Self {
        let clocks: Vec<ClockState> = (0..n_clocks.max(1)).map(|_| ClockState::idle()).collect();
        let n_player_clocks = n_players.min(clocks.len().saturating_sub(1)) as u32;
        Self {
            clocks,
            slot_clock: (0..n_slots).map(|_| AtomicU32::new(TRANSPORT_CLOCK)).collect(),
            n_player_clocks,
            carry_generation: AtomicU64::new(NO_GENERATION),
        }
    }

    /// Is `clock` in the reserved player range? The RT-side half of V-2 —
    /// see [`Playhead::exclusive`].
    #[inline]
    pub fn is_player_clock(&self, clock: u32) -> bool {
        clock != TRANSPORT_CLOCK && clock <= self.n_player_clocks
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
    pub fn fire(&self, clock: u32, start: u64, end: u64, looping: bool, gain: f32) {
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
        c.gain.store(gain.to_bits(), Relaxed);
        c.seq.store(FIRE_SEQ.fetch_add(1, Relaxed), Relaxed);
        // A press supersedes a quantized press that has not started yet:
        // whatever this fire is, it is the newer statement of intent.
        c.pending_at.store(NO_PENDING, Relaxed);
        c.discont.store(true, Relaxed);
        c.on.store(true, Relaxed);
        self.choke_others(clock, c.choke_group.load(Relaxed));
    }

    /// Arm a fire for the moment the transport reaches `at` (V-21). The
    /// fire's parameters go into the clock's own `start`/`end`/`looping`/
    /// `gain` immediately and only `on` waits, so a retrigger of a sounding
    /// pad keeps sounding the same material until the boundary arrives.
    ///
    /// The clock is NOT choked here and does not choke: both happen when it
    /// actually starts ([`ClockTable::arm_pending`]), which is the moment
    /// the group is about to be contested.
    pub fn fire_at(&self, clock: u32, at: u64, start: u64, end: u64, looping: bool, gain: f32) {
        let Some(c) = self.clocks.get(clock as usize) else { return };
        if clock == TRANSPORT_CLOCK {
            return;
        }
        // Same order and same argument as `fire`'s: the counter moves before
        // the state it stands for, so the only misreading available to
        // `reconcile_adoption` is the safe one.
        c.writes.fetch_add(1, Relaxed);
        c.start.store(start, Relaxed);
        c.end.store(end.max(start.saturating_add(1)), Relaxed);
        c.looping.store(looping, Relaxed);
        c.gain.store(gain.to_bits(), Relaxed);
        // The PRESS's order, not the beat's: two pads quantized to the same
        // boundary steal in the order they were pressed.
        c.seq.store(FIRE_SEQ.fetch_add(1, Relaxed), Relaxed);
        c.pending_at.store(at, Relaxed);
    }

    /// Is a quantized fire waiting on this clock? The control plane counts
    /// it against the voice cap: a pad that will sound on the next beat has
    /// already taken its voice, and letting 32 more presses in behind it
    /// would blow the cap the moment the beat arrived.
    pub fn is_pending(&self, clock: u32) -> bool {
        self.clocks
            .get(clock as usize)
            .is_some_and(|c| c.pending_at.load(Relaxed) != NO_PENDING)
    }

    /// The pad's choke group, from the document, at graph build (V-20).
    /// `None` leaves the clock choking nothing and choked by nothing, which
    /// is what every scene clock and every V2-era player has.
    pub fn set_choke_group(&self, clock: u32, group: Option<u8>) {
        let Some(c) = self.clocks.get(clock as usize) else { return };
        c.choke_group.store(group.map_or(NO_CHOKE, u32::from), Relaxed);
    }

    /// Cut every OTHER clock in `group`, the way reaching a clip's end cuts
    /// one: [`ClockTable::stop`], so the discontinuity the next
    /// `begin_block` latches carries the choked pad's `all_notes_off` on the
    /// one code path that already delivers it. A pad therefore falls silent
    /// within the block its choker starts in, which is the gate V3 owes.
    ///
    /// [`NO_CHOKE`] returns immediately, so this costs a fired scene one
    /// integer compare and no loop at all. Called from BOTH thread sides
    /// (`fire` on the control thread, `arm_pending` on the audio thread) and
    /// RT-safe by the same standard as everything else here: a bounded walk
    /// over a vector sized at graph build, relaxed atomics, no allocation.
    fn choke_others(&self, clock: u32, group: u32) {
        if group == NO_CHOKE {
            return;
        }
        for (i, c) in self.clocks.iter().enumerate().skip(1) {
            if i as u32 == clock || c.choke_group.load(Relaxed) != group {
                continue;
            }
            // A pending sibling loses its fire too: it is in the group, and
            // a hat that has been closed should not open again on the next
            // beat because the press had been queued.
            c.pending_at.store(NO_PENDING, Relaxed);
            self.stop(i as u32);
        }
    }

    /// AUDIO THREAD, once per block, BEFORE `begin_block`: start every
    /// quantized fire whose target lands inside `[base_pos, base_pos +
    /// frames)`. Returns whether anything started.
    ///
    /// The comparison is against the END of the block, so a fire starts on
    /// the block that CONTAINS its target rather than the one after it —
    /// half a block early on average instead of a whole block late. One
    /// block is the resolution this model can offer at all: a clock has one
    /// position per block, so there is no way to express "start 143 frames
    /// in" without a second, sample-accurate playhead concept (V-21).
    ///
    /// It runs before `begin_block` so that the discontinuity this fire owes
    /// its nodes is latched by the same block that starts it — one block, one
    /// jump, exactly as an immediate `fire` from the control thread gets.
    pub fn arm_pending(&self, base_pos: u64, frames: u64) -> bool {
        let block_end = base_pos.saturating_add(frames);
        let mut started = false;
        for (i, c) in self.clocks.iter().enumerate().skip(1) {
            let at = c.pending_at.load(Relaxed);
            if at == NO_PENDING || at >= block_end {
                continue;
            }
            c.pending_at.store(NO_PENDING, Relaxed);
            c.pos.store(c.start.load(Relaxed), Relaxed);
            c.discont.store(true, Relaxed);
            c.on.store(true, Relaxed);
            self.choke_others(i as u32, c.choke_group.load(Relaxed));
            started = true;
        }
        started
    }

    /// Drop every quantized fire that has not started (V-21). The transport
    /// stop path calls it: the grid such a press was waiting for has stopped
    /// existing, and `base_pos` will never reach the target, so the
    /// alternative to cancelling is a pad that fires whenever the song is
    /// next played past that point.
    pub fn cancel_pending(&self) {
        for c in self.clocks.iter().skip(1) {
            c.pending_at.store(NO_PENDING, Relaxed);
        }
    }

    /// The reserved PLAYER range, `1 ..= n_player_clocks` — empty when the
    /// document has no players. The voice cap is counted over exactly this:
    /// a scene is a region of the arrangement, not a voice on the deck.
    pub fn player_clocks(&self) -> std::ops::RangeInclusive<u32> {
        1..=self.n_player_clocks
    }

    /// Is this clock LIVE — sounding, or waiting on a beat to sound?
    ///
    /// The distinction matters wherever "is it off?" was previously the same
    /// question as "is it over?". It stopped being the same question when
    /// `fire_at` arrived: an armed clock is off and has not started yet, and
    /// reading that as "over" ends a scene before it ever begins (the launch
    /// drive thread's release edge) or hands its borrowed tracks back
    /// (`GraphTables::release_finished_scenes`).
    ///
    /// It is also V-19's voice question: a pad that will sound on the next
    /// beat has already taken its voice.
    #[inline]
    pub fn is_live(&self, clock: u32) -> bool {
        self.is_on(clock) || self.is_pending(clock)
    }

    /// How many player clocks are SOUNDING or waiting to (V-19's voice
    /// count). A pending fire counts: a pad that will sound on the next beat
    /// has already taken its voice.
    pub fn voices_in_use(&self) -> usize {
        self.player_clocks().filter(|&i| self.is_live(i)).count()
    }

    /// The voice that has been sounding longest — what V-19 steals when the
    /// cap is reached. `None` when nothing is sounding at all.
    ///
    /// Ties cannot happen: [`FIRE_SEQ`] is a `fetch_add`, so two presses
    /// never take the same number even from two threads.
    pub fn oldest_voice(&self) -> Option<u32> {
        self.player_clocks()
            .filter(|&i| self.is_live(i))
            .filter_map(|i| self.clocks.get(i as usize).map(|c| (c.seq.load(Relaxed), i)))
            .min()
            .map(|(_, i)| i)
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
        // Unconditionally, and BEFORE the `swap` guard: a stop must reach a
        // quantized fire that has not started, and such a fire leaves the
        // clock off, so the guard below would return before ever seeing it.
        c.pending_at.store(NO_PENDING, Relaxed);
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
            return Playhead {
                pos: base_pos,
                discontinuity: disc,
                on: false,
                is_transport: true,
                gain: 1.0,
                exclusive: false,
            };
        }
        let idx = self.clock_of(slot);
        let Some(c) = self.clocks.get(idx as usize) else {
            return Playhead {
                pos: base_pos,
                discontinuity: disc,
                on: false,
                is_transport: true,
                gain: 1.0,
                exclusive: false,
            };
        };
        let on = c.on.load(Relaxed);
        if idx == TRANSPORT_CLOCK {
            return Playhead {
                pos: base_pos,
                discontinuity: disc,
                on,
                is_transport: true,
                gain: 1.0,
                exclusive: false,
            };
        }
        Playhead {
            pos: c.pos.load(Relaxed),
            // Read, never cleared here — see `begin_block`. Any number of
            // slots sharing this clock see the same value this block.
            discontinuity: c.block_disc.load(Relaxed),
            on,
            is_transport: false,
            gain: f32::from_bits(c.gain.load(Relaxed)),
            // An integer compare against a field fixed at construction: no
            // atomic, no branch on shared state (V-2, see `Playhead`).
            exclusive: idx <= self.n_player_clocks,
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
        t.fire(1, 0, 1_000, false, 1.0);
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
        t.fire(1, 400, 900, false, 1.0);
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
        t.fire(1, 0, 1_000, false, 1.0);
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

        t.fire(1, 0, 1_000, false, 1.0); // lands between two blocks
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
        t.fire(1, 0, 100, false, 1.0);
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
        t.fire(1, 0, 100, true, 1.0);
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
        t.fire(1, 0, 100, true, 1.0);
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
        t.fire(1, 0, 10_000, false, 1.0);
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
        t.fire(1, 0, 100, false, 1.0);
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
        t.fire(1, 0, 100, false, 1.0);
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
        t.fire(1, 0, 1_000, false, 1.0);
        assert!(t.stop(1), "this call cut it");
        assert!(!t.stop(1), "the second press is a no-op");
    }

    /// What keeps the flush frame alive across a stopped transport: a clock
    /// that ended is no longer `any_running`, so `flush_pending` is the only
    /// thing left telling the engine to render the block that latches it.
    #[test]
    fn flush_pending_outlives_the_clock_that_stopped() {
        let t = table();
        t.fire(1, 0, 100, false, 1.0);
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
        prev.fire(1, 400, 900, true, 1.0);
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
        prev.fire(1, 400, 900, false, 1.0); // no begin_block: nobody saw it yet
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
        prev.fire(1, 400, 900, false, 1.0);
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
        prev.fire(1, 0, 100_000, false, 1.0);
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
        prev.fire(1, 0, 100_000, false, 1.0);
        prev.advance(64);

        let next = table();
        next.set_carry_source(7);
        next.carry_over(&prev, 1, 1);

        // The user presses the pad again while the fresh graph is queued.
        next.fire(1, 50_000, 60_000, false, 1.0);
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
        prev.fire(1, 0, 100_000, false, 1.0);
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
        prev.fire(1, 0, 100_000, false, 1.0);

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
        prev.fire(1, 0, 100_000, false, 1.0);
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
        prev.fire(1, 0, 100_000, false, 1.0);
        prev.fire(2, 0, 100_000, false, 1.0);
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
        t.fire(99, 0, 10, false, 1.0);
        t.bind_slot(99, 1);
        t.stop(99);
        assert!(!t.release_slot_if(99, 1));
        let ph = t.playhead(99, 1_234, false);
        assert!(!ph.on, "a slot outside the table renders nothing");
    }

    /// V-2, the RT half. A player's slot is bound to its clock for the
    /// LIFE of the graph — nothing releases it, unlike a scene's — so the
    /// idle state has to be silence, not "rejoin the arrangement". Were it
    /// the latter, `mixer::node_playhead` would hand the player's row the
    /// transport's position and its one clip (placed at 0) would sound at
    /// bar 1 of every playthrough.
    #[test]
    fn an_idle_player_clock_is_exclusive_so_its_slot_never_rejoins_the_transport() {
        let t = ClockTable::with_slots_clocks_and_players(4, 3, 1);
        t.set_transport_playing(true);
        t.bind_slot(1, 1); // the player's slot, bound at graph build
        t.bind_slot(2, 2); // a scene's track, borrowed from the timeline

        let player = t.playhead(1, 5_000, false);
        assert!(!player.on, "idle");
        assert!(player.exclusive, "and it has no arrangement to fall back to");

        let scene = t.playhead(2, 5_000, false);
        assert!(
            !scene.exclusive,
            "a scene's track IS a timeline track and does rejoin the song"
        );
    }

    /// The range is `1 ..= n_players`, and clock 0 is never in it whatever
    /// the caller passes — the transport is the arrangement.
    #[test]
    fn the_player_range_starts_at_one_and_excludes_the_transport() {
        let t = ClockTable::with_slots_clocks_and_players(2, 4, 2);
        assert!(!t.is_player_clock(TRANSPORT_CLOCK));
        assert!(t.is_player_clock(1));
        assert!(t.is_player_clock(2));
        assert!(!t.is_player_clock(3), "scenes start above the reserved range");
        assert!(
            !t.playhead(0, 9, false).exclusive,
            "a slot on the transport is the arrangement itself"
        );
    }

    /// A firing player clock behaves exactly like any other non-transport
    /// clock — `exclusive` only decides what happens when it is OFF.
    #[test]
    fn a_running_player_clock_plays_from_its_own_position() {
        let t = ClockTable::with_slots_clocks_and_players(4, 3, 1);
        t.set_transport_playing(true);
        t.bind_slot(1, 1);
        t.fire(1, 0, 1_000, false, 1.0);
        t.begin_block();
        let ph = t.playhead(1, 5_000, false);
        assert!(ph.on);
        assert_eq!(ph.pos, 0, "its own playhead, not the transport's 5000");
        assert!(ph.discontinuity, "the press is a jump");
    }

    /// The clamp: a caller cannot reserve clocks the table does not have,
    /// or every index would answer `exclusive` and no scene would ever
    /// rejoin the arrangement.
    #[test]
    fn the_reserved_player_range_is_clamped_to_the_clocks_that_exist() {
        let t = ClockTable::with_slots_clocks_and_players(2, 2, 9);
        assert!(t.is_player_clock(1));
        assert!(!t.is_player_clock(2), "clock 2 does not exist");
        let d = ClockTable::default();
        assert!(!d.is_player_clock(1), "a transport-only table reserves nothing");
    }

    /// What the offline bounce builds: no non-transport clocks at all.
    /// Every clock-mutating call must no-op cleanly rather than panic or
    /// silently create state that doesn't exist.
    #[test]
    fn a_transport_only_table_no_ops_every_clock_operation() {
        let t = ClockTable::with_slots_and_clocks(2, 1);
        t.fire(1, 0, 100, true, 1.0); // clock 1 doesn't exist — dropped
        t.bind_slot(0, 1); // dropped: slot 0 stays on the transport
        assert!(!t.any_running());
        assert_eq!(t.clock_of(0), TRANSPORT_CLOCK);

        t.set_transport_playing(true);
        let ph = t.playhead(0, 7_000, false);
        assert!(ph.is_transport);
        assert_eq!(ph.pos, 7_000);
        assert!(ph.on);
    }

    // ---- Plan V — V3: polyphony (V-18…V-21) ---------------------------

    /// A table whose clocks 1..=3 are the PLAYER range, so the voice-cap
    /// queries below have something to count.
    fn deck() -> ClockTable {
        ClockTable::with_slots_clocks_and_players(4, 5, 3)
    }

    /// V-18. The fire's gain reaches every slot bound to its clock, and a
    /// transport slot is never scaled by it.
    #[test]
    fn a_fires_gain_reaches_the_slots_bound_to_its_clock() {
        let t = deck();
        t.set_transport_playing(true);
        t.bind_slot(2, 1);
        t.fire(1, 0, 1_000, false, 0.25);
        assert_eq!(t.playhead(2, 0, false).gain, 0.25);
        assert_eq!(t.playhead(0, 0, false).gain, 1.0, "a transport slot is unscaled");
    }

    /// A press with no velocity is unity — which is what keeps every V2
    /// caller sounding exactly as it did.
    #[test]
    fn a_clock_that_was_never_fired_reports_unity_gain() {
        let t = deck();
        t.bind_slot(2, 1);
        assert_eq!(t.playhead(2, 0, false).gain, 1.0);
    }

    /// V-21. `fire_at` arms; nothing sounds until a block covers the target.
    /// The boundary is the block's END: a target AT `base_pos + frames` is
    /// the next block's, one at `block_end - 1` is this block's.
    #[test]
    fn a_quantized_fire_starts_in_the_block_that_contains_its_target() {
        let t = deck();
        t.fire_at(1, 10_000, 0, 1_000, false, 1.0);
        assert!(!t.is_on(1), "armed, not sounding");
        assert!(t.is_pending(1));

        assert!(!t.arm_pending(9_000, 512), "9_000..9_512 is short of the target");
        assert!(!t.is_on(1));
        assert!(!t.arm_pending(9_488, 512), "ends exactly AT 10_000, so not yet");
        assert!(!t.is_on(1));

        assert!(t.arm_pending(9_489, 512), "9_489..10_001 contains it");
        assert!(t.is_on(1));
        assert!(!t.is_pending(1), "and the arming consumed it");
        t.begin_block();
        assert!(t.playhead(0, 0, false).discontinuity || t.clocks[1].block_disc.load(Relaxed));
    }

    /// The armed fire's own parameters are the ones that start, and its
    /// discontinuity is latched by the block that starts it — `arm_pending`
    /// runs BEFORE `begin_block`, which is the ordering `mixer::render_impl`
    /// owes it.
    #[test]
    fn an_armed_fire_starts_at_its_own_start_with_a_discontinuity() {
        let t = deck();
        t.bind_slot(2, 1);
        t.fire_at(1, 5_000, 400, 900, true, 0.5);
        t.arm_pending(4_800, 512);
        t.begin_block();
        let ph = t.playhead(2, 0, false);
        assert_eq!(ph.pos, 400);
        assert!(ph.on);
        assert!(ph.discontinuity, "the fire's own jump, in the block that started it");
        assert_eq!(ph.gain, 0.5);
    }

    /// V-21. Stopping the transport cancels a press that has not started —
    /// the grid it was waiting for stopped existing. A SOUNDING pad is not
    /// touched (that is V-2, and `clear_launch_audible`'s whole ruling).
    #[test]
    fn cancel_pending_drops_armed_fires_and_leaves_sounding_ones_alone() {
        let t = deck();
        t.fire_at(1, 10_000, 0, 1_000, false, 1.0);
        t.fire(2, 0, 1_000, false, 1.0);
        t.cancel_pending();
        assert!(!t.is_pending(1));
        assert!(!t.arm_pending(0, 1_000_000), "nothing left to arm");
        assert!(t.is_on(2), "a performance is not a pending press");
    }

    /// A stop must reach an armed fire, and the `swap` guard cannot see one:
    /// an armed clock is OFF, so the guard returns before ever looking.
    #[test]
    fn stopping_a_pad_cancels_the_press_it_had_queued() {
        let t = deck();
        t.fire_at(1, 10_000, 0, 1_000, false, 1.0);
        assert!(!t.stop(1), "it was not sounding, so nothing was cut");
        assert!(!t.is_pending(1), "but the queued press is gone");
        assert!(!t.arm_pending(0, 1_000_000));
        assert!(!t.is_on(1));
    }

    /// A press supersedes a press: firing a pad that has a quantized fire
    /// queued drops the queue rather than sounding twice.
    #[test]
    fn firing_a_pad_supersedes_the_press_it_had_queued() {
        let t = deck();
        t.fire_at(1, 10_000, 0, 1_000, false, 1.0);
        t.fire(1, 0, 1_000, false, 1.0);
        assert!(t.is_on(1));
        assert!(!t.is_pending(1));
    }

    /// V-20. The second press cuts the first, and it cuts it the way an end
    /// does — a stopped clock owing one discontinuity — so the choked pad's
    /// `all_notes_off` rides the path that already delivers it.
    #[test]
    fn a_choke_group_cuts_its_siblings_when_the_next_one_starts() {
        let t = deck();
        t.set_choke_group(1, Some(3));
        t.set_choke_group(2, Some(3));
        t.set_choke_group(3, Some(9));
        t.fire(1, 0, 1_000, false, 1.0);
        t.fire(3, 0, 1_000, false, 1.0);
        t.begin_block(); // consume both fires' own jumps

        t.fire(2, 0, 1_000, false, 1.0);
        assert!(!t.is_on(1), "the open hat is closed");
        assert!(t.is_on(2), "by the pad that closed it");
        assert!(t.is_on(3), "and another group is untouched");
        assert!(t.flush_pending_for(1), "the cut owes its nodes an all-notes-off");
    }

    /// A pad in no group is the V2 state, and every scene clock's: it cuts
    /// nothing and nothing cuts it.
    #[test]
    fn a_pad_in_no_group_chokes_nothing() {
        let t = deck();
        t.fire(1, 0, 1_000, false, 1.0);
        t.fire(2, 0, 1_000, false, 1.0);
        assert!(t.is_on(1));
        assert!(t.is_on(2));
    }

    /// The choke reaches a sibling that is only ARMED. A hat that has been
    /// closed must not open again on the next beat because the press had
    /// been queued.
    #[test]
    fn a_choke_takes_a_siblings_queued_press_too() {
        let t = deck();
        t.set_choke_group(1, Some(1));
        t.set_choke_group(2, Some(1));
        t.fire_at(1, 10_000, 0, 1_000, false, 1.0);
        t.fire(2, 0, 1_000, false, 1.0);
        assert!(!t.is_pending(1));
        assert!(!t.arm_pending(0, 1_000_000));
        assert!(!t.is_on(1));
    }

    /// A quantized press chokes when it STARTS, not when it is pressed —
    /// otherwise the pad it closes falls silent early and leaves a hole
    /// until the beat arrives.
    #[test]
    fn a_quantized_press_chokes_on_the_beat_not_on_the_press() {
        let t = deck();
        t.set_choke_group(1, Some(1));
        t.set_choke_group(2, Some(1));
        t.fire(1, 0, 100_000, false, 1.0);
        t.fire_at(2, 10_000, 0, 1_000, false, 1.0);
        assert!(t.is_on(1), "still open: the beat has not come");
        t.arm_pending(9_800, 512);
        assert!(!t.is_on(1), "and now it is closed");
        assert!(t.is_on(2));
    }

    /// V-19's count: player clocks only, sounding OR waiting on a beat. A
    /// scene clock (index above the player range) is a region of the
    /// arrangement, not a voice on the deck.
    #[test]
    fn voices_in_use_counts_sounding_and_armed_players_and_no_scenes() {
        let t = deck();
        assert_eq!(t.voices_in_use(), 0);
        t.fire(1, 0, 1_000, false, 1.0);
        t.fire_at(2, 10_000, 0, 1_000, false, 1.0);
        t.fire(4, 0, 1_000, false, 1.0); // above `n_player_clocks` = a scene
        assert_eq!(t.voices_in_use(), 2);
        assert_eq!(*t.player_clocks().end(), 3);
    }

    /// V-19 steals oldest-first, and "oldest" is by PRESS, not by clock
    /// index — clock 3 pressed first is older than clock 1 pressed second.
    #[test]
    fn the_oldest_voice_is_the_one_pressed_first_not_the_lowest_clock() {
        let t = deck();
        assert_eq!(t.oldest_voice(), None);
        t.fire(3, 0, 1_000, false, 1.0);
        t.fire(1, 0, 1_000, false, 1.0);
        t.fire_at(2, 10_000, 0, 1_000, false, 1.0);
        assert_eq!(t.oldest_voice(), Some(3));
        t.stop(3);
        assert_eq!(t.oldest_voice(), Some(1));
        t.stop(1);
        assert_eq!(t.oldest_voice(), Some(2), "an armed press is a voice too");
    }

    /// The press order survives a rebuild, which is why the sequence comes
    /// from a process-wide counter rather than a per-table one: a fresh
    /// table's own counter starts at 0, and every carried voice would then
    /// look older than every new press.
    #[test]
    fn press_order_survives_a_rebuild() {
        let old = deck();
        old.fire(2, 0, 100_000, false, 1.0); // pressed first
        old.fire(1, 0, 100_000, false, 1.0);

        let fresh = deck();
        fresh.set_carry_source(7);
        fresh.carry_over(&old, 1, 1);
        fresh.carry_over(&old, 2, 2);
        assert_eq!(fresh.oldest_voice(), Some(2), "still the pad pressed first");

        fresh.fire(3, 0, 100_000, false, 1.0);
        assert_eq!(fresh.oldest_voice(), Some(2), "and a new press is younger than both");
    }

    /// The velocity gain and an armed press belong to the PRESS, so an edit
    /// made while a pad sounds must not turn it up or swallow a fire that is
    /// still waiting for its beat.
    #[test]
    fn a_rebuild_carries_the_press_gain_and_its_queued_fire() {
        let old = deck();
        old.fire(1, 0, 100_000, false, 0.25);
        old.fire_at(2, 10_000, 0, 1_000, false, 0.5);

        let fresh = deck();
        fresh.bind_slot(2, 1);
        fresh.carry_over(&old, 1, 1);
        fresh.carry_over(&old, 2, 2);
        assert_eq!(fresh.playhead(2, 0, false).gain, 0.25);
        assert!(fresh.is_pending(2));
        fresh.arm_pending(9_800, 512);
        assert!(fresh.is_on(2));
    }
}
