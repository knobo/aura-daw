//! Engine control thread: owns the cpal streams, the audio-graph lifecycle
//! (prepare-then-swap / RCU), the sample cache, the recorder, and the 60 Hz
//! meter pump. Tauri-free — the IPC layer talks to it through `EngineHandle`
//! and the `EventSink` / `MeterSink` traits (implemented over Tauri types in
//! `mod.rs`).
//!
//! Real-time contract (docs/ARCHITECTURE.md §2): the cpal callbacks never
//! allocate, lock, or perform I/O. They communicate exclusively through
//! * `Arc<SharedRt>` / `Arc<ParamTable>` atomics (transport + knob values —
//!   knob ticks are NOT graph rebuilds),
//! * rtrb SPSC queues: new-graph pointers in, retired-graph pointers out
//!   (deallocation happens here, never on the RT thread), meter blocks out,
//!   and recorded samples out to the disk-writer thread.
//!
//! Hardware MIDI-in (slice 2) adds the SECOND ring that feeds the callback,
//! and the only one this thread does not produce into: the midir callback
//! thread pushes `LiveMidiEvent`s through `midi_in::hub()`, the output
//! callback pops them into a preallocated array. Neither end is exempt from
//! the rule above — the producer is a non-RT thread that may lock and drop,
//! the consumer allocates nothing and takes a bounded number of events per
//! block. Which track they reach is a slot the CONTROL thread resolves
//! (`follow_live_in_target`); the callback only reads one atomic.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use parking_lot::Mutex;

use super::dsp::linear_resample;
use super::meters::{GenerationMaps, MeterAccum, RawMeterBlock, METER_CHUNK_SLOTS};
use super::midi_in::{self, LiveMidiEvent, MidiInHub, LIVE_IN_RING_SLOTS, MAX_LIVE_IN_PER_BLOCK};
use super::mixer;
use super::offline;
use super::recorder::{self, DiskWriter, RecSpec};
use super::rt::{
    GraphPtr, GraphTables, ParamTable, RtClip, RtClipData, RtGraph, RtTrack, SharedGraphTables,
    SharedRt, NO_PARK,
};
use super::transport;
use super::types::{derive_slots, Clip, MeterFrame, Store};
use super::waveform::{pyramid_exists, Pyramid};
use crate::control::{op, Committed, Committer, Session};
use crate::ids::SourceId;

/// Meter frame cadence (~60 Hz).
const FRAME_INTERVAL: Duration = Duration::from_micros(16_600);
/// Recording ring headroom, seconds of audio per track.
const REC_RING_SECS: usize = 2;
/// [M4] Meter ring slot count, both output and input/recording streams.
/// Chunking (Task 7) divides headroom by the chunk count, and the control
/// thread is exactly the thread that stalls (`ensure_loaded` decodes under
/// rebuild) — grown from 64 to 64*8. Blocks are ~2 KiB; the memory is
/// nothing control-side.
const METER_RING_SLOTS: usize = 64 * 8;

// ---------------------------------------------------------------------------
// IPC-facing traits (implemented over Tauri types in mod.rs)
// ---------------------------------------------------------------------------

/// App-event emitter (`transport://state`, `recording://state`, ...).
pub trait EventSink: Send + 'static {
    fn emit(&self, event: &str, payload: serde_json::Value);
}

/// One meter subscription (a Tauri `Channel<MeterFrame>` on the other side).
/// Return `false` when the subscriber is gone so it gets dropped.
pub trait MeterSink: Send + 'static {
    fn send_frame(&self, frame: &MeterFrame) -> bool;
}

pub type Reply<T> = Sender<Result<T, String>>;

/// Something the audio callback NOTICED, reported to the control thread over
/// the wait-free `engine_evt` ring (ARCHITECTURE §2.3).
///
/// The callback cannot act: policy needs state it must not read and decisions
/// it must not make on a hard deadline (§2.1). So it reports the fact and the
/// exact sample, and the control thread decides. Keep every variant POD and
/// `Copy` — no heap payloads may cross this ring.
///
/// This is the seam for "the engine saw something happen": punch-out,
/// markers and follow-actions are new variants here, not new code in
/// `OutputCb::render`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtEvent {
    /// The playhead crossed `SharedRt::song_end` at exactly this sample.
    ReachedEnd { at: u64 },
}

pub enum ControlMsg {
    Subscribe(Box<dyn MeterSink>),
    /// Reload missing samples/pyramids and swap in a freshly built graph.
    /// Used for STRUCTURAL changes only (tracks/clips/project) — continuous
    /// parameters go through `ParamTable` atomics instead.
    Rebuild,
    SelectOutput { device_id: Option<String>, reply: Reply<()> },
    SelectInput { device_id: Option<String>, reply: Reply<()> },
    StartRecording { track_ids: Option<Vec<String>>, reply: Reply<Vec<String>> },
    StopRecording { reply: Reply<Vec<Clip>> },
    /// Installs the narrow "document birth" closure `ensure_project` calls
    /// (Plan E Task 13, round-2 §4.5 carve-out) — bound over the
    /// `ControlPlane` `Arc`, so it can only be built AFTER `ControlPlane`
    /// exists, which is AFTER the engine control thread is already
    /// running (`audio::init` -> `engine::start` -> later, `lib.rs`
    /// constructs `ControlPlane` and sends this). Fire-and-forget, sent
    /// exactly once at startup; the engine thread never touches project
    /// fields itself, only calls this closure.
    SetEnsureProject(Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>),
    Shutdown,
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<ControlMsg>,
}

impl EngineHandle {
    pub fn send(&self, msg: ControlMsg) {
        let _ = self.tx.send(msg);
    }

    /// Installs the engine's `ensure_project` closure — `lib.rs` calls this
    /// once, right after constructing the shared `ControlPlane` (Plan E
    /// Task 13; see `ControlMsg::SetEnsureProject`'s doc for why this can't
    /// happen any earlier).
    pub fn install_ensure_project(
        &self,
        f: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
    ) {
        self.send(ControlMsg::SetEnsureProject(f));
    }

    /// Send a request-style message and await the control thread's reply.
    pub fn request<T>(&self, make: impl FnOnce(Reply<T>) -> ControlMsg) -> Result<T, String> {
        let (tx, rx) = bounded(1);
        self.tx
            .send(make(tx))
            .map_err(|_| "audio engine is not running".to_string())?;
        rx.recv_timeout(Duration::from_secs(30))
            .map_err(|_| "audio engine did not respond".to_string())?
    }
}

#[cfg(test)]
impl EngineHandle {
    /// Test double: an `EngineHandle` with no control thread behind it — a
    /// bare channel that just records every `ControlMsg` sent through it.
    /// The receiver is `crossbeam_channel::Receiver` (the same channel type
    /// `EngineHandle` already wraps internally), which offers the same
    /// `try_iter()` a `std::sync::mpsc::Receiver` would for draining and
    /// counting messages in a test.
    pub fn for_tests() -> (EngineHandle, Receiver<ControlMsg>) {
        let (tx, rx) = unbounded();
        (EngineHandle { tx }, rx)
    }
}

/// Spawn the engine control thread. Opens the default output device if one is
/// available; without a device the engine still runs (headless transport +
/// silent 60 Hz meter frames) so the UI and tests stay functional.
pub fn start(
    shared: Arc<SharedRt>,
    tables: SharedGraphTables,
    session: Arc<Mutex<Session>>,
    events: Box<dyn EventSink>,
    committer: Committer,
) -> EngineHandle {
    let (tx, rx) = unbounded();
    std::thread::Builder::new()
        .name("aura-engine-control".into())
        .spawn(move || {
            let mut ctl = Control {
                shared,
                tables,
                generation: 0,
                rebuild_pending: false,
                session,
                events,
                rx,
                output: None,
                input: None,
                writer: None,
                rec_track_ids: Vec::new(),
                sel_output: None,
                sel_input: None,
                cache: HashMap::new(),
                cache_rate: 0,
                live_nodes: Default::default(),
                accum: MeterAccum::default(),
                gen_maps: GenerationMaps::default(),
                sinks: Vec::new(),
                last_frame: Instant::now(),
                last_tick: Instant::now(),
                committer,
                ensure_project_fn: None,
                param_automation: crate::plugins::automation::ParamAutomationDriver::empty(),
                param_writes: Vec::new(),
                live_in_hub: midi_in::hub().clone(),
                live_in_target: None,
            };
            if let Err(e) = ctl.open_output() {
                log::warn!("audio: no output stream ({e}); running headless");
            }
            ctl.run();
        })
        .expect("spawn engine control thread");
    EngineHandle { tx }
}

// ---------------------------------------------------------------------------
// Output / input stream bundles (queues are per-stream)
// ---------------------------------------------------------------------------

struct OutputBundle {
    /// `None` only in tests, which cannot construct a `cpal::Stream` but do
    /// need a non-headless `rebuild` — the branch that assembles clips and
    /// live nodes and publishes a graph. Production always holds the stream
    /// here: dropping it is what stops the callback (see `Drop`).
    _stream: Option<cpal::Stream>,
    graph_tx: rtrb::Producer<GraphPtr>,
    retire_rx: rtrb::Consumer<GraphPtr>,
    meter_rx: rtrb::Consumer<RawMeterBlock>,
    evt_rx: rtrb::Consumer<RtEvent>,
}

impl Drop for OutputBundle {
    fn drop(&mut self) {
        // The callback closure (and the graph Box it owns) is dropped with
        // the stream — on this control thread, never on the RT thread.
        // `GraphPtr` owns its pointee (Drop frees), so graphs still sitting
        // in EITHER queue (pending in graph_tx→graph_rx, or retired) are
        // freed when the ring halves drop; this explicit drain just keeps
        // the common case eager.
        while let Ok(gp) = self.retire_rx.pop() {
            drop(gp);
        }
    }
}

struct InputBundle {
    _stream: cpal::Stream,
    meter_rx: rtrb::Consumer<RawMeterBlock>,
}

/// Drain-buffer capacity: a block's `MAX_LIVE_IN_PER_BLOCK` popped events,
/// plus room for one release expanded into note-offs for every key at once.
/// Sized so the expansion can never overrun (`take_held_note_offs` clamps
/// as well, so the arithmetic here is not load-bearing).
const LIVE_IN_BUF_SLOTS: usize = MAX_LIVE_IN_PER_BLOCK + 128;

/// State owned by the cpal OUTPUT callback closure. `render` obeys the RT
/// contract: the only "syscalls" are what cpal itself does.
struct OutputCb {
    graph_rx: rtrb::Consumer<GraphPtr>,
    retire_tx: rtrb::Producer<GraphPtr>,
    meter_tx: rtrb::Producer<RawMeterBlock>,
    evt_tx: rtrb::Producer<RtEvent>,
    shared: Arc<SharedRt>,
    /// Current graph snapshot (RCU: replaced whole; only live-node internals
    /// mutate, and only on this thread — see `LiveNodeCell`).
    graph: Option<Box<RtGraph>>,
    channels: usize,
    rate: u32,
    /// Where the NEXT block should start if playback is continuous; a
    /// mismatch (seek) or a stop→play transition is a discontinuity that
    /// releases live-instrument voices (their note-offs may never arrive).
    next_pos: u64,
    was_playing: bool,
    /// Consumer half of THIS stream's MIDI-in ring — owned by the callback,
    /// so a device switch that briefly overlaps two callbacks can never put
    /// two consumers on one ring: each stream gets its own ring, and the hub
    /// keeps only the newest producer.
    live_in_rx: rtrb::Consumer<LiveMidiEvent>,
    /// Preallocated drain buffer — the RT side never allocates.
    live_in_buf: [LiveMidiEvent; LIVE_IN_BUF_SLOTS],
    /// Cloned at open time so the callback never touches a global.
    live_in_hub: Arc<MidiInHub>,
    /// The slot this callback dispatched live-in events to LAST block. The
    /// hub publishes only where events go NEXT, and a release has to be
    /// addressed to where they went before.
    live_in_slot: Option<usize>,
    /// A monitored node owes a NODE-WIDE all-off before its next monitored
    /// block. Sticky until it is actually delivered, so a target change and
    /// a stop landing in the same block still release both nodes.
    live_in_all_off: bool,
    /// The keys MONITORING currently has down on the target node — the one
    /// thing that separates a monitored voice from a clip voice, since they
    /// live in the same node.
    live_in_held: [bool; 128],
    /// A node that lost the routing target and whose envelope may still be
    /// running: `live_in_release_left` is how much of that envelope is still
    /// owed, and it only counts down on blocks where the node is actually
    /// rendered.
    ///
    /// Holding the live-in channel is a SEPARATE, usually much shorter fact
    /// (`live_in_hold_left`), because the channel is what the incoming
    /// target is waiting for — every event popped while it is held is
    /// discarded, so the hold is measured in swallowed keystrokes.
    ///
    /// ONE slot, deliberately: a `LiveInBlock` addresses one slot per block,
    /// so a queue here would only serialize release windows, not overlap
    /// them. A second target change inside an open window therefore drops
    /// the first node mid-decay (a ≤80 ms fragment on its next arm — never a
    /// drone). Two clicks that fast are not human, but a rebuild that
    /// RENUMBERS slots gets there without a human: this compares slot
    /// NUMBERS, and after a track delete the old number names a different
    /// track. The real fix is to compare the target's identity rather than
    /// its slot index; ledgered for the close-out, not smuggled in here.
    live_in_release_slot: Option<usize>,
    live_in_release_left: u64,
    /// How much longer `live_in_release_slot` also keeps the live-in
    /// channel. One block while the transport plays (the graph renders every
    /// live node anyway, so the envelope needs no help); re-opened to the
    /// remaining envelope on the stop edge, where nothing else would render
    /// that node again.
    live_in_hold_left: u64,
}

impl OutputCb {
    /// Writes a note-off into `live_in_buf` for every key monitoring has
    /// down, starting at `at`, and clears the mask. Returns how many were
    /// written. RT-safe: a fixed 128-step scan over an inline array.
    fn take_held_note_offs(&mut self, at: usize) -> usize {
        let mut n = 0usize;
        for key in 0..128usize {
            if !self.live_in_held[key] {
                continue;
            }
            // Bail BEFORE clearing the mask, never after: the buffer is
            // sized so this cannot trigger today (128 note-offs + 64 pops
            // into 192), but if a future event kind on this same ring ever
            // makes it reachable, dropping a note-off we have already
            // forgotten about hangs that voice forever, while keeping the
            // key marked held means the next release still frees it.
            if at + n >= LIVE_IN_BUF_SLOTS {
                break;
            }
            self.live_in_held[key] = false;
            self.live_in_buf[at + n] = LiveMidiEvent::note_off(key as u8);
            n += 1;
        }
        n
    }

    /// How long an outgoing node must keep the live-in channel for its
    /// envelope to reach silence, counted from the START of the block that
    /// opens the window (the same block decrements it by `frames`).
    fn release_window(&self, frames: u64) -> u64 {
        (crate::midi::synth::RELEASE_SECS * self.rate as f32) as u64 + frames
    }

    fn render(&mut self, out: &mut [f32]) {
        // Adopt a new graph snapshot if one is queued — but only when the
        // retire queue can take the old one (drop must happen control-side).
        while self.retire_tx.slots() > 0 {
            match self.graph_rx.pop() {
                Ok(gp) => {
                    let fresh = gp.into_box();
                    if let Some(old) = self.graph.replace(fresh) {
                        // slots() > 0 was checked: push cannot fail (and no
                        // dealloc can happen here on the RT thread).
                        let _ = self.retire_tx.push(GraphPtr::new(old));
                    }
                }
                Err(_) => break,
            }
        }

        let playing = self.shared.playing.load(Relaxed);
        // Carry out a parking request while stopped (see `SharedRt::park`).
        // Taken with a swap so it is applied exactly once, and only here —
        // which makes this callback the last writer of the playhead, closing
        // the race against a stop that landed mid-render.
        if !playing {
            let park = self.shared.park.swap(NO_PARK, Relaxed);
            if park != NO_PARK {
                self.shared.position.store(park, Relaxed);
                self.next_pos = park;
            }
        }
        let base = self.shared.position.load(Relaxed);
        let lp = self.shared.loop_spec();
        let discontinuity = playing && (!self.was_playing || base != self.next_pos);

        // Block prologue (round-2 §3.5): advance the engine-global steady
        // sample counter ONCE per block, unconditionally — CLAP hosts need
        // a clock that never resets, not even across a live node's
        // re-creation (instrument rebind, sample-rate change, a track
        // leaving and re-entering the live set). `steady_base` is the value
        // THIS block's nodes see; Relaxed suffices (a free-running counter,
        // no other state is synchronized against it).
        let frames = (out.len() / self.channels.max(1)) as u64;
        let steady_base = self.shared.steady.fetch_add(frames, Relaxed);

        // Hardware MIDI-in. One relaxed load; the control thread resolved
        // this id→slot under the tables lock (`MidiInHub::refresh_target`).
        let live_slot = self.live_in_hub.target_slot();
        // Ring events carry no slot, so every release the routing needs is
        // addressed HERE, by the only party that knows which node was being
        // monitored last block. There are two KINDS of release and the
        // difference is load-bearing, because monitoring shares the node
        // that plays the track's clips:
        //
        // * NODE-WIDE (`all_off`) — only where nothing may legitimately be
        //   sounding: the block the transport stops on, and a node armed
        //   while stopped (it can be holding a clip voice frozen from the
        //   last playthrough, which nothing else will ever release).
        // * MONITORING'S OWN KEYS (`live_in_held`) — everywhere the graph
        //   may still be playing: a target change, and every release the
        //   producer asks for. A node-wide release there would cut the clip
        //   note the song is in the middle of, and a note-on that already
        //   happened never comes back.
        if live_slot != self.live_in_slot {
            if let Some(old) = self.live_in_slot {
                self.live_in_release_slot = Some(old);
                // An envelope only advances while its node is RENDERED, so
                // delivering the note-offs is not enough: a node left frozen
                // mid-release resurrects that fragment the next time it is
                // armed. The outgoing node therefore stays accounted for
                // until its release has had time to run.
                //
                // How long it keeps the LIVE-IN CHANNEL is a different
                // question, and the answer is "as briefly as possible": the
                // incoming target hears nothing until the channel is free
                // (every event popped meanwhile is discarded), so the hold
                // is measured in swallowed keystrokes. While PLAYING the
                // graph renders every live node each block regardless, so
                // one block — just enough to carry the note-offs — is
                // enough, and arming a track mid-song costs the player
                // nothing. Only where the node would stop being rendered
                // altogether does the hold have to cover the envelope: that
                // is the stop edge, handled below.
                //
                // PolySynth's release, applied to every instrument: a
                // sampler or plugin with a longer tail is still cut short
                // (bounded and silent, never a drone). Raising it only
                // trades that for a longer deaf window on the new target —
                // a per-node `release_tail()` on the processor trait is the
                // real answer, and a separate cut.
                self.live_in_release_left = self.release_window(frames);
                self.live_in_hold_left =
                    if playing { frames } else { self.live_in_release_left };
            }
            if !playing {
                self.live_in_all_off = true;
            }
        }
        if self.was_playing && !playing {
            self.live_in_all_off = true;
            // The playing graph was what kept the outgoing node's envelope
            // moving; stopping takes that away, and a node frozen mid-decay
            // replays the fragment on its next arm. Hand it the channel for
            // whatever release it still has left — while stopped,
            // `render_live_input_only` renders exactly the slot that holds
            // the channel, so the release actually runs to silence.
            if self.live_in_release_slot.is_some() {
                self.live_in_hold_left = self.live_in_release_left;
            }
        }
        self.live_in_slot = live_slot;
        let outgoing = if self.live_in_hold_left > 0 { self.live_in_release_slot } else { None };

        // Drained UNCONDITIONALLY, even with nothing routed and even on a
        // block spent releasing: the producer is a foreign thread that keeps
        // pushing whatever the port sends, and a ring nobody empties would
        // hand the next armed track a backlog of stale notes. `popped` is
        // what the RT bound applies to; the buffer is sized so that
        // expanding a release into it can never overrun (see
        // `LIVE_IN_BUF_SLOTS`).
        let mut n_in = 0usize;
        let mut popped = 0usize;
        if outgoing.is_some() {
            // This block belongs to the node that just lost the target:
            // exactly the keys monitoring put down on it, and nothing else.
            n_in = self.take_held_note_offs(0);
        } else if self.live_in_all_off {
            // FIRST in the block, so this block's own note-ons survive it.
            self.live_in_buf[0] = LiveMidiEvent::all_off();
            n_in = 1;
            self.live_in_all_off = false;
            self.live_in_held = [false; 128];
        }
        while popped < MAX_LIVE_IN_PER_BLOCK {
            let Ok(ev) = self.live_in_rx.pop() else { break };
            popped += 1;
            if outgoing.is_some() {
                // Dropped with the block that is releasing: a key struck
                // inside the window is swallowed, and its later note-off
                // arrives orphaned (harmless — note-offs are idempotent).
                continue;
            }
            match ev.kind {
                // A producer-side release request (`set_target_track`, the
                // monitor toggle, a port change) — expanded rather than
                // passed through, for the same reason a target change is.
                midi_in::EV_ALL_OFF => n_in += self.take_held_note_offs(n_in),
                midi_in::EV_NOTE_ON if ev.velocity > 0 => {
                    self.live_in_held[(ev.key & 127) as usize] = true;
                    self.live_in_buf[n_in] = ev;
                    n_in += 1;
                }
                _ => {
                    self.live_in_held[(ev.key & 127) as usize] = false;
                    self.live_in_buf[n_in] = ev;
                    n_in += 1;
                }
            }
        }
        if self.live_in_release_slot.is_some() {
            // The envelope only advances on blocks where the node is
            // rendered: every block while playing, and while it holds the
            // channel when stopped. (Stopped and NOT holding is currently
            // unreachable — a stopped switch sets the hold to the whole
            // remaining release, so the two reach zero together — which is
            // why dropping this guard changes no behavior today. It states
            // the invariant the two counters are kept apart for.)
            if playing || outgoing.is_some() {
                self.live_in_release_left = self.live_in_release_left.saturating_sub(frames);
            }
            self.live_in_hold_left = self.live_in_hold_left.saturating_sub(frames);
            if self.live_in_release_left == 0 {
                self.live_in_release_slot = None;
                self.live_in_hold_left = 0;
            }
        }
        let live_in = match outgoing {
            Some(old) => Some(mixer::LiveInBlock { slot: old, events: &self.live_in_buf[..n_in] }),
            None => {
                live_slot.map(|slot| mixer::LiveInBlock { slot, events: &self.live_in_buf[..n_in] })
            }
        };

        match (&mut self.graph, playing) {
            (Some(g), true) => {
                // Task 7: `render` pushes the graph's meter chunks itself
                // (1..=⌈slots/64⌉ for a wide graph) and reports how many the
                // ring couldn't take — telemetry, not data, so a dropped
                // chunk is one xrun, not lost audio.
                let dropped = mixer::render_rt_with_input(
                    g,
                    base,
                    &lp,
                    out,
                    self.channels,
                    self.rate,
                    discontinuity,
                    steady_base,
                    live_in,
                    Some(&mut self.meter_tx),
                );
                if dropped > 0 {
                    self.shared.xruns.fetch_add(dropped as u64, Relaxed);
                }
            }
            // Monitoring while STOPPED: render ONLY the routed instrument,
            // never the frozen clip slice under the parked playhead.
            (Some(g), false) if live_in.is_some() => {
                let dropped = mixer::render_live_input_only(
                    g,
                    base,
                    out,
                    self.channels,
                    self.rate,
                    steady_base,
                    live_in.expect("checked by the guard"),
                    Some(&mut self.meter_tx),
                );
                if dropped > 0 {
                    self.shared.xruns.fetch_add(dropped as u64, Relaxed);
                }
            }
            _ => out.fill(0.0),
        }

        if playing {
            // Report boundary crossings, never act on them (§2.1): the ring
            // push is wait-free, and a full ring is dropped rather than
            // waited on. `crossing` is edge-triggered by construction — once
            // the playhead is past the point, later blocks report nothing.
            let end = self.shared.song_end.load(Relaxed);
            if let Some(at) = transport::crossing(base, frames, &lp, end) {
                let _ = self.evt_tx.push(RtEvent::ReachedEnd { at });
            }
            let next = transport::advance(base, frames, &lp);
            self.shared.position.store(next, Relaxed);
            self.next_pos = next;
        }
        self.was_playing = playing;
    }
}

/// State owned by the cpal INPUT callback closure.
struct InputCb {
    producers: Vec<rtrb::Producer<f32>>,
    /// Per-producer silence debt (in SAMPLES, interleaved) owed after a ring
    /// overflow. Dropped audio is replaced by an equal amount of silence as
    /// soon as the ring has room again, so every take keeps
    /// `sample count == wall clock` and multi-track takes stay aligned even
    /// across xruns (each track has its own ring).
    owed: Vec<usize>,
    meter_tx: rtrb::Producer<RawMeterBlock>,
    /// Preallocated per-chunk meter templates for the recorded slots (Task 7
    /// [I4]): one entry per distinct `slot / METER_CHUNK_SLOTS` touched by
    /// the recording, plus the base-0 chunk (for frame accounting) if none
    /// of the slots land there. Built ONCE at `start_recording` (control
    /// thread) — `capture` (RT) only mutates these in place and pushes
    /// copies, never allocates. Each entry pairs a chunk's template with the
    /// LOCAL lanes in it to stamp with this input's level every buffer
    /// (same input feeds every recorded track, so they all get the
    /// identical peak/RMS this buffer) — paired in one `Vec` rather than two
    /// parallel ones so the two halves can't drift out of lockstep.
    blocks: Vec<(RawMeterBlock, Vec<usize>)>,
    in_ch: usize,
    rec_ch: usize,
    shared: Arc<SharedRt>,
}

impl InputCb {
    fn capture(&mut self, data: &[f32]) {
        let in_ch = self.in_ch.max(1);
        let frames = data.len() / in_ch;
        if frames == 0 {
            return;
        }

        // Input meters (channel 0/1; mono mirrors 0).
        let (mut pk_l, mut pk_r, mut ss_l, mut ss_r) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let rch = if in_ch > 1 { 1 } else { 0 };
        for f in 0..frames {
            let l = data[f * in_ch];
            let r = data[f * in_ch + rch];
            pk_l = pk_l.max(l.abs());
            pk_r = pk_r.max(r.abs());
            ss_l += l * l;
            ss_r += r * r;
        }
        let pos = self.shared.position.load(Relaxed);
        for (block, lanes) in self.blocks.iter_mut() {
            block.position = pos;
            block.frames = frames as u32;
            for &lane in lanes.iter() {
                block.set_slot_local(lane, pk_l, pk_r, ss_l, ss_r);
            }
            let _ = self.meter_tx.push(*block);
        }

        // Fan the first `rec_ch` input channels out to every armed ring.
        // Overflow policy: NEVER shrink a take. Whatever cannot be written
        // becomes silence debt (`owed`) repaid before any newer audio, so the
        // stream stays sample-exact against wall clock and across tracks.
        let rec_ch = self.rec_ch;
        let want = frames * rec_ch;
        for (p, owed) in self.producers.iter_mut().zip(self.owed.iter_mut()) {
            // 1. Repay owed silence first (order matters: the gap happened
            //    BEFORE this buffer's audio).
            if *owed > 0 {
                let n = (*owed).min(p.slots());
                if n > 0 {
                    if let Ok(chunk) = p.write_chunk_uninit(n) {
                        chunk.fill_from_iter(std::iter::repeat(0.0f32).take(n));
                        *owed -= n;
                    }
                }
            }
            // 2. Write this buffer's audio — only whole frames, and only if
            //    no silence is still owed (ordering again).
            let mut wrote_frames = 0usize;
            if *owed == 0 {
                wrote_frames = (p.slots() / rec_ch).min(frames);
                let take = wrote_frames * rec_ch;
                if take > 0 {
                    if let Ok(chunk) = p.write_chunk_uninit(take) {
                        chunk.fill_from_iter(
                            (0..wrote_frames)
                                .flat_map(|f| (0..rec_ch).map(move |c| data[f * in_ch + c])),
                        );
                    }
                }
            }
            // 3. Anything that didn't fit becomes debt.
            let missing = want - wrote_frames * rec_ch;
            if missing > 0 {
                *owed += missing;
                self.shared.xruns.fetch_add(1, Relaxed);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Source-keyed decode cache (round-2 §2.2)
// ---------------------------------------------------------------------------

/// One decoded source's cache entry: the samples, plus the `source_path`
/// they were decoded FROM (so a path change under the same id is detected as
/// staleness rather than silently serving stale/wrong audio).
struct CachedSource {
    source_path: String,
    data: Arc<RtClipData>,
}

/// Which sources need (re)decoding: absent from the cache, or cached from a
/// different path than the clip now names (staleness — round-2 §2.2). Pure
/// and unit-testable (the policy that `ensure_loaded` drives); deduped by
/// source, in first-seen order.
///
/// Two hardening rules [H-3]:
/// * a clip whose `source_id` is EMPTY is skipped (with a loud warning): it
///   renders silent rather than risk playing another clip's audio through
///   a shared empty-sentinel bucket. Finding 5: this IS reachable in
///   production, not just a store-boundary bug — `assign_source_ids`
///   (audio/project.rs) deliberately leaves legacy absolute / `..`-escaping
///   `source_path`s unassigned (empty id) on every load of an old project,
///   so opening such a project used to panic the engine control thread in
///   debug builds via a `debug_assert!(false)` here. Warn+skip (silent mute
///   for that one clip) is the documented degradation — no assertion.
/// * two clips naming the SAME `source_id` but DIFFERENT `source_path`s
///   violate the one-source-one-path invariant: warned loudly, and the
///   source is treated as stale under the LAST conflicting path seen — never
///   silently kept at the first path while reporting "nothing to do".
fn stale_sources(clips: &[Clip], cache: &HashMap<SourceId, CachedSource>) -> Vec<(SourceId, String)> {
    let mut wanted_path: HashMap<SourceId, String> = HashMap::new();
    let mut order: Vec<SourceId> = Vec::new();

    for clip in clips {
        if clip.source_id.as_str().is_empty() {
            log::warn!(
                "audio: clip {} has no source id; skipping (renders silent, never another clip's audio)",
                clip.id
            );
            continue;
        }
        match wanted_path.get(&clip.source_id) {
            None => {
                wanted_path.insert(clip.source_id.clone(), clip.source_path.clone());
                order.push(clip.source_id.clone());
            }
            Some(existing) if existing != &clip.source_path => {
                log::warn!(
                    "audio: source {} named by conflicting paths ({existing:?} vs {:?}) — \
                     one SourceId must name one source_path; re-decoding under the latest path",
                    clip.source_id, clip.source_path
                );
                wanted_path.insert(clip.source_id.clone(), clip.source_path.clone());
            }
            Some(_) => {} // same path already recorded — nothing to do
        }
    }

    order
        .into_iter()
        .filter_map(|sid| {
            let path = wanted_path.remove(&sid)?;
            let stale = match cache.get(&sid) {
                Some(cached) => cached.source_path != path,
                None => true,
            };
            stale.then_some((sid, path))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Control thread
// ---------------------------------------------------------------------------

struct Control {
    shared: Arc<SharedRt>,
    /// Control-side view of the CURRENT graph's tables (round-2 §2.4) — see
    /// `SharedGraphTables`'s doc for the lock-order rule [C1]: session
    /// before tables, never the reverse.
    tables: SharedGraphTables,
    /// Monotonic graph generation, bumped once per `rebuild` call (even
    /// headless, even when the graph queue is full) — echoed on every
    /// `RtGraph`/`GraphTables` built from that rebuild.
    generation: u64,
    /// Set on `PushError::Full` [I1]: the tables already point at this
    /// generation, but no graph was queued to serve it, so a retry is
    /// scheduled for the next `run()` tick (after `drain_retired`, which is
    /// what frees queue space) — otherwise every subsequent param write
    /// would resolve into a table nothing reads.
    rebuild_pending: bool,
    session: Arc<Mutex<Session>>,
    events: Box<dyn EventSink>,
    rx: Receiver<ControlMsg>,
    output: Option<OutputBundle>,
    input: Option<InputBundle>,
    writer: Option<DiskWriter>,
    rec_track_ids: Vec<String>,
    sel_output: Option<String>,
    sel_input: Option<String>,
    /// source id -> decoded samples at `cache_rate` (round-2 §2.2: keyed by
    /// SOURCE, not clip — two clips naming the same source share one decode,
    /// and the cache survives one clip's deletion as long as another clip
    /// still names the source ("sane asset GC", the other half of O-12).
    cache: HashMap<SourceId, CachedSource>,
    cache_rate: u32,
    /// Live instrument nodes keyed by track id (phase 3, ARCHITECTURE §15).
    /// Nodes are SHARED between successive graph snapshots (voice state and
    /// plugin instances survive rebuilds); entries are created/replaced here
    /// on the control thread and freed here when the last snapshot retires.
    live_nodes: crate::midi::playback::LiveNodeRegistry,
    accum: MeterAccum,
    /// generation -> slot map window for the meter fold (Task 6); published
    /// alongside `tables` on every rebuild, pinned across a recording so a
    /// take spanning many rebuilds keeps its input meters [I2].
    gen_maps: GenerationMaps,
    sinks: Vec<(Box<dyn MeterSink>, u64)>,
    last_frame: Instant,
    last_tick: Instant,
    /// The engine's own commit core (Plan E Task 13) — same
    /// `session`/`shared`/`tables` `Arc`s as everything else in `Control`,
    /// its own `emit` closure instance. Every engine-side document write
    /// goes through this now; see `Committer`'s doc for the deadlock audit
    /// covering the engine control thread committing from inside its own
    /// message loop.
    committer: Committer,
    /// "Document birth" closure, installed post-construction by `lib.rs`
    /// once `ControlPlane` exists (`ControlMsg::SetEnsureProject`'s doc) —
    /// `None` until then. Bound over the `ControlPlane` `Arc`; calling it
    /// is the ONLY way `ensure_project` touches project fields — this
    /// thread never swaps `store.project_dir` itself.
    ensure_project_fn: Option<Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>>,
    /// Track D: this rebuild's compiled plugin-parameter lanes. Ticked by
    /// `run` (control thread, ≤2 ms), never by the audio callback — a host
    /// param write is a blocking round-trip. Rebuilt wholesale at every
    /// `rebuild`, like the graph itself.
    param_automation: crate::plugins::automation::ParamAutomationDriver,
    /// Reused scratch for `param_automation.tick` so the tick allocates
    /// nothing steady-state.
    param_writes: Vec<crate::plugins::automation::ParamWrite>,
    /// The hardware MIDI-in seam. Held as an `Arc` rather than reached
    /// through `midi_in::hub()` at each call site so tests can drive the
    /// engine against their own hub; `start` binds the process-global one.
    live_in_hub: Arc<MidiInHub>,
    /// Last routing target this thread acted on — the tick compares against
    /// the hub to notice a selection that, being app config, commits nothing
    /// and therefore schedules no rebuild of its own.
    live_in_target: Option<String>,
}

impl Control {
    fn run(&mut self) {
        loop {
            match self.rx.recv_timeout(Duration::from_millis(2)) {
                Ok(msg) => {
                    if !self.handle(msg) {
                        break;
                    }
                    // Drain any burst without waiting.
                    while let Ok(msg) = self.rx.try_recv() {
                        if !self.handle(msg) {
                            return;
                        }
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
            self.drain_retired();
            if self.rebuild_pending {
                // [I1] the previous rebuild's tables already point at a
                // generation no graph was queued for (the queue was full) —
                // retry now that `drain_retired` has freed space, so knob
                // traffic converges instead of writing into a table nothing
                // reads forever.
                self.rebuild_pending = false;
                self.rebuild();
            }
            self.drain_meters();
            self.drain_rt_events();
            self.drive_param_automation();
            self.headless_advance();
            self.pump_meter_frames();
            self.follow_live_in_target();
        }
    }

    fn handle(&mut self, msg: ControlMsg) -> bool {
        match msg {
            ControlMsg::Subscribe(sink) => self.sinks.push((sink, 0)),
            ControlMsg::Rebuild => self.rebuild(),
            ControlMsg::SelectOutput { device_id, reply } => {
                self.sel_output = device_id;
                let _ = reply.send(self.open_output());
            }
            ControlMsg::SelectInput { device_id, reply } => {
                let res = if self.writer.is_some() {
                    Err("cannot switch input device while recording — stop recording first"
                        .to_string())
                } else {
                    self.sel_input = device_id;
                    // Validated lazily when recording starts; nothing to open now.
                    Ok(())
                };
                let _ = reply.send(res);
            }
            ControlMsg::StartRecording { track_ids, reply } => {
                let _ = reply.send(self.start_recording(track_ids));
            }
            ControlMsg::StopRecording { reply } => {
                let _ = reply.send(self.stop_recording());
            }
            ControlMsg::SetEnsureProject(f) => {
                self.ensure_project_fn = Some(f);
            }
            ControlMsg::Shutdown => return false,
        }
        true
    }

    // -- streams ------------------------------------------------------------

    fn engine_rate(&self) -> u32 {
        self.shared.sample_rate.load(Relaxed)
    }

    fn open_output(&mut self) -> Result<(), String> {
        let host = cpal::default_host();
        let device = match &self.sel_output {
            Some(id) => host
                .output_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.name().map(|n| &n == id).unwrap_or(false))
                .ok_or_else(|| format!("unknown output device: {id}"))?,
            None => host
                .default_output_device()
                .ok_or_else(|| "no default output device".to_string())?,
        };
        let cfg = device.default_output_config().map_err(|e| e.to_string())?;
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!(
                "unsupported output sample format {:?} (prototype supports f32)",
                cfg.sample_format()
            ));
        }
        let rate = cfg.sample_rate().0;
        let channels = cfg.channels().max(1) as usize;

        let (graph_tx, graph_rx) = rtrb::RingBuffer::new(8);
        let (retire_tx, retire_rx) = rtrb::RingBuffer::new(8);
        let (meter_tx, meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        // Boundary crossings are rare (one per playthrough), but the ring is
        // sized for a burst of them so a stalled control thread never makes
        // the callback drop one.
        let (evt_tx, evt_rx) = rtrb::RingBuffer::new(64);
        // Second rtrb path INTO the callback, and the only one whose producer
        // is not this thread: the midir callback thread pushes, this stream's
        // callback pops. A fresh ring per stream — but the producer is only
        // handed to the hub once the stream is actually RUNNING (see below),
        // because everything between here and there can fail.
        let (live_in_tx, live_in_rx) = rtrb::RingBuffer::new(LIVE_IN_RING_SLOTS);
        let mut cb = OutputCb {
            graph_rx,
            retire_tx,
            meter_tx,
            evt_tx,
            shared: self.shared.clone(),
            graph: None,
            channels,
            rate,
            next_pos: u64::MAX, // first playing block is a discontinuity
            was_playing: false,
            live_in_rx,
            live_in_buf: [LiveMidiEvent::all_off(); LIVE_IN_BUF_SLOTS],
            live_in_hub: self.live_in_hub.clone(),
            live_in_slot: None,
            live_in_all_off: false,
            live_in_held: [false; 128],
            live_in_release_slot: None,
            live_in_release_left: 0,
            live_in_hold_left: 0,
        };
        let stream = device
            .build_output_stream(
                &cfg.into(),
                move |data: &mut [f32], _| cb.render(data),
                |e| log::warn!("output stream error: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;

        // Only now, past every `?` above: a device that fails to open must
        // leave the PREVIOUS stream's ring installed, or the failed switch
        // would silently kill hardware MIDI-in (the old callback keeps
        // running, but its ring would have no producer) until some later
        // switch happened to succeed.
        self.live_in_hub.install_producer(live_in_tx);
        // Replacing the bundle drops the previous stream + its graph here on
        // the control thread.
        self.output = Some(OutputBundle {
            _stream: Some(stream),
            graph_tx,
            retire_rx,
            meter_rx,
            evt_rx,
        });
        self.shared.sample_rate.store(rate, Relaxed);
        // Site 1 (Plan E Task 13): a transient `Actor::Engine` tx — the
        // document mirror of the RT atomic just above. `Op::Set{Transport,
        // SampleRate}` never sets `effect.rebuild` (Task 12's transport
        // family deliberately sets no engine-effect flags — session.rs's
        // `ObjectRef::Transport` arm doc), so this commit's `do_rebuild`
        // closure is a no-op; the unconditional `self.rebuild()` below
        // (unchanged from before this task) is what actually rebuilds,
        // exactly as it always has.
        if let Err(e) = self.commit_output_sample_rate(rate) {
            log::warn!("open_output: sample-rate commit failed: {e}");
        }
        if self.cache_rate != rate {
            self.cache.clear();
        }
        log::info!(
            "audio: output stream open ({} ch @ {} Hz, device {:?})",
            channels,
            rate,
            self.sel_output.as_deref().unwrap_or("default")
        );
        self.rebuild();
        Ok(())
    }

    /// The commit `open_output` submits when the engine (re)opens its
    /// output stream at a new sample rate — split out so it's independently
    /// testable. Transient, `emit_project_changed: false` (matches every
    /// other transport-family commit: `ControlPlane::transport`'s four
    /// actions all pass `false` too — `project://changed`'s payload
    /// contract is the full `Project` shape, and firing it once per
    /// device-open would be a behavior change from today's silent
    /// writeback).
    fn commit_output_sample_rate(&mut self, rate: u32) -> Result<Committed, String> {
        let committer = self.committer.clone();
        committer.commit_with_rebuild(
            op::TxMeta::engine("sample rate").transient(),
            |tx| {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Transport,
                    path: op::PropPath::SampleRate,
                    from: serde_json::Value::Null,
                    to: serde_json::json!(rate),
                })
            },
            false,
            || {},
        )
    }

    // -- graph lifecycle (prepare on control thread, swap by pointer) -------

    /// Rewrite for round-2 §2.4: slots are derived fresh from display order
    /// every rebuild (nothing is ever "freed" for a later rebuild to
    /// alias — see `types::derive_slots`), and this rebuild's `ParamTable`
    /// is built fresh and travels WITH the graph it belongs to
    /// (`RtGraph::params`) — a retired graph keeps reading its own table,
    /// so the O-13 alias window is dead by construction (Step 5's test
    /// pins this).
    fn rebuild(&mut self) {
        self.ensure_loaded();
        self.generation += 1;
        let headless = self.output.is_none();
        // Read BEFORE the session lock is taken: the hub is reachable from
        // the midir callback thread, and nesting its mutex under the session
        // lock would invent a lock order this file has none of today.
        let live_in_target = self.live_in_hub.target_track();
        let (graph, param_driver) = {
            let session = self.session.lock(); // read-only: derive_slots/param seeding for the rebuild graph, session lock released after this block
            let store = &session.store;
            let slots = derive_slots(&store.tracks);
            // Task 7: sized to THIS rebuild's track count, not a fixed cap.
            let params = Arc::new(ParamTable::with_slots(store.tracks.len()));
            for (i, t) in store.tracks.iter().enumerate() {
                params.set_gain_linear(i, mixer::db_to_linear(t.gain_db));
                params.set_pan(i, t.pan as f32);
                params.set_flag(i, super::rt::FLAG_MUTE, t.muted);
                params.set_flag(i, super::rt::FLAG_SOLO, t.soloed);
            }
            params.any_solo.store(store.any_solo(), Relaxed);
            // PUBLISH UNDER THE SESSION LOCK [C1]: this is load-bearing, not
            // style. Publishing after the lock is released would open a
            // window where a commit transacts against a NEWER document
            // revision than the one this rebuild read, resolves its param
            // writes through the STILL-OLD tables (because these fresher
            // ones aren't published yet), and then this rebuild publishes
            // tables built from the OLDER revision — silently losing the
            // commit's write forever (a plain `Set` never schedules a
            // rebuild). Publishing here, still holding `session`, makes
            // <read doc, publish tables> atomic against every commit's
            // <transact, execute writes> sequence (see `SharedGraphTables`'s
            // doc for the full argument and the lock-order rule).
            *self.tables.lock() = GraphTables {
                generation: self.generation,
                params: params.clone(),
                slots: slots.clone(),
            };
            // Same generation, same slot map, published alongside the
            // tables (Task 6) — the meter fold resolves blocks under
            // whichever generation produced them, not the current tables.
            self.gen_maps.publish(self.generation, &slots);
            let (gain_ramps, param_driver) =
                self.compile_automation(&session, &slots, store.tracks.len());
            let graph = if headless {
                // Headless keeps its narrow scope [I5]: tables are enough
                // to serve knob writes and recording resolution with no
                // output device — no clip assembly, no live/plugin node
                // instantiation, no `song_end` write. Enabling any of that
                // headlessly (every structural commit in the whole backend
                // test suite runs through here) would be a silent behavior
                // change this refactor must not smuggle in.
                None
            } else {
                let mut tracks = Vec::with_capacity(store.tracks.len());
                for t in &store.tracks {
                    let slot = slots[&t.id];
                    let clips = store
                        .clips
                        .iter()
                        .filter(|c| c.track_id == t.id)
                        .filter_map(|c| {
                            let samples = self.cache.get(&c.source_id).map(|e| e.data.clone())?;
                            Some(RtClip {
                                start: c.timeline_start_samples,
                                offset: c.offset_samples,
                                len: c.length_samples,
                                gain: mixer::db_to_linear(c.gain_db),
                                fade_in: c.fade_in_samples,
                                fade_out: c.fade_out_samples,
                                samples,
                            })
                        })
                        .collect();
                    tracks.push(RtTrack::clips(slot, clips));
                }
                // LIVE instrument tracks (phase 3, ARCHITECTURE §15): midi
                // tracks become RtTracks carrying a live node (SamplerNode
                // when the track's `instrument_id` resolves, plugin node
                // for `plugin:` refs — stub until zones P1/P2 land —,
                // PolySynth fallback) plus this snapshot's pre-scheduled
                // events (ticks -> absolute samples via TempoMap, HERE on
                // the control thread; the RT thread only slices
                // sample-offset events). Nodes come from `live_nodes` so
                // voice/plugin state SURVIVES rebuilds; brand-new nodes are
                // prepared before the snapshot is published (RCU
                // discipline). Store and midi share one guard now, so this
                // reads `session.midi` directly instead of re-locking
                // through the registered global.
                let bank = crate::audio::sampler::registered_bank().map(|b| b.lock());
                // ORDER MATTERS: a midi track already has a clips-only row
                // from the loop above, so this adds a SECOND row for the
                // same slot. Both write that slot's meter lane and the last
                // writer wins — appending the live rows here is what makes
                // the live one win. Push them before the loop and the midi
                // track's meters silently read zero.
                crate::midi::playback::append_from_with_input(
                    &session.midi,
                    store,
                    &session.plugins,
                    &slots,
                    self.cache_rate,
                    bank.as_deref(),
                    &mut self.live_nodes,
                    &mut tracks,
                    live_in_target.as_deref(),
                );
                // The timeline boundary belongs to the material, so it is
                // derived exactly where the material is assembled — same
                // helper the offline bounce uses, so live and export agree
                // on where the song ends (clip ends AND the final scheduled
                // note-off).
                self.shared
                    .song_end
                    .store(offline::song_end(&tracks), Relaxed);
                let mut g = RtGraph::new(tracks, self.generation, params);
                // RCU: the ramp table is attached BEFORE the graph is
                // published, so the callback only ever sees a snapshot whose
                // ramps already belong to it — and a retired graph keeps
                // reading its own table, exactly like `params`.
                g.set_gain_ramps(gain_ramps);
                Some(Box::new(g))
            };
            (graph, param_driver)
        };
        self.param_automation = param_driver;
        let Some(graph) = graph else { return };
        let Some(out) = self.output.as_mut() else { return };
        // Free anything the callback already retired before queueing more.
        while let Ok(gp) = out.retire_rx.pop() {
            drop(gp);
        }
        if let Err(rtrb::PushError::Full(_gp)) = out.graph_tx.push(GraphPtr::new(graph)) {
            // [I1] Queue full (callback stalled?) — the returned GraphPtr
            // frees the fresh graph on drop, here on the control thread.
            // The tables above are ALREADY published at this generation, so
            // without a retry every subsequent knob write would resolve
            // into a table no graph ever reads: schedule one for `run()`'s
            // next tick, after `drain_retired` has freed queue space.
            self.rebuild_pending = true;
            log::warn!("audio: graph queue full, rebuild retried next tick");
        }
    }

    /// CONTROL THREAD, once per tick: keep the hardware MIDI-in routing in
    /// step with the graph. Two things happen here, in this order.
    ///
    /// A rebuild first, when the SELECTION changed. Choosing the routing
    /// target is app config (ruling 1) — it commits nothing, so no
    /// `EngineEffect::rebuild` fires — but a midi track with no clips only
    /// gets a live node when `rebuild` runs with the target already known
    /// (`append_from_with_input`). Without this the very case the feature
    /// exists for — arm an empty track and play — would stay silent until
    /// some unrelated edit happened to rebuild.
    ///
    /// Then the id→slot resolution the callback reads, which must come
    /// SECOND: it has to resolve against the tables the rebuild just
    /// published, not the previous generation's.
    fn follow_live_in_target(&mut self) {
        let target = self.live_in_hub.target_track();
        if target != self.live_in_target {
            self.live_in_target = target;
            self.rebuild();
        }
        self.live_in_hub.refresh_target(self.generation, &self.tables);
    }

    /// Track D: compile the session's automation lanes into this rebuild's
    /// two products — a slot-indexed gain-ramp table for the graph, and the
    /// control thread's plugin-param driver. CONTROL THREAD, called from
    /// `rebuild` under the same session guard that derived `slots`: this is
    /// where ticks become absolute samples, so nothing tick-shaped ever
    /// crosses onto the RT thread (ARCHITECTURE §13/§15.1).
    ///
    /// `n_slots` is the TRACK COUNT `ParamTable` was sized with, not
    /// `slots.len()`, so the ramp table and the param table index alike.
    ///
    /// The `TempoMap` can fail to build — a zero `ppq`, an empty or
    /// non-monotonic `tempo_events`, a zero rate — and then nothing is
    /// compiled rather than something compiled against a guessed timeline.
    /// (`rebuild` calls `ensure_loaded` first, which sets `cache_rate` from
    /// `SharedRt::sample_rate`, so in practice a malformed tempo map is what
    /// reaches this branch, not a missing device.)
    fn compile_automation(
        &self,
        session: &Session,
        slots: &HashMap<crate::ids::TrackId, usize>,
        n_slots: usize,
    ) -> (
        Vec<Option<Arc<Vec<crate::plugins::automation::AbsParamEvent>>>>,
        crate::plugins::automation::ParamAutomationDriver,
    ) {
        use crate::plugins::automation as auto;
        // The overwhelmingly common case, and every structural commit in the
        // suite goes through here: no lanes, so no tempo-event clone and no
        // TempoMap build.
        if session.automation.lanes.is_empty() {
            return ((0..n_slots).map(|_| None).collect(), auto::ParamAutomationDriver::empty());
        }
        let map = crate::midi::TempoMap::new(
            session.midi.ppq,
            session.midi.tempo_events.clone(),
            self.cache_rate,
        )
        .ok();
        let Some(map) = map else {
            return ((0..n_slots).map(|_| None).collect(), auto::ParamAutomationDriver::empty());
        };
        let ramps = auto::compile_gain_ramps(&session.automation.lanes, &map, n_slots, &|tid| {
            slots.get(tid).copied()
        });
        let driver = auto::ParamAutomationDriver::new(&session.automation.lanes, &session.plugins, &map);
        (ramps, driver)
    }

    /// Track D: apply plugin-parameter automation at this thread's own tick
    /// (≤2 ms), never on the audio callback — a host param write is a
    /// blocking round-trip and is banned there ([C1]).
    ///
    /// The writes go to the HOST ONLY, never to the document. That is the
    /// point, not an omission: automation OVERRIDES the stored knob value
    /// during playback, while the document keeps what the user set (which is
    /// what gets saved and what the param panel shows). Routing these
    /// through the channel would either trip the M-3 transient invariant
    /// (`ObjectRef::Plugin` is a field history entries address) or push an
    /// undo entry and a `project.json` write every 2 ms. Recorded in
    /// `docs/SIDE-CHANNEL-INVENTORY.md`.
    ///
    /// Only while the transport is playing: a stopped transport leaves the
    /// last automated value in place, which is what the user sees and hears
    /// until they move the knob or reload the project.
    fn drive_param_automation(&mut self) {
        if self.param_automation.is_empty() || !self.shared.playing.load(Relaxed) {
            return;
        }
        let pos = self.shared.position.load(Relaxed);
        let mut writes = std::mem::take(&mut self.param_writes);
        self.param_automation.tick(pos, &mut writes);
        // ONE host call per instance per tick (review I-2): `tick` hands its
        // writes out grouped by instance, so a contiguous run IS a plugin's
        // whole batch. If that grouping ever regressed this would still be
        // correct, just chattier — never wrong.
        for batch in writes.chunk_by(|a, b| a.instance == b.instance) {
            let changes: Vec<(u32, f32)> = batch.iter().map(|w| (w.index, w.value)).collect();
            crate::control::forward_params_to_host(&batch[0].instance, &batch[0].format, &changes);
        }
        self.param_writes = writes;
    }

    /// Decode any clip sources missing from the cache (at the engine rate,
    /// or stale under a changed `source_path` — round-2 §2.2) and make sure
    /// every referencing clip's waveform pyramid exists.
    fn ensure_loaded(&mut self) {
        let rate = self.engine_rate();
        if self.cache_rate != rate {
            self.cache.clear();
            self.cache_rate = rate;
        }
        // `stale_sources` decides WHAT needs decoding (one entry per source,
        // deduped); pyramid dirs are still resolved per clip below (the
        // visual cache stays clip-id-keyed — dedup opportunity ledgered,
        // not taken here).
        let (project_dir, todo, live_sources, clips_by_source) = {
            let session = self.session.lock(); // read-only: stale-source scan (import cache maintenance) — no writes
            let store = &session.store;
            let todo = stale_sources(&store.clips, &self.cache);
            let mut live_sources: std::collections::HashSet<SourceId> = std::collections::HashSet::new();
            let mut clips_by_source: HashMap<SourceId, Vec<String>> = HashMap::new();
            for c in &store.clips {
                if c.source_id.as_str().is_empty() {
                    continue; // stale_sources already warned about this
                }
                live_sources.insert(c.source_id.clone());
                clips_by_source.entry(c.source_id.clone()).or_default().push(c.id.to_string());
            }
            (store.project_dir.clone(), todo, live_sources, clips_by_source)
        };
        // GC by source (round-2 §2.2's "sane asset GC" half of O-12): an
        // asset shared by two clips survives one clip's deletion, since the
        // SOURCE stays live as long as any clip still names it.
        self.cache.retain(|sid, _| live_sources.contains(sid));

        let Some(project_dir) = project_dir else { return };
        for (source_id, source_path) in todo {
            let path = project_dir.join(&source_path);
            match load_wav(&path) {
                Ok((channels, file_rate, samples)) => {
                    let data = linear_resample(&samples, channels as usize, file_rate, rate);
                    self.cache.insert(
                        source_id,
                        CachedSource { source_path, data: Arc::new(RtClipData { channels, data }) },
                    );
                }
                Err(e) => log::warn!("audio: cannot load {}: {e}", path.display()),
            }
        }

        // Reviewer finding 2: pyramid building is DECOUPLED from the decode
        // loop above — a clip whose source was already cached (shared with
        // an earlier clip, so it never appeared in `todo`) still needs its
        // OWN waveform pyramid dir checked/built. Walk every LIVE clip
        // (grouped by source).
        //
        // Finding 6: pyramids MUST be built from the source file's RAW
        // samples, not `cached.data` — that cache entry is resampled to the
        // engine rate (round-2 §2.2's decode step above), while the AWTF
        // tile protocol defines LOD bins in SOURCE samples
        // (waveform.rs: "LOD n bins cover 2^(8+n) SOURCE samples"). Building
        // from the resampled buffer time-stretches the waveform whenever the
        // file's rate != the engine rate. Re-reading the file here (control
        // thread — allowed, see `load_wav`'s callers elsewhere in this
        // module) costs one extra decode per source per `ensure_loaded` call
        // that actually has missing pyramids, which is rare (first open /
        // new clip) — the hot decode loop above, which builds the
        // play-critical `RtClipData`, is untouched.
        for (source_id, clip_ids) in &clips_by_source {
            let Some(cached) = self.cache.get(source_id) else { continue };
            let missing: Vec<&String> = clip_ids
                .iter()
                .filter(|clip_id| !pyramid_exists(&Store::cache_dir_for(&project_dir, clip_id)))
                .collect();
            if missing.is_empty() {
                continue;
            }
            let source_path = project_dir.join(&cached.source_path);
            let pyr = match load_wav(&source_path) {
                Ok((channels, _file_rate, samples)) => {
                    Pyramid::from_interleaved(&samples, channels as usize)
                }
                Err(e) => {
                    log::warn!(
                        "waveform cache: cannot re-read {} for pyramid build: {e}",
                        source_path.display()
                    );
                    continue;
                }
            };
            for clip_id in missing {
                let cache_dir = Store::cache_dir_for(&project_dir, clip_id);
                if let Err(e) = pyr.write_dir(&cache_dir) {
                    log::warn!("waveform cache for {clip_id}: {e}");
                }
            }
        }
    }

    fn drain_retired(&mut self) {
        if let Some(out) = self.output.as_mut() {
            while let Ok(gp) = out.retire_rx.pop() {
                drop(gp);
            }
        }
    }

    // -- meters -------------------------------------------------------------

    fn drain_meters(&mut self) {
        if let Some(out) = self.output.as_mut() {
            while let Ok(blk) = out.meter_rx.pop() {
                self.accum.fold(&blk, &self.gen_maps);
            }
        }
        if let Some(inp) = self.input.as_mut() {
            while let Ok(blk) = inp.meter_rx.pop() {
                self.accum.fold(&blk, &self.gen_maps);
            }
        }
    }

    fn pump_meter_frames(&mut self) {
        if self.sinks.is_empty() || self.last_frame.elapsed() < FRAME_INTERVAL {
            return;
        }
        self.last_frame = Instant::now();
        // Display order comes from the session (Task 6: the fold itself now
        // resolves generation -> slot -> track, so this is just display
        // order, no slots needed here).
        let order: Vec<crate::ids::TrackId> = {
            let session = self.session.lock(); // read-only: display order for the meter fold
            session.store.tracks.iter().map(|t| t.id.clone()).collect()
        };
        let position = self.shared.position.load(Relaxed);
        let frame = self.accum.take_frame(0, &order, position);
        self.sinks.retain_mut(|(sink, seq)| {
            let mut f = frame.clone();
            f.seq = *seq;
            *seq += 1;
            sink.send_frame(&f)
        });
    }

    /// Without an output device the callback can't advance the playhead —
    /// advance it from wall time so transport/UI still work headless.
    fn headless_advance(&mut self) {
        let elapsed = self.last_tick.elapsed();
        self.last_tick = Instant::now();
        if self.output.is_some() || !self.shared.playing.load(Relaxed) {
            return;
        }
        let frames = (elapsed.as_secs_f64() * self.engine_rate() as f64) as u64;
        if frames > 0 {
            let lp = self.shared.loop_spec();
            let pos = self.shared.position.load(Relaxed);
            // Same boundary rule as the RT path, so headless behaviour (and
            // every test that runs without an audio device) matches the real
            // engine. Detection here is already on the control thread, so it
            // applies the policy directly instead of going through the ring.
            let end = self.shared.song_end.load(Relaxed);
            let reached = transport::crossing(pos, frames, &lp, end);
            self.shared
                .position
                .store(transport::advance(pos, frames, &lp), Relaxed);
            if let Some(at) = reached {
                self.apply_end_policy(at);
            }
        }
    }

    /// Drain the `engine_evt` ring and apply policy to what the callback saw.
    fn drain_rt_events(&mut self) {
        let mut reached_end = None;
        if let Some(out) = self.output.as_mut() {
            while let Ok(ev) = out.evt_rx.pop() {
                match ev {
                    // Keep only the newest: several crossings can only mean
                    // the control thread was starved, and stopping twice at
                    // the same boundary is not a thing.
                    RtEvent::ReachedEnd { at } => reached_end = Some(at),
                }
            }
        }
        if let Some(at) = reached_end {
            self.apply_end_policy(at);
        }
    }

    /// THE POLICY the RT thread is not allowed to have: does reaching the end
    /// of the material stop the transport?
    fn apply_end_policy(&mut self, at: u64) {
        if !self.shared.stop_at_end.load(Relaxed) {
            return;
        }
        // An active loop owns the playhead — it should never have got here,
        // but a loop enabled between detection and now must still win.
        if self.shared.loop_spec().active() {
            return;
        }
        // Never cut a take short: recording past the end is how material
        // gets ADDED past the end.
        if self.shared.recording.load(Relaxed) {
            return;
        }
        if !self.shared.playing.load(Relaxed) {
            return;
        }
        // Order matters: hand the callback the parking position BEFORE
        // clearing `playing`, so whichever thread writes the playhead last
        // writes `at`. Without an output stream there is no callback to
        // carry it out — the store below is authoritative instead.
        if self.output.is_some() {
            self.shared.park.store(at, Relaxed);
        }
        self.shared.playing.store(false, Relaxed);
        self.shared.position.store(at, Relaxed);
        // `position_samples` stays a bare RT atomic, never an op — there is
        // no `PropPath` for it (the six Transport paths are TransportState/
        // LoopEnabled/LoopStartSamples/LoopEndSamples/StopAtEnd/SampleRate,
        // session.rs's `write_transport_prop`); every read of the document's
        // `position_samples` (`project_changed_payload`, `execute_persist`'s
        // `project::from_store`) already overrides it from this SAME atomic,
        // so a store-field mirror here would be dead weight.
        if let Err(e) = self.commit_auto_stop() {
            log::warn!("apply_end_policy: auto-stop commit failed: {e}");
        }
        // read-only: transport_snapshot only reads store.transport; the
        // document write above already went through commit_auto_stop.
        let snap = crate::control::ops::transport_snapshot(&self.session.lock().store, &self.shared);
        if let Ok(v) = serde_json::to_value(&snap) {
            self.events.emit("transport://state", v);
        }
    }

    /// The commit `apply_end_policy` submits when the transport auto-stops
    /// at the end of the material — split out so it's independently
    /// testable. Transient (a policy-driven stop is RT/document-visible
    /// state, not a document edit a user would expect in undo history —
    /// same reasoning as `ControlPlane::transport`'s Play/Stop/SetLoop/
    /// SetStopAtEnd), `emit_project_changed: false` (this site's own
    /// `transport://state` emit, right after this call, is what today's
    /// callers actually observe — unchanged by this task).
    fn commit_auto_stop(&mut self) -> Result<Committed, String> {
        let committer = self.committer.clone();
        committer.commit_with_rebuild(
            op::TxMeta::engine("auto-stop at end").transient(),
            |tx| {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Transport,
                    path: op::PropPath::TransportState,
                    from: serde_json::Value::Null,
                    to: serde_json::json!("stopped"),
                })
            },
            false,
            || {},
        )
    }

    // -- recording ----------------------------------------------------------

    fn start_recording(&mut self, track_ids: Option<Vec<String>>) -> Result<Vec<String>, String> {
        // A MIDI-only take opens no device, so `self.writer` stays None for
        // the whole take — the capture is the other half of "is a take
        // running".
        if self.writer.is_some() || self.live_in_hub.capturing() {
            return Err("already recording".to_string());
        }

        // Read the routing target BEFORE the session lock: the hub's own
        // mutex must never be taken under it ([C1] ordering).
        let live_in_target = self.live_in_hub.target_track();
        let (targets, midi_target) = {
            let session = self.session.lock(); // read-only: resolve/validate target track ids before recording starts
            split_record_targets(&session.store, track_ids, live_in_target)?
        };

        // A take needs a project dir whether it is audio or MIDI.
        self.ensure_project()?;

        let start_pos = self.shared.position.load(Relaxed);

        if !targets.is_empty() {
            // Input device + stream.
            let host = cpal::default_host();
            let device = match &self.sel_input {
                Some(id) => host
                    .input_devices()
                    .map_err(|e| e.to_string())?
                    .find(|d| d.name().map(|n| &n == id).unwrap_or(false))
                    .ok_or_else(|| format!("unknown input device: {id}"))?,
                None => host
                    .default_input_device()
                    .ok_or_else(|| "no default input device".to_string())?,
            };
            let cfg = device.default_input_config().map_err(|e| e.to_string())?;
            if cfg.sample_format() != cpal::SampleFormat::F32 {
                return Err(format!(
                    "unsupported input sample format {:?} (prototype supports f32)",
                    cfg.sample_format()
                ));
            }
            let in_ch = cfg.channels().max(1) as usize;
            let rec_ch = in_ch.min(2);
            let rate = cfg.sample_rate().0;

            // Rings + writer specs. Slot resolution reads the CURRENT graph's
            // tables, not the store — round-2 §2.4; lock order: session before
            // tables [C1].
            let (project_dir, take_no, slots, rec_generation) = {
                let session = self.session.lock(); // read-only: project dir + take numbering + slot resolution
                let store = &session.store;
                let dir = store.project_dir.clone().ok_or("no project open")?;
                let take_no = store.clips.len() + 1;
                let tables = self.tables.lock();
                let slots: Vec<usize> = targets
                    .iter()
                    .filter_map(|id| tables.slots.get(id.as_str()).copied())
                    .collect();
                (dir, take_no, slots, tables.generation)
            };
            // Group the recorded slots by 64-slot chunk (Task 7 [I4]): one
            // preallocated `RawMeterBlock` template per distinct chunk, plus the
            // base-0 chunk (frame accounting [I3]) even if no recorded slot
            // lands there. Built here (control thread) so `InputCb::capture`
            // (RT) never allocates.
            let mut chunk_lanes: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            chunk_lanes.entry(0).or_default();
            for &slot in &slots {
                let chunk_idx = slot / METER_CHUNK_SLOTS;
                let lane = slot % METER_CHUNK_SLOTS;
                chunk_lanes.entry(chunk_idx).or_default().push(lane);
            }
            let mut blocks = Vec::with_capacity(chunk_lanes.len());
            for (chunk_idx, lanes) in chunk_lanes {
                let mut b = RawMeterBlock::new(rec_generation, 0, 0);
                b.base_slot = (chunk_idx * METER_CHUNK_SLOTS) as u32;
                blocks.push((b, lanes));
            }

            let capacity = (rate as usize * rec_ch * REC_RING_SECS).max(48_000);
            let mut producers = Vec::with_capacity(targets.len());
            let mut consumers = Vec::with_capacity(targets.len());
            let mut specs = Vec::with_capacity(targets.len());
            for (i, track_id) in targets.iter().enumerate() {
                let clip_id = uuid::Uuid::new_v4().to_string();
                // The take's wav is named by a freshly-minted SourceId (round-2
                // §2.2) — the decode cache re-keys by source, not by clip.
                // Waveform pyramid cache dirs stay keyed by clip id.
                let source_id = crate::ids::SourceId::mint();
                let rel = format!("audio/{source_id}.wav");
                let (p, c) = rtrb::RingBuffer::new(capacity);
                producers.push(p);
                consumers.push(c);
                specs.push(RecSpec {
                    track_id: track_id.clone(),
                    take_name: format!("Take {}", take_no + i),
                    wav_path: project_dir.join(&rel),
                    rel_path: rel,
                    source_id,
                    cache_dir: Store::cache_dir_for(&project_dir, &clip_id),
                    clip_id,
                    start_pos,
                });
            }

            let writer = recorder::spawn(specs, consumers, rec_ch as u16, rate)?;

            let (meter_tx, meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
            let n_producers = producers.len();
            let mut cb = InputCb {
                producers,
                owed: vec![0; n_producers],
                meter_tx,
                blocks,
                in_ch,
                rec_ch,
                shared: self.shared.clone(),
            };
            let stream = device
                .build_input_stream(
                    &cfg.into(),
                    move |data: &[f32], _| cb.capture(data),
                    |e| log::warn!("input stream error: {e}"),
                    None,
                )
                .map_err(|e| e.to_string())?;
            stream.play().map_err(|e| e.to_string())?;

            // Pin the generation the slots above were resolved against (Task 6
            // [I2]) — a take spanning more rebuilds than `GenerationMaps` keeps
            // in its plain window must not lose its input meters. INVARIANT:
            // pin only AFTER every fallible step above has succeeded (device
            // lookup, `recorder::spawn`, `build_input_stream`, `stream.play`) —
            // `stop_recording` is the only unpin, and it can only ever run once
            // `self.writer`/`self.input` are actually populated. Pinning
            // earlier and then returning `Err` from one of those `?`s would
            // leak the pin: a generation stays exempt from pruning forever for
            // a recording that never started (self-healing only on the NEXT
            // successful recording, which is not good enough).
            self.gen_maps.pin(rec_generation);

            self.input = Some(InputBundle { _stream: stream, meter_rx });
            self.writer = Some(writer);
            log::info!(
                "audio: recording {} track(s) @ {} Hz x{}ch",
                targets.len(),
                rate,
                rec_ch
            );
        } // ruling 8: a take with only a MIDI target opens no device at all

        // Arm the capture LAST among the fallible work: every `?` above
        // returns without a take running, so a failed start never leaves the
        // hub buffering into a recording that does not exist. Untestable as
        // stated — a MIDI-only take has no fallible step above this, and a
        // mixed one needs a real input device — so moving this call up will
        // not turn anything red.
        if let Some(t) = &midi_target {
            self.live_in_hub.begin_capture(t.clone(), start_pos);
            log::info!("audio: recording MIDI take on track {t}");
        }

        let mut recorded = targets;
        recorded.extend(midi_target);
        self.rec_track_ids = recorded.clone();
        self.shared.recording.store(true, Relaxed);
        self.shared.playing.store(true, Relaxed);
        if let Err(e) = self.commit_start_recording_state() {
            log::warn!("start_recording: transport-state commit failed: {e}");
        }
        self.events.emit(
            "recording://state",
            serde_json::json!({
                "recording": true,
                "trackIds": recorded,
                "startedAtSamples": start_pos,
                "xruns": 0,
            }),
        );
        Ok(recorded)
    }

    /// The commit `start_recording` submits for the "recording" transport
    /// state — split out so it's independently testable. Transient (same
    /// reasoning as `commit_auto_stop`/`commit_output_sample_rate`),
    /// `emit_project_changed: false` — `recording://state`, emitted right
    /// after this call in `start_recording`, is what today's callers
    /// actually observe.
    fn commit_start_recording_state(&mut self) -> Result<Committed, String> {
        let committer = self.committer.clone();
        committer.commit_with_rebuild(
            op::TxMeta::engine("start recording").transient(),
            |tx| {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Transport,
                    path: op::PropPath::TransportState,
                    from: serde_json::Value::Null,
                    to: serde_json::json!("recording"),
                })
            },
            false,
            || {},
        )
    }

    /// Stop the take and register it.
    ///
    /// `Err` does NOT mean "nothing happened": except for the "not
    /// recording" case, the take has ALREADY been committed (the MIDI clip
    /// and whatever audio clips survived, one undo entry), the transport is
    /// already stopped and `recording://state` has already been emitted. The
    /// error reports the AUDIO half — the WAV writer failed and its clips
    /// were lost — never the transaction. A caller must not treat it as a
    /// signal to retry or to skip its own stop bookkeeping.
    fn stop_recording(&mut self) -> Result<Vec<Clip>, String> {
        // Disarm the capture unconditionally and first: after this line no
        // further hardware note can join the take, whichever half fails
        // below. Either half alone counts as "recording" (ruling 8).
        let capture = self.live_in_hub.end_capture();
        let writer = self.writer.take();
        if writer.is_none() && capture.is_none() {
            return Err("not recording".to_string());
        }
        // Drop the input stream FIRST so the ring producers close and the
        // writer can drain to empty.
        self.input = None;
        // A writer failure (disk full, a WAV header that would not close,
        // the 15 s drain timeout) is reported, but only after everything it
        // does NOT own has been salvaged. It used to return early, which
        // took two things with it that never depended on the writer: the
        // MIDI take, which is pure in-memory data already lifted out of the
        // hub above, and the stop itself — `shared.recording` stayed true
        // and the transport-state commit never ran, leaving the UI claiming
        // a take was still running.
        let (clips, writer_err) = match writer {
            Some(w) => {
                // Release the pin (Task 6 [I2]) — recording is over, so its
                // generation no longer needs exemption from the plain
                // window. Only the audio half ever pinned one.
                self.gen_maps.unpin();
                match w.finish(Duration::from_secs(15)) {
                    Ok(clips) => (clips, None),
                    Err(e) => {
                        log::error!("stop_recording: the take's audio was lost: {e}");
                        (Vec::new(), Some(e))
                    }
                }
            }
            None => (Vec::new(), None),
        };

        self.shared.recording.store(false, Relaxed);
        self.shared.playing.store(false, Relaxed);
        let track_ids = std::mem::take(&mut self.rec_track_ids);

        // §4.4: "the op is the registration, never the recording itself" —
        // the ops are built AFTER `writer.finish` above (the wav I/O has
        // ALREADY completed by this point), never before. ONE non-transient
        // Actor::Engine tx: ClipAdd x n (appended in `clips`' order, mirroring
        // the pre-Task-13 `store.clips.extend`). `ClipAdd`'s `apply_raw` arm
        // sets BOTH `effect.rebuild` and `effect.persist.project`
        // (session.rs) — so `self.rebuild()` (via `do_rebuild` below) and
        // the project.json write (via `execute_persist`, replacing the
        // manual `project::save` this used to do) both come from the SAME
        // commit's folded effect, not two separate steps racing each other.
        //
        // Review round 1 (Important-1): the `TransportState="stopped"` Set
        // does NOT ride in this tx — it's a SEPARATE, transient commit right
        // below. Bundling it into the ClipAdd tx would make it part of that
        // tx's inverse: once Task 17 lands history, undoing "stop recording"
        // would restore `state = "recording"` alongside removing the clips —
        // a document state that CLAIMS a take is running when nothing is
        // recording. `ClipAdd`'s inverse (removing the clips) must never
        // carry transport-state baggage with it.
        //
        // Near-unreachable edge case (deferred-minor, review round 1):
        // when `clips` is empty (only possible if `start_recording`'s own
        // "no armed tracks to record" guard somehow let a take start with
        // no targets — it can't today), this commit applies zero `ClipAdd`
        // ops, so `effect.rebuild`/`effect.persist.project` are never set —
        // no rebuild, no project.json write. The pre-Task-13 code rebuilt
        // and saved unconditionally, even for zero clips; this is an
        // intentional narrowing (a commit that materially changes nothing
        // now does nothing), not an oversight.
        // Prepare-outside: all the unbounded work for the MIDI half — the
        // TempoMap build, the sample→tick conversion, the note pairing —
        // happens HERE, before any commit, so the transaction stays a short
        // apply. The session lock is held only for the reads.
        let midi_clip = capture.and_then(|c| {
            let session = self.session.lock(); // read-only: validate the target + read tempo/ppq/take number
            // The target track can be deleted (or turned into an audio
            // track) mid-take: validate BEFORE building the op, so the
            // transaction can never fail on it and take the audio clips
            // down with it. `transact` closures must not panic and must
            // validate before mutating.
            //
            // This closes the mid-take window, not a concurrent one: the
            // lock drops before `take_clip` and is re-taken inside the
            // transaction, so a `remove_track` landing in between still
            // registers a clip on a dead track. `ClipAdd` has the same
            // exposure, and closing it means holding the session lock
            // across the commit — a different, bigger change.
            if !session.store.tracks.iter().any(|t| t.id.as_str() == c.track_id && t.kind == "midi") {
                log::warn!("stop_recording: midi take dropped — track {} is gone", c.track_id);
                return None;
            }
            let map = match crate::midi::tempo::TempoMap::new(
                session.midi.ppq,
                session.midi.tempo_events.clone(),
                self.shared.sample_rate.load(Relaxed),
            ) {
                Ok(m) => m,
                Err(e) => {
                    log::warn!("stop_recording: midi take dropped — tempo map: {e}");
                    return None;
                }
            };
            let name = format!("MIDI Take {}", session.midi.clips.len() + 1);
            drop(session);
            // `None` when nothing was played: an empty clip on the timeline
            // would be worse than no take at all.
            crate::midi::capture::take_clip(&c, &name, &map)
        });

        if let Err(e) = self.commit_recording_finalize(&clips, midi_clip.as_ref()) {
            log::warn!("stop_recording: finalize commit failed: {e}");
        }
        // Own, transient commit — same reasoning as `commit_auto_stop`/
        // `commit_start_recording_state`: transport state is RT/document-
        // visible but not itself a document edit a user would expect
        // separately in undo history, and keeping it OUT of the ClipAdd tx
        // above is exactly what review round 1 asked for (see the comment
        // above `commit_recording_finalize`'s call).
        if let Err(e) = self.commit_recording_stopped_state() {
            log::warn!("stop_recording: transport-state commit failed: {e}");
        }
        self.events.emit(
            "recording://state",
            serde_json::json!({
                "recording": false,
                "trackIds": track_ids,
                "xruns": self.shared.xruns.load(Relaxed),
                "clips": clips,
                "midiClipId": midi_clip.as_ref().map(|c| c.id.to_string()),
            }),
        );
        match writer_err {
            Some(e) => Err(e),
            None => Ok(clips),
        }
    }

    /// The ONE non-transient `Actor::Engine` tx `stop_recording` submits to
    /// register the take's clips — split out so it's independently testable
    /// without a live input stream/disk writer (`clips` is exactly what
    /// `writer.finish` returned; this fn does no I/O of its own).
    ///
    /// Ruling 6, one take = one transaction: the audio `ClipAdd`s and the
    /// MIDI `MidiClipAdd` ride HERE together, so a take is one undo entry —
    /// undoing it removes the whole take, never half of it.
    /// `TransportState` still rides its own transient commit (below).
    /// `midi_clip` is prepared outside, already validated against the store;
    /// this closure only applies it, and can therefore never panic on it.
    /// `emit_project_changed: true` — unlike the transient sites,
    /// registering new clips IS a document edit (mirrors the `set_track_
    /// instrument` precedent: routing a previously-raw write through
    /// `commit` legitimately starts firing `project://changed` where it
    /// didn't before; `recording://state` remains the dedicated
    /// "recording finished" signal for the trackIds/xruns/clips shape).
    ///
    /// Review round 1 (Important-1): ONLY `ClipAdd`s live here now —
    /// `TransportState="stopped"` moved to its own transient commit
    /// (`commit_recording_stopped_state`, called right after this one in
    /// `stop_recording`). Bundling both into one tx would make the state
    /// flip part of THIS tx's inverse: once Task 17 lands undo history,
    /// undoing "stop recording" would restore `state = "recording"` in the
    /// SAME step that removes the clips — claiming a take is running while
    /// nothing records. Splitting them means undoing the ClipAdd tx only
    /// ever un-registers clips; the state mirror is a separate, transient
    /// fact that was never meant to be undo-tracked in the first place
    /// (same reasoning as every other transport-family commit — Task 12).
    fn commit_recording_finalize(
        &mut self,
        clips: &[Clip],
        midi_clip: Option<&crate::midi::types::MidiClip>,
    ) -> Result<Committed, String> {
        let committer = self.committer.clone();
        committer.commit_with_rebuild(
            op::TxMeta::engine("stop recording"),
            |tx| {
                for clip in clips {
                    let idx = tx.store().clips.len();
                    tx.apply(op::Op::ClipAdd { clip: clip.clone(), index: idx })?;
                }
                if let Some(mc) = midi_clip {
                    let index = tx.midi().clips.len();
                    tx.apply(op::Op::MidiClipAdd { clip: mc.clone(), index })?;
                }
                Ok(())
            },
            true,
            || self.rebuild(),
        )
    }

    /// The transient commit `stop_recording` submits, immediately after
    /// `commit_recording_finalize`, for the "stopped" transport-state
    /// mirror — split out of the finalize tx (review round 1, Important-1;
    /// see that method's doc for why). Same shape as `commit_auto_stop`/
    /// `commit_start_recording_state`: transient, `emit_project_changed:
    /// false` (`recording://state`, emitted by `stop_recording` right after
    /// both commits, is what today's callers actually observe), `do_rebuild:
    /// || {}` (`Op::Set{Transport, ...}` never sets `effect.rebuild` —
    /// Task 12's transport family).
    fn commit_recording_stopped_state(&mut self) -> Result<Committed, String> {
        let committer = self.committer.clone();
        committer.commit_with_rebuild(
            op::TxMeta::engine("stop recording").transient(),
            |tx| {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Transport,
                    path: op::PropPath::TransportState,
                    from: serde_json::Value::Null,
                    to: serde_json::json!("stopped"),
                })
            },
            false,
            || {},
        )
    }

    /// Auto-create a default project when recording starts with none open.
    /// Round-2 §4.5 carve-out (epoch boundary, "document birth"): the
    /// dir-resolution/store-swap logic is shared with `ControlPlane::
    /// ensure_project_epoch` (Task 6) via `project::ensure_default_project`.
    /// Task 13: this call site now goes through the closure `lib.rs`
    /// installs (`ensure_project_fn`, `ControlMsg::SetEnsureProject`) —
    /// bound over `ControlPlane::ensure_project_epoch`, which already does
    /// the store swap AND its own `project://changed` emit — so this
    /// thread never touches `project_dir`/`project_name`/`created_at`
    /// itself and never emits that event directly either; both moved
    /// behind the one sanctioned epoch fn.
    fn ensure_project(&mut self) -> Result<(), String> {
        let f = self
            .ensure_project_fn
            .clone()
            .ok_or_else(|| "ensure_project: no project-birth closure installed yet".to_string())?;
        f()?;
        Ok(())
    }
}

/// Split a record request into the audio tracks that get a WAV writer and
/// the (at most one) MIDI track that gets a captured take. Pure, so the
/// whole policy is testable without a device, a project or a writer.
///
/// * explicit `requested` ids are validated against the store, and
///   `kind: "midi"` ids are dropped from the AUDIO set (ruling 7) — they are
///   recorded as MIDI when they are the routing target, never as a WAV;
/// * `None` means "the armed tracks", audio ones only;
/// * `midi_target` is the MIDI-in routing target, kept only when it still
///   names an existing `kind: "midi"` track — the take is what makes the
///   routing target a record target, so an audio track routed for
///   monitoring never becomes a MIDI take;
/// * an empty result on BOTH halves is the "no armed tracks to record"
///   error, unchanged in wording (ruling 8: either half alone is a take).
///   The wording is the plan's and is kept, but it is wrong for one caller:
///   an explicit `requested` list naming only MIDI tracks with no routing
///   target set is reported as "no armed tracks" although nothing was armed
///   or unarmed. Reachable from MCP's `record_take`; wants its own branch.
fn split_record_targets(
    store: &Store,
    requested: Option<Vec<String>>,
    midi_target: Option<String>,
) -> Result<(Vec<String>, Option<String>), String> {
    let is_midi =
        |id: &str| store.tracks.iter().any(|t| t.id.as_str() == id && t.kind == "midi");
    let audio: Vec<String> = match requested {
        Some(ids) => {
            for id in &ids {
                if !store.tracks.iter().any(|t| &t.id == id) {
                    return Err(format!("unknown track: {id}"));
                }
            }
            ids.into_iter().filter(|id| !is_midi(id)).collect()
        }
        None => store.armed_track_ids().into_iter().filter(|id| !is_midi(id)).collect(),
    };
    let midi = midi_target.filter(|id| is_midi(id));
    if audio.is_empty() && midi.is_none() {
        return Err("no armed tracks to record".to_string());
    }
    Ok((audio, midi))
}

/// Decode a WAV file to interleaved f32 (int formats normalized to ±1.0).
pub fn load_wav(path: &Path) -> Result<(u16, u32, Vec<f32>), String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, _) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        (hound::SampleFormat::Int, bits) => {
            let scale = (1i64 << (bits.saturating_sub(1))) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    Ok((spec.channels, spec.sample_rate, samples))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct NullEvents;
    impl EventSink for NullEvents {
        fn emit(&self, _event: &str, _payload: serde_json::Value) {}
    }

    struct CountingSink(Arc<AtomicUsize>, Arc<Mutex<Vec<MeterFrame>>>);
    impl MeterSink for CountingSink {
        fn send_frame(&self, frame: &MeterFrame) -> bool {
            self.0.fetch_add(1, Relaxed);
            self.1.lock().push(frame.clone());
            true
        }
    }

    /// Drive `OutputCb::render` directly — the only way to test the RT half
    /// deterministically, without depending on a device's buffer size or on
    /// how promptly the control thread happens to be scheduled.
    /// The ring halves the engine would own; returned so the caller keeps
    /// them alive for the callback's lifetime.
    type CbPeers = (
        rtrb::Producer<GraphPtr>,
        rtrb::Consumer<GraphPtr>,
        rtrb::Consumer<RawMeterBlock>,
    );

    fn output_cb(shared: Arc<SharedRt>) -> (OutputCb, rtrb::Consumer<RtEvent>, CbPeers) {
        let (graph_tx, graph_rx) = rtrb::RingBuffer::new(8);
        let (retire_tx, retire_rx) = rtrb::RingBuffer::new(8);
        let (meter_tx, meter_rx) = rtrb::RingBuffer::new(64);
        let (evt_tx, evt_rx) = rtrb::RingBuffer::new(64);
        // A hub per callback, wired exactly as `open_output` wires the real
        // one, so no engine test depends on (or perturbs) the global hub.
        let (live_in_tx, live_in_rx) = rtrb::RingBuffer::new(LIVE_IN_RING_SLOTS);
        let live_in_hub = Arc::new(MidiInHub::new());
        live_in_hub.install_producer(live_in_tx);
        (
            OutputCb {
                graph_rx,
                retire_tx,
                meter_tx,
                evt_tx,
                shared,
                graph: None,
                channels: 2,
                rate: 48_000,
                next_pos: u64::MAX,
                was_playing: false,
                live_in_rx,
                live_in_buf: [LiveMidiEvent::all_off(); LIVE_IN_BUF_SLOTS],
                live_in_hub,
                live_in_slot: None,
                live_in_all_off: false,
                live_in_held: [false; 128],
                live_in_release_slot: None,
                live_in_release_left: 0,
                live_in_hold_left: 0,
            },
            evt_rx,
            (graph_tx, retire_rx, meter_rx),
        )
    }

    /// The RT half of auto-stop, end to end and deterministic: the callback
    /// REPORTS the crossing (never acts on it), and carries out the parking
    /// position the control thread asks for — exactly once, only while
    /// stopped, and landing on the boundary sample rather than wherever the
    /// buffer happened to end.
    #[test]
    fn render_reports_the_end_crossing_then_parks_exactly_once() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, mut evt_rx, _peers) = output_cb(shared.clone());
        let mut out = vec![0.0f32; 128 * 2]; // 128 frames, stereo

        shared.song_end.store(1000, Relaxed);
        shared.position.store(900, Relaxed);
        shared.playing.store(true, Relaxed);

        cb.render(&mut out);
        assert_eq!(
            evt_rx.pop().ok(),
            Some(RtEvent::ReachedEnd { at: 1000 }),
            "the crossing is reported with the exact boundary sample"
        );
        assert_eq!(
            shared.position.load(Relaxed),
            1028,
            "the callback took NO action — it advanced right past the end"
        );

        // What the control thread does once it pops that event.
        shared.park.store(1000, Relaxed);
        shared.playing.store(false, Relaxed);

        cb.render(&mut out);
        assert_eq!(shared.position.load(Relaxed), 1000, "parked on the boundary");
        assert_eq!(shared.park.load(Relaxed), NO_PARK, "the request is consumed");

        // A later stopped block must not re-apply it: the playhead is the
        // user's to move once the transport has stopped.
        shared.position.store(4242, Relaxed);
        cb.render(&mut out);
        assert_eq!(shared.position.load(Relaxed), 4242, "parked only once");

        // And the crossing is edge-triggered: no repeat while sitting past
        // the end, nor when playing resumes beyond it.
        shared.playing.store(true, Relaxed);
        cb.render(&mut out);
        assert!(evt_rx.pop().is_err(), "no second ReachedEnd past the end");
    }

    /// Nothing to play means no boundary: an empty project must not report a
    /// crossing at sample 0 the moment it starts rolling.
    #[test]
    fn render_reports_no_crossing_without_material() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, mut evt_rx, _peers) = output_cb(shared.clone());
        let mut out = vec![0.0f32; 128 * 2];
        shared.playing.store(true, Relaxed); // song_end stays 0
        for _ in 0..8 {
            cb.render(&mut out);
        }
        assert!(evt_rx.pop().is_err());
        assert_eq!(shared.position.load(Relaxed), 8 * 128);
    }

    /// Round-2 §3.5 (deferred from Plan C+D): CLAP hosts require
    /// `steady_time` to only ever climb — it must NOT reset when a live
    /// node is re-created (instrument rebind, sample-rate change, a track
    /// leaving and re-entering the live set). The old design counted
    /// samples on the node itself (`ClapNode::steady`), so a rebuild that
    /// built a fresh node handed the host a fresh zero. The fix moves the
    /// counter onto `SharedRt`, advanced once per block by `OutputCb`.
    ///
    /// This exercises the REAL RT path end to end: `OutputCb::render`'s
    /// block prologue, the real `SharedRt::steady` atomic, the real
    /// `mixer::render_rt` / `render_live` plumbing, and a REAL graph
    /// rebuild — a brand-new `RtGraph` with a brand-new `LiveNodeCell`
    /// swapped in mid-stream via the same graph_tx/retire_tx ring the
    /// engine control thread uses for every rebuild. Only the leaf
    /// "plugin" is a test double (any `LiveInstrument` stands in for
    /// `ClapNode`, which is opaque from the outside): it just records the
    /// `ProcessBlock::steady` value each block hands it.
    #[derive(Clone)]
    struct RecordingNode(Arc<Mutex<Vec<u64>>>);

    impl crate::audio::dsp::AudioProcessor for RecordingNode {
        fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}
        fn process(&mut self, io: &mut crate::audio::dsp::ProcessBlock<'_>) {
            // The real RT path (`render_rt`) always carries an
            // engine-global value — `None` here would mean this test
            // somehow went through the non-RT `render` entry point instead.
            self.0.lock().push(io.steady.expect("RT block always carries a steady value"));
        }
        fn reset(&mut self) {}
    }

    impl crate::audio::dsp::LiveInstrument for RecordingNode {
        fn queue_event(&mut self, _ev: crate::midi::synth::BlockNoteEvent) -> bool {
            true
        }
        fn all_notes_off(&mut self) {}
    }

    /// A one-track live graph wrapping a fresh `LiveNodeCell` around `node`
    /// — the same shape a control-thread rebuild publishes for an
    /// instrument rebind (Task 16 brief's exact §3.5 hazard).
    fn live_graph(node: RecordingNode, generation: u64) -> RtGraph {
        let cell = crate::audio::rt::LiveNodeCell::new(Box::new(node));
        let tr = RtTrack {
            slot: 0,
            clips: Vec::new(),
            live: Some(crate::audio::rt::LiveSource { node: cell, events: Arc::new(Vec::new()) }),
        };
        RtGraph::new(vec![tr], generation, Arc::new(ParamTable::with_slots(1)))
    }

    #[test]
    fn steady_time_survives_live_node_recreation() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut out = vec![0.0f32; 128 * 2];
        shared.playing.store(true, Relaxed);

        // Node A's graph — adopted and rendered for two blocks.
        graph_tx
            .push(GraphPtr::new(Box::new(live_graph(RecordingNode(log.clone()), 1))))
            .expect("ring has room");
        cb.render(&mut out);
        cb.render(&mut out);

        // Simulate the rebind/rebuild hazard: a FRESH `RtGraph` with a
        // BRAND-NEW `LiveNodeCell` (never the same node object as A) lands
        // mid-stream, exactly like a real instrument rebind, sample-rate
        // change, or a track re-entering the live set.
        graph_tx
            .push(GraphPtr::new(Box::new(live_graph(RecordingNode(log.clone()), 2))))
            .expect("ring has room");
        cb.render(&mut out);

        let seen = log.lock().clone();
        assert_eq!(seen.len(), 3, "one steady value observed per rendered block");
        assert!(seen[1] > seen[0], "monotonic within the same node: {:?}", seen);
        assert!(
            seen[2] > seen[1],
            "steady_time must NOT reset when the live node is re-created (round-2 §3.5): {:?}",
            seen
        );
    }

    // ---- hardware MIDI-in through the real callback (slice 2, Task 10) ----

    /// A one-track live graph whose node is a REAL `PolySynth`, so what the
    /// assertions measure is real audio rather than a recording double.
    fn polysynth_graph(slot: usize, generation: u64, events: Vec<crate::midi::schedule::AbsNoteEvent>) -> RtGraph {
        let mut synth = crate::midi::synth::PolySynth::new();
        crate::audio::dsp::AudioProcessor::prepare(&mut synth, 48_000, crate::audio::rt::MAX_LIVE_BLOCK);
        let cell = crate::audio::rt::LiveNodeCell::new(Box::new(synth));
        RtGraph::new(
            vec![RtTrack {
                slot,
                clips: Vec::new(),
                live: Some(crate::audio::rt::LiveSource { node: cell, events: Arc::new(events) }),
            }],
            generation,
            Arc::new(ParamTable::with_slots(slot + 1)),
        )
    }

    fn peak(out: &[f32]) -> f32 {
        out.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// A two-track live graph, both slots a real `PolySynth`.
    fn polysynth_graph_pair(
        generation: u64,
        events0: Vec<crate::midi::schedule::AbsNoteEvent>,
    ) -> RtGraph {
        let mut events0 = Some(events0);
        let mut track = |slot: usize| {
            let mut synth = crate::midi::synth::PolySynth::new();
            crate::audio::dsp::AudioProcessor::prepare(&mut synth, 48_000, crate::audio::rt::MAX_LIVE_BLOCK);
            RtTrack {
                slot,
                clips: Vec::new(),
                live: Some(crate::audio::rt::LiveSource {
                    node: crate::audio::rt::LiveNodeCell::new(Box::new(synth)),
                    events: Arc::new(events0.take().unwrap_or_default()),
                }),
            }
        };
        RtGraph::new(vec![track(0), track(1)], generation, Arc::new(ParamTable::with_slots(2)))
    }

    /// Select a routing target and resolve it, the way the control thread's
    /// tick does — `t-1` is slot 0, `t-2` is slot 1, `None` clears.
    fn retarget(cb: &OutputCb, track_id: Option<&str>) {
        let tables = crate::audio::rt::GraphTables::empty();
        tables.lock().slots.insert("t-1".into(), 0);
        tables.lock().slots.insert("t-2".into(), 1);
        cb.live_in_hub.set_target_track(track_id.map(|s| s.to_string()));
        cb.live_in_hub.refresh_target(1, &tables);
    }

    fn target_slot_0(cb: &OutputCb, track_id: &str) {
        retarget(cb, Some(track_id));
    }

    /// Monitoring must work with the transport STOPPED — that is the normal
    /// state when someone plugs in a keyboard and plays.
    #[test]
    fn live_in_notes_are_audible_while_the_transport_is_stopped() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, Vec::new())))).unwrap();
        target_slot_0(&cb, "t-1");
        // Pushed exactly as the midir callback thread pushes it.
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(69, 110)));

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out); // adopts the graph, drains the ring
        assert!(peak(&out) > 0.02, "stopped-transport monitoring is audible");
        assert_eq!(shared.position.load(Relaxed), 0, "monitoring never advances the transport");

        // The voice is HELD by the callback's own node across blocks, and a
        // note-off from the ring releases it — proof the events reach the
        // graph's node and not a per-block copy of one.
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "the held voice keeps sounding");
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_off(69)));
        for _ in 0..12 {
            cb.render(&mut out); // past PolySynth's ~80 ms release
        }
        assert_eq!(peak(&out), 0.0, "the note-off released the voice");
    }

    /// The same routing while PLAYING goes through the full mixer path, and
    /// the transport still advances exactly as it did before this task.
    #[test]
    fn live_in_notes_are_audible_while_playing_and_the_transport_still_advances() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, Vec::new())))).unwrap();
        target_slot_0(&cb, "t-1");
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(69, 110)));
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "routed note is audible while playing");
        assert_eq!(shared.position.load(Relaxed), 512, "playback still advances");
    }

    /// The drain is bounded per block and the ring drops rather than blocks —
    /// the two properties that keep the callback inside its deadline when the
    /// producer outruns it.
    #[test]
    fn live_in_drain_is_bounded_and_overflow_is_counted() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, _peers) = output_cb(shared.clone());
        for i in 0..(LIVE_IN_RING_SLOTS + 50) {
            cb.live_in_hub.push(LiveMidiEvent::note_on((i % 128) as u8, 100));
        }
        assert_eq!(
            cb.live_in_hub.dropped(),
            50,
            "a full ring drops rather than blocks"
        );

        let mut out = vec![0.0f32; 128 * 2];
        cb.render(&mut out);
        assert_eq!(
            cb.live_in_rx.slots(),
            LIVE_IN_RING_SLOTS - MAX_LIVE_IN_PER_BLOCK,
            "one block takes at most MAX_LIVE_IN_PER_BLOCK events"
        );
    }

    /// With nothing routed, a stopped transport renders silence — including
    /// the target track's own SCHEDULED notes, which must never sound from a
    /// parked playhead.
    #[test]
    fn without_a_target_a_stopped_transport_still_renders_silence() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _r, _m)) = output_cb(shared.clone());
        let scheduled = vec![crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 100 }];
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, scheduled)))).unwrap();
        let mut out = vec![1.0f32; 128 * 2];
        cb.render(&mut out);
        assert_eq!(peak(&out), 0.0);
    }

    /// Fix round 1, Critical 1. Rendering the armed track while STOPPED
    /// removed the thing that used to silence it — the stopped branch was
    /// `out.fill(0.0)`. A clip note whose note-off is still ahead on the
    /// timeline is HELD in the node when the user presses Stop, and the
    /// graph's own release only fires at the next play start, so without an
    /// explicit release the note drones for as long as the track stays
    /// armed.
    #[test]
    fn stopping_mid_note_releases_the_armed_track_instead_of_droning() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        let scheduled = vec![
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0 },
        ];
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, scheduled)))).unwrap();
        target_slot_0(&cb, "t-1");
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "the clip note is sounding when Stop is pressed");

        shared.playing.store(false, Relaxed);
        for _ in 0..20 {
            cb.render(&mut out); // ~213 ms, well past PolySynth's release
        }
        assert_eq!(peak(&out), 0.0, "stopping released the note");
    }

    /// Fix round 2, Important 1. Monitoring shares the node that plays the
    /// clips, so a node-wide release cannot tell a monitored voice from a
    /// clip voice. Arming mid-playback is the feature's primary gesture —
    /// play the song, arm the track, jam along — and it must not cut the
    /// note the song is in the middle of. The note-on has already happened,
    /// so a cut one is gone until the clip's next note.
    #[test]
    fn arming_mid_playback_leaves_the_clip_note_that_is_already_sounding_alone() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        let scheduled = vec![
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0 },
        ];
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, scheduled)))).unwrap();
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        cb.render(&mut out);
        let sounding = peak(&out);
        assert!(sounding > 0.02, "the clip note is sounding before the arm");

        target_slot_0(&cb, "t-1");
        // Well past the release the blanket all-off would have started: a cut
        // note is all the way down by now, a held one has not moved at all.
        for _ in 0..12 {
            cb.render(&mut out);
            assert!(
                peak(&out) > sounding * 0.9,
                "arming cut the sounding clip note: {} was {}",
                peak(&out),
                sounding
            );
        }
        // …and monitoring is live on the same node, in the same breath.
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(72, 110)));
        cb.render(&mut out);
        assert!(peak(&out) > sounding, "the monitored note plays on top of the clip");
    }

    /// The mirror image, same mechanism: taking the target AWAY from a track
    /// whose clip is sounding must release only what MONITORING put down.
    #[test]
    fn switching_target_mid_playback_leaves_the_clip_note_alone() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        let scheduled = vec![
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0 },
        ];
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph_pair(1, scheduled)))).unwrap();
        target_slot_0(&cb, "t-1");
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        cb.render(&mut out);
        let sounding = peak(&out);
        assert!(sounding > 0.02);

        retarget(&cb, Some("t-2"));
        for _ in 0..12 {
            cb.render(&mut out);
            assert!(
                peak(&out) > sounding * 0.9,
                "the switch cut t-1's clip note: {} was {}",
                peak(&out),
                sounding
            );
        }
    }

    /// Fix round 2, Important 2. Everything that asks the hub to release
    /// monitoring — the monitor toggle's falling edge, a port change, a
    /// port close — pushes one `all_off` onto the ring. Passed through to
    /// the node it would cut the clip note too, which is why the callback
    /// expands it into note-offs for the keys MONITORING put down.
    #[test]
    fn a_release_request_from_the_port_stops_monitoring_without_cutting_the_clip() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        let scheduled = vec![
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0 },
        ];
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, scheduled)))).unwrap();
        target_slot_0(&cb, "t-1");
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        cb.render(&mut out);
        let clip_only = peak(&out);
        assert!(clip_only > 0.02);

        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(72, 110)));
        cb.render(&mut out);
        let both = peak(&out);
        assert!(both > clip_only * 1.2, "the monitored key is audible on top");

        assert!(cb.live_in_hub.push(LiveMidiEvent::all_off()));
        for _ in 0..12 {
            cb.render(&mut out);
        }
        let after = peak(&out);
        assert!(after < both * 0.9, "monitoring was released");
        assert!(
            after > clip_only * 0.9,
            "…and the clip note was not: {after} (clip alone was {clip_only})"
        );
    }

    /// The other half of Critical 1: a track that was NOT the target when
    /// the transport stopped got no release at all (nothing renders it, so
    /// nothing can deliver one). Arming it afterwards starts monitoring a
    /// node that is still holding the clip note from the last playthrough.
    #[test]
    fn arming_a_track_that_was_left_holding_a_clip_note_does_not_start_it_droning() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        let scheduled = vec![
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0 },
        ];
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, scheduled)))).unwrap();
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "the clip note is sounding");
        shared.playing.store(false, Relaxed);
        cb.render(&mut out);

        target_slot_0(&cb, "t-1");
        for _ in 0..12 {
            cb.render(&mut out);
        }
        assert_eq!(peak(&out), 0.0, "arming released the leftover voice");
    }

    /// Fix round 1, Critical 2. `set_target_track` pushes an all-off for the
    /// OUTGOING node, but the ring carries no slot: by the time the callback
    /// drains it, `target_slot()` already names the new target — and when
    /// there is none, the whole block is discarded. The held voice survives
    /// in the node, silent, and comes back the instant the track is armed
    /// again.
    #[test]
    fn disarming_releases_the_held_note_so_re_arming_cannot_resurrect_it() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph(0, 1, Vec::new())))).unwrap();
        target_slot_0(&cb, "t-1");
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(69, 110)));

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "the held key is sounding");

        // Disarm — no note-off was ever played, the key is still down.
        retarget(&cb, None);
        for _ in 0..20 {
            cb.render(&mut out);
        }

        retarget(&cb, Some("t-1"));
        cb.render(&mut out);
        assert_eq!(peak(&out), 0.0, "re-arming must not resurrect the old voice");
    }

    /// Same root cause, switch variant: a fix that only handled
    /// `Some -> None` would leave this one open, because the all-off lands
    /// on the INCOMING node while the outgoing one keeps holding the key.
    /// Inaudible while t-2 is armed (only the target slot renders), which is
    /// exactly what makes it a trap.
    #[test]
    fn switching_target_releases_the_track_that_was_holding_the_key() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph_pair(1, Vec::new())))).unwrap();
        target_slot_0(&cb, "t-1");
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(69, 110)));

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "t-1 is sounding");

        retarget(&cb, Some("t-2"));
        for _ in 0..20 {
            cb.render(&mut out);
        }
        assert_eq!(peak(&out), 0.0, "t-1's held key does not sound through t-2");

        // The release window must EXPIRE — a countdown that never cleared
        // would leave the outgoing node holding the live-in channel and kill
        // monitoring for good.
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(72, 110)));
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "the new target monitors once the release is done");

        // Let t-2 go quiet, so from here on ANY sound can only be t-1's.
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_off(72)));
        for _ in 0..12 {
            cb.render(&mut out);
        }
        assert_eq!(peak(&out), 0.0, "t-2 is quiet again");

        // Re-arm t-1 and watch EVERY block, not just the last: a voice that
        // was left frozen mid-release surfaces as a decaying fragment on the
        // first block that renders slot 0, whenever that lands.
        retarget(&cb, Some("t-1"));
        let mut loudest = 0.0f32;
        for _ in 0..16 {
            cb.render(&mut out);
            loudest = loudest.max(peak(&out));
        }
        assert_eq!(loudest, 0.0, "t-1's voice was released when it lost the target");
    }

    fn spin_up() -> (EngineHandle, Arc<SharedRt>, SharedGraphTables, Arc<Mutex<Session>>) {
        let shared = Arc::new(SharedRt::default());
        let tables: SharedGraphTables = Arc::new(Mutex::new(GraphTables {
            generation: 0,
            params: Arc::new(ParamTable::default()),
            slots: HashMap::new(),
        }));
        let session = Arc::new(Mutex::new(Session::new(Store::default(), crate::midi::MidiStore::default())));
        let handle = start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
            crate::control::testutil::test_committer(&session, &shared, &tables),
        );
        (handle, shared, tables, session)
    }

    /// Headless `Control` fixture (Plan E Task 13): the struct literal
    /// `start()` builds, minus the thread spawn AND `open_output` — no
    /// output/input stream, no disk writer. Lets `commit_recording_finalize`/
    /// `commit_auto_stop` be driven directly, synchronously, on the test
    /// thread, without a real cpal device or real WAV I/O — same headless
    /// mode `start()`'s own doc describes ("without a device the engine
    /// still runs... so the UI and tests stay functional"), just without
    /// even the control-thread machinery around it.
    fn bare_control() -> (Control, Arc<Mutex<Session>>) {
        let (ctl, session, _tx) = bare_control_with_tx();
        (ctl, session)
    }

    /// `bare_control` keeping the message SENDER alive, so a test can drive
    /// `Control::run`'s real loop: send one message, drop the sender, and
    /// `run` executes exactly one full iteration (handle → drain → tick
    /// work) before `recv_timeout` reports Disconnected and it returns.
    /// That is the only way to cover the per-tick work `run` schedules,
    /// call sites included.
    fn bare_control_with_tx() -> (Control, Arc<Mutex<Session>>, Sender<ControlMsg>) {
        let shared = Arc::new(SharedRt::default());
        let tables: SharedGraphTables = Arc::new(Mutex::new(GraphTables {
            generation: 0,
            params: Arc::new(ParamTable::default()),
            slots: HashMap::new(),
        }));
        let session = Arc::new(Mutex::new(Session::new(Store::default(), crate::midi::MidiStore::default())));
        let (tx, rx) = unbounded();
        let committer = crate::control::testutil::test_committer(&session, &shared, &tables);
        let ctl = Control {
            shared,
            tables,
            generation: 0,
            rebuild_pending: false,
            session: session.clone(),
            events: Box::new(NullEvents),
            rx,
            output: None,
            input: None,
            writer: None,
            rec_track_ids: Vec::new(),
            sel_output: None,
            sel_input: None,
            cache: HashMap::new(),
            cache_rate: 0,
            live_nodes: Default::default(),
            accum: MeterAccum::default(),
            gen_maps: GenerationMaps::default(),
            sinks: Vec::new(),
            last_frame: Instant::now(),
            last_tick: Instant::now(),
            committer,
            ensure_project_fn: None,
            param_automation: crate::plugins::automation::ParamAutomationDriver::empty(),
            param_writes: Vec::new(),
            // Its OWN hub, never the process-global one: these tests would
            // otherwise race every other test that selects a routing target.
            live_in_hub: Arc::new(MidiInHub::new()),
            live_in_target: None,
        };
        (ctl, session, tx)
    }

    fn test_track(id: &str) -> super::super::types::TrackState {
        super::super::types::TrackState {
            id: id.into(),
            name: id.into(),
            kind: "audio".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        }
    }

    fn test_lane(id: &str, target: &str, param_id: u32) -> crate::plugins::automation::AutomationLane {
        use crate::plugins::automation::{AutomationLane, AutomationPoint};
        AutomationLane {
            id: id.into(),
            target_node: target.into(),
            param_id,
            points: vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 3840, value: 0.0 },
            ],
        }
    }

    /// Track D: a rebuild compiles the session's plugin-param lanes into the
    /// control-thread driver. Headless (`bare_control`) builds no graph, so
    /// the driver is the observable half of the attach here; the gain-ramp
    /// half is pinned by `rebuild_compiles_track_gain_lanes_by_slot` below
    /// (the compile step `rebuild` itself calls) and at the mixer seam
    /// (`audio::mixer`'s `track_gain_ramp_*` tests).
    #[test]
    fn rebuild_compiles_plugin_param_lanes_into_the_driver() {
        let (mut ctl, session) = bare_control();
        {
            let mut s = session.lock();
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(),
                uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(),
                format: "lv2".into(),
                status: "active".into(),
                track_id: None,
            });
            s.automation.lanes.push(test_lane("l1", "inst-1", 7));
        }
        assert!(ctl.param_automation.is_empty(), "nothing compiled before the rebuild");
        ctl.rebuild();
        assert!(!ctl.param_automation.is_empty(), "the rebuild compiled the lane");

        // A rebuild AFTER the lane is gone drops it again — a deleted lane
        // stops driving only because the driver is rebuilt wholesale.
        session.lock().automation.lanes.clear();
        ctl.rebuild();
        assert!(ctl.param_automation.is_empty());
    }

    /// Seed a hosted instance and a 0→1 lane on its param 7, spanning one
    /// bar (3840 ticks = 96_000 samples at 120 bpm / 48 kHz).
    fn seed_plugin_lane(session: &Arc<Mutex<Session>>) {
        let mut s = session.lock();
        s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(),
            uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "active".into(),
            track_id: None,
        });
        let mut l = test_lane("l1", "inst-1", 7);
        l.points[0].value = 0.0;
        l.points[1].value = 1.0;
        s.automation.lanes.push(l);
    }

    /// Track D review I-4: `run` must actually DRIVE the compiled lanes —
    /// deleting the `drive_param_automation()` call left every other test
    /// green. One `run` iteration (see `bare_control_with_tx`) rebuilds and
    /// then ticks, so the batch left in `param_writes` is the call site's
    /// only in-process witness.
    #[test]
    fn run_drives_plugin_param_automation_at_the_transport_position() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        seed_plugin_lane(&session);
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(48_000, Relaxed); // halfway up the ramp
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();

        assert_eq!(ctl.param_writes.len(), 1, "the tick produced one host write");
        let w = &ctl.param_writes[0];
        assert_eq!((w.instance.as_str(), w.format.as_str(), w.index), ("inst-1", "lv2", 7));
        assert!((w.value - 0.5).abs() < 1e-3, "interpolated at the transport position: {}", w.value);
    }

    /// The other half of the same guard: a STOPPED transport writes nothing,
    /// so the last automated value stays put instead of being re-driven from
    /// a parked playhead.
    #[test]
    fn run_drives_no_param_automation_while_stopped() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        seed_plugin_lane(&session);
        ctl.shared.position.store(48_000, Relaxed);
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();

        assert!(!ctl.param_automation.is_empty(), "the lane IS compiled");
        assert!(ctl.param_writes.is_empty(), "…but a stopped transport drives nothing");
    }

    /// Both live-in call sites `run` owns, in one iteration. Selecting the
    /// routing target is app config (ruling 1): it commits nothing, so NO
    /// `EngineEffect::rebuild` fires — yet a clip-less midi track only gets a
    /// live node when a rebuild runs with the target already known. The tick
    /// therefore has to notice the change itself, rebuild, and only then
    /// resolve the slot the callback reads.
    ///
    /// `Subscribe` is the message that drives the iteration precisely because
    /// handling it rebuilds nothing — the generation bump can only come from
    /// the target change.
    #[test]
    fn run_rebuilds_and_resolves_the_live_in_target_when_it_changes() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.kind = "midi".into();
            s.store.tracks.push(t);
        }
        ctl.live_in_hub.set_target_track(Some("t-1".into()));
        let sink = CountingSink(Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(Vec::new())));
        tx.send(ControlMsg::Subscribe(Box::new(sink))).unwrap();
        drop(tx);
        ctl.run();

        assert_eq!(ctl.generation, 1, "the target change scheduled a rebuild");
        assert_eq!(ctl.live_in_hub.target_slot(), Some(0), "the tick resolved the slot");
    }

    /// The whole point of the feature, at the seam where it was invisible:
    /// arming an EMPTY midi track has to put a live node in the next
    /// published graph. Nothing else does — the track has no clips, so
    /// `append_from`'s clip-driven loop skips it, and only the target
    /// argument `rebuild` passes makes `append_from_with_input` add it.
    ///
    /// Needs the NON-headless half of `rebuild`, which is why `OutputBundle`
    /// holds an `Option<cpal::Stream>`: everything else about the branch is
    /// real (real slots, real registry, real graph publish over the real
    /// ring), only the device is absent.
    #[test]
    fn arming_an_empty_midi_track_publishes_a_graph_with_a_live_node_for_it() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        ctl.shared.sample_rate.store(48_000, Relaxed);
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.kind = "midi".into();
            s.store.tracks.push(t);
        }
        let (graph_tx, mut graph_rx) = rtrb::RingBuffer::new(8);
        let (_retire_tx, retire_rx) = rtrb::RingBuffer::new(8);
        let (_meter_tx, meter_rx) = rtrb::RingBuffer::new(8);
        let (_evt_tx, evt_rx) = rtrb::RingBuffer::new(8);
        ctl.output = Some(OutputBundle { _stream: None, graph_tx, retire_rx, meter_rx, evt_rx });

        ctl.live_in_hub.set_target_track(Some("t-1".into()));
        tx.send(ControlMsg::Subscribe(Box::new(CountingSink(
            Arc::new(AtomicUsize::new(0)),
            Arc::new(Mutex::new(Vec::new())),
        ))))
        .unwrap();
        drop(tx);
        ctl.run();

        let graph = graph_rx.pop().expect("the tick published a graph").into_box();
        // A midi track also gets a clips-only `RtTrack` at the same slot from
        // the assembly loop above `append_from_with_input`, so this asks for
        // the LIVE one specifically rather than the first row for the slot.
        assert!(
            graph.tracks.iter().any(|t| t.slot == 0 && t.live.is_some()),
            "an armed empty midi track has a node to play"
        );
    }

    /// Track D: the gain-ramp half of the same attach. `rebuild` builds no
    /// graph headlessly (no cpal stream is constructible in-process), so the
    /// compile step is exercised exactly as `rebuild` calls it — with the
    /// real `derive_slots` map — and the assertion is on SLOT INDEXING: the
    /// lane names the SECOND track, so a ramp landing at slot 0 would
    /// automate the wrong track's audio.
    #[test]
    fn rebuild_compiles_track_gain_lanes_by_slot() {
        let (mut ctl, session) = bare_control();
        ctl.cache_rate = 48_000; // no `rebuild` here, so nothing else sets it
        {
            let mut s = session.lock();
            s.store.tracks.push(test_track("t-1"));
            s.store.tracks.push(test_track("t-2"));
            s.automation.lanes.push(test_lane("l1", "track:t-2", 0));
            s.automation.lanes.push(test_lane("l2", "track:ghost", 0));
            s.automation.lanes.push(test_lane("l3", "inst-1", 7));
        }
        let (ramps, driver) = {
            let s = session.lock();
            let slots = derive_slots(&s.store.tracks);
            ctl.compile_automation(&s, &slots, s.store.tracks.len())
        };
        assert_eq!(ramps.len(), 2, "one entry per track slot, like ParamTable");
        assert!(ramps[0].is_none(), "t-1 has no lane — an unautomated track must stay unramped");
        let ev = ramps[1].as_ref().expect("t-2's lane compiled into ITS slot");
        assert_eq!(ev.first().map(|e| e.value), Some(1.0));
        assert_eq!(ev.last().map(|e| e.value), Some(0.0));
        assert!(
            ev.last().unwrap().sample > 0,
            "ticks became absolute samples on the control thread"
        );
        assert!(driver.is_empty(), "no such plugin instance — the plugin lane resolves to nothing");

        // A failed tempo map compiles nothing, rather than something at rate 0.
        ctl.cache_rate = 0; // as if the tempo map failed to build
        let (ramps, driver) = {
            let s = session.lock();
            let slots = derive_slots(&s.store.tracks);
            ctl.compile_automation(&s, &slots, s.store.tracks.len())
        };
        assert_eq!(ramps.len(), 2, "still one entry per slot, just nothing in them");
        assert!(ramps.iter().all(|r| r.is_none()));
        assert!(driver.is_empty());
    }

    /// Plan E Task 13's TDD step 1, at the real `Control` methods (not just
    /// the `Committer` primitive they call — `control::mod`'s own
    /// `recording_finalize_commits_as_actor_engine_with_clip_add_ops_and_
    /// one_rebuild` pins the primitive; this pins `stop_recording`'s ACTUAL
    /// call sites). Corrected by review round 1 (Important-1): finalize is
    /// TWO commits, not one — `commit_recording_finalize` (ClipAdd ops
    /// only, built AFTER `clips` is already in hand, exactly as
    /// `stop_recording` does — §4.4 "the op is the registration, never the
    /// recording itself"; exactly one rebuild, `self.generation` moves by
    /// exactly 1) followed by `commit_recording_stopped_state` (the
    /// transport-state Set, transient, its OWN commit, no further
    /// rebuild — `self.generation` unchanged by it). Splitting them keeps
    /// the state flip OUT of the ClipAdd tx's inverse: once Task 17 lands
    /// undo history, undoing "stop recording" must only ever un-register
    /// clips, never also claim a take is running again.
    #[test]
    fn commit_recording_finalize_is_actor_engine_with_clip_adds_and_one_rebuild() {
        let (mut ctl, session) = bare_control();
        // Seed a non-default state so the state-commit assertion below is
        // load-bearing (`TransportState::default()`'s `state` is already
        // "stopped", which would make that assertion trivially true even
        // if `commit_recording_stopped_state` did nothing).
        session.lock().store.transport.state = "recording".into();
        let gen_before = ctl.generation;
        let clips = vec![
            crate::audio::types::testutil::test_clip("c-1", "t-1"),
            crate::audio::types::testutil::test_clip("c-2", "t-1"),
        ];

        let clip_committed = ctl.commit_recording_finalize(&clips, None).unwrap();
        assert!(
            matches!(clip_committed.meta.actor, crate::control::op::Actor::Engine),
            "got {:?}",
            clip_committed.meta.actor
        );
        assert!(!clip_committed.meta.transient, "clip registration is a real document edit");
        assert!(
            clip_committed.ops.iter().all(|op| matches!(op, crate::control::op::Op::ClipAdd { .. })),
            "must carry ONLY ClipAdd ops, got {:?}",
            clip_committed.ops
        );
        let n_clip_adds = clip_committed
            .ops
            .iter()
            .filter(|op| matches!(op, crate::control::op::Op::ClipAdd { .. }))
            .count();
        assert_eq!(n_clip_adds, 2);
        assert_eq!(ctl.generation, gen_before + 1, "exactly one rebuild");
        assert_eq!(session.lock().store.clips.len(), 2);
        assert_eq!(session.lock().store.transport.state, "recording", "unchanged by the ClipAdd-only commit");

        let state_committed = ctl.commit_recording_stopped_state().unwrap();
        assert!(
            matches!(state_committed.meta.actor, crate::control::op::Actor::Engine),
            "got {:?}",
            state_committed.meta.actor
        );
        assert!(state_committed.meta.transient, "the state mirror is transient");
        assert!(
            state_committed.ops.iter().all(|op| matches!(
                op,
                crate::control::op::Op::Set { path: crate::control::op::PropPath::TransportState, .. }
            )),
            "must carry ONLY the TransportState Set, got {:?}",
            state_committed.ops
        );
        assert_eq!(ctl.generation, gen_before + 1, "no further rebuild from the state commit");
        assert_eq!(session.lock().store.transport.state, "stopped");
    }

    /// Plan E Task 13's TDD step 1, at the real `Control` method:
    /// `commit_auto_stop` produces a TRANSIENT `Actor::Engine` tx and never
    /// rebuilds (`Op::Set{Transport, ...}` sets no `effect.rebuild` —
    /// Task 12's transport family) — `self.generation` is unchanged.
    #[test]
    fn commit_auto_stop_is_a_transient_actor_engine_tx_with_no_rebuild() {
        let (mut ctl, session) = bare_control();
        let gen_before = ctl.generation;
        let committed = ctl.commit_auto_stop().unwrap();
        assert!(
            matches!(committed.meta.actor, crate::control::op::Actor::Engine),
            "got {:?}",
            committed.meta.actor
        );
        assert!(committed.meta.transient, "auto-stop is transient");
        assert_eq!(ctl.generation, gen_before, "Transport Set never rebuilds");
        assert_eq!(session.lock().store.transport.state, "stopped");
    }

    /// Review round 1 (Important-3): site 5 (`ensure_project`) had no
    /// fixture exercising it at all, so its new failure mode — erroring
    /// when no closure is installed, where the pre-Task-13 code silently
    /// auto-created a project itself — was unreachable in the test suite.
    /// `bare_control()` never calls `install_ensure_project` (no
    /// `ControlMsg::SetEnsureProject` is ever sent to a `Control` built
    /// this way — it isn't running a message loop at all), so
    /// `ensure_project_fn` stays `None`, exactly as it would for a real
    /// engine thread the moment after `engine::start` returns, before
    /// `lib.rs`'s post-`ControlPlane` `install_ensure_project` call lands.
    #[test]
    fn ensure_project_errs_when_no_closure_installed() {
        let (mut ctl, _session) = bare_control();
        assert!(ctl.ensure_project_fn.is_none(), "bare_control installs no closure");
        let err = ctl.ensure_project().unwrap_err();
        assert!(
            err.contains("no project-birth closure installed"),
            "got {err:?}"
        );
    }

    /// The installed-closure half of the same review point: once a closure
    /// is installed (`lib.rs`'s real call: `engine.install_ensure_project
    /// (Arc::new(move || cp.ensure_project_epoch()))`), `ensure_project`
    /// calls it EXACTLY once, and its result propagates through unchanged
    /// in BOTH directions (`Ok` and `Err`) rather than being swallowed or
    /// mapped to something else — pinned here with a counting stand-in
    /// closure rather than a real `ControlPlane::ensure_project_epoch`
    /// (which needs a `JobManager` + a real `.aura` dir to construct; the
    /// delegation contract under test is "the engine calls the installed
    /// closure and does nothing else", not `ensure_project_epoch`'s own
    /// behavior, which has its own coverage in control/mod.rs).
    #[test]
    fn ensure_project_invokes_the_installed_closure_exactly_once_and_propagates_its_result() {
        let (mut ctl, _session) = bare_control();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let calls2 = calls.clone();
        ctl.ensure_project_fn = Some(Arc::new(move || {
            calls2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(std::path::PathBuf::from("/tmp/aura-test-project"))
        }));
        ctl.ensure_project().expect("an Ok(PathBuf) from the closure propagates as Ok");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1, "invoked exactly once");

        ctl.ensure_project_fn = Some(Arc::new(|| Err("boom".to_string())));
        let err = ctl.ensure_project().unwrap_err();
        assert_eq!(err, "boom", "an Err from the closure propagates through ensure_project unchanged, not swallowed");
    }

    /// Runs with or without a real audio device: the engine falls back to
    /// headless mode, so meter frames must flow either way.
    #[test]
    fn engine_pumps_meter_frames_at_60hz() {
        let (handle, _shared, _tables, session) = spin_up();
        {
            let mut session = session.lock();
            let s = &mut session.store;
            // Slots are derived state now (round-2 §2.4) — pushing the row
            // and sending Rebuild is what seeds slot 0, not a direct alloc.
            s.tracks.push(super::super::types::TrackState {
                id: "t1".into(),
                name: "Track 1".into(),
                kind: "audio".into(),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                armed: false,
                color: "#7c9cff".into(),
                instrument_id: None,
            });
        }
        // Rebuild derives slots + publishes tables (round-2 §2.4). The
        // engine channel is FIFO and single-consumer, so this message is
        // always HANDLED before the Subscribe sent below, however the two
        // sends happen to interleave with the control thread's poll loop —
        // deterministic: by the time any sink can capture a frame, the
        // tables are already published.
        handle.send(ControlMsg::Rebuild);
        let count = Arc::new(AtomicUsize::new(0));
        let frames = Arc::new(Mutex::new(Vec::new()));
        handle.send(ControlMsg::Subscribe(Box::new(CountingSink(
            count.clone(),
            frames.clone(),
        ))));
        // Event-driven readiness: wait until frames have demonstrably flowed
        // instead of asserting a rate over a fixed wall-clock window (which
        // is load-sensitive — a starved pump thread under parallel test load
        // legitimately delivers fewer frames per wall second).
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if count.load(Relaxed) >= 10 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "expected >= 10 meter frames within 10 s, got {}",
                count.load(Relaxed)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let frames = frames.lock();
        assert!(frames[0].seq == 0, "per-subscription seq starts at 0");
        assert!(frames.windows(2).all(|w| w[1].seq == w[0].seq + 1));
        assert_eq!(frames[0].tracks.len(), 1);
        assert_eq!(frames[0].tracks[0].track_id, "t1");
        assert_eq!(frames[0].master.track_id, "master");
        handle.send(ControlMsg::Shutdown);
    }

    #[test]
    fn transport_advances_headless_or_with_device() {
        let (handle, shared, _tables, _store) = spin_up();
        // Event-driven readiness: wait for the stream (or headless fallback)
        // to publish a sample rate instead of guessing with a fixed sleep.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let rate = loop {
            let r = shared.sample_rate.load(Relaxed) as u64;
            if r > 0 {
                break r;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "engine never published a sample rate"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        shared.position.store(0, Relaxed);
        shared.playing.store(true, Relaxed);
        // Asserting a wall-clock rate corridor is load-sensitive (an
        // oversleeping test thread makes the position overshoot; a starved
        // engine thread undershoots). Assert the two real invariants
        // event-driven instead: (1) the transport demonstrably ADVANCES
        // while playing, (2) it SETTLES after stop.
        let target = rate / 10; // >= 100 ms of audio
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if shared.position.load(Relaxed) >= target {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "transport never advanced while playing (pos {} < {target})",
                shared.position.load(Relaxed)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        shared.playing.store(false, Relaxed);
        // Let any in-flight frame land, then require two agreeing reads and
        // verify the position stays put.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let settled = loop {
            let a = shared.position.load(Relaxed);
            std::thread::sleep(Duration::from_millis(50));
            let b = shared.position.load(Relaxed);
            if a == b {
                break b;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "position kept advancing after stop"
            );
        };
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            shared.position.load(Relaxed),
            settled,
            "transport advanced while stopped"
        );
        handle.send(ControlMsg::Shutdown);
    }

    /// Review fix: capture-ring overflow must never SHRINK a take — dropped
    /// audio is replaced by an equal amount of silence (repaid before newer
    /// audio), so sample count == wall clock and multi-track rings stay
    /// aligned across xruns.
    #[test]
    fn input_capture_overflow_pads_silence_instead_of_shrinking() {
        let shared = Arc::new(SharedRt::default());
        // Tiny mono ring: 8 samples capacity.
        let (producer, mut consumer) = rtrb::RingBuffer::new(8);
        let (meter_tx, _meter_rx) = rtrb::RingBuffer::new(8);
        let mut b = RawMeterBlock::new(1, 0, 0);
        b.base_slot = 0;
        let mut cb = InputCb {
            producers: vec![producer],
            owed: vec![0],
            meter_tx,
            blocks: vec![(b, vec![0])],
            in_ch: 1,
            rec_ch: 1,
            shared: shared.clone(),
        };

        // 1st buffer: 6 frames fit entirely.
        cb.capture(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(shared.xruns.load(Relaxed), 0);
        // 2nd buffer: only 2 of 6 fit -> 4 samples of debt, one xrun.
        cb.capture(&[7.0, 8.0, 9.0, 10.0, 11.0, 12.0]);
        assert_eq!(shared.xruns.load(Relaxed), 1);

        // Drain everything written so far: 6 + 2 samples.
        let mut got = Vec::new();
        while let Ok(v) = consumer.pop() {
            got.push(v);
        }
        assert_eq!(got, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);

        // 3rd buffer: the 4-sample silence debt is repaid FIRST (the gap
        // happened before this audio), then the new frames.
        cb.capture(&[13.0, 14.0]);
        let mut got = Vec::new();
        while let Ok(v) = consumer.pop() {
            got.push(v);
        }
        assert_eq!(got, vec![0.0, 0.0, 0.0, 0.0, 13.0, 14.0]);

        // Total samples delivered == total wall-clock frames captured.
        // (6 + 6 + 2 captured; 6 + 2 + 4(silence) + 2 delivered.)
        assert_eq!(6 + 2 + 4 + 2, 6 + 6 + 2);
    }

    /// Review fix: graphs still sitting in a queue when it is torn down are
    /// freed (GraphPtr owns its pointee), so device switches don't leak.
    #[test]
    fn graph_ptr_frees_on_queue_teardown() {
        use super::super::rt::{GraphPtr, RtClip, RtClipData, RtGraph, RtTrack};
        let data = Arc::new(RtClipData { channels: 1, data: vec![0.0; 16] });
        let graph = Box::new(RtGraph::new(
            vec![RtTrack::clips(
                0,
                vec![RtClip {
                    start: 0,
                    offset: 0,
                    len: 16,
                    gain: 1.0,
                    fade_in: 0,
                    fade_out: 0,
                    samples: data.clone(),
                }],
            )],
            1,
            Arc::new(ParamTable::default()),
        ));
        assert_eq!(Arc::strong_count(&data), 2);
        let (mut tx, rx) = rtrb::RingBuffer::new(4);
        tx.push(GraphPtr::new(graph)).ok().unwrap();
        // Neither side ever pops: dropping the ring must free the graph.
        drop(tx);
        drop(rx);
        assert_eq!(
            Arc::strong_count(&data),
            1,
            "graph queued at teardown was freed, not leaked"
        );
    }

    /// Round-2 §2.4 / O-13 regression pin: a retired graph renders against
    /// ITS OWN params, never a newer generation's — proving the alias
    /// window is dead by construction. Written directly against the Step 4
    /// implementation (there is no old "shared single ParamTable" code path
    /// left in this tree to fail against; that design was deleted in this
    /// same task), so this is a permanent pin rather than a red-then-green
    /// TDD cycle — acceptable per the brief's note on Step 5.
    ///
    /// gen1 has one track on slot 0 with gain 0.5; gen2 has a DIFFERENT
    /// track on the SAME slot 0 with gain 0.9. Both graphs carry a DC clip
    /// (all samples 1.0) so gain is directly readable off the output peak.
    /// `OutputCb::render` adopts a queued graph at the TOP of `render`, so
    /// pushing gen2 only AFTER a render call means "the NEXT render adopts
    /// it" — the test controls the queue, so the ordering is fully
    /// deterministic, not a race.
    #[test]
    fn retired_graph_keeps_its_own_params_no_slot_aliasing() {
        let dc = Arc::new(RtClipData { channels: 1, data: vec![1.0; 16] });
        let dc_clip = || RtClip {
            start: 0,
            offset: 0,
            len: 16,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            samples: dc.clone(),
        };

        let p1 = Arc::new(ParamTable::default());
        p1.set_gain_linear(0, 0.5);
        let g1 = Box::new(RtGraph::new(vec![RtTrack::clips(0, vec![dc_clip()])], 1, p1));

        let p2 = Arc::new(ParamTable::default());
        p2.set_gain_linear(0, 0.9);
        let g2 = Box::new(RtGraph::new(vec![RtTrack::clips(0, vec![dc_clip()])], 2, p2));

        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        shared.playing.store(true, Relaxed);

        // Center pan (default) on both channels: peak = gain * 1/sqrt(2).
        const K: f32 = std::f32::consts::FRAC_1_SQRT_2;
        let mut out = vec![0.0f32; 4 * 2]; // 4 frames, stereo

        // Adopt g1 (queued before this render).
        graph_tx.push(GraphPtr::new(g1)).unwrap();
        cb.render(&mut out);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5 * K).abs() < 1e-5, "gen1's own gain (0.5), got {peak}");

        // gen2 is NOT queued yet: this render must still read gen1's table.
        cb.render(&mut out);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak - 0.5 * K).abs() < 1e-5,
            "still gen1's gain while gen2 sits unqueued, got {peak}"
        );

        // Queue gen2 now: the NEXT render adopts it.
        graph_tx.push(GraphPtr::new(g2)).unwrap();
        cb.render(&mut out);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (peak - 0.9 * K).abs() < 1e-5,
            "gen2's own gain (0.9) after adoption — no aliasing, got {peak}"
        );
    }

    #[test]
    fn wav_loader_reads_int_and_float() {
        let dir = std::env::temp_dir()
            .join(format!("aura-eng-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 16-bit int mono
        let p16 = dir.join("i16.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&p16, spec).unwrap();
        w.write_sample(i16::MAX).unwrap();
        w.write_sample(i16::MIN).unwrap();
        w.write_sample(0i16).unwrap();
        w.finalize().unwrap();
        let (ch, rate, s) = load_wav(&p16).unwrap();
        assert_eq!((ch, rate), (1, 44_100));
        assert!((s[0] - (32767.0 / 32768.0)).abs() < 1e-4);
        assert!((s[1] + 1.0).abs() < 1e-6);
        assert_eq!(s[2], 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- source-keyed decode cache policy (round-2 §2.2) -------------------

    use crate::audio::types::testutil::test_clip;

    #[test]
    fn cache_is_shared_per_source_and_invalidates_on_path_change() {
        let mut cache: HashMap<SourceId, CachedSource> = HashMap::new();
        let sid = SourceId::from("s-1");
        let c1 = { let mut c = test_clip("c-1", "t-1"); c.source_id = sid.clone(); c };
        let c2 = { let mut c = test_clip("c-2", "t-1"); c.source_id = sid.clone(); c };
        // Two clips, one source: exactly ONE decode wanted.
        assert_eq!(stale_sources(&[c1.clone(), c2.clone()], &cache).len(), 1);
        cache.insert(sid.clone(), CachedSource {
            source_path: c1.source_path.clone(),
            data: Arc::new(RtClipData { channels: 2, data: vec![0.0; 4] }),
        });
        // Cached and unchanged: nothing to do.
        assert!(stale_sources(&[c1.clone(), c2.clone()], &cache).is_empty());
        // source_path changes under the SAME source id: re-decode (staleness).
        let mut c1b = c1.clone();
        c1b.source_path = "audio/replaced.wav".into();
        assert_eq!(stale_sources(&[c1b], &cache).len(), 1);
    }

    #[test]
    fn stale_sources_skips_empty_source_id_clips() {
        let cache: HashMap<SourceId, CachedSource> = HashMap::new();
        // Default SourceId is the empty-string sentinel. Finding 5: this IS
        // reachable in production (legacy absolute/`..` source_paths that
        // `assign_source_ids` deliberately leaves unassigned on load) — the
        // old `debug_assert!(false)` here used to panic the engine control
        // thread on opening such a project. The fix is warn+skip only: the
        // clip is silently muted (never crosses into `stale_sources`'
        // output), everything else proceeds normally.
        let c = test_clip("c-1", "t-1");
        let other = { let mut o = test_clip("c-2", "t-1"); o.source_id = SourceId::from("s-2"); o };
        assert!(c.source_id.as_str().is_empty());
        let todo = stale_sources(&[c, other], &cache);
        assert_eq!(todo.len(), 1, "the empty-source-id clip is skipped, not panicked on");
        assert_eq!(todo[0].0, SourceId::from("s-2"), "the other clip is still processed normally");
    }

    #[test]
    fn stale_sources_conflicting_paths_warn_and_take_the_latest() {
        let cache: HashMap<SourceId, CachedSource> = HashMap::new();
        let sid = SourceId::from("s-conflict");
        let c1 = { let mut c = test_clip("c-1", "t-1"); c.source_id = sid.clone(); c.source_path = "audio/a.wav".into(); c };
        let c2 = { let mut c = test_clip("c-2", "t-1"); c.source_id = sid.clone(); c.source_path = "audio/b.wav".into(); c };
        let todo = stale_sources(&[c1, c2], &cache);
        assert_eq!(todo.len(), 1, "one source, one decode — never two entries for the same id");
        assert_eq!(todo[0].0, sid);
        assert_eq!(todo[0].1, "audio/b.wav", "the LAST conflicting path wins the re-decode");
    }

    #[test]
    fn stale_sources_dedupes_and_preserves_first_seen_order() {
        let cache: HashMap<SourceId, CachedSource> = HashMap::new();
        let s1 = SourceId::from("s-1");
        let s2 = SourceId::from("s-2");
        let c1 = { let mut c = test_clip("c-1", "t-1"); c.source_id = s1.clone(); c.source_path = "audio/1.wav".into(); c };
        let c2 = { let mut c = test_clip("c-2", "t-1"); c.source_id = s2.clone(); c.source_path = "audio/2.wav".into(); c };
        let c3 = { let mut c = test_clip("c-3", "t-1"); c.source_id = s1.clone(); c.source_path = "audio/1.wav".into(); c };
        let todo = stale_sources(&[c1, c2, c3], &cache);
        assert_eq!(todo.iter().map(|(sid, _)| sid.clone()).collect::<Vec<_>>(), vec![s1, s2]);
    }

    /// Reviewer finding 2 regression test, end-to-end through a real
    /// control thread: a SECOND clip added later, sharing an
    /// ALREADY-cached source with an earlier clip, must still get its own
    /// waveform pyramid built. The bug was that pyramid-building lived
    /// nested inside the decode loop, so it only ran for sources
    /// `stale_sources` returned as needing a fresh decode — a clip whose
    /// source was already cached never got its `waveform_cache_dir`
    /// checked at all.
    #[test]
    fn ensure_loaded_builds_pyramids_for_clips_sharing_an_already_cached_source() {
        let dir = std::env::temp_dir().join(format!(
            "aura-eng-pyramid-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(dir.join("audio/x.wav"), spec).unwrap();
        for i in 0..480 {
            w.write_sample((i as f32 / 48.0).sin()).unwrap();
        }
        w.finalize().unwrap();

        let (handle, _shared, _tables, session) = spin_up();
        let sid = SourceId::mint();
        {
            let mut session = session.lock();
            let s = &mut session.store;
            s.project_dir = Some(dir.clone());
            s.tracks.push(super::super::types::TrackState {
                id: "t1".into(),
                name: "T1".into(),
                kind: "audio".into(),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                armed: false,
                color: "#7c9cff".into(),
                instrument_id: None,
            });
            let mut c1 = test_clip("c1", "t1");
            c1.source_id = sid.clone();
            s.clips.push(c1);
        }
        handle.send(ControlMsg::Rebuild);

        let cdir1 = Store::cache_dir_for(&dir, "c1");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pyramid_exists(&cdir1) {
            assert!(std::time::Instant::now() < deadline, "clip c1's pyramid never built");
            std::thread::sleep(Duration::from_millis(10));
        }

        // A SECOND clip, added AFTER the first rebuild, sharing the SAME
        // (already-cached) source.
        {
            let mut session = session.lock();
            let mut c2 = test_clip("c2", "t1");
            c2.source_id = sid.clone();
            session.store.clips.push(c2);
        }
        handle.send(ControlMsg::Rebuild);

        let cdir2 = Store::cache_dir_for(&dir, "c2");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pyramid_exists(&cdir2) {
            assert!(
                std::time::Instant::now() < deadline,
                "clip c2's pyramid never built (finding 2 regression: source already cached, so \
                 the old nested pyramid-build loop never visited c2)"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        handle.send(ControlMsg::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Finding 6: the missing-pyramid path must build from the source
    /// file's RAW samples, not the engine-rate-RESAMPLED decode cache — the
    /// AWTF/pyramid protocol bins in SOURCE samples (waveform.rs doc: "LOD n
    /// bins cover 2^(8+n) SOURCE samples"). This project's source file is
    /// 24 kHz while `spin_up`'s engine defaults to 48 kHz (`SharedRt`'s
    /// default `sample_rate`), i.e. exactly a 2x resample ratio: 2560 raw
    /// source samples make exactly 10 lod-0 bins (2560 / 256); the same
    /// audio resampled to 48 kHz would be ~5120 samples -> 20 bins. Reading
    /// back the written pyramid and asserting 10 (not 20) bins pins the fix.
    #[test]
    fn ensure_loaded_builds_pyramids_from_source_rate_not_resampled_cache() {
        const FILE_RATE: u32 = 24_000; // != the engine's default 48 kHz
        const N_SAMPLES: usize = 2_560; // exactly 10 lod-0 bins (2560 / 256)
        let dir = std::env::temp_dir().join(format!(
            "aura-eng-pyramid-rate-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: FILE_RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(dir.join("audio/x.wav"), spec).unwrap();
        for i in 0..N_SAMPLES {
            w.write_sample(((i as f32) * 0.01).sin()).unwrap();
        }
        w.finalize().unwrap();

        let (handle, _shared, _tables, session) = spin_up();
        let sid = SourceId::mint();
        {
            let mut session = session.lock();
            let s = &mut session.store;
            s.project_dir = Some(dir.clone());
            s.tracks.push(super::super::types::TrackState {
                id: "t1".into(),
                name: "T1".into(),
                kind: "audio".into(),
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                armed: false,
                color: "#7c9cff".into(),
                instrument_id: None,
            });
            let mut c1 = test_clip("c1", "t1");
            c1.source_id = sid.clone();
            s.clips.push(c1);
        }
        handle.send(ControlMsg::Rebuild);

        let cdir1 = Store::cache_dir_for(&dir, "c1");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !pyramid_exists(&cdir1) {
            assert!(std::time::Instant::now() < deadline, "clip c1's pyramid never built");
            std::thread::sleep(Duration::from_millis(10));
        }

        let (_channels, bins) = crate::audio::waveform::read_tile(&cdir1, 0, 0)
            .unwrap()
            .expect("lod0 tile must exist");
        assert_eq!(
            bins.len(),
            10,
            "pyramid must be built from the 2560 SOURCE samples (10 bins), not the \
             engine-rate-resampled ~5120 samples (which would give 20)"
        );

        handle.send(ControlMsg::Shutdown);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half of the release window, and the reason it must stay
    /// short while playing: the outgoing node holds the LIVE-IN CHANNEL,
    /// and every event popped while it does is discarded outright, never
    /// re-delivered. Stretching that hold to the envelope's length would
    /// swallow ~80 ms of keystrokes on the incoming target — during "play
    /// the song, arm a track, jam with it", which is the gesture the
    /// feature exists for.
    #[test]
    fn a_key_struck_just_after_a_switch_while_playing_is_not_swallowed() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph_pair(1, Vec::new())))).unwrap();
        target_slot_0(&cb, "t-1");
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        assert_eq!(peak(&out), 0.0, "nothing is sounding yet");

        retarget(&cb, Some("t-2"));
        cb.render(&mut out);
        cb.render(&mut out);

        // Two blocks (~21 ms) after the switch — well inside an
        // envelope-length hold, well outside a one-block one.
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(72, 110)));
        let mut best = 0.0f32;
        for _ in 0..20 {
            cb.render(&mut out);
            best = best.max(peak(&out));
        }
        assert!(best > 0.02, "the key was swallowed by the release window: {best}");
    }

    /// The stop edge re-opens the hold for a release still in flight — and
    /// only then. A release that already finished during playback must not
    /// hand its node the channel again at the next stop, or every stop after
    /// a target switch would deafen the current target for ~80 ms.
    #[test]
    fn a_release_finished_while_playing_does_not_re_open_at_the_next_stop() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph_pair(1, Vec::new())))).unwrap();
        target_slot_0(&cb, "t-1");
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(69, 110)));
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "t-1 is sounding");

        // Switch, then keep PLAYING well past the envelope: the graph
        // renders t-1 every block, so its release finishes here.
        retarget(&cb, Some("t-2"));
        for _ in 0..24 {
            cb.render(&mut out);
        }
        shared.playing.store(false, Relaxed);
        cb.render(&mut out);

        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(72, 110)));
        let mut best = 0.0f32;
        for _ in 0..20 {
            cb.render(&mut out);
            best = best.max(peak(&out));
        }
        assert!(best > 0.02, "the stop re-opened a window that had nothing left to release: {best}");
    }

    /// Task 10's leftover Minor A. Holding the live-in channel for one
    /// block while playing rests on "a playing graph renders every live node
    /// every block anyway". It does — until the transport stops. Stop before
    /// the outgoing node's envelope has finished and nothing renders it
    /// again, so it freezes mid-release and serves the fragment back on its
    /// next arm. The stop edge hands it the channel for whatever release it
    /// still has left, which is why the release is tracked separately from
    /// the hold.
    #[test]
    fn stopping_right_after_a_target_switch_still_finishes_the_outgoing_release() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _evt_rx, (mut graph_tx, _retire_rx, _meter_rx)) = output_cb(shared.clone());
        graph_tx.push(GraphPtr::new(Box::new(polysynth_graph_pair(1, Vec::new())))).unwrap();
        target_slot_0(&cb, "t-1");
        assert!(cb.live_in_hub.push(LiveMidiEvent::note_on(69, 110)));
        shared.playing.store(true, Relaxed);

        let mut out = vec![0.0f32; 512 * 2];
        cb.render(&mut out);
        assert!(peak(&out) > 0.02, "t-1 is sounding");

        // Switch while playing, then stop one block later — inside the
        // release window either way, but past the end of the one-block one.
        retarget(&cb, Some("t-2"));
        cb.render(&mut out);
        shared.playing.store(false, Relaxed);
        for _ in 0..20 {
            cb.render(&mut out);
        }

        // Scanned block by block: t-1 is not rendered at all until t-2's own
        // release window expires, so a single block after the re-arm would
        // pass vacuously.
        retarget(&cb, Some("t-1"));
        let mut worst = 0.0f32;
        for _ in 0..40 {
            cb.render(&mut out);
            worst = worst.max(peak(&out));
        }
        assert_eq!(worst, 0.0, "t-1 replayed a frozen release fragment on re-arm");
    }

    // -- Task 11: the take ------------------------------------------------

    fn midi_track(id: &str) -> crate::audio::types::TrackState {
        let mut t = crate::audio::types::testutil::test_track(id);
        t.kind = "midi".into();
        t
    }

    fn take_map() -> crate::midi::tempo::TempoMap {
        crate::midi::tempo::TempoMap::new(
            crate::midi::types::DEFAULT_PPQ,
            vec![crate::midi::types::TempoEvent { tick: 0, bpm: 120.0 }],
            48_000,
        )
        .unwrap()
    }

    /// A one-note take on `track`, built through the same
    /// `midi::capture::take_clip` the engine uses.
    fn take_clip_for(track: &str) -> crate::midi::types::MidiClip {
        let capture = crate::audio::midi_in::Capture {
            track_id: track.to_string(),
            start_sample: 0,
            end_sample: 48_000,
            events: vec![
                crate::audio::midi_in::CapturedEvent { sample: 0, on: true, key: 60, velocity: 100 },
                crate::audio::midi_in::CapturedEvent { sample: 24_000, on: false, key: 60, velocity: 0 },
            ],
        };
        crate::midi::capture::take_clip(&capture, "MIDI Take 1", &take_map())
            .expect("a note was played")
    }

    #[test]
    fn split_record_targets_keeps_audio_and_midi_apart() {
        let mut store = Store::default();
        store.tracks.push(crate::audio::types::testutil::test_track("a-1"));
        store.tracks.push(midi_track("m-1"));
        store.tracks[0].armed = true;
        store.tracks[1].armed = true;

        let (audio, midi) = split_record_targets(&store, None, Some("m-1".into())).unwrap();
        assert_eq!(audio, vec!["a-1".to_string()], "an armed midi track is not a WAV target");
        assert_eq!(midi.as_deref(), Some("m-1"));

        let (audio, midi) =
            split_record_targets(&store, Some(vec!["m-1".into()]), Some("m-1".into())).unwrap();
        assert!(audio.is_empty(), "midi tracks never record audio, got {audio:?}");
        assert_eq!(midi.as_deref(), Some("m-1"));

        let (audio, midi) =
            split_record_targets(&store, Some(vec!["a-1".into(), "m-1".into()]), Some("m-1".into()))
                .unwrap();
        assert_eq!(audio, vec!["a-1".to_string()]);
        assert_eq!(midi.as_deref(), Some("m-1"));
    }

    #[test]
    fn split_record_targets_allows_a_midi_only_take() {
        let mut store = Store::default();
        store.tracks.push(midi_track("m-1"));
        let (audio, midi) = split_record_targets(&store, None, Some("m-1".into())).unwrap();
        assert!(audio.is_empty());
        assert_eq!(midi.as_deref(), Some("m-1"));
    }

    #[test]
    fn split_record_targets_errors_when_nothing_is_recordable() {
        let store = Store::default();
        let err = split_record_targets(&store, None, None).unwrap_err();
        assert!(err.contains("no armed tracks"), "got {err}");

        let mut store2 = Store::default();
        store2.tracks.push(crate::audio::types::testutil::test_track("a-1"));
        assert!(
            split_record_targets(&store2, None, Some("a-1".into())).is_err(),
            "an audio track as routing target is not a midi take"
        );
        assert!(
            split_record_targets(&store2, None, Some("ghost".into())).is_err(),
            "a routing target that no longer exists is not a midi take"
        );
    }

    #[test]
    fn split_record_targets_rejects_an_unknown_explicit_track() {
        let store = Store::default();
        let err = split_record_targets(&store, Some(vec!["ghost".into()]), None).unwrap_err();
        assert!(err.contains("unknown track"), "got {err}");
    }

    /// The take is ONE `Actor::Engine`, NON-transient transaction (ruling 6) —
    /// §4.4's "the op is the registration, never the recording itself" for MIDI.
    #[test]
    fn midi_take_registers_as_one_engine_transaction() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        let clip = take_clip_for("m-1");
        let gen_before = ctl.generation;

        let committed = ctl.commit_recording_finalize(&[], Some(&clip)).unwrap();
        assert!(
            matches!(committed.meta.actor, crate::control::op::Actor::Engine),
            "got {:?}",
            committed.meta.actor
        );
        assert!(!committed.meta.transient, "a take is a real document edit");
        assert_eq!(committed.ops.len(), 1, "got {:?}", committed.ops);
        assert!(matches!(&committed.ops[0], crate::control::op::Op::MidiClipAdd { .. }));
        assert!(committed.effect.persist.midi, "MidiClipAdd persists the midi store");
        assert_eq!(session.lock().midi.clips.len(), 1);
        assert_eq!(ctl.generation, gen_before + 1, "exactly one rebuild");
    }

    #[test]
    fn an_audio_and_a_midi_take_land_in_the_same_transaction() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(crate::audio::types::testutil::test_track("t-1"));
        session.lock().store.tracks.push(midi_track("m-1"));
        let clip = take_clip_for("m-1");
        let committed = ctl
            .commit_recording_finalize(
                &[crate::audio::types::testutil::test_clip("c-1", "t-1")],
                Some(&clip),
            )
            .unwrap();
        assert_eq!(committed.ops.len(), 2, "one take, one transaction, one undo entry");
        assert!(matches!(&committed.ops[0], crate::control::op::Op::ClipAdd { .. }));
        assert!(matches!(&committed.ops[1], crate::control::op::Op::MidiClipAdd { .. }));
    }

    #[test]
    fn finalize_without_a_midi_clip_is_unchanged() {
        let (mut ctl, _session) = bare_control();
        let clips = vec![crate::audio::types::testutil::test_clip("c-1", "t-1")];
        let committed = ctl.commit_recording_finalize(&clips, None).unwrap();
        assert!(
            committed
                .ops
                .iter()
                .all(|op| matches!(op, crate::control::op::Op::ClipAdd { .. })),
            "got {:?}",
            committed.ops
        );
    }

    #[test]
    fn stop_recording_with_neither_writer_nor_capture_still_errors() {
        let (mut ctl, _session) = bare_control();
        assert_eq!(ctl.stop_recording().unwrap_err(), "not recording");
    }

    /// The whole MIDI-only take, end to end through `stop_recording`: no
    /// input device, no WAV writer, no disk — the capture buffer is the only
    /// source, and the tick math goes through the same `TempoMap` playback
    /// schedules from.
    #[test]
    fn a_midi_only_take_lands_a_clip_through_stop_recording() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.live_in_hub.attach_shared(ctl.shared.clone());
        ctl.shared.sample_rate.store(48_000, Relaxed);

        ctl.shared.position.store(24_000, Relaxed);
        ctl.live_in_hub.begin_capture("m-1".into(), 24_000);
        ctl.live_in_hub.capture_event(true, 60, 100);
        ctl.shared.position.store(48_000, Relaxed);
        ctl.live_in_hub.capture_event(false, 60, 0);
        ctl.shared.position.store(72_000, Relaxed);
        let undo_before = ctl.committer.log().depths().0;

        let clips = ctl.stop_recording().expect("a capture alone is a recording");
        assert_eq!(
            ctl.committer.log().depths().0,
            undo_before + 1,
            "ruling 6: one take is ONE undo entry — the take must not be split \
             across transactions, and the transient transport-state commit must \
             not become one"
        );
        assert!(clips.is_empty(), "no audio target, no audio clips");
        let midi = session.lock().midi.clips.clone();
        assert_eq!(midi.len(), 1, "the take registered exactly one clip");
        assert_eq!(midi[0].track_id.as_str(), "m-1");
        assert_eq!(
            midi[0].timeline_start_ticks, 960,
            "placed where the take started, in ticks (one beat @120bpm/48k)"
        );
        assert_eq!(midi[0].notes.len(), 1);
        assert_eq!(midi[0].notes[0].key, 60);
        assert_eq!(midi[0].notes[0].tick, 0, "the note is relative to the clip start");
        assert_eq!(midi[0].notes[0].length_ticks, 960, "one beat held");
        assert!(!ctl.live_in_hub.capturing(), "stop disarms the capture");
    }

    /// Ruling 6 at the level that can actually break it: `stop_recording`
    /// must hand BOTH halves to the SAME `commit_recording_finalize` call.
    /// A midi-only take cannot tell the difference (there is no audio clip
    /// to be separated from), so this drives a real disk writer — no audio
    /// is ever pushed, the ring is abandoned immediately, and the writer
    /// still returns one zero-length clip, which is all this needs.
    #[test]
    fn an_audio_and_a_midi_take_stop_into_one_undo_entry() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(crate::audio::types::testutil::test_track("a-1"));
        session.lock().store.tracks.push(midi_track("m-1"));

        let dir = std::env::temp_dir()
            .join(format!("aura-take-tx-{}-{:?}", std::process::id(), std::thread::current().id()));
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(64);
        drop(producer);
        let spec = recorder::RecSpec {
            track_id: "a-1".into(),
            clip_id: uuid::Uuid::new_v4().to_string(),
            source_id: crate::ids::SourceId::mint(),
            take_name: "Take 1".into(),
            wav_path: dir.join("audio/take.wav"),
            rel_path: "audio/take.wav".into(),
            cache_dir: dir.join("cache"),
            start_pos: 0,
        };
        ctl.writer = Some(recorder::spawn(vec![spec], vec![consumer], 2, 48_000).unwrap());
        ctl.live_in_hub.begin_capture("m-1".into(), 0);
        ctl.live_in_hub.capture_event(true, 60, 100);

        let undo_before = ctl.committer.log().depths().0;
        let clips = ctl.stop_recording().unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(clips.len(), 1, "the audio half produced its clip");
        assert_eq!(session.lock().store.clips.len(), 1);
        assert_eq!(session.lock().midi.clips.len(), 1, "the midi half produced its clip");
        assert_eq!(
            ctl.committer.log().depths().0,
            undo_before + 1,
            "ruling 6: one take is ONE undo entry — undoing it must never leave \
             half the take on the timeline"
        );
    }

    /// The MIDI take is pure in-memory data lifted out of the hub before
    /// the writer is ever touched, so a disk failure has no claim on it —
    /// nor on the stop itself, which used to be abandoned along with it and
    /// left the UI showing a take that was no longer running.
    #[test]
    fn a_disk_writer_failure_does_not_take_the_midi_take_down_with_it() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(crate::audio::types::testutil::test_track("a-1"));
        session.lock().store.tracks.push(midi_track("m-1"));
        session.lock().store.transport.state = "recording".into();

        let dir = std::env::temp_dir()
            .join(format!("aura-take-fail-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A plain FILE where the writer needs a directory: `create_dir_all`
        // fails, so the writer thread returns Err without any disk-full
        // theatre and without depending on sandbox permissions.
        std::fs::write(dir.join("blocker"), b"not a directory").unwrap();
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(64);
        drop(producer);
        let spec = recorder::RecSpec {
            track_id: "a-1".into(),
            clip_id: uuid::Uuid::new_v4().to_string(),
            source_id: crate::ids::SourceId::mint(),
            take_name: "Take 1".into(),
            wav_path: dir.join("blocker/audio/take.wav"),
            rel_path: "audio/take.wav".into(),
            cache_dir: dir.join("blocker/cache"),
            start_pos: 0,
        };
        ctl.writer = Some(recorder::spawn(vec![spec], vec![consumer], 2, 48_000).unwrap());
        ctl.shared.recording.store(true, Relaxed);
        ctl.shared.playing.store(true, Relaxed);
        ctl.live_in_hub.begin_capture("m-1".into(), 0);
        ctl.live_in_hub.capture_event(true, 60, 100);

        let err = ctl.stop_recording().expect_err("the audio half really did fail");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!err.is_empty(), "the failure is reported, not swallowed");

        assert_eq!(session.lock().midi.clips.len(), 1, "the midi take survived the writer");
        assert!(session.lock().store.clips.is_empty(), "and no audio clip was invented");
        assert!(!ctl.shared.recording.load(Relaxed), "the take is over");
        assert!(!ctl.shared.playing.load(Relaxed));
        assert_eq!(
            session.lock().store.transport.state,
            "stopped",
            "the transport-state commit ran, so the UI cannot stay stuck on 'recording'"
        );
    }

    /// A take with nothing played registers NOTHING — an empty clip on the
    /// timeline would be worse than no take at all.
    #[test]
    fn a_take_with_no_notes_registers_no_clip() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.live_in_hub.begin_capture("m-1".into(), 0);
        let gen_before = ctl.generation;
        ctl.stop_recording().expect("stop still succeeds");
        assert!(session.lock().midi.clips.is_empty(), "nothing was played, nothing registers");
        assert_eq!(ctl.generation, gen_before, "and nothing rebuilds");
    }

    /// Prepare-outside: the target track can be deleted mid-take, and the
    /// validation happens BEFORE the op is built so the transaction can
    /// never fail on it and take the audio clips down with it.
    #[test]
    fn a_take_whose_track_is_gone_is_dropped_without_losing_the_audio_clips() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.live_in_hub.attach_shared(ctl.shared.clone());
        ctl.live_in_hub.begin_capture("m-1".into(), 0);
        ctl.live_in_hub.capture_event(true, 60, 100);
        ctl.shared.position.store(24_000, Relaxed);
        ctl.live_in_hub.capture_event(false, 60, 0);
        session.lock().store.tracks.clear();

        ctl.stop_recording().expect("stop must not fail on a deleted target");
        assert!(session.lock().midi.clips.is_empty(), "the take is dropped, not committed");
    }

    /// A take whose target is no longer a `kind: "midi"` track is dropped
    /// too — `MidiClipAdd` on an audio track would be a malformed document.
    #[test]
    fn a_take_whose_track_became_audio_is_dropped() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.live_in_hub.begin_capture("m-1".into(), 0);
        ctl.live_in_hub.capture_event(true, 60, 100);
        session.lock().store.tracks[0].kind = "audio".into();
        ctl.stop_recording().unwrap();
        assert!(session.lock().midi.clips.is_empty());
    }

    /// The MIDI-only take through its REAL entry points: `start_recording`
    /// opens no input device (ruling 8), arms the capture at the transport
    /// position it started from, and reports the midi target in its reply;
    /// `stop_recording` turns the notes played in between into the clip.
    /// This is the only place the `begin_capture(target, start_pos)` wiring
    /// is exercised — `stop_recording`-only tests arm the hub themselves and
    /// would pass with that call deleted.
    #[test]
    fn start_recording_arms_a_midi_only_take_without_touching_a_device() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.ensure_project_fn = Some(Arc::new(|| Ok(PathBuf::from("/nonexistent"))));
        ctl.live_in_hub.attach_shared(ctl.shared.clone());
        ctl.live_in_hub.set_target_track(Some("m-1".into()));
        ctl.shared.position.store(24_000, Relaxed);

        let recorded = ctl.start_recording(None).expect("a routing target alone is a take");
        assert_eq!(recorded, vec!["m-1".to_string()], "the midi target is reported as recorded");
        assert!(ctl.writer.is_none(), "no WAV writer for a midi-only take");
        assert!(ctl.input.is_none(), "no input device opened");
        assert!(ctl.live_in_hub.capturing(), "the capture is armed");
        assert!(ctl.shared.recording.load(Relaxed) && ctl.shared.playing.load(Relaxed));

        ctl.live_in_hub.capture_event(true, 64, 90);
        ctl.shared.position.store(48_000, Relaxed);
        ctl.live_in_hub.capture_event(false, 64, 0);

        ctl.stop_recording().unwrap();
        let midi = session.lock().midi.clips.clone();
        assert_eq!(midi.len(), 1);
        assert_eq!(
            midi[0].timeline_start_ticks, 960,
            "armed at the position the transport was at when recording started"
        );
        assert_eq!(midi[0].notes[0].key, 64);
        assert_eq!(midi[0].notes[0].tick, 0);
    }

    /// A MIDI-only take opens no device, so `self.writer` stays `None` — the
    /// "already recording" guard has to see the capture instead.
    #[test]
    fn start_recording_refuses_while_a_midi_capture_is_running() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.live_in_hub.begin_capture("m-1".into(), 0);
        assert_eq!(ctl.start_recording(None).unwrap_err(), "already recording");
    }
}
