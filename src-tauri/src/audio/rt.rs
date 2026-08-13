//! Real-time shared state: atomics read by the audio callback and the
//! preallocated audio graph that gets pointer-swapped in.
//!
//! Everything the callback touches is either owned by the callback closure,
//! an atomic, or reached through a wait-free rtrb queue. No locks, no
//! allocation on the RT path (`Box::from_raw`/`into_raw` only move pointers;
//! deallocation of retired graphs happens on the control thread).

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use super::dsp::LiveInstrument;
use super::transport::LoopSpec;
use super::types::MAX_TRACKS;
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
pub struct ParamTable {
    pub gain: [AtomicU32; MAX_TRACKS],
    pub pan: [AtomicU32; MAX_TRACKS],
    pub flags: [AtomicU32; MAX_TRACKS],
    pub any_solo: AtomicBool,
}

impl Default for ParamTable {
    fn default() -> Self {
        Self {
            gain: std::array::from_fn(|_| AtomicU32::new(1.0f32.to_bits())),
            pan: std::array::from_fn(|_| AtomicU32::new(0.0f32.to_bits())),
            flags: std::array::from_fn(|_| AtomicU32::new(0)),
            any_solo: AtomicBool::new(false),
        }
    }
}

impl ParamTable {
    pub fn set_gain_linear(&self, slot: usize, gain: f32) {
        if slot < MAX_TRACKS {
            self.gain[slot].store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_pan(&self, slot: usize, pan: f32) {
        if slot < MAX_TRACKS {
            self.pan[slot].store(pan.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_flag(&self, slot: usize, flag: u32, on: bool) {
        if slot < MAX_TRACKS {
            if on {
                self.flags[slot].fetch_or(flag, Ordering::Relaxed);
            } else {
                self.flags[slot].fetch_and(!flag, Ordering::Relaxed);
            }
        }
    }

    /// Reset a slot to unity/center/no-flags (used when a slot is reassigned).
    pub fn reset_slot(&self, slot: usize) {
        if slot < MAX_TRACKS {
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

#[derive(Default)]
pub struct RtGraph {
    pub tracks: Vec<RtTrack>,
    /// Preallocated stereo scratch for live-node rendering
    /// (`MAX_LIVE_BLOCK * 2` once any track is live; empty otherwise).
    /// Allocated at BUILD time on the control thread — never on the RT path.
    pub scratch: Vec<f32>,
}

impl RtGraph {
    /// Build a snapshot, allocating live-node scratch when needed.
    pub fn new(tracks: Vec<RtTrack>) -> Self {
        let scratch = if tracks.iter().any(|t| t.live.is_some()) {
            vec![0.0; MAX_LIVE_BLOCK * 2]
        } else {
            Vec::new()
        };
        Self { tracks, scratch }
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
