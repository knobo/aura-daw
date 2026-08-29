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
// Bit 2 is FREE. It carried "this track is a live launch target" until
// Plan V — V2 replaced the overlay with `audio::clock`: which playhead a
// node reads is now the clock its mixer slot is bound to, so "heard past
// another track's solo" is DERIVED from that binding (`clock_of(slot) !=
// TRANSPORT_CLOCK`) instead of stored a second time here. Do not reuse the
// bit for a playhead concept — see ruling V-4.
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
    /// Live send amounts as LINEAR gain, indexed by
    /// `types::derive_send_slots` (Plan G2). A send knob is a mix change,
    /// so it is an atomic store here — never a graph rebuild (§10). Sized
    /// per-graph like every other lane in this table.
    pub send_amount: Vec<AtomicU32>,
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
        Self::with_slots_and_sends(n, 0)
    }

    /// [`Self::with_slots`] plus `sends` send-amount lanes (Plan G2). Send
    /// amounts default to unity so a lane the rebuild forgot to populate is
    /// audible rather than silently dead — the same "neutral is the
    /// default" rule the gain lanes follow.
    pub fn with_slots_and_sends(n: usize, sends: usize) -> Self {
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
            send_amount: (0..sends).map(|_| AtomicU32::new(1.0f32.to_bits())).collect(),
        }
    }

    /// Store one send's amount as linear gain. Out-of-range indices are
    /// dropped, exactly like the slot-indexed setters — a knob write can
    /// race a rebuild that renumbered the sends, and dropping the write is
    /// correct there (the rebuild already read the document value).
    pub fn set_send_amount_linear(&self, idx: usize, amount: f32) {
        if let Some(a) = self.send_amount.get(idx) {
            a.store(amount.to_bits(), Ordering::Relaxed);
        }
    }

    /// Read one send's amount. Unity for an unknown index: a compiled edge
    /// whose lane vanished must not silently mute the return.
    #[inline]
    pub fn send_amount_linear(&self, idx: usize) -> f32 {
        self.send_amount
            .get(idx)
            .map_or(1.0, |a| f32::from_bits(a.load(Ordering::Relaxed)))
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
    /// Compiled send edges into `RtGraph::buses` (Plan G2). Empty = this
    /// track only feeds the master.
    pub sends: Vec<RtSend>,
    /// Compensating delay (Task 6): pads this track's path up to the
    /// slowest sibling's (see `pdc::compile_pdc`), applied on the mixer
    /// strip after inserts and before the fader. `None` = no compensation
    /// needed (this track IS the slowest path, or PDC isn't wired up yet —
    /// Task 6 adds the primitive; attaching it during graph build is Task 7).
    pub pdc: Option<DelayLine>,
    /// Plan G2: delay on this track's OUTPUT path — applied after the send
    /// taps and after the fader, so only what continues to the destination
    /// waits for whatever else arrives there.
    ///
    /// Why a SECOND delay line and not a bigger `pdc`: `pdc` sits before
    /// the taps, so growing it would delay the sends by the same amount and
    /// move the whole return with the dry signal — the two would never
    /// converge. The taps must leave at the source-aligned time; only the
    /// output path waits.
    pub out_pdc: Option<DelayLine>,
    /// Where this track's fader output goes: an index into
    /// `RtGraph::buses`, or `None` for the master.
    pub output: Option<usize>,
    /// How long this row keeps SOUNDING after it stops being fed: the
    /// applied insert-chain latency plus every delay line the row owns
    /// (`pdc`, `out_pdc`, the widest send edge). Derived from the row by
    /// [`RtTrack::recompute_tail_frames`], which every `RtGraph`
    /// constructor calls — a number kept in step with the lines by
    /// recomputing it from them, not by remembering to update it.
    ///
    /// A raw player is 0 by construction (V-6 gives it no inserts and no
    /// sends, and `node.rs` forces `output: None`, so no `out_pdc`), which
    /// is what keeps the idle-pad skip in `mixer` free.
    pub tail_frames: usize,
    /// Frames of [`Self::tail_frames`] not yet flushed out of the row. Held
    /// at `tail_frames` while the row's clock is on; counted down by the
    /// window length once it goes off. RT-owned; see the exclusive-idle
    /// early-out in `mixer`.
    pub flush_left: usize,
    /// Carry state across the `MAX_LIVE_BLOCK` windows of ONE callback block
    /// (Plan G2). It lives on the ROW, not in a parallel vector on the
    /// graph: a parallel vector has to be sized at construction, and every
    /// caller that pushes a row afterwards (several tests do) would silently
    /// lose that row's meters. Reset at the top of every render.
    pub win: TrackWindow,
}

/// The flush allowance a row carrying a LIVE NODE gets on top of its strip,
/// expressed at 48 kHz (Plan V — V2, Task 10; rate-true since fix round 3).
/// Use [`live_tail_frames`], never this constant directly.
///
/// Every other term in [`RtTrack::computed_tail_frames`] is a line the row
/// can be asked how long it is. An instrument's own release is not: it lives
/// inside the node, and `all_notes_off` does not perform it — it only MARKS
/// the voices released (`midi::synth::PolySynth`, `SamplerNode`, and both
/// plugin hosts, which merely QUEUE a CC 123 for the next `process`). The
/// ramp to zero runs inside `process`. So a row that stops being processed
/// the block after its clock ends does not truncate a decaying tail; it
/// strands a voice at whatever amplitude it had — full sustain, if the pad
/// was cut mid-note — and dumps it into the onset of the next press.
///
/// Why a fixed allowance rather than asking the node
/// (`LiveInstrument::tail_samples()`): it could not be honest for the nodes
/// that matter most. LV2 has no tail concept at all and CLAP's is advisory,
/// so such an accessor would return a confident number for `PolySynth` and a
/// guess for a plugin — a second source of truth that lies exactly where the
/// cost is highest. And why not a hard kill at the freeze point: not
/// implementable generically for the same reason. `PolySynth` and
/// `SamplerNode` could reset their voices; LV2 and CLAP have no generic
/// reset short of deactivate/reactivate, which is not an RT-path operation.
/// A bounded allowance is what V-17 (b) already commits to — an unbounded
/// tail is hard-cut at the end of the window, exactly as a transport stop
/// cuts a track's inserts.
///
/// 4096 frames at 48 kHz is 85 ms, comfortably past
/// `PolySynth::RELEASE_SECS` (80 ms, 3840 frames).
const LIVE_TAIL_FRAMES_AT_48K: usize = 4096;

/// The sample rate a graph falls back to when it is built without one — a
/// test rig, or `Control::rebuild` before a device has opened and set
/// `cache_rate`. See [`RtGraph::with_buses`], the ONE place that clamps.
pub const FALLBACK_RATE: u32 = 48_000;

/// [`LIVE_TAIL_FRAMES_AT_48K`] scaled to `rate`, in frames.
///
/// The allowance is a DURATION — an instrument's release ramp is 80 ms at
/// every sample rate — so it has to be counted in that rate's frames. Fixing
/// it at 4096 frames covered the ramp at 48 kHz and only half of it at
/// 96 kHz, which turned a full-amplitude stranded voice into a
/// half-amplitude one rather than into none.
///
/// Integer math, exact at the reference rate (48 kHz → 4096, 96 kHz → 8192).
pub fn live_tail_frames(rate: u32) -> usize {
    let rate = if rate == 0 { FALLBACK_RATE } else { rate };
    (LIVE_TAIL_FRAMES_AT_48K as u64 * rate as u64 / FALLBACK_RATE as u64) as usize
}

impl RtTrack {
    /// Clip-only track (audio tracks; also keeps tests terse).
    pub fn clips(slot: usize, clips: Vec<RtClip>) -> Self {
        Self {
            slot,
            clips,
            live: None,
            inserts: Vec::new(),
            sends: Vec::new(),
            pdc: None,
            out_pdc: None,
            output: None,
            tail_frames: 0,
            flush_left: 0,
            win: TrackWindow::default(),
        }
    }

    /// Re-derive [`Self::tail_frames`] from the row as it now stands.
    /// CONTROL THREAD, before publication.
    ///
    /// The arithmetic follows the STRIP, which is inserts → `pdc` →
    /// pre-fader taps → fader → post-fader taps → `out_pdc` → `route_out`
    /// (`mixer::render_impl`). Inserts and `pdc` are in series with
    /// everything after them, so they add. The send taps are taken from the
    /// strip BEFORE `out_pdc.process`, so `out_pdc` and the send delays are
    /// PARALLEL branches out of the same point: the worst path through the
    /// row is the longer of the two, not their sum.
    ///
    /// Over-counting never truncates, but it is not free either. V-17 makes
    /// a bus-routed pad's `out_pdc` and its send edge into that same bus
    /// EQUAL, so summing them would double the window — and the window is
    /// blocks of FULL-STRIP processing, inserts included, after every
    /// release. With one linear-phase EQ in that bus that is ~4096 extra
    /// frames, 85 ms, of it.
    pub fn recompute_tail_frames(&mut self, rate: u32) {
        self.tail_frames = self.computed_tail_frames(rate);
    }

    /// What [`Self::tail_frames`] would be for the row as it now stands.
    /// Split out so `mixer::render_impl` can `debug_assert` the stored value
    /// against it — see the funnel note on [`RtGraph::with_buses`].
    pub fn computed_tail_frames(&self, rate: u32) -> usize {
        let inserts: usize = self
            .inserts
            .iter()
            .filter(|i| !i.bypassed)
            .map(|i| i.latency)
            .sum();
        let line = |d: &Option<DelayLine>| d.as_ref().map_or(0, |d| d.delay());
        let sends = self
            .sends
            .iter()
            .map(|s| line(&s.delay))
            .max()
            .unwrap_or(0);
        let strip = inserts + line(&self.pdc) + line(&self.out_pdc).max(sends);
        // ...and the one term that is not a line on the strip: the
        // instrument's own release, which runs INSIDE the node.
        //
        // It ADDS, and running first is exactly why. `tail_frames` is the
        // row's whole path latency, so a fragment ENTERING the strip when the
        // flush starts leaves precisely as the window closes. Release
        // material is not one fragment: it keeps entering for the whole
        // release, so the LAST of it enters at `allowance` and still needs
        // `strip` more frames to traverse the inserts, the `pdc` and the
        // longer output branch. A `max` closes the window while that material
        // is still inside the chain — with a 2048-frame chain the tail of a
        // 3840-frame release is stranded at 4096 and replayed at the next
        // onset. Over-counting costs flush blocks; under-counting swallows
        // audio, and this row has swallowed it three times already.
        //
        // ONE rule, applied to any row with a live node, not just a player's.
        // It is inert for a track — `tail_frames` is only ever consumed under
        // `flushing`, and `flushing` is `exclusive_idle`, which no track row
        // is — so scoping it to players would buy nothing and would put a
        // second condition on a funnel whose whole value is having one.
        let live = if self.live.is_some() { live_tail_frames(rate) } else { 0 };
        strip + live
    }
}

/// One compiled send edge (Plan G2): a copy of the source strip's signal,
/// scaled by a live amount, added into a bus accumulator.
pub struct RtSend {
    /// Index into `RtGraph::buses`. Compiled, so the RT thread never
    /// resolves a track id.
    pub bus: usize,
    /// Index into `ParamTable::send_amount`. Read once per run, exactly
    /// like the fader gain — no smoothing, same convention.
    pub amount: usize,
    /// Tap point (see `types::SendSlot::pre_fader`).
    pub pre_fader: bool,
    /// Compensation for THIS EDGE: what the copy waits so it reaches the
    /// bus in step with that bus's other inputs. `None` (the common case)
    /// when everything arriving there is already aligned — see
    /// `audio::bus`'s module doc for when it is not.
    pub delay: Option<DelayLine>,
}

/// One compiled bus/return strip (Plan G2). Its SOURCE is the accumulator
/// that this block's send taps wrote into; from there it is an ordinary
/// strip — inserts, compensating delay, fader, meters, master.
pub struct RtBus {
    /// Index into `ParamTable`: a bus has a fader, a pan, a mute and a
    /// meter lane like any other strip.
    pub slot: usize,
    /// Compiled insert chain (document order) — this is where the shared
    /// convolution reverb actually lives.
    pub inserts: Vec<InsertNode>,
    /// Copies this bus peels off into OTHER buses. A bus is an ordinary
    /// node in the routing graph, so it sends like anything else.
    pub sends: Vec<RtSend>,
    /// Where this bus's fader output goes: an index into `RtGraph::buses`,
    /// or `None` for the master. Always a LATER index than this bus's own —
    /// `bus::compile_routing` hands the renderer a topological order, so one
    /// forward pass is enough and a cycle cannot be expressed.
    pub output: Option<usize>,
    /// Compensation on this bus's OUTPUT path, applied after its send taps
    /// so only what continues downstream waits. `None` = nothing at its
    /// destination is slower than this.
    pub out_pdc: Option<DelayLine>,
    /// Window carry state, for the same reason `RtTrack::win` exists.
    pub win: TrackWindow,
}

/// One strip's carry state across the windows of a single callback block
/// (Plan G2 — see `RtTrack::win`). Reset at the top of every render.
#[derive(Debug, Default, Clone, Copy)]
pub struct TrackWindow {
    /// This run follows a discontinuity (seek, stop→play, loop wrap), so
    /// the live node owes an `all_notes_off` before it processes.
    pub disc: bool,
    /// Meter fold for the whole block: peak is a running max, sum-of-squares
    /// a running sum, so folding window by window gives the same numbers the
    /// single-pass version produced.
    pub pk_l: f32,
    pub pk_r: f32,
    pub ss_l: f32,
    pub ss_r: f32,
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
    /// Compiled bus/return strips (Plan G2), rendered AFTER every track in
    /// each window so their accumulators are complete. Empty on a project
    /// with no buses, which is byte-for-byte today's graph.
    pub buses: Vec<RtBus>,
    /// Preallocated stereo strip buffer (`MAX_LIVE_BLOCK * 2`, always).
    /// Clips + live sum here, then inserts REPLACE, then the shared fader.
    /// Allocated at BUILD time on the control thread — never on the RT path.
    pub track_buf: Vec<f32>,
    /// Post-fader stereo scratch (`MAX_LIVE_BLOCK * 2`, always). The fader
    /// writes here instead of straight into `out` so a POST-fader send has
    /// something to tap and so `RtTrack::out_pdc` can delay the dry path
    /// after the tap. Preallocated at BUILD time, like `track_buf`.
    pub post_buf: Vec<f32>,
    /// Per-bus input accumulators, `buses.len() * MAX_LIVE_BLOCK * 2`
    /// interleaved stereo frames. Zeroed per window, written by the send
    /// taps, consumed by the bus pass. Preallocated at BUILD time; empty
    /// when the graph has no buses, so a project without returns allocates
    /// nothing extra.
    pub bus_buf: Vec<f32>,
    /// Scratch for one send tap (`MAX_LIVE_BLOCK * 2`). A tap is a COPY that
    /// may carry its own compensating delay, so it cannot be scaled straight
    /// out of the strip buffer into the accumulator. Preallocated at BUILD
    /// time; empty when the graph has no buses.
    pub tap_buf: Vec<f32>,
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
    /// THIS graph's playheads (Plan V — V2). Versioned with the snapshot for
    /// the same reason `params` is: a retired graph keeps reading the table
    /// it was built with, so a rebuild that renumbers clocks cannot bleed
    /// into a render already in flight.
    ///
    /// A running clock's state is CARRIED ACROSS a rebuild by
    /// `ClockTable::carry_over` (`engine::rebuild`), because the overlay
    /// this replaces lived on `SharedRt` and a clip edit made while a pad
    /// sounded did not rewind it.
    pub clocks: Arc<crate::audio::clock::ClockTable>,
    /// The engine rate this graph was BUILT at, and the single source of
    /// truth for anything derived from a rate at build time — today
    /// [`live_tail_frames`], through `RtTrack::computed_tail_frames`.
    ///
    /// `mixer::render_impl` re-derives every row's tail in a `debug_assert`
    /// and MUST read this, never its own `sample_rate` argument: the two
    /// disagree across a device rate change until the next rebuild, and a
    /// row built at one rate and checked against another would fire the
    /// assert on the RT thread for a graph that is perfectly correct.
    pub rate: u32,
}

impl RtGraph {
    /// Build a snapshot, always allocating the strip `track_buf`
    /// (unified clip/live/insert path).
    /// Build a snapshot at the [`FALLBACK_RATE`].
    ///
    /// For TEST RIGS. Every production path has its real rate in hand —
    /// `offline::build_graph` takes one, `Control::rebuild` has
    /// `self.cache_rate`, `loopjam::render_region_stereo` has `engine_rate` —
    /// and passes it through [`Self::with_buses`]. A production caller that
    /// reaches for this instead silently sizes its live rows' flush windows
    /// at 48 kHz whatever the device is doing.
    pub fn new(tracks: Vec<RtTrack>, generation: u64, params: Arc<ParamTable>) -> Self {
        Self::with_buses(tracks, Vec::new(), generation, params, FALLBACK_RATE)
    }

    /// [`Self::new`] plus compiled bus strips (Plan G2). Sizes the per-bus
    /// accumulators here, on the control thread — the RT pass only ever
    /// zeroes and adds into them.
    pub fn with_buses(
        tracks: Vec<RtTrack>,
        buses: Vec<RtBus>,
        generation: u64,
        params: Arc<ParamTable>,
        rate: u32,
    ) -> Self {
        // THE one clamp. `Control::rebuild` runs with `cache_rate == 0` until
        // a device opens, and a rate of 0 would size every live row's flush
        // window at 0 — the bare skip, i.e. exactly the defect the window
        // exists to prevent. Clamped here, where the value is STORED, so
        // `graph.rate` is the single sane number every later reader sees and
        // `live_tail_frames` never has to guess.
        let rate = if rate == 0 { FALLBACK_RATE } else { rate };
        // The one place every graph — engine rebuild, offline bounce, every
        // test rig — passes through, so a row's flush window is derived from
        // the lines it actually carries.
        //
        // The funnel only covers what the row carries WHEN IT IS BUILT.
        // Production never adds a line afterwards; test rigs do, and such a
        // row would keep `tail_frames = 0` — a flush window of nothing,
        // which is exactly the defect fix round 3 closed, arriving silently
        // through a rig instead of through the code. `mixer::render_impl`
        // therefore `debug_assert`s the stored value against
        // `RtTrack::computed_tail_frames` on every row it renders, so the
        // convention is checked rather than merely documented.
        let mut tracks = tracks;
        for tr in tracks.iter_mut() {
            tr.recompute_tail_frames(rate);
        }
        let track_buf = vec![0.0; MAX_LIVE_BLOCK * 2];
        let post_buf = vec![0.0; MAX_LIVE_BLOCK * 2];
        let bus_buf = vec![0.0; buses.len() * MAX_LIVE_BLOCK * 2];
        let tap_buf = vec![0.0; if buses.is_empty() { 0 } else { MAX_LIVE_BLOCK * 2 }];
        let n_chunks = (params.len() + METER_CHUNK_SLOTS - 1) / METER_CHUNK_SLOTS;
        let n_chunks = n_chunks.max(1);
        let meter_scratch = (0..n_chunks)
            .map(|i| {
                let mut b = RawMeterBlock::new(generation, 0, 0);
                b.base_slot = (i * METER_CHUNK_SLOTS) as u32;
                b
            })
            .collect();
        // Plan V — V2: a graph nobody has given a clock table to IS the
        // transport, playing. That is what every caller of `mixer::render`
        // meant before clocks existed (the offline bounce — V-15 — the
        // headless previews, and every mixer test), so expressing it as the
        // default keeps this swap behaviour-neutral for all of them.
        // `engine::rebuild` overwrites the field with the real, scene-bearing
        // table before the graph is published.
        let clocks = crate::audio::clock::ClockTable::with_slots_and_clocks(params.len(), 1);
        clocks.set_transport_playing(true);
        Self {
            tracks,
            buses,
            track_buf,
            post_buf,
            bus_buf,
            tap_buf,
            generation,
            params,
            meter_scratch,
            track_ramps: Vec::new(),
            clicks: Arc::new(Vec::new()),
            clocks: Arc::new(clocks),
            rate,
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
    /// The CURRENT graph's playheads (Plan V — V2), published here for the
    /// same reason `params` is: firing a pad must reach the table the
    /// rendering graph actually reads, and a pad press must never rebuild
    /// the graph.
    pub clocks: Arc<crate::audio::clock::ClockTable>,
    /// Launch binding id -> its clock in `clocks` (Plan V — V2, Task 8).
    /// The scene half of `slots`: it is what turns a pad press into an
    /// atomic write into a lane that already exists, instead of a graph
    /// rebuild. Sized and numbered by `engine::rebuild` from the document,
    /// so a binding added since the last rebuild is simply absent — and a
    /// fire naming it is dropped with a warn rather than firing whichever
    /// clock happens to sit at that index.
    pub scene_clocks: HashMap<String, u32>,
    /// `PlayerId` -> its clock in `clocks` (Plan V — V2, Task 9). The player
    /// half of `scene_clocks`, and the same argument: firing a pad has to be
    /// an atomic write into a lane that already exists, because a pad press
    /// must never rebuild the graph (the RT contract).
    ///
    /// The clocks it names are the RESERVED range `1 ..= players.len()`,
    /// allocated by `engine::rebuild` in document order and bound to each
    /// player's mixer slot for the life of the graph — a player, unlike a
    /// scene, never borrows another node's slot, so nothing ever releases it
    /// (`ClockTable::with_slots_clocks_and_players` is what makes the idle
    /// state silence rather than "rejoin the arrangement", V-2).
    ///
    /// A player added since the last rebuild is simply absent, and
    /// `ControlPlane::player_fire` reports that rather than firing whichever
    /// clock happens to sit at that index.
    pub player_clocks: HashMap<crate::ids::PlayerId, u32>,
    /// The clock `engine::rebuild` minted for tracks stranded by a binding
    /// DELETED while its scene sounded (or cut with its flush still unread):
    /// stopped, owing one discontinuity, and in no `scene_clocks` map because
    /// it has no binding left. Present only when something was actually
    /// stranded — a clock every graph carried would be one `begin_block` and
    /// `advance` walk every block for a case that is not happening.
    ///
    /// Published so `release_finished_scenes` can hand those tracks back once
    /// the flush has been delivered; see there for why leaving them bound
    /// indefinitely is not an option.
    pub orphan_clock: Option<u32>,
    pub slots: HashMap<TrackId, usize>,
    /// `SendSlot::id` -> index into `ParamTable::send_amount` (Plan G2),
    /// derived by `types::derive_send_slots` in the same rebuild that built
    /// `params`. This is what turns a send-amount knob write into an atomic
    /// store instead of a graph rebuild — the send half of `slots`.
    pub send_slots: HashMap<String, usize>,
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
    /// Hand back every track whose scene has ended AND has been told so.
    /// One pass over the slots, run by the launch drive thread's poll — the
    /// ONLY place a scene's slots are released. `ControlPlane::cut_scene`,
    /// `stop_launch_overlay` and `clear_launch_audible` all deliberately stop
    /// a clock without releasing what is bound to it.
    ///
    /// Two conditions, and both are load-bearing:
    ///
    /// * the clock is not running — the scene is over, so the node rejoins
    ///   the arrangement (`mixer::node_playhead`'s third case);
    /// * and the block that latches its parting discontinuity has BEGUN
    ///   (`ClockTable::flush_pending_for`). Releasing before that drops the
    ///   `all_notes_off` the cut left behind and the note hangs, and "the
    ///   drive poll came after the cut" does NOT imply "a block ran in
    ///   between": the poll is 8 ms and a block at 48 kHz / 512 frames is
    ///   10.7 ms.
    ///
    ///   BEGUN, not "read by this node". `begin_block` clears the pending flag
    ///   at the top of a block, so a poll landing between that and this
    ///   slot's own `node_playhead` call inside the SAME callback can still
    ///   release early. Sub-millisecond, strictly narrower than what it
    ///   replaced, and closing it needs a blocks-rendered counter the release
    ///   waits on — booked as a follow-up, not built here.
    ///
    /// V-14 falls out of the shape: the clock released from is the one the
    /// slot currently reads, so a track a second scene has since claimed is
    /// skipped while that scene still runs, and `release_slot_if` covers the
    /// write that lands between the read and the release.
    ///
    /// The ORPHAN clock (`engine::rebuild`, a binding deleted while its scene
    /// sounded) is released here too, by the same two rules. It has to be:
    /// `mixer::node_playhead` reports a slot on any non-transport clock as
    /// `own_clock`, which `audible_with_launch` lets override another track's
    /// SOLO — and unlike a normal ending, nothing else would ever release
    /// this one (mute/solo are plain `Set`s that schedule no rebuild), so
    /// soloing a track would silently fail to silence the stranded one until
    /// some unrelated edit happened to rebuild the graph.
    pub fn release_finished_scenes(&self) {
        if self.scene_clocks.is_empty() && self.orphan_clock.is_none() {
            return;
        }
        let scenes: std::collections::HashSet<u32> =
            self.scene_clocks.values().copied().collect();
        for slot in 0..self.params.len() {
            let clock = self.clocks.clock_of(slot);
            let ours = scenes.contains(&clock) || self.orphan_clock == Some(clock);
            if !ours || self.clocks.is_on(clock) || self.clocks.flush_pending_for(clock) {
                continue;
            }
            self.clocks.release_slot_if(slot, clock);
        }
    }

    /// A fresh gen-0 table with no params and no slots — the shape every
    /// `AudioState::default()` and every control-module test fixture wants
    /// before the first real rebuild publishes something. Single source of
    /// truth for "what does an empty `GraphTables` look like".
    pub fn empty() -> SharedGraphTables {
        Arc::new(parking_lot::Mutex::new(GraphTables {
            generation: 0,
            params: Arc::new(ParamTable::default()),
            // Sized to MATCH `params`. Only the transport clock: with
            // per-binding scene clocks (Task 8) there is no such thing as
            // "the" scene clock to pre-create, and `scene_clocks` below is
            // correspondingly empty — a fire before the first real rebuild
            // is dropped with a warn, which is the same "unknown index means
            // drop the write" rule `ParamTable`'s setters follow.
            clocks: Arc::new(crate::audio::clock::ClockTable::with_slots_and_clocks(
                ParamTable::default().len(),
                1,
            )),
            scene_clocks: HashMap::new(),
            player_clocks: HashMap::new(),
            orphan_clock: None,
            slots: HashMap::new(),
            send_slots: HashMap::new(),
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

    /// V-4's gate. The overlay's single atomic set is DELETED, not
    /// deprecated: `audio::clock` is the only playhead mechanism now, and a
    /// reintroduced `launch_*` atomic here would silently give the engine a
    /// second, contradictory notion of where a node is — the exact defect
    /// the clock table exists to make impossible.
    #[test]
    fn the_launch_overlay_is_gone_from_this_file() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/audio/rt.rs"),
        )
        .expect("rt.rs is readable");
        // Skip this test's own body, which necessarily names them.
        let body = src
            .split("fn the_launch_overlay_is_gone_from_this_file")
            .next()
            .expect("split always yields a first part");
        for banned in [
            "launch_on",
            "launch_pos",
            "launch_start",
            "launch_end",
            "launch_discont",
            "launch_ended",
            "LaunchPlayhead",
            "FLAG_LAUNCH",
        ] {
            assert!(
                !body.contains(banned),
                "{banned} is back in rt.rs \u{2014} see audio::clock and ruling V-4"
            );
        }
    }

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
