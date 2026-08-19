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
use super::insert::InsertNode;
use super::meters::{RawMeterBlock, METER_CHUNK_SLOTS};
use super::pdc::DelayLine;
use super::transport::LoopSpec;
use crate::ids::TrackId;
use crate::midi::schedule::AbsNoteEvent;
use crate::plugins::automation::AbsParamEvent;

pub const FLAG_MUTE: u32 = 1 << 0;
pub const FLAG_SOLO: u32 = 1 << 1;
/// Track is a live launch target — mixer must hear it even if another
/// track is soloed or this one is muted.
pub const FLAG_LAUNCH: u32 = 1 << 2;
/// A live Write/Touch/Latch controller owns track gain. While set, the
/// mixer bypasses the compiled gain lane so the fader value is heard
/// exactly once (the lane remains a relative multiplier when read back).
pub const NO_GAIN_AUTOMATION_OWNER: u64 = u64::MAX;

#[inline]
pub fn advance_automation_pass(pass: &AtomicU64) -> u64 {
    loop {
        let observed = pass.load(Ordering::Acquire);
        let current = if observed == NO_GAIN_AUTOMATION_OWNER { 0 } else { observed };
        let incremented = current.wrapping_add(1);
        let next = if incremented == NO_GAIN_AUTOMATION_OWNER { 0 } else { incremented };
        if pass
            .compare_exchange_weak(observed, next, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return current;
        }
    }
}

/// Relative automation value for a coherent live/base gain snapshot. Every
/// finite positive base is meaningful, including subnormal values; only zero
/// or non-finite bases use the safe silence fallback.
pub fn relative_gain_multiplier(live: f32, base: f32) -> f32 {
    if base.is_finite() && base > 0.0 { live / base } else { 0.0 }
}

/// One-shot shadow playhead for a drive-clip launch. Launched tracks
/// render at `pos` instead of the arrangement playhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchPlayhead {
    pub pos: u64,
    pub discontinuity: bool,
    /// When true (stopped preview), only FLAG_LAUNCH tracks render.
    pub exclusive: bool,
    /// Overlay just reached its marked end — FLAG_LAUNCH tracks must
    /// all-notes-off; they render at the arrangement playhead.
    pub ended: bool,
}

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
    /// Monotonic automation-pass identity carried by gesture endpoints.
    pub automation_pass: AtomicU64,
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
    /// Frames in the output callback's most recent block, or 0 before the
    /// first one.
    ///
    /// Published so a consumer of [`Self::position`] can tell **block
    /// quantization apart from a transport jump**. `position` only moves when
    /// a callback runs, so anything reading it against a continuous clock sees
    /// it lag by up to one full block and then catch up in one step. A reader
    /// that treats that step as a seek will do so on every block, forever:
    /// `midi_out`'s clock engine did exactly that on a PipeWire quantum of
    /// 1024 frames at 48 kHz (21.3 ms) against a fixed 20 ms tolerance, and
    /// re-cued the external device roughly 1500 times a second. See
    /// `midi_out::drift_tolerance`.
    ///
    /// Written by the RT callback (one relaxed store per block, no
    /// synchronization implied); read by non-RT threads.
    pub block_frames: AtomicU32,
    /// Ring-buffer over/underrun count since engine start.
    pub xruns: AtomicU64,
    /// Engine-global CLAP `steady_time` base, in samples — advanced ONCE
    /// per RT output block by the callback (round-2 §3.5). Unlike
    /// `position`, this never resets: not on stop/play, not on a seek, and
    /// not when a live node is re-created (instrument rebind, sample-rate
    /// change, a track leaving and re-entering the live set). CLAP hosts
    /// require a steady_time that only ever climbs; per-node self-counting
    /// broke that guarantee the instant a node was rebuilt, because the
    /// counter lived on the node and a rebuild makes a new one. Nodes read
    /// this shared value instead — see `dsp::ProcessBlock::steady` and
    /// `plugins::clap_host::ClapNode::process`.
    pub steady: AtomicU64,
    /// Click track. Read by the RT callback; written from the control
    /// thread when the user toggles CLICK or the volume. Not a graph
    /// rebuild — the schedule lives on the snapshot, the enable/gain do not.
    pub metro_on: AtomicBool,
    /// Linear click gain, stored as f32 bits (same as ParamTable).
    pub metro_gain: AtomicU32,
    /// Samples of count-in still to play before a pending take arms.
    /// The callback decrements this and freezes the playhead while > 0.
    pub countin_left: AtomicU64,
    /// Samples already played in the current count-in (click clock).
    pub countin_elapsed: AtomicU64,
    /// Count-in click period (one beat), samples. 0 = no period.
    pub countin_beat: AtomicU64,
    /// Beats in the bar the count-in started in (for downbeat accents).
    pub countin_beats_per_bar: AtomicU32,
    /// Shadow playhead for a drive-clip launch. The main transport (loop,
    /// seek, FOLLOW) is left alone; launched tracks render at `launch_pos`.
    pub launch_on: AtomicBool,
    pub launch_pos: AtomicU64,
    pub launch_start: AtomicU64,
    pub launch_end: AtomicU64,
    pub launch_discont: AtomicBool,
    /// Set when the overlay playhead crosses `launch_end`. Consumed by the
    /// next mixer block so FLAG_LAUNCH tracks all-notes-off.
    pub launch_ended: AtomicBool,
}

impl Default for SharedRt {
    fn default() -> Self {
        Self {
            position: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            automation_pass: AtomicU64::new(0),
            sample_rate: AtomicU32::new(48_000),
            loop_enabled: AtomicBool::new(false),
            loop_start: AtomicU64::new(0),
            loop_end: AtomicU64::new(0),
            song_end: AtomicU64::new(0),
            stop_at_end: AtomicBool::new(true),
            park: AtomicU64::new(NO_PARK),
            block_frames: AtomicU32::new(0),
            xruns: AtomicU64::new(0),
            steady: AtomicU64::new(0),
            metro_on: AtomicBool::new(false),
            metro_gain: AtomicU32::new(0.35f32.to_bits()),
            countin_left: AtomicU64::new(0),
            countin_elapsed: AtomicU64::new(0),
            countin_beat: AtomicU64::new(0),
            countin_beats_per_bar: AtomicU32::new(4),
            launch_on: AtomicBool::new(false),
            launch_pos: AtomicU64::new(0),
            launch_start: AtomicU64::new(0),
            launch_end: AtomicU64::new(0),
            launch_discont: AtomicBool::new(false),
            launch_ended: AtomicBool::new(false),
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

    pub fn arm_launch(&self, start: u64, end: u64) {
        let end = end.max(start.saturating_add(1));
        self.launch_start.store(start, Ordering::Relaxed);
        self.launch_end.store(end, Ordering::Relaxed);
        self.launch_pos.store(start, Ordering::Relaxed);
        self.launch_discont.store(true, Ordering::Relaxed);
        self.launch_ended.store(false, Ordering::Relaxed);
        self.launch_on.store(true, Ordering::Relaxed);
    }

    pub fn clear_launch(&self) {
        self.launch_on.store(false, Ordering::Relaxed);
    }

    pub fn take_launch_ended(&self) -> bool {
        self.launch_ended.swap(false, Ordering::Relaxed)
    }

    pub fn launch_overlay(&self) -> Option<LaunchPlayhead> {
        if !self.launch_on.load(Ordering::Relaxed) {
            return None;
        }
        Some(LaunchPlayhead {
            pos: self.launch_pos.load(Ordering::Relaxed),
            discontinuity: self.launch_discont.swap(false, Ordering::Relaxed),
            exclusive: false,
            ended: false,
        })
    }

    pub fn advance_launch(&self, frames: u64) {
        if !self.launch_on.load(Ordering::Relaxed) {
            return;
        }
        let pos = self.launch_pos.load(Ordering::Relaxed);
        let end = self.launch_end.load(Ordering::Relaxed);
        let next = pos.saturating_add(frames);
        if next >= end {
            self.launch_on.store(false, Ordering::Relaxed);
            self.launch_ended.store(true, Ordering::Relaxed);
            self.launch_pos.store(end, Ordering::Relaxed);
        } else {
            self.launch_pos.store(next, Ordering::Relaxed);
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
    /// Current live fader value (gestures may override it without moving the document base).
    pub gain: Vec<AtomicU32>,
    /// Persisted base-fader value used as the denominator for relative gain recording.
    pub base_gain: Vec<AtomicU32>,
    /// Even = stable; odd = a live/base writer is in progress.
    gain_seq: Vec<AtomicU32>,
    gain_automation_owner: Vec<AtomicU64>,
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
            base_gain: (0..n).map(|_| AtomicU32::new(1.0f32.to_bits())).collect(),
            gain_seq: (0..n).map(|_| AtomicU32::new(0)).collect(),
            gain_automation_owner: (0..n)
                .map(|_| AtomicU64::new(NO_GAIN_AUTOMATION_OWNER))
                .collect(),
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

    fn begin_gain_write(&self, slot: usize) -> Option<&AtomicU32> {
        let seq = self.gain_seq.get(slot)?;
        loop {
            let current = seq.load(Ordering::Acquire);
            if current & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            if seq
                .compare_exchange_weak(
                    current,
                    current.wrapping_add(1),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Some(seq);
            }
        }
    }

    fn end_gain_write(seq: &AtomicU32) {
        seq.fetch_add(1, Ordering::Release);
    }

    pub fn set_gain_linear(&self, slot: usize, gain: f32) {
        let Some(seq) = self.begin_gain_write(slot) else { return };
        self.gain[slot].store(gain.to_bits(), Ordering::Relaxed);
        Self::end_gain_write(seq);
    }

    pub fn gain_linear(&self, slot: usize) -> f32 {
        self.gain.get(slot).map_or(1.0, |g| f32::from_bits(g.load(Ordering::Relaxed)))
    }

    pub fn set_base_gain_linear(&self, slot: usize, gain: f32) {
        let Some(seq) = self.begin_gain_write(slot) else { return };
        self.base_gain[slot].store(gain.to_bits(), Ordering::Relaxed);
        Self::end_gain_write(seq);
    }

    /// Publish a persisted fader change as one coherent live/base pair.
    pub fn set_gain_pair_linear(&self, slot: usize, gain: f32) {
        let Some(seq) = self.begin_gain_write(slot) else { return };
        let bits = gain.to_bits();
        self.gain[slot].store(bits, Ordering::Relaxed);
        self.base_gain[slot].store(bits, Ordering::Relaxed);
        Self::end_gain_write(seq);
    }

    /// Coherent snapshot for relative automation recording.
    pub fn gain_pair_linear(&self, slot: usize) -> (f32, f32) {
        let (Some(seq), Some(live), Some(base)) = (
            self.gain_seq.get(slot),
            self.gain.get(slot),
            self.base_gain.get(slot),
        ) else {
            return (1.0, 1.0);
        };
        loop {
            let before = seq.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let live = f32::from_bits(live.load(Ordering::Relaxed));
            let base = f32::from_bits(base.load(Ordering::Relaxed));
            if seq.load(Ordering::Acquire) == before {
                return (live, base);
            }
        }
    }

    pub fn base_gain_linear(&self, slot: usize) -> f32 {
        self.base_gain
            .get(slot)
            .map_or(1.0, |g| f32::from_bits(g.load(Ordering::Relaxed)))
    }

    pub fn set_pan(&self, slot: usize, pan: f32) {
        if slot < self.len() {
            self.pan[slot].store(pan.clamp(-1.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub fn set_gain_automation_owner(&self, slot: usize, pass: Option<u64>) {
        let Some(owner) = self.gain_automation_owner.get(slot) else { return };
        owner.store(pass.unwrap_or(NO_GAIN_AUTOMATION_OWNER), Ordering::Release);
    }

    #[inline]
    pub fn gain_automation_owner(&self, slot: usize) -> Option<u64> {
        let pass = self.gain_automation_owner.get(slot)?.load(Ordering::Acquire);
        (pass != NO_GAIN_AUTOMATION_OWNER).then_some(pass)
    }

    pub fn clear_gain_automation_owner_if(&self, slot: usize, expected_pass: u64) -> bool {
        let Some(owner) = self.gain_automation_owner.get(slot) else { return false };
        owner
            .compare_exchange(
                expected_pass,
                NO_GAIN_AUTOMATION_OWNER,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
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
            self.gain_automation_owner[slot]
                .store(NO_GAIN_AUTOMATION_OWNER, Ordering::Relaxed);
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
    /// Compiled insert chain (document order). Empty = dry strip.
    pub inserts: Vec<InsertNode>,
    /// Compensating delay (Task 6): pads this track's path up to the
    /// slowest sibling's (see `pdc::compile_pdc`), applied on the mixer
    /// strip after inserts and before the fader. `None` = no compensation
    /// needed (this track IS the slowest path, or PDC isn't wired up yet —
    /// Task 6 adds the primitive; attaching it during graph build is Task 7).
    pub pdc: Option<DelayLine>,
}

impl RtTrack {
    /// Clip-only track (audio tracks; also keeps tests terse).
    pub fn clips(slot: usize, clips: Vec<RtClip>) -> Self {
        Self {
            slot,
            clips,
            live: None,
            inserts: Vec::new(),
            pdc: None,
        }
    }
}

/// One slot's compiled built-in-param ramps. `None` means the atomic
/// (`ParamTable`) value is authoritative for that param.
#[derive(Debug, Default, Clone)]
pub struct TrackRamps {
    pub gain: Option<Arc<Vec<AbsParamEvent>>>,
    pub pan: Option<Arc<Vec<AbsParamEvent>>>,
}

pub struct RtGraph {
    pub tracks: Vec<RtTrack>,
    /// Preallocated stereo strip buffer (`MAX_LIVE_BLOCK * 2`, always).
    /// Clips + live sum here, then inserts REPLACE, then the shared fader.
    /// Allocated at BUILD time on the control thread — never on the RT path.
    pub track_buf: Vec<f32>,
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
    /// This snapshot's compiled built-in-param automation, indexed BY SLOT —
    /// exactly like `ParamTable` (round-2 §2.4: per-graph, versioned with
    /// the snapshot, so a retired graph keeps reading its own). An EMPTY
    /// vec means "no automation at all", which is what `new` leaves behind
    /// so every existing construction site is unchanged.
    ///
    /// Why here and not on the live node (Track D scope ruling 1): the
    /// registry reuses live nodes ACROSS rebuilds to keep voice and plugin
    /// state, so a ramp baked into a node could only change by discarding
    /// that state — and a node-side ramp could never reach an audio-clip
    /// track at all.
    pub track_ramps: Vec<TrackRamps>,
    /// Compiled click schedule for this snapshot (control-thread, tempo-map
    /// driven). Empty when there is nothing to click. The RT mixer only
    /// reads it when `SharedRt::metro_on` is set.
    pub clicks: Arc<Vec<crate::audio::metronome::Click>>,
}

impl RtGraph {
    /// Build a snapshot, always allocating the strip `track_buf`
    /// (unified clip/live/insert path).
    pub fn new(tracks: Vec<RtTrack>, generation: u64, params: Arc<ParamTable>) -> Self {
        let track_buf = vec![0.0; MAX_LIVE_BLOCK * 2];
        let n_chunks = (params.len() + METER_CHUNK_SLOTS - 1) / METER_CHUNK_SLOTS;
        let n_chunks = n_chunks.max(1);
        let meter_scratch = (0..n_chunks)
            .map(|i| {
                let mut b = RawMeterBlock::new(generation, 0, 0);
                b.base_slot = (i * METER_CHUNK_SLOTS) as u32;
                b
            })
            .collect();
        Self {
            tracks,
            track_buf,
            generation,
            params,
            meter_scratch,
            track_ramps: Vec::new(),
            clicks: Arc::new(Vec::new()),
        }
    }

    /// Attach this rebuild's compiled track ramps (`engine::rebuild`'s one
    /// call). CONTROL THREAD, before the graph is published — after
    /// publication the snapshot is immutable, RCU-style.
    pub fn set_track_ramps(&mut self, ramps: Vec<TrackRamps>) {
        self.track_ramps = ramps;
    }

    /// Shim: fill only the `gain` field so Track D's mixer tests keep
    /// compiling. Pan stays `None` (atomic pan remains authoritative).
    pub fn set_gain_ramps(&mut self, ramps: Vec<Option<Arc<Vec<AbsParamEvent>>>>) {
        self.track_ramps = ramps
            .into_iter()
            .map(|gain| TrackRamps { gain, pan: None })
            .collect();
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
/// LOCK ORDER: session before tables, never the reverse [C1]. Plan F Task 5
/// added a THIRD lock below both: `Session::published`'s inner mutex is a
/// LEAF — it is taken only for a pointer clone or swap, never across I/O and
/// never while any other lock is acquired inside its scope, so it can be
/// taken from under the session lock (which `capture_and_publish` does, by
/// design) and orders after tables too. Nothing ever takes session or tables
/// while holding it — `engine::rebuild` (Task 6) reads the leaf and RELEASES
/// it before acquiring session, so its two acquisitions are sequential, not
/// nested. `rebuild` publishes a fresh `GraphTables` INSIDE the session-lock
/// scope it holds while reading the store's PARAM VALUES AND SLOT MAP (Task
/// 6 shrank that read to exactly this; the graph assembly moved out from
/// under the lock, onto the published image) — publishing there is
/// load-bearing, not style:
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

    #[test]
    fn gain_linear_reads_back_what_set_gain_linear_wrote() {
        let t = ParamTable::with_slots(4);
        t.set_gain_linear(2, 0.5);
        assert_eq!(t.gain_linear(2), 0.5);
    }

    #[test]
    fn coherent_gain_pair_never_observes_a_torn_persisted_write() {
        let table = Arc::new(ParamTable::with_slots(1));
        let writer = table.clone();
        let thread = std::thread::spawn(move || {
            for i in 0..50_000 {
                writer.set_gain_pair_linear(0, if i % 2 == 0 { 2.0 } else { 4.0 });
            }
        });
        for _ in 0..50_000 {
            let (live, base) = table.gain_pair_linear(0);
            assert_eq!(live, base);
        }
        thread.join().unwrap();
    }

    #[test]
    fn automation_pass_advance_skips_the_reserved_owner_sentinel_at_wrap() {
        let pass = AtomicU64::new(NO_GAIN_AUTOMATION_OWNER - 2);
        assert_eq!(advance_automation_pass(&pass), NO_GAIN_AUTOMATION_OWNER - 2);
        assert_eq!(pass.load(Ordering::Relaxed), NO_GAIN_AUTOMATION_OWNER - 1);
        assert_eq!(advance_automation_pass(&pass), NO_GAIN_AUTOMATION_OWNER - 1);
        assert_eq!(pass.load(Ordering::Relaxed), 0);
        assert_eq!(advance_automation_pass(&pass), 0);
        assert_eq!(pass.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn automation_owner_compare_clear_cannot_erase_a_newer_pass() {
        let table = Arc::new(ParamTable::with_slots(1));
        table.set_gain_automation_owner(0, Some(7));
        let cleanup_table = table.clone();
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            let observed = cleanup_table.gain_automation_owner(0).unwrap();
            observed_tx.send(observed).unwrap();
            continue_rx.recv().unwrap();
            cleanup_table.clear_gain_automation_owner_if(0, observed)
        });

        assert_eq!(observed_rx.recv().unwrap(), 7);
        table.set_gain_automation_owner(0, Some(8));
        continue_tx.send(()).unwrap();

        assert!(!cleanup.join().unwrap(), "stale cleanup must lose the CAS");
        assert_eq!(table.gain_automation_owner(0), Some(8));
        assert!(table.gain_automation_owner(0).is_some(), "the same owner atomic keeps RT bypass active");
    }

    #[test]
    fn relative_gain_divides_by_every_finite_positive_base() {
        let subnormal = f32::MIN_POSITIVE / 2.0;
        assert_eq!(relative_gain_multiplier(subnormal * 2.0, subnormal), 2.0);
        assert_eq!(relative_gain_multiplier(1.0, 0.0), 0.0);
        assert_eq!(relative_gain_multiplier(1.0, f32::NAN), 0.0);
    }

    #[test]
    fn gain_linear_out_of_range_slot_returns_unity() {
        let t = ParamTable::with_slots(2);
        assert_eq!(t.gain_linear(99), 1.0);
    }
}
