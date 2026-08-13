//! Real-time shared state: atomics read by the audio callback and the
//! preallocated audio graph that gets pointer-swapped in.
//!
//! Everything the callback touches is either owned by the callback closure,
//! an atomic, or reached through a wait-free rtrb queue. No locks, no
//! allocation on the RT path (`Box::from_raw`/`into_raw` only move pointers;
//! deallocation of retired graphs happens on the control thread).

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use super::dsp::LiveInstrument;
use super::meters::{RawMeterBlock, METER_CHUNK_SLOTS};
use super::transport::LoopSpec;
use crate::ids::TrackId;
use crate::midi::schedule::AbsNoteEvent;

pub const FLAG_MUTE: u32 = 1 << 0;
pub const FLAG_SOLO: u32 = 1 << 1;

/// `SharedRt::park` sentinel: no parking position pending. (Sample 0 is a
/// legitimate park target, so absence needs a value outside the timeline.)
pub const NO_PARK: u64 = u64::MAX;

/// Atomics shared between commands, control thread and RT callbacks.
pub struct SharedRt {
    /// Playhead, samples at `sample_rate`. Written by the output callback
    /// while playing; written by seek/stop from the control plane.
    pub position: AtomicU64,
    pub playing: AtomicBool,
    pub recording: AtomicBool,
    /// Actual engine/stream sample rate.
    pub sample_rate: AtomicU32,
    pub loop_enabled: AtomicBool,
    pub loop_start: AtomicU64,
    pub loop_end: AtomicU64,
    /// Last audible sample of the current graph (clip ends + final note-off),
    /// recomputed by the control thread on every structural rebuild. `0` =
    /// nothing to play, so no end exists — same "degenerate means off"
    /// convention as `LoopSpec`. Read by the RT callback to detect the
    /// crossing; what to DO about it is policy and lives control-side.
    pub song_end: AtomicU64,
    /// Policy: stop the transport when the playhead reaches `song_end`.
    /// Read by the control thread only — never by the audio callback.
    pub stop_at_end: AtomicBool,
    /// Where to park the playhead once the transport is no longer playing —
    /// [`NO_PARK`] when nothing is pending.
    ///
    /// Stopping the transport from the control thread cannot place the
    /// playhead by itself: a callback that read `playing == true` before the
    /// stop still writes its advanced position afterwards, landing the
    /// playhead a buffer past where it should be. So the control thread
    /// says WHERE (policy), and the callback applies it the moment it sees
    /// the transport stopped (mechanism) — the last writer is then always
    /// the one that knows. Mechanism, not policy: the callback never decides
    /// to park, only carries out a position already computed for it.
    pub park: AtomicU64,
    /// Ring-buffer over/underrun count since engine start.
    pub xruns: AtomicU64,
}

impl Default for SharedRt {
    fn default() -> Self {
        Self {
            position: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            sample_rate: AtomicU32::new(48_000),
            loop_enabled: AtomicBool::new(false),
            loop_start: AtomicU64::new(0),
            loop_end: AtomicU64::new(0),
            song_end: AtomicU64::new(0),
            stop_at_end: AtomicBool::new(true),
            park: AtomicU64::new(NO_PARK),
            xruns: AtomicU64::new(0),
        }
    }
}

impl SharedRt {
    #[inline]
    pub fn loop_spec(&self) -> LoopSpec {
        LoopSpec {
            enabled: self.loop_enabled.load(Ordering::Relaxed),
            start: self.loop_start.load(Ordering::Relaxed),
            end: self.loop_end.load(Ordering::Relaxed),
        }
    }
}

/// Per-slot mixer parameters as atomics (f32 stored as bits). The callback
/// reads these Relaxed every buffer; commands write them directly — no queue
/// round-trip needed for continuous controls.
///
/// Round-2 §2.4: sized PER-GRAPH (`with_slots`), not by a fixed cap — a
/// retired graph keeps reading the table it was built with (Task 5's O-13
/// argument), and a rebuild always sizes the fresh table to the CURRENT
/// track count, however wide that gets.
pub struct ParamTable {
    pub gain: Vec<AtomicU32>,
    pub pan: Vec<AtomicU32>,
    pub flags: Vec<AtomicU32>,
    pub any_solo: AtomicBool,
}

/// `ParamTable::default() == with_slots(0)` would be wrong for tests that
/// poke arbitrary small slots without sizing the table explicitly first —
/// keep the historical 64-slot default so those stay valid. Production code
/// always goes through `with_slots(store.tracks.len())` at rebuild.
impl Default for ParamTable {
    fn default() -> Self {
        Self::with_slots(64)
    }
}

impl ParamTable {
    /// A table sized for `n` slots (per-graph — round-2 §2.4). All setters
    /// bounds-check against the actual size (out-of-range writes are
    /// dropped, matching the old `MAX_TRACKS` guards).
    pub fn with_slots(n: usize) -> Self {
        Self {
            gain: (0..n).map(|_| AtomicU32::new(1.0f32.to_bits())).collect(),
            pan: (0..n).map(|_| AtomicU32::new(0.0f32.to_bits())).collect(),
            flags: (0..n).map(|_| AtomicU32::new(0)).collect(),
            any_solo: AtomicBool::new(false),
        }
    }

    pub fn len(&self) -> usize {
        self.gain.len()
    }

    pub fn is_empty(&self) -> bool {
        self.gain.is_empty()
    }

    pub fn set_gain_linear(&self, slot: usize, gain: f32) {
        if slot < self.len() {
            self.gain[slot].store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_pan(&self, slot: usize, pan: f32) {
        if slot < self.len() {
            self.pan[slot].store(pan.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_flag(&self, slot: usize, flag: u32, on: bool) {
        if slot < self.len() {
            if on {
                self.flags[slot].fetch_or(flag, Ordering::Relaxed);
            } else {
                self.flags[slot].fetch_and(!flag, Ordering::Relaxed);
            }
        }
    }

    /// Reset a slot to unity/center/no-flags (used when a slot is reassigned).
    pub fn reset_slot(&self, slot: usize) {
        if slot < self.len() {
            self.gain[slot].store(1.0f32.to_bits(), Ordering::Relaxed);
            self.pan[slot].store(0.0f32.to_bits(), Ordering::Relaxed);
            self.flags[slot].store(0, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------
// The audio graph (immutable once published to the RT thread)
// ---------------------------------------------------------------------------

/// Decoded audio, interleaved f32 at the ENGINE sample rate.
pub struct RtClipData {
    pub channels: u16,
    pub data: Vec<f32>,
}

impl RtClipData {
    #[inline]
    pub fn frames(&self) -> u64 {
        (self.data.len() / self.channels.max(1) as usize) as u64
    }
}

/// A clip placed on the timeline, ready for RT playback.
pub struct RtClip {
    /// Timeline position of the clip's left edge (engine samples).
    pub start: u64,
    /// Offset into the source where playback begins.
    pub offset: u64,
    /// Audible length on the timeline.
    pub len: u64,
    /// Linear clip gain (from clip.gainDb).
    pub gain: f32,
    pub fade_in: u64,
    pub fade_out: u64,
    pub samples: Arc<RtClipData>,
}

// ---------------------------------------------------------------------------
// Live instrument nodes (phase 3, ARCHITECTURE §15)
// ---------------------------------------------------------------------------

/// Largest contiguous run a live node renders in one `process` call; the
/// graph's scratch buffer is sized to this. Callback blocks larger than this
/// are rendered in chunks (no RT allocation either way).
pub const MAX_LIVE_BLOCK: usize = 4096;

/// A live instrument node shared BETWEEN SUCCESSIVE GRAPH SNAPSHOTS.
///
/// Why a cell: node state (voice pools, later plugin instances) must survive
/// RCU graph swaps — recreating a plugin per rebuild is unacceptable and
/// resetting voices cuts held notes. Successive snapshots therefore hold
/// `Arc`s to the SAME cell; the control thread's node registry
/// (`engine::Control::live_nodes`) holds one more.
///
/// Safety contract (why the unsafe impls are sound):
/// * Only the RT thread calls [`LiveNodeCell::rt_mut`], and it renders exactly
///   ONE graph snapshot at a time — the old snapshot is retired in the same
///   callback invocation that adopts its successor, so no two renders can
///   alias the node.
/// * The control thread constructs and `prepare`s the node BEFORE the first
///   snapshot referencing it is published, and never touches it again while
///   any snapshot references it (parameter traffic goes through atomics or
///   wait-free queues owned by the node).
/// * Deallocation happens on the control thread when the last `Arc` drops
///   (retired snapshots are dropped control-side; so is the registry).
pub struct LiveNodeCell(UnsafeCell<Box<dyn LiveInstrument>>);

// SAFETY: see the contract above — access is exclusive by construction
// (single active snapshot + single RT thread), not by type-system proof.
unsafe impl Send for LiveNodeCell {}
unsafe impl Sync for LiveNodeCell {}

impl LiveNodeCell {
    pub fn new(node: Box<dyn LiveInstrument>) -> Arc<Self> {
        Arc::new(Self(UnsafeCell::new(node)))
    }

    /// RT-thread access during render. See the safety contract on the type.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn rt_mut(&self) -> &mut dyn LiveInstrument {
        &mut **self.0.get()
    }
}

/// A live instrument source on a track: the shared node plus this snapshot's
/// pre-scheduled note events (absolute engine samples, sorted — computed on
/// the control thread from ticks via `TempoMap`; the RT thread only slices).
pub struct LiveSource {
    pub node: Arc<LiveNodeCell>,
    pub events: Arc<Vec<AbsNoteEvent>>,
}

pub struct RtTrack {
    /// Index into `ParamTable`.
    pub slot: usize,
    pub clips: Vec<RtClip>,
    /// Live instrument (midi tracks): PolySynth / SamplerNode / plugin node.
    pub live: Option<LiveSource>,
}

impl RtTrack {
    /// Clip-only track (audio tracks; also keeps tests terse).
    pub fn clips(slot: usize, clips: Vec<RtClip>) -> Self {
        Self { slot, clips, live: None }
    }
}

pub struct RtGraph {
    pub tracks: Vec<RtTrack>,
    /// Preallocated stereo scratch for live-node rendering
    /// (`MAX_LIVE_BLOCK * 2` once any track is live; empty otherwise).
    /// Allocated at BUILD time on the control thread — never on the RT path.
    pub scratch: Vec<f32>,
    /// Monotonic graph generation; meter blocks echo it (Task 6).
    pub generation: u64,
    /// THIS graph's parameters — round-2 §2.4: the param table versions
    /// with the graph snapshot. A retired graph keeps reading its own
    /// table (the `Arc` it holds), so the O-13 alias window (a freed slot
    /// reused by a newer graph while an old graph still renders under it)
    /// cannot happen — there is nothing to free; every rebuild derives
    /// fresh slots and builds its own table. Knob traffic always targets
    /// the NEWEST table via `GraphTables`/`SharedGraphTables`, never this
    /// field directly.
    pub params: Arc<ParamTable>,
    /// Preallocated meter-block chunk templates for THIS graph's slot count
    /// (`⌈params.len() / METER_CHUNK_SLOTS⌉`, at least one so master meters
    /// and frame accounting keep flowing with zero tracks) — Task 7:
    /// chunking replaces the single `MAX_TRACKS`-wide block. Allocated at
    /// BUILD time on the control thread; `mixer::render` only mutates
    /// entries in place and pushes copies, so a wide graph's meter output
    /// costs N pushes per callback, never an RT allocation.
    pub meter_scratch: Vec<RawMeterBlock>,
}

impl RtGraph {
    /// Build a snapshot, allocating live-node scratch when needed.
    pub fn new(tracks: Vec<RtTrack>, generation: u64, params: Arc<ParamTable>) -> Self {
        let scratch = if tracks.iter().any(|t| t.live.is_some()) {
            vec![0.0; MAX_LIVE_BLOCK * 2]
        } else {
            Vec::new()
        };
        let n_chunks = (params.len() + METER_CHUNK_SLOTS - 1) / METER_CHUNK_SLOTS;
        let n_chunks = n_chunks.max(1);
        let meter_scratch = (0..n_chunks)
            .map(|i| {
                let mut b = RawMeterBlock::new(generation, 0, 0);
                b.base_slot = (i * METER_CHUNK_SLOTS) as u32;
                b
            })
            .collect();
        Self { tracks, scratch, generation, params, meter_scratch }
    }
}

/// Owning graph pointer shipped through rtrb queues (control -> callback for
/// the new graph, callback -> control for the retired one, which is dropped
/// control-side).
///
/// `Drop` frees the graph, so elements still sitting in a queue when the
/// queue itself is torn down (device switch, engine shutdown) are freed
/// instead of leaked. On the hot path the callback uses [`GraphPtr::into_box`]
/// (which defuses the destructor) so no deallocation ever happens RT-side;
/// queue teardown only ever runs on the control thread.
pub struct GraphPtr(*mut RtGraph);

impl GraphPtr {
    pub fn new(graph: Box<RtGraph>) -> Self {
        Self(Box::into_raw(graph))
    }

    /// Take ownership of the pointee. Never deallocates (safe on RT).
    pub fn into_box(self) -> Box<RtGraph> {
        let b = unsafe { Box::from_raw(self.0) };
        std::mem::forget(self);
        b
    }
}

impl Drop for GraphPtr {
    fn drop(&mut self) {
        drop(unsafe { Box::from_raw(self.0) });
    }
}

// SAFETY: the pointee is only ever owned by exactly one side at a time; the
// queue transfers ownership. RtGraph contains no thread-affine state.
unsafe impl Send for GraphPtr {}

// ---------------------------------------------------------------------------
// Control-side view of the current graph's tables (round-2 §2.4)
// ---------------------------------------------------------------------------

/// Control-side view of the CURRENT graph's tables, published by the engine
/// control thread on every rebuild (also headless — tables exist without an
/// output device, so knob writes and recording keep working with no device
/// open). Readers: `ControlPlane::commit` (knob writes), recording slot
/// resolution, the meter fold (Task 6).
///
/// Live-node state already moves across rebuilds via `LiveNodeRegistry`
/// keyed by track id (the pattern round-2 §2.4 names); parameter smoothing
/// does not exist yet — when it lands (engine round), it keys by `TrackId`
/// like the registry, not by slot (slots are per-generation and therefore
/// not a stable key across rebuilds).
pub struct GraphTables {
    pub generation: u64,
    pub params: Arc<ParamTable>,
    pub slots: HashMap<TrackId, usize>,
}

/// `Mutex` (not `RwLock`): writes are rare (once per rebuild) and reads are
/// short (a slot lookup + a handful of atomic stores), so a plain mutex is
/// simplest and cheap enough — this is control-side only, never touched by
/// an RT thread.
///
/// LOCK ORDER: session before tables, never the reverse [C1]. `rebuild`
/// publishes a fresh `GraphTables` INSIDE the session-lock scope it already
/// holds while reading the store — that is load-bearing, not style:
/// publishing after the lock is released opens a window where a commit
/// transacts against a newer document revision, resolves its param writes
/// through the OLD tables (because the new ones aren't published yet), and
/// the rebuild then publishes tables built from the OLDER revision it read
/// before that commit — the knob write is silently lost forever, since a
/// plain `Set` never schedules a rebuild. Publishing under the lock makes
/// <read doc, publish tables> atomic against every commit's
/// <transact, execute writes> sequence. `ControlPlane::commit` (tables
/// only, after transact), `start_recording` (session then tables), and
/// `pump_meter_frames` (session then tables) all conform to this order.
pub type SharedGraphTables = Arc<parking_lot::Mutex<GraphTables>>;

impl GraphTables {
    /// A fresh gen-0 table with no params and no slots — the shape every
    /// `AudioState::default()` and every control-module test fixture wants
    /// before the first real rebuild publishes something. Single source of
    /// truth for "what does an empty `GraphTables` look like".
    pub fn empty() -> SharedGraphTables {
        Arc::new(parking_lot::Mutex::new(GraphTables {
            generation: 0,
            params: Arc::new(ParamTable::default()),
            slots: HashMap::new(),
        }))
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::GraphTables;
    use super::SharedGraphTables;

    /// Test-only alias for [`GraphTables::empty`] — kept as a separate name
    /// so test modules read `testutil::empty_tables()` like the other
    /// fixture helpers, without duplicating the literal.
    pub fn empty_tables() -> SharedGraphTables {
        GraphTables::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_table_sizes_beyond_sixty_four() {
        let p = ParamTable::with_slots(100);
        p.set_gain_linear(99, 0.5);
        assert_eq!(f32::from_bits(p.gain[99].load(Ordering::Relaxed)), 0.5);
        p.set_gain_linear(100, 0.5); // out of range: dropped, no panic
        assert_eq!(p.len(), 100);
    }

    #[test]
    fn default_table_keeps_the_historical_sixty_four_slots() {
        // Tests that poke arbitrary small slots without sizing explicitly
        // first must keep working.
        let p = ParamTable::default();
        assert_eq!(p.len(), 64);
        p.set_pan(63, 1.0);
        assert_eq!(f32::from_bits(p.pan[63].load(Ordering::Relaxed)), 1.0);
    }

    #[test]
    fn wide_graph_gets_multiple_meter_chunks() {
        let g = RtGraph::new(Vec::new(), 1, Arc::new(ParamTable::with_slots(200)));
        // ceil(200/64) = 4 chunks.
        assert_eq!(g.meter_scratch.len(), 4);
        assert_eq!(g.meter_scratch[0].base_slot, 0);
        assert_eq!(g.meter_scratch[3].base_slot, 192);
    }

    #[test]
    fn zero_slot_table_still_gets_one_meter_chunk() {
        // Master meters + frame accounting must keep flowing even with no
        // tracks (an empty project, still playing) — the chunk count floors
        // at 1, it never goes to 0.
        let g = RtGraph::new(Vec::new(), 1, Arc::new(ParamTable::with_slots(0)));
        assert_eq!(g.meter_scratch.len(), 1);
    }
}
