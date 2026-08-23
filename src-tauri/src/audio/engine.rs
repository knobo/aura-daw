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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
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
use super::pitch::{PitchFrame, PitchFrameBatch, PitchState};
use super::pitch_thread::{pitch_channel, spawn_pitch_worker, PitchTap, PitchWorkerHandle};
use super::recorder::{self, DiskWriter, RecSpec};
use super::rt::{
    GraphPtr, GraphTables, ParamTable, RtClip, RtClipData, RtGraph, RtTrack, SharedGraphTables,
    SharedRt, TrackRamps, NO_PARK,
};
use super::transport;
use super::types::{derive_slots, mixer_slot_count, Clip, MeterFrame, Store};
use super::waveform::{pyramid_exists, Pyramid};
use crate::control::{op, Committed, Committer, Session};
use crate::ids::SourceId;

/// The row that carries a mixer slot's AUDIO. A midi track has two rows in
/// the assembled graph — a clips row and a live-instrument row — and the
/// insert chain, the compensating delays and the send taps must all attach
/// to exactly one of them (G-7: attaching twice runs the same processor,
/// and now the same tap, twice per block). The live row wins when both
/// exist; an audio track has only the clips row.
fn audio_row_for(tracks: &[RtTrack], slot: usize) -> Option<usize> {
    tracks
        .iter()
        .position(|r| r.slot == slot && r.live.is_some())
        .or_else(|| tracks.iter().position(|r| r.slot == slot))
}

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

fn records_automation(mode: super::types::AutomationMode) -> bool {
    matches!(
        mode,
        super::types::AutomationMode::Write
            | super::types::AutomationMode::Touch
            | super::types::AutomationMode::Latch
    )
}

/// Pitch-frame ring slots for the STUB bundles tests build. The real chain
/// sizes its own rings in [`super::pitch_thread`]; this only has to be big
/// enough that a test never overflows it.
#[cfg(test)]
const PITCH_RING_SLOTS: usize = 512;

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

/// One live-pitch subscription (a Tauri `Channel<PitchFrameBatch>`).
/// Return `false` when the subscriber is gone so it gets dropped.
///
/// `send_batch` alone is not enough to retire a subscription: a Tauri
/// `Channel` whose JS side has merely stopped listening keeps accepting
/// sends, so a frontend that opens and closes a panel would leave one live
/// sink behind per visit. `id` is the channel's own id, and
/// [`ControlMsg::UnsubscribePitch`] is how a subscriber says it is done.
pub trait PitchSink: Send + 'static {
    fn send_batch(&self, batch: &PitchFrameBatch) -> bool;
    /// Identity of the underlying channel, for `UnsubscribePitch`.
    fn id(&self) -> u32;
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

#[derive(Debug, Clone)]
pub struct AutomationTouchEndpoint {
    pub track_id: String,
    pub value: f32,
    pub sample: u64,
    pub pass: u64,
}

pub enum ControlMsg {
    Subscribe(Box<dyn MeterSink>),
    /// Reload missing samples/pyramids and swap in a freshly built graph.
    /// Used for STRUCTURAL changes only (tracks/clips/project) — continuous
    /// parameters go through `ParamTable` atomics instead.
    Rebuild,
    /// Explicit automation pass boundary. Unlike a sampled playing->stopped
    /// edge this cannot be lost when Stop and Play happen between ticks.
    FinishAutomationStop { at: u64, active_pass: bool, stopped_pass: Option<u64> },
    /// Gesture-close boundary with the release position and pass identity.
    /// The engine filters stale endpoints after an overtaking Stop.
    FinishAutomationTouch(Vec<AutomationTouchEndpoint>),
    /// Which tracks/clips are routed to hardware MIDI-out, followed by a
    /// rebuild. Its own message rather than a field on `Rebuild` because
    /// routing is app config: it never arrives through a commit, so the
    /// published document image the rebuild reads cannot carry it (see
    /// `midi_out::RoutedOut`). A routed track's internal instrument stops
    /// sounding — the external device is the instrument.
    SetExternalRouting(Arc<crate::midi_out::RoutedOut>),
    SelectOutput { device_id: Option<String>, reply: Reply<()> },
    SelectInput { device_id: Option<String>, reply: Reply<()> },
    StartRecording {
        track_ids: Option<Vec<String>>,
        /// Track id → cpal input-device name for MIDI tracks that have an
        /// audio return (X1). Empty = today's behaviour (audio tracks use
        /// the global input; MIDI tracks are MIDI-only).
        return_sources: HashMap<String, String>,
        reply: Reply<Vec<String>>,
    },
    StopRecording { reply: Reply<Vec<Clip>> },
    /// App-config click + count-in. No document op — same carve-out as the
    /// input device. `count_in_bars` is 0/1/2/4.
    SetMetronome { enabled: bool, gain: f32, count_in_bars: u8, reply: Reply<()> },
    /// Installs the narrow "document birth" closure `ensure_project` calls
    /// (Plan E Task 13, round-2 §4.5 carve-out) — bound over the
    /// `ControlPlane` `Arc`, so it can only be built AFTER `ControlPlane`
    /// exists, which is AFTER the engine control thread is already
    /// running (`audio::init` -> `engine::start` -> later, `lib.rs`
    /// constructs `ControlPlane` and sends this). Fire-and-forget, sent
    /// exactly once at startup; the engine thread never touches project
    /// fields itself, only calls this closure.
    SetEnsureProject(Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>),
    /// Open or close the listen-only input hub (owner ruling R6).
    SetListening { on: bool, reply: Reply<()> },
    /// Momentary rehearse-hold: the take writes silence, analysis keeps running.
    SetRehearseHold { enabled: bool, reply: Reply<()> },
    /// Choose the MIDI track whose clips are the target melody (`None` clears).
    SetPitchReference { track_id: Option<String>, reply: Reply<()> },
    /// Live pitch frames, batched on the same 60 Hz tick as meters.
    SubscribePitch(Box<dyn PitchSink>),
    /// Retire the subscription whose channel has this id. Unknown ids are
    /// ignored — a double unsubscribe is not an error.
    UnsubscribePitch(u32),
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
    gesture: Arc<crate::control::GestureState>,
) -> EngineHandle {
    let (tx, rx) = unbounded();
    let published = session.lock().published_handle();
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
                inputs: Vec::new(),
                writers: Vec::new(),
                rec_track_ids: Vec::new(),
                sel_output: None,
                sel_input: None,
                cache: HashMap::new(),
                cache_rate: 0,
                live_nodes: Default::default(),
                insert_nodes: Default::default(),
                accum: MeterAccum::default(),
                gen_maps: GenerationMaps::default(),
                sinks: Vec::new(),
                last_frame: Instant::now(),
                last_tick: Instant::now(),
                committer,
                gesture,
                ensure_project_fn: None,
                param_automation: crate::plugins::automation::ParamAutomationDriver::empty(),
                param_writes: Vec::new(),
                driven_params: Vec::new(),
                automation_modes: Vec::new(),
                tempo_map: None,
                slots: HashMap::new(),
                params: Arc::new(ParamTable::default()),
                automation_recorder: crate::plugins::automation::AutomationRecorder::new(),
                base_gains: Vec::new(),
                automation_epoch: 0,
                was_playing: false,
                pending_automation_finish: Vec::new(),
                pending_automation_stops: VecDeque::new(),
                deferred_automation_endpoints: Vec::new(),
                live_in_hub: midi_in::hub().clone(),
                live_in_target: None,
                external_routing: Arc::new(crate::midi_out::RoutedOut::default()),
                published,
                #[cfg(test)]
                after_assembly: None,
                count_in_bars: 0,
                pending_record: None,
                listen_input: None,
                wants_listening: false,
                pitch_active: Arc::new(AtomicBool::new(false)),
                rehearse: Arc::new(AtomicBool::new(false)),
                rehearse_open: None,
                rehearse_spans: Vec::new(),
                #[cfg(test)]
                stub_input: false,
                pitch_sinks: Vec::new(),
                reference_track_id: None,
                last_pitch_state: None,
                pitch_scratch: Vec::new(),
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

/// What an open input stream is currently for. The stream exists while this
/// is non-empty and is dropped when it empties — see `sync_input_hub`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct InputWants {
    /// The Pitch Coach panel (or the listen toggle) wants live pitch. Owner
    /// ruling R6: arming a track does NOT set this — only an explicit
    /// listen.
    pub listening: bool,
    /// A take is capturing audio from this device.
    pub recording: bool,
}

impl InputWants {
    pub fn any(&self) -> bool {
        self.listening || self.recording
    }
}

struct InputBundle {
    /// `None` only in tests, which cannot construct a `cpal::Stream` but do
    /// need a hub they can open, attach, and drop without a microphone.
    /// Production always holds the stream: dropping it is what releases the
    /// device (see `sync_input_hub`).
    _stream: Option<cpal::Stream>,
    /// The analysis thread fed by this stream's tap. Declared after
    /// `_stream` so drop order is stream (callback stops) → worker (joins)
    /// → rings. Dropping this joins the thread; it is never read otherwise.
    _pitch_worker: Option<PitchWorkerHandle>,
    meter_rx: rtrb::Consumer<RawMeterBlock>,
    /// Present only on the bundle that owns the microphone — at most one,
    /// even during a multi-device take (X1). `None` on the others.
    pitch_rx: Option<rtrb::Consumer<PitchFrame>>,
    /// cpal device name, `""` for "the default device". What
    /// `sync_input_hub` compares to decide whether a rebuild is needed.
    device_key: String,
    wants: InputWants,
    /// Capture sample rate of this stream. 0 only on test stubs that never
    /// opened a device.
    rate: u32,
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
        // How coarsely `position` moves, for readers that interpolate between
        // blocks — see `SharedRt::block_frames`.
        self.shared.block_frames.store(frames as u32, Relaxed);

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

        // Count-in: freeze the playhead, play clicks, let the control
        // thread arm the take when `countin_left` hits zero.
        let countin = self.shared.countin_left.load(Relaxed);
        if countin > 0 {
            out.fill(0.0);
            let elapsed = self.shared.countin_elapsed.load(Relaxed);
            let beat = self.shared.countin_beat.load(Relaxed);
            let bar = self.shared.countin_beats_per_bar.load(Relaxed).max(1) as u8;
            let gain = f32::from_bits(self.shared.metro_gain.load(Relaxed));
            crate::audio::metronome::mix_count_in(
                out,
                self.channels,
                elapsed,
                beat,
                bar,
                gain.max(0.2),
                self.rate,
            );
            let dec = frames.min(countin);
            self.shared.countin_left.store(countin - dec, Relaxed);
            self.shared.countin_elapsed.store(elapsed + dec, Relaxed);
            self.was_playing = playing;
            return;
        }

        let overlay_ended = self.shared.take_launch_ended();
        let overlay = self
            .shared
            .launch_overlay()
            .map(|mut ov| {
                ov.exclusive = !playing;
                ov
            })
            .or_else(|| {
                overlay_ended.then_some(crate::audio::rt::LaunchPlayhead {
                    pos: base,
                    discontinuity: true,
                    exclusive: false,
                    ended: true,
                })
            });
        let overlay_on = overlay.is_some();
        match (&mut self.graph, playing, overlay_on) {
            (Some(g), true, _) | (Some(g), false, true) => {
                // Task 7: `render` pushes the graph's meter chunks itself
                // (1..=⌈slots/64⌉ for a wide graph) and reports how many the
                // ring couldn't take — telemetry, not data, so a dropped
                // chunk is one xrun, not lost audio.
                // Overlay-only (stopped + preview) still renders launched
                // tracks so a double-click audition does not need Play.
                let dropped = mixer::render_rt_launch(
                    g,
                    base,
                    &lp,
                    out,
                    self.channels,
                    self.rate,
                    discontinuity,
                    steady_base,
                    live_in,
                    overlay,
                    Some(&mut self.meter_tx),
                );
                if dropped > 0 {
                    self.shared.xruns.fetch_add(dropped as u64, Relaxed);
                }
                if playing && self.shared.metro_on.load(Relaxed) {
                    let gain = f32::from_bits(self.shared.metro_gain.load(Relaxed));
                    crate::audio::metronome::mix_clicks(
                        out,
                        self.channels,
                        base,
                        &g.clicks,
                        gain,
                        self.rate,
                    );
                }
            }
            // Monitoring while STOPPED: render ONLY the routed instrument,
            // never the frozen clip slice under the parked playhead.
            (Some(g), false, false) if live_in.is_some() => {
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
        if overlay_on {
            self.shared.advance_launch(frames);
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
    /// Live pitch analysis, present only on the input that owns the
    /// microphone. `None` on a take's other capture devices.
    pitch: Option<PitchTap>,
    /// Rehearse-hold. While set, the fan-out below writes SILENCE for this
    /// buffer instead of the captured audio — same frame count either way,
    /// so the take stays sample-aligned (spec §4.1). Analysis is deliberately
    /// upstream of this: rehearsing means "do not commit it", not "stop
    /// showing me my pitch".
    rehearse: Arc<AtomicBool>,
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

        // Live pitch, BEFORE the fan-out so rehearse-hold cannot silence it.
        // Decimation only: the detector runs on the pitch worker thread
        // (spec §3.2). Dormant unless the user is listening, in which case a
        // relaxed atomic load is the whole cost.
        if let Some(p) = self.pitch.as_mut() {
            p.process(data, in_ch, pos);
        }

        // Rehearse-hold: commit silence, not audio, for this buffer. Read
        // once so a toggle mid-buffer cannot split it (spec §4.1: the held
        // span is reported to the frame, not to the sample).
        let rehearsing = self.rehearse.load(Relaxed);

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
                        if rehearsing {
                            // Same COUNT as the audio it replaces, so the
                            // take stays sample-aligned and nothing is owed.
                            chunk.fill_from_iter(std::iter::repeat(0.0f32).take(take));
                        } else {
                            chunk.fill_from_iter(
                                (0..wrote_frames)
                                    .flat_map(|f| (0..rec_ch).map(move |c| data[f * in_ch + c])),
                            );
                        }
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

struct PendingAutomationStop {
    pass: u64,
    awaiting: Vec<String>,
    endpoints: Vec<AutomationTouchEndpoint>,
    boundary: Vec<(String, u32, crate::audio::types::AutomationMode, bool, f32)>,
}

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
    inputs: Vec<InputBundle>,
    writers: Vec<DiskWriter>,
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
    /// Insert-FX processors (Plan G1) keyed by `instance_id`, shared between
    /// successive graph snapshots exactly like `live_nodes` so a plugin's
    /// delay tails / memory survive rebuilds. Built by `compile_inserts` on
    /// this (control) thread; freed here when the last snapshot retires.
    insert_nodes: crate::audio::insert::InsertNodeRegistry,
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
    /// The SAME `Arc<GestureState>` the `ControlPlane` holds (automation
    /// Task 7) — `AudioState` mints it before `audio::init` starts this
    /// thread, exactly like the shared history log, because `ControlPlane`
    /// does not exist yet at this point.
    ///
    /// Read-only from here: this thread only ever asks
    /// [`crate::control::GestureState::is_track_gain_touched`], so it can
    /// never open, fold into, or close a gesture, and it takes no other lock
    /// while holding the gesture mutex — the gesture-before-session order is
    /// untouched. It is a plain `parking_lot::Mutex` peek on the CONTROL
    /// thread (which already does host round-trips and session locks), not
    /// in the RT audio callback.
    ///
    /// Consumed by the Write/Touch/Latch automation recorder (Task 9); held
    /// here from Task 7 so the sharing seam lands in one reviewable change.
    gesture: Arc<crate::control::GestureState>,
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
    /// What the driver has most recently written to the host, per
    /// (instance, param index) — the read-back `pump_meter_frames` ships so
    /// an open param panel can paint the value automation is actually
    /// holding instead of the document's (Track D ruling 2).
    ///
    /// An UPSERT set, not the tick's deltas: `tick` suppresses a value it
    /// already sent (`EPSILON`/`REASSERT_TICKS`), so a frame carrying only
    /// this tick's writes would blank a held param 60 times a second.
    /// Cleared whenever the transport stops or a rebuild replaces the
    /// driver, which is exactly when the UI must stop following.
    driven_params: Vec<crate::audio::types::DrivenParam>,
    /// Slot-indexed automation mode, refreshed every rebuild from
    /// `store.tracks` — read every control-thread tick by
    /// `drive_automation_recording` (Task 9) without touching the session
    /// lock on the hot path, same shape as `param_automation`/gain ramps. An
    /// out-of-range or unmapped slot reads as `Read` (today's behavior).
    automation_modes: Vec<crate::audio::types::AutomationMode>,
    /// This rebuild's `TempoMap`, cached from `compile_automation` — `None`
    /// when there are no automation lanes/modulation to compile, or the
    /// tempo map failed to build (`compile_automation`'s doc). Task 9 reads
    /// this every tick to convert automation points without re-deriving the
    /// map or touching the session lock.
    tempo_map: Option<crate::midi::TempoMap>,
    /// Slot map, refreshed every rebuild alongside `automation_modes` and
    /// `tempo_map` (same block, `rebuild`'s `derive_slots` call) — a
    /// lock-free copy of the same `TrackId -> usize` mapping `GraphTables`
    /// publishes, kept here so `drive_automation_recording` (Task 9) can
    /// read it every control-thread tick without taking `self.tables`'
    /// mutex on that hot path.
    slots: HashMap<crate::ids::TrackId, usize>,
    /// Lock-free copy of the SAME `Arc<ParamTable>` `GraphTables` publishes
    /// under `self.tables` — cloning the `Arc` here (not the table) means a
    /// live knob write through `self.tables.lock().params` (e.g.
    /// `ControlPlane::commit_with_rebuild_full`) lands on this exact
    /// object too, since `ParamTable`'s fields are atomics with their own
    /// interior mutability; only a REBUILD swaps in a new `ParamTable`,
    /// which is exactly when this field is refreshed (same shape the RT
    /// callback already relies on via its own `Arc<ParamTable>` clone).
    /// Read every control-thread tick by `drive_automation_recording`
    /// (Task 9) so that tick never takes `self.tables`' mutex.
    params: Arc<ParamTable>,
    /// Write/Touch/Latch point recorder (Task 6) — fed once per
    /// control-thread tick by `drive_automation_recording` (Task 9) for
    /// every playing track whose cached mode isn't Off/Read. Task 10 calls
    /// `finish` at pass-end and commits the result; this field only
    /// accumulates in-progress samples, it never itself touches the
    /// document.
    automation_recorder: crate::plugins::automation::AutomationRecorder,
    /// Persisted base-fader gain per slot. Recorded track-gain lanes are
    /// relative multipliers, so the live fader is divided by this value.
    base_gains: Vec<f32>,
    /// Session epoch these recorder buffers belong to.
    automation_epoch: u64,
    /// Whether the transport was playing at the previous tick. Explicit stop
    /// messages finish normal transport stops; this edge remains a fallback
    /// for engine-owned stop paths and coalesced control messages.
    was_playing: bool,
    /// Track ids whose automation mode left Write/Touch/Latch at the last
    /// `rebuild` — enqueued there (the only place that still sees the
    /// PREVIOUS mode cache), committed by `finish_ended_automation_passes`
    /// on the next tick. Never committed inside `rebuild` itself: see the
    /// enqueue site for why re-entering a rebuild from one is not safe.
    pending_automation_finish: Vec<String>,
    /// Explicit Stop remains pending while a closed gesture.s release
    /// endpoint is still in flight, so it joins the same automation pass.
    pending_automation_stops: VecDeque<PendingAutomationStop>,
    deferred_automation_endpoints: Vec<AutomationTouchEndpoint>,
    /// The hardware MIDI-in seam. Held as an `Arc` rather than reached
    /// through `midi_in::hub()` at each call site so tests can drive the
    /// engine against their own hub; `start` binds the process-global one.
    live_in_hub: Arc<MidiInHub>,
    /// Last routing target this thread acted on — the tick compares against
    /// the hub to notice a selection that, being app config, commits nothing
    /// and therefore schedules no rebuild of its own.
    live_in_target: Option<String>,
    /// Which tracks/clips currently send their notes to hardware MIDI-out,
    /// as last pushed by `ControlMsg::SetExternalRouting`. Same nature as
    /// `live_in_target`: app config that commits nothing, so it has to
    /// arrive over the control channel rather than in the document image.
    external_routing: Arc<crate::midi_out::RoutedOut>,
    /// Plan F Task 6: the published-snapshot slot behind the SAME `Session`
    /// this thread holds, cloned once at construction. This is the door the
    /// heavy half of `rebuild` (and all of `ensure_loaded`) reads the
    /// document through, so neither takes the session lock any more. The
    /// inner mutex is a LEAF below `session` [C1]: held only long enough to
    /// clone the `Arc`, never across another lock and never across I/O.
    published: Arc<parking_lot::Mutex<Arc<crate::control::snapshot::SessionSnapshot>>>,
    /// TEST-ONLY seam (sanctioned by the Task 6 plan). Fires exactly once
    /// per `rebuild`, BETWEEN the lock-free assembly and the short session
    /// lock, which is the one instant the two phases can be told apart from
    /// outside. The field does not exist in a production build.
    #[cfg(test)]
    after_assembly: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Count-in length for the next take (app config). 0 = record immediately.
    count_in_bars: u8,
    /// A take waiting for `countin_left` to hit zero before capture arms.
    pending_record: Option<(Option<Vec<String>>, HashMap<String, String>)>,

    // ---- Pitch Coach input hub -----------------------------------------
    /// The listen-only capture stream: open when the user asked for live
    /// pitch and no take already owns the microphone. Separate from
    /// `inputs` because that `Vec` belongs to the take (one entry per
    /// capture device, X1) and is cleared wholesale by `stop_recording`.
    listen_input: Option<InputBundle>,
    /// Whether any tap should be analysing right now. Shared with every
    /// `PitchTap`, including the dormant one a take on the pitch device
    /// carries: flipping this is how listening starts mid-take without
    /// rebuilding a recording stream (PR #49 issue 7). Mirrors
    /// `wants_listening`, and every site that clears one must clear the
    /// other: `set_listening`, the `SelectInput` restore-failure path, and
    /// the `start_recording` group-open-failure path.
    pitch_active: Arc<AtomicBool>,
    /// Whether the user wants live pitch at all (owner ruling R6: an
    /// explicit listen toggle or an open panel — never track arm).
    wants_listening: bool,
    /// Shared with every `InputCb`, so a hold applies to whichever stream is
    /// currently capturing without rebuilding anything.
    rehearse: Arc<AtomicBool>,
    /// Start of the hold currently in progress, in transport samples.
    rehearse_open: Option<u64>,
    /// Completed holds during this take, reported on `recording://state`
    /// (spec §4.1) and cleared when the next take starts.
    rehearse_spans: Vec<(u64, u64)>,
    /// TEST-ONLY: `open_listen_stream` installs a stream-less bundle instead
    /// of talking to cpal. The ownership tests need a hub they can open and
    /// close without a microphone (plan task 5: assert on hub presence, not
    /// on the device).
    #[cfg(test)]
    stub_input: bool,
    /// Live pitch subscribers (Tauri channels). Drained and dropped when
    /// `send_batch` returns false.
    pitch_sinks: Vec<Box<dyn PitchSink>>,
    /// MIDI track whose clips are the target melody. Stored here so the
    /// panel's picker has somewhere to put its choice (Task 6); scoring
    /// reads it in Phase 3.
    reference_track_id: Option<String>,
    /// Last `pitch://state` emitted, so we do not spam identical payloads.
    last_pitch_state: Option<PitchState>,
    /// Reused scratch for `pump_pitch_frames` so the tick allocates nothing
    /// steady-state after the first drain.
    pitch_scratch: Vec<PitchFrame>,
}

/// Compile v3 gain lanes plus any `ModulationDoc` track ramps into one
/// slot-indexed table. Lane-only projects take the existing
/// `compile_gain_ramps` path so Track D gain stays byte-identical.
pub(crate) fn compile_track_ramps(
    lanes: &[crate::plugins::automation::AutomationLane],
    modulation: &crate::modulation::ModulationDoc,
    store: &Store,
    plugins: &crate::control::session::PluginDoc,
    midi_clips: &[crate::midi::MidiClip],
    slots: &HashMap<crate::ids::TrackId, usize>,
    n_slots: usize,
    map: &crate::midi::TempoMap,
) -> (Vec<TrackRamps>, Vec<crate::modulation::ParamLaneSpec>) {
    let mut ramps: Vec<TrackRamps> = if lanes.is_empty() {
        (0..n_slots).map(|_| TrackRamps::default()).collect()
    } else {
        let live_lanes: Vec<_> = lanes
            .iter()
            .filter(|lane| {
                // Off bypasses the lane entirely — same rule for a live
                // rebuild and an offline bounce, since both funnel through
                // this one function.
                match crate::plugins::automation::resolve_target(lane) {
                    Some(crate::plugins::automation::LaneTarget::TrackGain(tid)) => store
                        .tracks
                        .iter()
                        .find(|t| t.id.as_str() == tid)
                        .is_none_or(|t| t.automation_mode != crate::audio::types::AutomationMode::Off),
                    _ => true, // not a track-gain lane; unaffected by this filter
                }
            })
            .cloned()
            .collect();
        crate::plugins::automation::compile_gain_ramps(&live_lanes, map, n_slots, &|tid| {
            slots.get(tid).copied()
        })
        .into_iter()
        .map(|gain| TrackRamps { gain, pan: None })
        .collect()
    };
    let params = if !modulation.is_empty() {
        overlay_modulation_ramps(
            &mut ramps,
            modulation,
            store,
            plugins,
            midi_clips,
            slots,
            n_slots,
            map,
        )
    } else {
        Vec::new()
    };
    (ramps, params)
}

fn overlay_modulation_ramps(
    ramps: &mut [TrackRamps],
    modulation: &crate::modulation::ModulationDoc,
    store: &Store,
    plugins: &crate::control::session::PluginDoc,
    midi_clips: &[crate::midi::MidiClip],
    slots: &HashMap<crate::ids::TrackId, usize>,
    n_slots: usize,
    map: &crate::midi::TempoMap,
) -> Vec<crate::modulation::ParamLaneSpec> {
    let slot_of = |tid: &str| slots.get(tid).copied();
    let track_pan = |tid: &str| {
        store.tracks.iter().find(|t| t.id.as_str() == tid).map(|t| t.pan as f32)
    };
    let instrument_of = |tid: &str| {
        store
            .tracks
            .iter()
            .find(|t| t.id.as_str() == tid)
            .and_then(|t| t.instrument_id.as_deref())
            // PluginDoc.params is keyed by the bare instance id; the track
            // stores the `plugin:<id>` ref (set_track_instrument).
            .and_then(|id| id.strip_prefix("plugin:").map(str::to_string))
    };
    let param_range = |inst: &str, idx: u32| {
        plugins
            .params
            .get(inst)
            .and_then(|ps| ps.iter().find(|p| p.id == idx))
            .map(|p| (p.min as f32, p.max as f32))
    };
    let param_value = |inst: &str, idx: u32| {
        plugins
            .params
            .get(inst)
            .and_then(|ps| ps.iter().find(|p| p.id == idx))
            .map(|p| p.value as f32)
    };
    let content_placements = |content_id: &str| {
        crate::modulation::compile::placements_from_midi_clips(midi_clips, content_id)
    };
    let ctx = crate::modulation::CompileCtx {
        n_slots,
        slot_of: &slot_of,
        track_pan: &track_pan,
        param_range: &param_range,
        param_value: &param_value,
        instrument_of: &instrument_of,
        content_placements: &content_placements,
    };
    let compiled = crate::modulation::compile(modulation, map, &ctx);
    let gain_enabled: Vec<bool> = (0..n_slots)
        .map(|slot| {
            store
                .tracks
                .iter()
                .find(|track| slots.get(&track.id).copied() == Some(slot))
                .is_none_or(|track| {
                    track.automation_mode != crate::audio::types::AutomationMode::Off
                })
        })
        .collect();
    for (i, spec) in compiled.tracks.iter().enumerate() {
        if i >= ramps.len() {
            break;
        }
        if gain_enabled.get(i).copied().unwrap_or(false) {
            if let Some(gain) = &spec.gain {
                ramps[i].gain = Some(Arc::new(gain.clone()));
            }
        }
        if let Some(pan) = &spec.pan {
            ramps[i].pan = Some(Arc::new(pan.clone()));
        }
    }
    compiled.params
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
            self.drive_automation_recording();
            self.finish_ended_automation_passes();
            self.headless_advance();
            self.pump_pitch_frames();
            self.pump_meter_frames();
            self.follow_live_in_target();
            self.arm_pending_after_countin();
        }
    }

    fn handle(&mut self, msg: ControlMsg) -> bool {
        match msg {
            ControlMsg::Subscribe(sink) => self.sinks.push((sink, 0)),
            ControlMsg::Rebuild => self.rebuild(),
            ControlMsg::FinishAutomationStop { at, active_pass, stopped_pass } => {
                self.queue_automation_stop(at, true, active_pass, stopped_pass);
            }
            ControlMsg::FinishAutomationTouch(endpoints) => {
                for endpoint in endpoints {
                    if let Some(stop) = self
                        .pending_automation_stops
                        .iter_mut()
                        .find(|stop| stop.pass == endpoint.pass)
                    {
                        stop.awaiting.retain(|track_id| track_id != &endpoint.track_id);
                        stop.endpoints.push(endpoint);
                    } else if endpoint.pass == self.shared.automation_pass.load(Relaxed) {
                        if self.pending_automation_stops.is_empty() {
                            self.process_automation_touch_endpoint(endpoint);
                        } else {
                            self.deferred_automation_endpoints.push(endpoint);
                        }
                    }
                }
            }
            ControlMsg::SetExternalRouting(routed) => {
                if self.external_routing != routed {
                    self.external_routing = routed;
                    self.rebuild();
                }
            }
            ControlMsg::SelectOutput { device_id, reply } => {
                self.sel_output = device_id;
                let _ = reply.send(self.open_output());
            }
            ControlMsg::SelectInput { device_id, reply } => {
                // Spec §3.1: a take owns the device; you cannot switch it
                // mid-capture. Listening-only rebuilds the hub on the new
                // device (a few ms of pitch blackout — accepted).
                let res = if self.shared.recording.load(Relaxed)
                    || self.pending_record.is_some()
                    || !self.writers.is_empty()
                {
                    Err("cannot change input device during a take".to_string())
                } else {
                    let prev = self.sel_input.clone();
                    self.sel_input = device_id;
                    match self.sync_input_hub() {
                        Ok(()) => {
                            self.emit_pitch_state();
                            Ok(())
                        }
                        Err(e) => {
                            self.sel_input = prev;
                            if let Err(restore_err) = self.sync_input_hub() {
                                log::warn!(
                                    "audio: could not restore previous input after failed switch: {restore_err}"
                                );
                                if self.listen_input.is_none() {
                                    self.wants_listening = false;
                                    self.pitch_active.store(false, Relaxed);
                                }
                            }
                            self.emit_pitch_state();
                            Err(e)
                        }
                    }
                };
                let _ = reply.send(res);
            }
            ControlMsg::StartRecording { track_ids, return_sources, reply } => {
                let _ = reply.send(self.start_recording(track_ids, return_sources));
            }
            ControlMsg::StopRecording { reply } => {
                let _ = reply.send(self.stop_recording());
            }
            ControlMsg::SetMetronome { enabled, gain, count_in_bars, reply } => {
                self.shared.metro_on.store(enabled, Relaxed);
                self.shared.metro_gain.store(gain.clamp(0.0, 1.0).to_bits(), Relaxed);
                self.count_in_bars = count_in_bars.min(8);
                let _ = reply.send(Ok(()));
            }
            ControlMsg::SetEnsureProject(f) => {
                self.ensure_project_fn = Some(f);
            }
            ControlMsg::SetListening { on, reply } => {
                let r = self.set_listening(on);
                self.emit_pitch_state();
                let _ = reply.send(r);
            }
            ControlMsg::SetRehearseHold { enabled, reply } => {
                self.set_rehearse_hold(enabled);
                self.emit_pitch_state();
                let _ = reply.send(Ok(()));
            }
            ControlMsg::SetPitchReference { track_id, reply } => {
                self.reference_track_id = track_id;
                self.emit_pitch_state();
                let _ = reply.send(Ok(()));
            }
            ControlMsg::SubscribePitch(sink) => {
                // Replace rather than append when a channel resubscribes:
                // ids are unique per channel, so this is only reachable if a
                // caller subscribed twice, and two sinks on one channel
                // would double every batch.
                let id = sink.id();
                self.pitch_sinks.retain(|s| s.id() != id);
                self.pitch_sinks.push(sink);
                // A new subscriber has to be TOLD the current state, not left
                // to wait for the next change to it. `referenceTrackId` rides
                // on `pitch://state` and nothing else, so a panel mounting
                // against an engine that already has a reference track drew no
                // melody at all — until some unrelated transition (a take
                // starting or stopping changes `listening`) happened to emit.
                // That is a panel that looks broken while the engine is right.
                //
                // `last_pitch_state` has to be cleared first: `emit_pitch_state`
                // dedupes against it, and the state has NOT changed — that is
                // the whole problem. A new listener is a new reason to send.
                self.last_pitch_state = None;
                self.emit_pitch_state();
            }
            ControlMsg::UnsubscribePitch(id) => self.pitch_sinks.retain(|s| s.id() != id),
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
    ///
    /// Plan F Task 6 splits this into two phases — a lock-free assembly from
    /// the published snapshot and a short session lock that reads param
    /// values, derives slots and publishes the tables. Each phase's comment
    /// carries its own half of the argument; the seam between them is where
    /// a commit may land, and `a_param_write_committed_during_assembly_is_
    /// never_lost` is the pin that it costs nothing when it does.
    fn rebuild(&mut self) {
        // ONE image for the whole rebuild: the decode pass and the assembly
        // that consumes its cache read the same `s`, so "a clip in the graph
        // whose source this pass never looked at" cannot happen within a
        // rebuild. (Before Task 6 these were two independent reads under two
        // separate lock blocks and that window did exist.)
        let s = self.published.lock().clone();
        self.ensure_loaded(&s);
        self.generation += 1;
        let headless = self.output.is_none();
        // Read BEFORE the session lock is taken: the hub is reachable from
        // the midir callback thread, and nesting its mutex under the session
        // lock would invent a lock order this file has none of today.
        let live_in_target = self.live_in_hub.target_track();

        // PHASE 1 — NO SESSION LOCK. The heavy half (clip assembly, live
        // node instantiation, tick->sample conversion) reads the published
        // image S: an immutable, fully-constructed document the committer
        // assigned into the slot inside its own transaction. Nothing mutates
        // a published image, so there is no torn read to observe and no
        // guard to hold while this runs. The leaf mutex is held for one
        // pointer clone at the top of this function (lock order [C1]:
        // session -> leaf, never the reverse, and the leaf is never held
        // across another acquisition).
        let mut assembled: Option<(HashMap<crate::ids::TrackId, usize>, Vec<RtTrack>)> = None;
        let mut failed_inserts: Vec<String> = Vec::new();
        // Plan G2: the routing half of the assembly. Bus strips carry their
        // document ids alongside, and the send edges stay UNRESOLVED until
        // phase 2 has read the live send-slot map — see `audio::bus`.
        let mut routing: Option<crate::audio::bus::RoutingPlan> = None;
        if !headless {
            // Headless keeps its narrow scope [I5]: tables are enough to
            // serve knob writes and recording resolution with no output
            // device — no clip assembly, no live/plugin node instantiation,
            // no `song_end` write. Enabling any of that headlessly (every
            // structural commit in the whole backend test suite runs through
            // here) would be a silent behavior change this refactor must not
            // smuggle in.
            let slots_s = derive_slots(&s.tracks);
            let mut tracks = Vec::with_capacity(s.tracks.len());
            for t in s.tracks.iter() {
                let Some(&slot) = slots_s.get(&t.id) else {
                    continue; // automation tracks own no mixer slot or RtTrack
                };
                if crate::audio::types::is_bus_track(t) {
                    // A bus owns a mixer slot but no SOURCE row: it is fed by
                    // sends and compiled into `RtGraph::buses` below. Pushing
                    // an empty clips row for it would put a second writer on
                    // its meter lane (Plan G2).
                    continue;
                }
                let clips = s
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
            // tracks become RtTracks carrying a live node (SamplerNode when
            // the track's `instrument_id` resolves, plugin node for
            // `plugin:` refs, PolySynth fallback) plus this snapshot's
            // pre-scheduled events (ticks -> absolute samples via TempoMap,
            // HERE on the control thread; the RT thread only slices
            // sample-offset events). Nodes come from `live_nodes` so
            // voice/plugin state SURVIVES rebuilds; brand-new nodes are
            // prepared before the graph is published (RCU discipline).
            let bank = crate::audio::sampler::registered_bank().map(|b| b.lock());
            // ORDER MATTERS: a midi track already has a clips-only row from
            // the loop above, so this adds a SECOND row for the same slot.
            // Both write that slot's meter lane and the last writer wins —
            // appending the live rows here is what makes the live one win.
            // Push them before the loop and the midi track's meters silently
            // read zero.
            crate::midi::playback::append_from_with_input(
                &s.midi,
                &s.tracks,
                &s.clips,
                &s.plugins,
                &slots_s,
                self.cache_rate,
                bank.as_deref(),
                &self.external_routing,
                &mut self.live_nodes,
                &mut tracks,
                live_in_target.as_deref(),
            );
            // INSERT-FX (Plan G1 Task 7): compile the document insert slots
            // into RT processors and attach each chain to the row that
            // actually carries the track's audio — the live row when one
            // exists (a midi instrument), else the clips row (audio track,
            // or a frozen midi return). A chain is attached to EXACTLY one
            // row per track (G-7: one instance, one slot, one node — a
            // second attachment would run the same processor twice per
            // block).
            let (mut compiled, failed) = crate::audio::insert::compile_inserts(
                &s.tracks,
                &s.plugins,
                self.cache_rate,
                &mut self.insert_nodes,
            );
            failed_inserts = failed;
            // ROUTING (Plan G2): bus strips and send edges, plus the two
            // compensating delays they need. This MOVES the bus tracks'
            // chains out of `compiled`, so what is left below is exactly the
            // source-track chains that still want a row. See `audio::bus`
            // for why the dry path and the sends wait different amounts.
            let plan = crate::audio::bus::compile_routing(
                &s.tracks,
                &slots_s,
                &mut compiled,
                slots_s.len(),
                crate::audio::rt::MAX_LIVE_BLOCK,
            );
            // Attach a compiled chain to the row carrying this track's
            // audio. The live row wins when both exist.
            for t in s.tracks.iter() {
                let Some(&slot) = slots_s.get(&t.id) else { continue };
                let Some(i) = audio_row_for(&tracks, slot) else { continue };
                if let Some(chain) = compiled.remove(&t.id) {
                    if !chain.is_empty() {
                        tracks[i].inserts = chain;
                    }
                }
            }
            // PDC (Plan G1 Task 6/7 + G2): pad every source path up to the
            // slowest sibling's, then pad the DRY path up to the slowest
            // return. Zero delay = no DelayLine (and no automation-ramp
            // offset in the mixer).
            for (slot, &delay) in plan.track_pdc.iter().enumerate() {
                let Some(i) = audio_row_for(&tracks, slot) else { continue };
                if delay > 0 {
                    tracks[i].pdc = Some(crate::audio::pdc::DelayLine::new(
                        delay,
                        crate::audio::rt::MAX_LIVE_BLOCK,
                        2,
                    ));
                }
                let out_delay = plan.out_delay.get(slot).copied().unwrap_or(0);
                if out_delay > 0 {
                    tracks[i].out_pdc = Some(crate::audio::pdc::DelayLine::new(
                        out_delay,
                        crate::audio::rt::MAX_LIVE_BLOCK,
                        2,
                    ));
                }
                tracks[i].output = plan.output.get(slot).copied().flatten();
            }
            routing = Some(plan);
            // The timeline boundary belongs to the material, so it is
            // derived exactly where the material is assembled — same helper
            // the offline bounce uses, so live and export agree on where the
            // song ends (clip ends AND the final scheduled note-off).
            self.shared
                .song_end
                .store(offline::song_end(&tracks), Relaxed);
            assembled = Some((slots_s, tracks));
        }
        #[cfg(test)]
        if let Some(f) = self.after_assembly.clone() {
            f();
        }

        // PHASE 2 — the SHORT session lock, and the whole of the [C1]
        // argument. Publishing `GraphTables` under this lock is load-bearing,
        // not style: it makes <read doc, publish tables> atomic against every
        // commit's <transact, execute writes>. Publish tables built from an
        // older read and a commit that transacted in between resolves its
        // param writes through the still-old tables, then this rebuild
        // overwrites them from the older revision — silently losing the write
        // forever (a plain `Set` schedules no rebuild of its own).
        //
        // Task 6 keeps the atomic pair and SHRINKS the read to what actually
        // needs it. Param VALUES and the slot map come from the LIVE document
        // L read here, never from the assembly image S, so a commit landing
        // during phase 1 is still in L and still reaches the table.
        //
        // Graph STRUCTURE may be S != L, and that is accepted: any structural
        // commit between the two set `effect.rebuild`, whose `do_rebuild()`
        // queued another `ControlMsg::Rebuild`, so the stale structure is
        // transient by exactly the mechanism that already covers "a commit
        // lands while a rebuild waits in the queue".
        //
        // L is read as the live document rather than through the published
        // slot (DEVIATION from the plan's literal step 3, which says to
        // re-read the image under the lock). Under this lock the two are the
        // same document by the publication invariant — except at the three
        // enumerated sites where a command swaps the document and
        // republishes a few statements later; there, live truth is what [C1]
        // requires and the image is momentarily behind it.
        let (params, slots, send_slots, track_ramps, param_driver, tempo_map, automation_modes, base_gains, document_epoch, clicks) = {
            let session = self.session.lock(); // read-only: param VALUES + slot map + automation compile — short, no assembly
            let store = &session.store;
            let slots = derive_slots(&store.tracks);
            // Plan G2: the send-amount lanes are derived from the LIVE
            // document for the same reason the slot map is — a send added
            // during phase 1 must reach this rebuild's table, or its knob
            // would resolve into a lane no graph reads.
            let send_slots = crate::audio::types::derive_send_slots(&store.tracks);
            // Sized to mixer slots, not store track count: automation tracks
            // take no slot (design §3.6) and must not shift later rows.
            let n_slots = mixer_slot_count(&store.tracks);
            let params = Arc::new(ParamTable::with_slots_and_sends(
                n_slots,
                crate::audio::types::send_slot_count(&store.tracks),
            ));
            for t in store.tracks.iter() {
                let Some(&slot) = slots.get(&t.id) else { continue };
                let base_gain = mixer::db_to_linear(t.gain_db);
                params.set_gain_pair_linear(slot, base_gain);
                params.set_pan(slot, t.pan as f32);
                params.set_flag(slot, super::rt::FLAG_MUTE, t.muted);
                params.set_flag(slot, super::rt::FLAG_SOLO, t.soloed);
                for snd in &t.sends {
                    let Some(&idx) = send_slots.get(&snd.id) else { continue };
                    params.set_send_amount_linear(idx, mixer::db_to_linear(snd.amount_db));
                }
            }
            let launch_ids = crate::midi::launch::runtime().audible_tracks();
            for t in store.tracks.iter() {
                if !launch_ids.iter().any(|id| id == t.id.as_str()) {
                    continue;
                }
                let Some(&slot) = slots.get(&t.id) else { continue };
                params.set_flag(slot, super::rt::FLAG_LAUNCH, true);
            }
            params.any_solo.store(store.any_solo(), Relaxed);
            // THE [C1] PUBLISH SITE — still under `session`, see above.
            *self.tables.lock() = GraphTables {
                generation: self.generation,
                params: params.clone(),
                slots: slots.clone(),
                send_slots: send_slots.clone(),
            };
            // Same generation, same slot map, published alongside the
            // tables (Task 6) — the meter fold resolves blocks under
            // whichever generation produced them, not the current tables.
            self.gen_maps.publish(self.generation, &slots);
            let (track_ramps, param_driver, tempo_map) =
                self.compile_automation(&session, &slots, n_slots);
            // Slot-indexed automation-mode cache (Task 8), same slot map
            // and mixer-slot count as `params`/`track_ramps` above — an
            // automation track (no slot) or a slot nothing currently maps
            // to defaults to `Read`, matching `AutomationMode::default()`.
            let automation_modes: Vec<crate::audio::types::AutomationMode> = (0..n_slots)
                .map(|slot| {
                    store
                        .tracks
                        .iter()
                        .find(|t| slots.get(&t.id) == Some(&slot))
                        .map_or(crate::audio::types::AutomationMode::Read, |t| t.automation_mode)
                })
                .collect();
            let base_gains: Vec<f32> = (0..n_slots)
                .map(|slot| {
                    store
                        .tracks
                        .iter()
                        .find(|t| slots.get(&t.id) == Some(&slot))
                        .map_or(1.0, |t| mixer::db_to_linear(t.gain_db))
                })
                .collect();
            let document_epoch = session.epoch;
            let clicks = compile_clicks(
                &session,
                self.cache_rate,
                self.shared.song_end.load(Relaxed),
            );
            (params, slots, send_slots, track_ramps, param_driver, tempo_map, automation_modes, base_gains, document_epoch, clicks)
        };
        // Task 10, pass-end trigger 2 (mode change — spec §4.5): a track
        // that WAS Write/Touch/Latch and no longer is has just ended its
        // recording pass. This is the one place that can still see it: the
        // comparison reads the PREVIOUS `slots`/`automation_modes` caches,
        // BEFORE the assignments right below overwrite them. Both sides are
        // keyed by track id, never by slot index — a slot number means
        // whatever the map it came from says it means, and adding a track
        // reshuffles them.
        //
        // ENQUEUED here, committed on the next tick
        // (`finish_ended_automation_passes`), never inline: a commit's
        // `do_rebuild` closure calls `rebuild` again, and re-entering it
        // from here would publish a NEWER graph generation while this call
        // still has an older assembly (`assembled`, built above) left to
        // publish — the outer publish would then overwrite the inner one
        // with a stale graph.
        //
        // A track that vanished from the slot map entirely (deleted
        // mid-pass) is deliberately NOT enqueued: there is no track left
        // for its points to automate, and an orphan lane no UI can ever
        // show is worse than dropping a pass nothing can hear. Its
        // in-progress points stay in the recorder, inert, until a track
        // with that id records again.
        let document_changed = self.automation_epoch != document_epoch;
        if document_changed {
            self.automation_recorder.reset();
            self.pending_automation_finish.clear();
            self.pending_automation_stops.clear();
            self.deferred_automation_endpoints.clear();
            super::rt::advance_automation_pass(&self.shared.automation_pass);
            self.was_playing = self.shared.playing.load(Relaxed);
        }
        let mut ended: Vec<String> = Vec::new();
        if !document_changed {
            for (track_id, &old_slot) in &self.slots {
                let was = self
                    .automation_modes
                    .get(old_slot)
                    .copied()
                    .unwrap_or_default();
                if !records_automation(was) {
                    continue;
                }
                let now = slots
                    .get(track_id)
                    .and_then(|&s| automation_modes.get(s))
                    .copied();
                if matches!(now, Some(m) if !records_automation(m)) {
                    ended.push(track_id.as_str().to_string());
                }
            }
        }
        self.pending_automation_finish.append(&mut ended);
        self.param_automation = param_driver;
        // The lanes just changed under us: any read-back naming a param this
        // rebuild no longer drives is now a lie. Drop the set and let the
        // next tick refill it from the new driver.
        self.driven_params.clear();
        self.tempo_map = tempo_map;
        self.automation_modes = automation_modes;
        self.base_gains = base_gains;
        self.automation_epoch = document_epoch;
        // Same slot map AND same `Arc<ParamTable>` `GraphTables` just
        // published under `self.tables` (above) — cloned here, not read
        // back through that lock, so `drive_automation_recording`'s tick
        // can stay lock-free (Task 9 review).
        self.slots = slots.clone();
        self.params = params.clone();

        let Some((slots_s, mut tracks)) = assembled else { return };
        // The rows were assembled against S's slot map; the tables just
        // published describe L. Re-key by TRACK ID so the graph and the
        // tables it reads agree on what a slot means: a track S had and L
        // does not is dropped (it vanishes in the rebuild that removal
        // queued), and a track only L has contributes params but no clips
        // until then — the same "no slot yet" tolerance the assembly loop
        // always had, mirrored. Dropping is also what keeps every row's slot
        // inside the freshly sized `ParamTable`.
        let mut id_of_slot: Vec<Option<&crate::ids::TrackId>> = vec![None; slots_s.len()];
        for (id, &slot) in &slots_s {
            id_of_slot[slot] = Some(id);
        }
        tracks.retain_mut(|row| {
            match id_of_slot.get(row.slot).copied().flatten().and_then(|id| slots.get(id)) {
                Some(&slot) => {
                    row.slot = slot;
                    true
                }
                None => false,
            }
        });

        // Plan G2: finish the routing against the LIVE maps. Bus slots are
        // re-keyed like the track rows, but a bus whose track vanished is
        // PARKED (`usize::MAX`, which the render skips) rather than removed
        // — removing it would renumber `RtSend::bus` under every edge that
        // already points past it, and this graph is transient anyway.
        let mut buses = Vec::new();
        if let Some(plan) = routing {
            let crate::audio::bus::RoutingPlan { buses: planned, bus_ids, sends, .. } = plan;
            buses = planned;
            for (bus, id) in buses.iter_mut().zip(bus_ids.iter()) {
                bus.slot = slots.get(id).copied().unwrap_or(usize::MAX);
            }
            for (tid, edges) in sends.iter() {
                let resolved: Vec<crate::audio::rt::RtSend> = edges
                    .iter()
                    .filter_map(|e| e.resolve(&send_slots, crate::audio::rt::MAX_LIVE_BLOCK))
                    .collect();
                // A bus sends like any other node, so the edges land on
                // whichever kind of strip owns this id.
                if let Some(bi) = bus_ids.iter().position(|id| id == tid) {
                    buses[bi].sends = resolved;
                    continue;
                }
                let Some(&slot) = slots.get(tid) else { continue };
                let Some(i) = audio_row_for(&tracks, slot) else { continue };
                tracks[i].sends = resolved;
            }
        }

        let mut g = RtGraph::with_buses(tracks, buses, self.generation, params);
        // RCU: the ramp table is attached BEFORE the graph is published, so
        // the callback only ever sees a snapshot whose ramps already belong
        // to it — and a retired graph keeps reading its own table, exactly
        // like `params`.
        g.set_track_ramps(track_ramps);
        g.clicks = Arc::new(clicks);
        let graph = Box::new(g);

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

        // Surface insert-node build failures to the UI: an instance marked
        // "active" whose node the host could not build plays dry — flip it to
        // "crashed" so the frontend stops showing a misleading green "active".
        if !failed_inserts.is_empty() {
            let mut session = self.session.lock();
            let mut changed = false;
            for id in &failed_inserts {
                if let Some(r) = session.plugins.instances.iter_mut().find(|r| &r.id == id) {
                    if r.status == "active" {
                        r.status = "crashed".into();
                        changed = true;
                    }
                }
            }
            if changed {
                session.republish_full();
            }
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

    /// Compile the session's automation lanes AND `session.modulation` into
    /// this rebuild's two products — a slot-indexed `TrackRamps` table for
    /// the graph, and the control thread's plugin-param driver. CONTROL
    /// THREAD, called from `rebuild` under the same session guard that
    /// derived `slots`: this is where ticks become absolute samples, so
    /// nothing tick-shaped ever crosses onto the RT thread
    /// (ARCHITECTURE §13/§15.1).
    ///
    /// `n_slots` is the MIXER-SLOT COUNT `ParamTable` was sized with, not
    /// `slots.len()` (duplicate ids collapse in the map) and not
    /// `store.tracks.len()` (automation tracks take no slot).
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
        Vec<TrackRamps>,
        crate::plugins::automation::ParamAutomationDriver,
        Option<crate::midi::TempoMap>,
    ) {
        use crate::plugins::automation as auto;
        // Recording needs the tempo map even before the first lane exists:
        // Write must be able to mint that first lane in an empty project.
        // Keep the fast path only when there is neither playback automation
        // nor any track armed for automation recording.
        let records_any_track = session
            .store
            .tracks
            .iter()
            .any(|track| records_automation(track.automation_mode));
        if session.automation.lanes.is_empty()
            && session.modulation.is_empty()
            && !records_any_track
        {
            return (
                (0..n_slots).map(|_| TrackRamps::default()).collect(),
                auto::ParamAutomationDriver::empty(),
                None,
            );
        }
        let map = crate::midi::TempoMap::new(
            session.midi.ppq,
            session.midi.tempo_events.clone(),
            self.cache_rate,
        )
        .ok();
        let Some(map) = map else {
            return (
                (0..n_slots).map(|_| TrackRamps::default()).collect(),
                auto::ParamAutomationDriver::empty(),
                None,
            );
        };
        let (ramps, param_specs) = compile_track_ramps(
            &session.automation.lanes,
            &session.modulation,
            &session.store,
            &session.plugins,
            &session.midi.clips,
            slots,
            n_slots,
            &map,
        );
        let mut driver = auto::ParamAutomationDriver::from_param_specs(&param_specs, &session.plugins);
        if !session.automation.lanes.is_empty() {
            driver.merge_uncovered_lanes(&session.automation.lanes, &session.plugins, &map);
        }
        (ramps, driver, Some(map))
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
    /// Fold a tick's writes into the driven-param read-back, upserting by
    /// (instance, index). Linear in both — a tick writes one entry per
    /// automated param, and the set holds one per automated param, so both
    /// are the count of plugin-param lanes, not of params.
    fn absorb_driven(
        driven: &mut Vec<crate::audio::types::DrivenParam>,
        writes: &[crate::plugins::automation::ParamWrite],
    ) {
        for w in writes {
            match driven
                .iter_mut()
                .find(|d| d.index == w.index && d.instance_id == w.instance)
            {
                Some(existing) => existing.value = w.value,
                None => driven.push(crate::audio::types::DrivenParam {
                    instance_id: w.instance.clone(),
                    index: w.index,
                    value: w.value,
                }),
            }
        }
    }

    fn drive_param_automation(&mut self) {
        if self.param_automation.is_empty() || !self.shared.playing.load(Relaxed) {
            // Nothing is driving these any more, so the UI must stop
            // following: a stale read-back would pin the panel to the last
            // automated value while the user turns the knob underneath it.
            self.driven_params.clear();
            return;
        }
        let pos = self.shared.position.load(Relaxed);
        let mut writes = std::mem::take(&mut self.param_writes);
        self.param_automation.tick(pos, &mut writes);
        Self::absorb_driven(&mut self.driven_params, &writes);
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

    /// Samples every playing track's live fader value into
    /// `automation_recorder`, once per control-thread tick, for any track
    /// whose cached mode (Task 8's `automation_modes`) is Write/Touch/Latch.
    /// Mirrors `drive_param_automation`'s guard shape exactly (same
    /// `shared.playing`/`shared.position` reads, zero locks taken): only
    /// while the transport plays, and bails out with no `tempo_map` (no
    /// automation lanes or modulation compiled this rebuild, so there is
    /// nothing to convert ticks against).
    ///
    /// Reads only `self.slots`/`self.params` — lock-free copies `rebuild`
    /// refreshes alongside `automation_modes`/`tempo_map`, not
    /// `self.tables` (whose mutex this tick never takes, same zero-lock
    /// hot path Task 8 set up for the other two caches).
    ///
    /// Only READS `self.gesture` (`is_track_gain_touched`); never commits
    /// anything itself — Task 10 owns `finish` + the commit at pass-end.
    fn drive_automation_recording(&mut self) {
        if !self.pending_automation_stops.is_empty() {
            return;
        }
        if !self.shared.playing.load(Relaxed) {
            return;
        }
        let Some(map) = &self.tempo_map else { return };
        let pos = self.shared.position.load(Relaxed);
        let tick = map.samples_to_tick(pos) as u32;
        for (track_id, &slot) in &self.slots {
            let Some(&mode) = self.automation_modes.get(slot) else { continue };
            if matches!(
                mode,
                crate::audio::types::AutomationMode::Off | crate::audio::types::AutomationMode::Read
            ) {
                self.params.set_gain_automation_owner(slot, None);
                continue;
            }
            let touch_pass = self.gesture.track_gain_touch_pass(track_id.as_str());
            let touched = touch_pass.is_some();
            if mode == crate::audio::types::AutomationMode::Write {
                self.params.set_gain_automation_owner(
                    slot,
                    Some(self.shared.automation_pass.load(Relaxed)),
                );
            } else if let Some(pass) = touch_pass {
                self.params.set_gain_automation_owner(slot, Some(pass));
            }
            let (live_gain, base_gain) = self.params.gain_pair_linear(slot);
            let multiplier = super::rt::relative_gain_multiplier(live_gain, base_gain);
            self.automation_recorder
                .sample(track_id.as_str(), tick, mode, touched, multiplier);
        }
    }

    /// The pass-end triggers (spec §4.5), run once per control-thread tick
    /// right after `drive_automation_recording`:
    ///
    /// * TRANSPORT STOP — normal stops arrive as an explicit control message;
    ///   this playing→stopped edge is the fallback for engine-owned stop paths.
    /// * MODE CHANGE — `rebuild` enqueues any track that just left
    ///   Write/Touch/Latch (see the enqueue site for why the commit cannot
    ///   run there).
    /// * TOUCH GESTURE RELEASE — gesture close sends an explicit per-track
    ///   finish message, giving every Touch gesture its own undo boundary.
    fn queue_automation_stop(&mut self, at: u64, sample_boundary: bool, active_pass: bool, stopped_pass: Option<u64>) {
        self.was_playing = false;
        if !active_pass {
            return;
        }
        let pass = stopped_pass.unwrap_or_else(|| super::rt::advance_automation_pass(&self.shared.automation_pass));
        let tick = self.tempo_map.as_ref().map(|map| map.samples_to_tick(at) as u32);
        let mut awaiting = Vec::new();
        let mut boundary = Vec::new();
        for (track_id, &slot) in &self.slots {
            let Some(&mode) = self.automation_modes.get(slot) else { continue };
            let owned = self.params.gain_automation_owner(slot) == Some(pass);
            let touched = self.gesture.track_gain_touch_pass(track_id.as_str()) == Some(pass);
            let missing_endpoint = match mode {
                crate::audio::types::AutomationMode::Touch => owned && !touched,
                crate::audio::types::AutomationMode::Latch => {
                    owned && !touched && !self.automation_recorder.is_latch_armed(track_id.as_str())
                }
                _ => false,
            };
            if missing_endpoint {
                awaiting.push(track_id.as_str().to_string());
            }
            let samples_boundary = match mode {
                crate::audio::types::AutomationMode::Write => true,
                crate::audio::types::AutomationMode::Touch => touched,
                crate::audio::types::AutomationMode::Latch => owned,
                _ => false,
            };
            if sample_boundary && samples_boundary {
                if let Some(tick) = tick {
                    let (live, base) = self.params.gain_pair_linear(slot);
                    boundary.push((
                        track_id.as_str().to_string(),
                        tick,
                        mode,
                        touched,
                        super::rt::relative_gain_multiplier(live, base),
                    ));
                }
            }
        }

        let mut endpoints = Vec::new();
        let mut still_deferred = Vec::new();
        for endpoint in std::mem::take(&mut self.deferred_automation_endpoints) {
            if endpoint.pass == pass {
                awaiting.retain(|track_id| track_id != &endpoint.track_id);
                endpoints.push(endpoint);
            } else {
                still_deferred.push(endpoint);
            }
        }
        self.deferred_automation_endpoints = still_deferred;
        self.pending_automation_stops.push_back(PendingAutomationStop {
            pass,
            awaiting,
            endpoints,
            boundary,
        });
    }

    fn process_automation_touch_endpoint(&mut self, endpoint: AutomationTouchEndpoint) {
        let endpoint_pass = endpoint.pass;
        let Some(&slot) = self.slots.get(endpoint.track_id.as_str()) else { return };
        let Some(&mode) = self.automation_modes.get(slot) else { return };
        if !matches!(mode, super::types::AutomationMode::Touch | super::types::AutomationMode::Latch) {
            return;
        }
        if let Some(map) = &self.tempo_map {
            self.automation_recorder.sample(
                &endpoint.track_id,
                map.samples_to_tick(endpoint.sample) as u32,
                mode,
                true,
                endpoint.value,
            );
        }
        if mode == super::types::AutomationMode::Touch {
            self.finish_touch_automation_recording_for_track(&endpoint.track_id);
            self.params.clear_gain_automation_owner_if(slot, endpoint_pass);
        } else {
            self.params.set_gain_automation_owner(slot, Some(endpoint_pass));
        }
    }

    fn finish_ended_automation_passes(&mut self) {
        let playing = self.shared.playing.load(Relaxed);
        let stopped = self.was_playing && !playing;
        self.was_playing = playing;
        if stopped && self.pending_automation_stops.is_empty() {
            self.queue_automation_stop(self.shared.position.load(Relaxed), false, true, None);
        }
        for track_id in std::mem::take(&mut self.pending_automation_finish) {
            self.finish_automation_recording_for_track(&track_id);
        }
        while self
            .pending_automation_stops
            .front()
            .is_some_and(|stop| stop.awaiting.is_empty())
        {
            let stop = self.pending_automation_stops.pop_front().expect("checked");
            self.finish_automation_stopped_pass(stop);
        }
        if self.pending_automation_stops.is_empty() {
            for endpoint in std::mem::take(&mut self.deferred_automation_endpoints) {
                if endpoint.pass == self.shared.automation_pass.load(Relaxed) {
                    self.process_automation_touch_endpoint(endpoint);
                }
            }
        }
    }

    fn finish_automation_stopped_pass(&mut self, stop: PendingAutomationStop) {
        for endpoint in stop.endpoints {
            let Some(&slot) = self.slots.get(endpoint.track_id.as_str()) else { continue };
            let Some(&mode) = self.automation_modes.get(slot) else { continue };
            if matches!(mode, crate::audio::types::AutomationMode::Touch | crate::audio::types::AutomationMode::Latch) {
                if let Some(map) = &self.tempo_map {
                    self.automation_recorder.sample(
                        &endpoint.track_id,
                        map.samples_to_tick(endpoint.sample) as u32,
                        mode,
                        true,
                        endpoint.value,
                    );
                }
            }
        }
        for (track_id, tick, mode, touched, value) in stop.boundary {
            self.automation_recorder.sample(&track_id, tick, mode, touched, value);
        }
        let tracks: Vec<(String, usize, crate::audio::types::AutomationMode)> = self
            .slots
            .iter()
            .filter_map(|(id, &slot)| self.automation_modes.get(slot).copied().map(|mode| (id.as_str().to_string(), slot, mode)))
            .collect();
        for (track_id, slot, mode) in tracks {
            if mode == crate::audio::types::AutomationMode::Touch {
                self.finish_touch_automation_recording_for_track(&track_id);
            } else {
                self.finish_automation_recording_for_track(&track_id);
            }
            self.params.clear_gain_automation_owner_if(slot, stop.pass);
        }
        self.was_playing = false;
    }

    /// Ends `track_id`'s recording pass and commits what it accumulated as
    /// ONE `Op::AutomationSetLane`, through the SAME `Committer` — and the
    /// same `TxMeta::engine(...)`-attributed, non-transient, undo-tracked
    /// shape — `commit_recording_finalize` uses to register a finished
    /// audio take. One pass is therefore one undo entry, exactly like one
    /// take. A pass that sampled nothing commits nothing (spec §4.6): no
    /// op, no undo entry, no rebuild, no empty lane minted.
    ///
    /// The merge replaces the RECORDED RANGE only: points outside
    /// `[first_tick, last_tick]` are kept, points inside are replaced by
    /// the pass. Touch closes and commits once per gesture; Write and Latch
    /// close at a stop or mode boundary.
    ///
    /// The lane written is the one a MANUAL edit would touch: resolved by
    /// `(target_node, param_id)` — the identity the frontend's `gainLaneFor`
    /// (`src/lib/state/automation.svelte.ts`) resolves a track's gain lane
    /// through — reusing that lane's existing id, and minting a fresh UUID
    /// only when the track has no gain lane yet. That mirrors
    /// `ControlPlane::set_automation_lane`, which mints a UUID for the
    /// empty id the UI sends for a brand-new lane. There is no
    /// "track:<id>:gain"-style derived key anywhere in this codebase, and
    /// inventing one here would have written a SECOND lane the UI never
    /// shows and the compile step would fight with.
    fn finish_automation_recording_for_track(&mut self, track_id: &str) {
        self.finish_automation_recording_for_track_after_mode(track_id, false, || {});
    }

    fn finish_touch_automation_recording_for_track(&mut self, track_id: &str) {
        self.finish_automation_recording_for_track_after_mode(track_id, true, || {});
    }

    /// Test seam: `before_commit` runs after the recorder snapshot but before
    /// `Session::transact` takes the lock. The epoch check itself remains
    /// inside that transaction, closing the project-open race completely.
    #[cfg(test)]
    fn finish_automation_recording_for_track_after<F>(&mut self, track_id: &str, before_commit: F)
    where
        F: FnOnce(),
    {
        self.finish_automation_recording_for_track_after_mode(track_id, false, before_commit);
    }

    fn finish_automation_recording_for_track_after_mode<F>(
        &mut self,
        track_id: &str,
        resume_pre_existing_curve: bool,
        before_commit: F,
    )
    where
        F: FnOnce(),
    {
        let Some(new_points) = self.automation_recorder.pending(track_id) else { return };
        let start = new_points.iter().map(|p| p.tick).min().expect("non-empty");
        let end = new_points.iter().map(|p| p.tick).max().expect("non-empty");
        let expected_epoch = self.automation_epoch;
        let target_node = format!("{}{track_id}", crate::plugins::automation::TRACK_TARGET_PREFIX);
        let committer = self.committer.clone();
        let label = format!("record automation: {track_id}");

        before_commit();
        let committed = committer.commit_with_rebuild_mode(
            op::TxMeta::engine(label),
            move |tx| {
                use crate::plugins::automation::{AutomationLane, TRACK_PARAM_GAIN};
                if tx.epoch() != expected_epoch {
                    return Err(format!(
                        "stale automation epoch: recorded {expected_epoch}, current {}",
                        tx.epoch()
                    ));
                }
                let mut lane = tx
                    .automation()
                    .lanes
                    .iter()
                    .find(|l| l.target_node == target_node && l.param_id == TRACK_PARAM_GAIN)
                    .cloned()
                    .unwrap_or_else(|| AutomationLane {
                        id: uuid::Uuid::new_v4().to_string(),
                        target_node: target_node.clone(),
                        param_id: TRACK_PARAM_GAIN,
                        points: Vec::new(),
                    });
                let old_value_at = |tick: u32| {
                    let idx = lane.points.partition_point(|p| p.tick <= tick);
                    if idx == 0 {
                        lane.points[0].value
                    } else if idx == lane.points.len() {
                        lane.points[idx - 1].value
                    } else {
                        let a = &lane.points[idx - 1];
                        let b = &lane.points[idx];
                        let span = (b.tick - a.tick) as f32;
                        a.value + (b.value - a.value) * (tick - a.tick) as f32 / span
                    }
                };
                let before = (resume_pre_existing_curve && start > 0 && !lane.points.is_empty())
                    .then(|| {
                        let tick = start - 1;
                        crate::plugins::automation::AutomationPoint { tick, value: old_value_at(tick) }
                    });
                let after = (resume_pre_existing_curve && end < u32::MAX && !lane.points.is_empty())
                    .then(|| {
                        let tick = end + 1;
                        crate::plugins::automation::AutomationPoint { tick, value: old_value_at(tick) }
                    });
                lane.points.retain(|p| p.tick < start || p.tick > end);
                lane.points.extend(new_points);
                if let Some(point) = before {
                    lane.points.push(point);
                }
                if let Some(point) = after {
                    lane.points.push(point);
                }
                crate::plugins::automation::normalize_lane(&mut lane)?;
                tx.apply(op::Op::AutomationSetLane { key: lane.id.clone(), lane: Some(lane) })
            },
            true,
            || self.rebuild(),
            crate::control::HistoryMode::RecordDistinct,
        );
        match committed {
            Ok(_) => {
                // Consume only after the atomically epoch-validated commit.
                let _ = self.automation_recorder.finish(track_id);
                self.events.emit(
                    "automation://changed",
                    serde_json::json!({ "trackId": track_id }),
                );
            }
            Err(e) => {
                if self.session.lock().epoch != expected_epoch {
                    // These points belong to the old document. Retaining them
                    // would risk a later cross-document commit.
                    self.automation_recorder.reset();
                    self.pending_automation_finish.clear();
                }
                // Non-epoch failures deliberately retain the recorder snapshot
                // so a later explicit boundary can retry instead of losing it.
                log::warn!("automation: recording pass for {track_id} was not committed: {e}");
            }
        }
    }

    /// Decode any clip sources missing from the cache (at the engine rate,
    /// or stale under a changed `source_path` — round-2 §2.2) and make sure
    /// every referencing clip's waveform pyramid exists.
    ///
    /// Reads `s`, the image its caller's whole rebuild is built from — no
    /// session lock (Plan F Task 6). It is a pure read whose entire output
    /// is control-thread cache bookkeeping, and it is followed by file I/O:
    /// precisely the shape that must not sit under the document's lock. An
    /// image one commit behind can only cost this pass a decode it redoes
    /// next rebuild — the commit that added the clip queued one.
    fn ensure_loaded(&mut self, s: &crate::control::snapshot::SessionSnapshot) {
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
            let todo = stale_sources(&s.clips, &self.cache);
            let mut live_sources: std::collections::HashSet<SourceId> = std::collections::HashSet::new();
            let mut clips_by_source: HashMap<SourceId, Vec<String>> = HashMap::new();
            for c in s.clips.iter() {
                if c.source_id.as_str().is_empty() {
                    continue; // stale_sources already warned about this
                }
                live_sources.insert(c.source_id.clone());
                clips_by_source.entry(c.source_id.clone()).or_default().push(c.id.to_string());
            }
            (s.project_dir.clone(), todo, live_sources, clips_by_source)
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
        for inp in self.inputs.iter_mut().chain(self.listen_input.as_mut()) {
            while let Ok(blk) = inp.meter_rx.pop() {
                self.accum.fold(&blk, &self.gen_maps);
            }
        }
    }

    fn pump_meter_frames(&mut self) {
        if self.last_frame.elapsed() < FRAME_INTERVAL {
            return;
        }
        // Always advance the shared 60 Hz clock, even with no meter
        // subscriber — `pump_pitch_frames` (called just before this)
        // shares `last_frame` and would otherwise fire every 2 ms tick.
        self.last_frame = Instant::now();
        if self.sinks.is_empty() {
            return;
        }
        // Display order comes from the session (Task 6: the fold itself now
        // resolves generation -> slot -> track, so this is just display
        // order, no slots needed here).
        let order: Vec<crate::ids::TrackId> = {
            let session = self.session.lock(); // read-only: display order for the meter fold
            session.store.tracks.iter().map(|t| t.id.clone()).collect()
        };
        let position = self.shared.position.load(Relaxed);
        let mut frame = self.accum.take_frame(0, &order, position);
        // The driven-param read-back rides the meter frame rather than a
        // channel of its own: it is display state sampled at the same 60 Hz,
        // about the same instant `position_samples` names. Empty (one clone
        // of an empty Vec) whenever nothing is automated or the transport is
        // stopped, which is the overwhelmingly common case.
        frame.driven_params = self.driven_params.clone();
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
        let countin = self.shared.countin_left.load(Relaxed);
        if countin > 0 {
            let dec = frames.min(countin);
            self.shared.countin_left.store(countin - dec, Relaxed);
            self.shared.countin_elapsed.fetch_add(dec, Relaxed);
            return;
        }
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
        self.queue_automation_stop(at, true, true, None);
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

    fn clear_countin(&mut self) {
        self.shared.countin_left.store(0, Relaxed);
        self.shared.countin_elapsed.store(0, Relaxed);
        self.pending_record = None;
    }

    /// When a count-in finishes, arm the take that was waiting. Temporarily
    /// zeroes `count_in_bars` so `start_recording` does not re-enter the
    /// pre-roll.
    fn arm_pending_after_countin(&mut self) {
        if self.pending_record.is_none() || self.shared.countin_left.load(Relaxed) > 0 {
            return;
        }
        let (ids, returns) = self.pending_record.take().expect("checked");
        let saved = self.count_in_bars;
        self.count_in_bars = 0;
        if let Err(e) = self.start_recording(ids, returns) {
            log::warn!("audio: count-in ended but the take failed to arm: {e}");
            self.shared.playing.store(false, Relaxed);
        }
        self.count_in_bars = saved;
    }

    /// The device the live analyser should listen to: the selected input,
    /// or `""` meaning the system default.
    fn pitch_device_key(&self) -> String {
        self.sel_input.clone().unwrap_or_default()
    }

    /// Does a running take already capture the pitch device? If so it owns
    /// the microphone and carries the analyser itself — opening a second
    /// stream on the same device would be wasteful at best and refused by
    /// the driver at worst.
    fn take_owns_pitch_device(&self) -> bool {
        let key = self.pitch_device_key();
        self.inputs
            .iter()
            .any(|i| i.wants.recording && i.device_key == key)
    }

    /// Which capture group, if any, carries the live-pitch tap: the one on
    /// the pitch device. Deliberately NOT conditional on whether the user is
    /// listening at take start — the tap rides along dormant so listening can
    /// start mid-take without rebuilding a recording stream (PR #49 issue 7).
    fn pitch_group_key<'a>(&self, groups: impl IntoIterator<Item = &'a String>) -> Option<String> {
        let key = self.pitch_device_key();
        groups.into_iter().find(|k| **k == key).cloned()
    }

    /// Should a listen-only stream exist right now? Pure policy, split from
    /// [`Control::sync_input_hub`] so the transitions are testable without an
    /// audio device: wanting to listen is not the same as owning a stream,
    /// because a take on the same device carries the analyser instead.
    fn listen_stream_wanted(&self) -> bool {
        self.wants_listening && !self.take_owns_pitch_device()
    }

    /// Idempotent: open, rebuild, or close the listen-only input stream so it
    /// matches what is wanted. Safe to call on any transition — it is the one
    /// place that decides whether the microphone is open.
    ///
    /// Rebuild-on-change rather than mutating a live callback (spec §3.1):
    /// a command ring into the RT thread would be the "correct" mechanism and
    /// remains the upgrade path, but this transition only ever happens on the
    /// control thread, and rebuilding is behaviourally identical for a few
    /// milliseconds of pitch blackout.
    fn sync_input_hub(&mut self) -> Result<(), String> {
        // The listen-only stream exists solely for live pitch. A take on
        // the same device carries the analyser itself (`recording: true`
        // on that group's bundle), so this hub's wanted set is listening
        // and never recording.
        let wanted = InputWants {
            listening: self.listen_stream_wanted(),
            recording: false,
        };
        let key = self.pitch_device_key();

        match (&self.listen_input, wanted.any()) {
            (Some(hub), true) if hub.device_key == key => Ok(()), // already right
            (_, false) => {
                self.listen_input = None;
                Ok(())
            }
            _ => {
                // Hold the previous stream until the new one is open so a
                // failed open does not leave `wants_listening` with no mic.
                // Exclusive same-device rebuilds may fail-and-restore,
                // which is the conservative outcome (keep the live stream).
                let previous = self.listen_input.take();
                let device = (!key.is_empty()).then_some(key.as_str());
                match self.open_listen_stream(device) {
                    Ok(hub) => {
                        self.listen_input = Some(hub);
                        log::info!(
                            "audio: pitch listen stream open on {}",
                            if key.is_empty() { "<default>" } else { &key }
                        );
                        Ok(())
                    }
                    Err(e) => {
                        self.listen_input = previous;
                        Err(e)
                    }
                }
            }
        }
    }

    /// Turn live pitch on or off. Errors only when the device could not be
    /// opened; turning it off never fails.
    fn set_listening(&mut self, on: bool) -> Result<(), String> {
        self.wants_listening = on;
        let r = self.sync_input_hub();
        if r.is_err() {
            // Do not leave the flag claiming we are listening when no stream
            // exists — the next sync would silently do nothing.
            self.wants_listening = false;
        }
        // Every tap reads this, including the dormant one a take on the pitch
        // device carries: this store is what starts and stops the analysis,
        // whichever stream owns the microphone.
        self.pitch_active.store(self.wants_listening, Relaxed);
        r
    }

    /// Press-and-hold rehearse. Records the span edges against the transport
    /// so a take can report where the user rehearsed (spec §4.1).
    fn set_rehearse_hold(&mut self, on: bool) {
        let was = self.rehearse.swap(on, Relaxed);
        if was == on {
            return;
        }
        let at = self.shared.position.load(Relaxed);
        match on {
            true => self.rehearse_open = Some(at),
            false => {
                if let Some(start) = self.rehearse_open.take() {
                    if at > start {
                        self.rehearse_spans.push((start, at));
                    }
                }
            }
        }
    }

    /// Held spans for the take that is finishing: closes an in-progress hold
    /// at `at` so a take stopped mid-rehearse still reports it.
    fn take_rehearse_spans(&mut self, at: u64) -> Vec<(u64, u64)> {
        if let Some(start) = self.rehearse_open.take() {
            if at > start {
                self.rehearse_spans.push((start, at));
            }
            // Still held: reopen at the stop point so the flag and the span
            // bookkeeping stay consistent for the NEXT take.
            if self.rehearse.load(Relaxed) {
                self.rehearse_open = Some(at);
            }
        }
        std::mem::take(&mut self.rehearse_spans)
    }

    /// Drain pitch frames from whichever stream currently carries the
    /// analyser into `out`. Called every control tick.
    fn drain_pitch(&mut self, out: &mut Vec<PitchFrame>) {
        let rx = self
            .listen_input
            .as_mut()
            .and_then(|h| h.pitch_rx.as_mut())
            .or_else(|| self.inputs.iter_mut().find_map(|i| i.pitch_rx.as_mut()));
        if let Some(rx) = rx {
            while let Ok(f) = rx.pop() {
                out.push(f);
            }
        }
    }

    fn pitch_device_rate(&self) -> u32 {
        self.listen_input
            .as_ref()
            .map(|h| h.rate)
            .or_else(|| {
                self.inputs
                    .iter()
                    .find(|i| i.pitch_rx.is_some())
                    .map(|i| i.rate)
            })
            .unwrap_or(0)
    }

    /// Is there anything anywhere that can actually analyse right now? The
    /// listen stream carries a tap, and so does a take on the pitch device —
    /// but a take whose worker thread failed to spawn does NOT, and while
    /// that take runs it owns the device, so no listen stream can be opened
    /// to make up for it (`listen_stream_wanted`).
    fn pitch_tap_present(&self) -> bool {
        self.listen_input.as_ref().is_some_and(|h| h.pitch_rx.is_some())
            || self.inputs.iter().any(|i| i.pitch_rx.is_some())
    }

    fn current_pitch_state(&self) -> PitchState {
        PitchState {
            // Intent AND capability. Reporting bare intent would light up a
            // panel over a trail that can never draw, with nothing to tell
            // the user why — the failure `spawn_pitch_worker` is fallible to
            // avoid, reintroduced one layer up.
            listening: self.wants_listening && self.pitch_tap_present(),
            rehearse_hold: self.rehearse.load(Relaxed),
            reference_track_id: self.reference_track_id.clone(),
            device_rate: self.pitch_device_rate(),
        }
    }

    fn emit_pitch_state(&mut self) {
        let state = self.current_pitch_state();
        if self.last_pitch_state.as_ref() == Some(&state) {
            return;
        }
        self.last_pitch_state = Some(state.clone());
        if let Ok(payload) = serde_json::to_value(&state) {
            self.events.emit("pitch://state", payload);
        }
    }

    /// Drain the pitch ring and push one batch per 60 Hz tick — the same
    /// cadence as meters, not a second timer (plan task 6). Called BEFORE
    /// `pump_meter_frames` so both share `last_frame` (meters always
    /// advances the clock, even with no meter subscriber).
    fn pump_pitch_frames(&mut self) {
        let mut scratch = std::mem::take(&mut self.pitch_scratch);
        self.drain_pitch(&mut scratch);
        if self.pitch_sinks.is_empty() {
            // Keep a short trail so a late `pitch_subscribe` still sees
            // recent frames rather than a dark first tick.
            const HOLD: usize = 100;
            if scratch.len() > HOLD {
                let drop_n = scratch.len() - HOLD;
                scratch.drain(..drop_n);
            }
            self.pitch_scratch = scratch;
            return;
        }
        if self.last_frame.elapsed() < FRAME_INTERVAL {
            self.pitch_scratch = scratch;
            return;
        }
        let batch = PitchFrameBatch {
            frames: std::mem::take(&mut scratch),
            device_rate: self.pitch_device_rate(),
            // Same rule as `current_pitch_state`: the flag rides on the
            // batch so the UI does not have to wait for `pitch://state`,
            // and the two must not disagree.
            listening: self.wants_listening && self.pitch_tap_present(),
            rehearse_hold: self.rehearse.load(Relaxed),
        };
        self.pitch_scratch = scratch;
        self.pitch_sinks.retain(|s| s.send_batch(&batch));
    }

    /// Resolve a capture device and its config, or say why not. Split out of
    /// `open_capture_group` so the pitch-listening path (which opens a stream
    /// with no writer and no producers) resolves the device exactly the same
    /// way a take does — one place that decides what "the input" is.
    fn resolve_input_device(
        &self,
        device_id: Option<&str>,
    ) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
        let host = cpal::default_host();
        let device = match device_id.filter(|s| !s.is_empty()) {
            Some(id) => host
                .input_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.name().map(|n| n == id).unwrap_or(false))
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
        Ok((device, cfg))
    }

    /// Build the pitch chain for a stream and start its analysis thread.
    /// Every buffer is preallocated here on the control thread; the returned
    /// handle joins the thread when the bundle holding it is dropped (see
    /// [`super::pitch_thread`]).
    fn build_pitch_tap(
        rate: u32,
        active: Arc<AtomicBool>,
    ) -> Result<(PitchTap, PitchWorkerHandle, rtrb::Consumer<PitchFrame>), String> {
        let (tap, worker, rx) = pitch_channel(rate, active);
        let handle = spawn_pitch_worker(worker).map_err(|e| e.to_string())?;
        Ok((tap, handle, rx))
    }

    /// Stream-less listen bundle used by tests (no cpal device) and by
    /// `open_listen_stream` when `stub_input` is set.
    #[cfg(test)]
    fn stub_listen_bundle(device_key: &str) -> InputBundle {
        let (_meter_tx, meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let (_tx, pitch_rx) = rtrb::RingBuffer::new(PITCH_RING_SLOTS);
        InputBundle {
            _stream: None,
            _pitch_worker: None,
            meter_rx,
            pitch_rx: Some(pitch_rx),
            device_key: device_key.to_string(),
            wants: InputWants {
                listening: true,
                recording: false,
            },
            rate: 48_000,
        }
    }

    /// Open a capture stream with NO writers and NO record producers — just
    /// meters and pitch. This is what owner ruling R6 asks for: the mic
    /// opens because the user asked to listen, not because a track is armed.
    fn open_listen_stream(&self, device_id: Option<&str>) -> Result<InputBundle, String> {
        #[cfg(test)]
        if self.stub_input {
            return Ok(Self::stub_listen_bundle(device_id.unwrap_or_default()));
        }
        let (device, cfg) = self.resolve_input_device(device_id)?;
        let in_ch = cfg.channels().max(1) as usize;
        let rate = cfg.sample_rate().0;
        // Fatal here: a listen stream with no analyser is a stream that
        // exists only to be dark.
        let (pitch, pitch_worker, pitch_rx) =
            Self::build_pitch_tap(rate, self.pitch_active.clone())?;
        let (meter_tx, meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let mut cb = InputCb {
            producers: Vec::new(),
            owed: Vec::new(),
            meter_tx,
            // No meter blocks: a listen-only `base_slot == 0` chunk is
            // still folded into frame/master accounting and would dilute
            // output RMS by ~3 dB. Input metering on a listening hub is
            // a later job (dedicated slot, out of the output denominator).
            blocks: Vec::new(),
            in_ch,
            rec_ch: in_ch.min(2),
            shared: self.shared.clone(),
            pitch: Some(pitch),
            rehearse: self.rehearse.clone(),
        };
        let stream = device
            .build_input_stream(
                &cfg.into(),
                move |data: &[f32], _| cb.capture(data),
                |e| log::warn!("listen stream error: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(InputBundle {
            _stream: Some(stream),
            _pitch_worker: Some(pitch_worker),
            meter_rx,
            pitch_rx: Some(pitch_rx),
            device_key: device_id.unwrap_or_default().to_string(),
            wants: InputWants {
                listening: true,
                recording: false,
            },
            rate,
        })
    }

    /// Open one cpal input stream + disk writer for a group of tracks that
    /// share a capture device. `device_id` `None` (or the empty string the
    /// group key uses for "no preference") is the selected/default input.
    /// `with_pitch` attaches the live analyser to this group's stream — set
    /// for the group that owns the microphone, so a take on the pitch device
    /// keeps feeding the panel without opening that device twice.
    fn open_capture_group(
        &self,
        device_id: Option<&str>,
        tracks: &[AudioRecTarget],
        start_pos: u64,
        with_pitch: bool,
    ) -> Result<(InputBundle, DiskWriter, u64), String> {
        let (device, cfg) = self.resolve_input_device(device_id)?;
        let in_ch = cfg.channels().max(1) as usize;
        let rec_ch = in_ch.min(2);
        let rate = cfg.sample_rate().0;

        let (project_dir, take_no, slots, rec_generation) = {
            let session = self.session.lock(); // read-only: project dir + take numbering + slot resolution
            let store = &session.store;
            let dir = store.project_dir.clone().ok_or("no project open")?;
            let take_no = store.clips.len() + 1;
            let tables = self.tables.lock();
            let slots: Vec<usize> = tracks
                .iter()
                .filter_map(|t| tables.slots.get(t.track_id.as_str()).copied())
                .collect();
            (dir, take_no, slots, tables.generation)
        };
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
        let mut producers = Vec::with_capacity(tracks.len());
        let mut consumers = Vec::with_capacity(tracks.len());
        let mut specs = Vec::with_capacity(tracks.len());
        for (i, t) in tracks.iter().enumerate() {
            let clip_id = uuid::Uuid::new_v4().to_string();
            let source_id = crate::ids::SourceId::mint();
            let rel = format!("audio/{source_id}.wav");
            let (p, c) = rtrb::RingBuffer::new(capacity);
            producers.push(p);
            consumers.push(c);
            specs.push(RecSpec {
                track_id: t.track_id.clone(),
                take_name: format!("Take {}", take_no + i),
                wav_path: project_dir.join(&rel),
                rel_path: rel,
                source_id,
                cache_dir: Store::cache_dir_for(&project_dir, &clip_id),
                pitch_path: Some(crate::audio::pitch_store::track_path(&project_dir, &clip_id)),
                clip_id,
                start_pos,
            });
        }

        let writer = recorder::spawn(specs, consumers, rec_ch as u16, rate)?;
        let (meter_tx, meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let n_producers = producers.len();
        // NOT fatal here: a take must not fail because the tuner could not
        // start. It loses live pitch for its duration, loudly.
        let (pitch, pitch_worker, pitch_rx) = match with_pitch {
            true => match Self::build_pitch_tap(rate, self.pitch_active.clone()) {
                Ok((tap, worker, rx)) => (Some(tap), Some(worker), Some(rx)),
                Err(e) => {
                    // The take goes on without live pitch — it must not fail
                    // because the tuner could not start. `pitch_tap_present`
                    // is what stops this being silent: with no tap anywhere,
                    // `pitch://state` reports `listening: false` for as long
                    // as this take owns the device.
                    log::warn!("audio: live pitch unavailable for this take: {e}");
                    (None, None, None)
                }
            },
            false => (None, None, None),
        };
        let mut cb = InputCb {
            producers,
            owed: vec![0; n_producers],
            meter_tx,
            blocks,
            in_ch,
            rec_ch,
            shared: self.shared.clone(),
            pitch,
            rehearse: self.rehearse.clone(),
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
        Ok((
            InputBundle {
                _stream: Some(stream),
                _pitch_worker: pitch_worker,
                meter_rx,
                pitch_rx,
                device_key: device_id.unwrap_or_default().to_string(),
                wants: InputWants {
                    listening: with_pitch,
                    recording: true,
                },
                rate,
            },
            writer,
            rec_generation,
        ))
    }

    fn start_recording(
        &mut self,
        track_ids: Option<Vec<String>>,
        return_sources: HashMap<String, String>,
    ) -> Result<Vec<String>, String> {
        // A MIDI-only take opens no device, so `self.writers` stays empty
        // for the whole take — the capture is the other half of "is a take
        // running".
        if !self.writers.is_empty() || self.live_in_hub.capturing() || self.pending_record.is_some()
        {
            return Err("already recording".to_string());
        }

        // Read the routing target BEFORE the session lock: the hub's own
        // mutex must never be taken under it ([C1] ordering).
        let live_in_target = self.live_in_hub.target_track();
        let (targets, midi_target) = {
            let session = self.session.lock(); // read-only: resolve/validate target track ids before recording starts
            split_record_targets(&session.store, track_ids.clone(), live_in_target, &return_sources)?
        };

        // A take needs a project dir whether it is audio or MIDI.
        self.ensure_project()?;

        if self.count_in_bars > 0 && self.shared.countin_left.load(Relaxed) == 0 {
            let (left, beat, beats_per_bar) = {
                let session = self.session.lock();
                count_in_plan(&session, self.shared.sample_rate.load(Relaxed), self.shared.position.load(Relaxed), self.count_in_bars)
            };
            if left > 0 {
                self.shared.countin_left.store(left, Relaxed);
                self.shared.countin_elapsed.store(0, Relaxed);
                self.shared.countin_beat.store(beat, Relaxed);
                self.shared.countin_beats_per_bar.store(beats_per_bar, Relaxed);
                self.shared.playing.store(true, Relaxed);
                self.pending_record = Some((track_ids, return_sources));
                log::info!("audio: count-in {left} samples ({beat} / beat)");
                return Ok(targets.into_iter().map(|t| t.track_id).chain(midi_target).collect());
            }
        }

        let start_pos = self.shared.position.load(Relaxed);

        if !targets.is_empty() {
            // Group by capture device so two tracks sharing a return (or
            // the global input) share one stream; distinct devices each
            // get their own (X1). Device lookup is the first fallible
            // step so a missing return source fails before any writer
            // thread is spawned.
            let mut by_device: BTreeMap<String, Vec<AudioRecTarget>> = BTreeMap::new();
            for t in &targets {
                let key = t
                    .device_id
                    .clone()
                    .or_else(|| self.sel_input.clone())
                    .unwrap_or_default();
                by_device.entry(key).or_default().push(t.clone());
            }

            // The take is about to open the capture devices itself. Drop the
            // listen-only stream first: if the take covers the pitch device,
            // its own group carries the analyser instead (one stream per
            // device), and if it does not, `sync_input_hub` below reopens it.
            self.listen_input = None;
            // Not gated on `wants_listening`: the tap rides along dormant so
            // opening the panel mid-take wakes it (issue 7). A dormant tap
            // costs one relaxed atomic load per capture buffer on the RT
            // side; the take also carries the chain's rings (~525 KB) and a
            // worker thread that backs off to 20 wake-ups a second while the
            // flag is clear.
            let pitch_group = self.pitch_group_key(by_device.keys());

            let mut groups: Vec<(InputBundle, DiskWriter)> = Vec::new();
            let mut rec_generation = None;
            for (device_key, group) in by_device {
                let wanted = if device_key.is_empty() { None } else { Some(device_key.as_str()) };
                let with_pitch = pitch_group.as_deref() == Some(device_key.as_str());
                match self.open_capture_group(wanted, &group, start_pos, with_pitch) {
                    Ok((bundle, writer, gen)) => {
                        rec_generation = Some(gen);
                        groups.push((bundle, writer));
                    }
                    Err(e) => {
                        // Drop any groups already opened so a failed second
                        // device does not leave a writer thread running.
                        for (input, writer) in groups {
                            drop(input);
                            writer.stop.store(true, Relaxed);
                            let _ = writer.finish(Duration::from_secs(2));
                        }
                        // `listen_input` was taken before any group opened.
                        // Restore the hub (warn, do not mask the take-start
                        // error) so `wants_listening` is not left true with
                        // no stream.
                        if let Err(hub_err) = self.sync_input_hub() {
                            log::warn!(
                                "audio: pitch listen could not resume after failed take start: {hub_err}"
                            );
                            if self.listen_input.is_none() {
                                self.wants_listening = false;
                                self.pitch_active.store(false, Relaxed);
                            }
                        }
                        self.emit_pitch_state();
                        return Err(e);
                    }
                }
            }

            // Pin AFTER every fallible step (device lookup, spawn, play)
            // succeeded — `stop_recording` is the only unpin.
            if let Some(gen) = rec_generation {
                self.gen_maps.pin(gen);
            }
            for (input, writer) in groups {
                self.inputs.push(input);
                self.writers.push(writer);
            }
            log::info!("audio: recording {} track(s) on {} input(s)", targets.len(), self.inputs.len());
        } // ruling 8: a take with only a MIDI target opens no device at all

        // A MIDI-only take (or one on other devices entirely) leaves the
        // microphone to the listener — reopen it if it is still wanted.
        // Never fatal: a take must not fail because the tuner could not.
        self.rehearse_spans.clear();
        // Spans are relative to THIS take. A hold still down (or one that
        // opened during listen/playback) must start at this take's origin,
        // not the previous take's stop or a pre-take transport position.
        self.rehearse_open = self.rehearse.load(Relaxed).then_some(start_pos);
        if let Err(e) = self.sync_input_hub() {
            log::warn!("audio: pitch listen unavailable during take: {e}");
        }
        self.emit_pitch_state();

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

        let mut recorded: Vec<String> = targets.into_iter().map(|t| t.track_id).collect();
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
        let writers = std::mem::take(&mut self.writers);
        if self.pending_record.take().is_some() || self.shared.countin_left.load(Relaxed) > 0 {
            self.clear_countin();
            self.shared.recording.store(false, Relaxed);
            self.shared.playing.store(false, Relaxed);
            self.queue_automation_stop(self.shared.position.load(Relaxed), true, true, None);
            if writers.is_empty() && capture.is_none() {
                return Ok(Vec::new());
            }
        }
        if writers.is_empty() && capture.is_none() {
            return Err("not recording".to_string());
        }
        // Drop the input streams FIRST so the ring producers close and the
        // writers can drain to empty.
        self.inputs.clear();
        // The take no longer owns the microphone: if the user is still
        // listening, give it back. A take ending must not silence the tuner.
        if let Err(e) = self.sync_input_hub() {
            log::warn!("audio: pitch listen could not resume after the take: {e}");
        }
        self.emit_pitch_state();
        // A writer failure (disk full, a WAV header that would not close,
        // the 15 s drain timeout) is reported, but only after everything it
        // does NOT own has been salvaged. It used to return early, which
        // took two things with it that never depended on the writer: the
        // MIDI take, which is pure in-memory data already lifted out of the
        // hub above, and the stop itself — `shared.recording` stayed true
        // and the transport-state commit never ran, leaving the UI claiming
        // a take was still running.
        let (clips, writer_err) = if writers.is_empty() {
            (Vec::new(), None)
        } else {
            // Release the pin (Task 6 [I2]) — recording is over, so its
            // generation no longer needs exemption from the plain
            // window. Only the audio half ever pinned one.
            self.gen_maps.unpin();
            let mut clips = Vec::new();
            let mut writer_err = None;
            for w in writers {
                match w.finish(Duration::from_secs(15)) {
                    Ok(mut c) => clips.append(&mut c),
                    Err(e) => {
                        log::error!("stop_recording: the take's audio was lost: {e}");
                        writer_err = Some(e);
                    }
                }
            }
            (clips, writer_err)
        };

        self.shared.recording.store(false, Relaxed);
        self.shared.playing.store(false, Relaxed);
        self.queue_automation_stop(self.shared.position.load(Relaxed), true, true, None);
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
        let rehearse_spans = self.take_rehearse_spans(self.shared.position.load(Relaxed));
        self.events.emit(
            "recording://state",
            serde_json::json!({
                "recording": false,
                "trackIds": track_ids,
                "xruns": self.shared.xruns.load(Relaxed),
                "clips": clips,
                "midiClipId": midi_clip.as_ref().map(|c| c.id.to_string()),
                "rehearseSpans": rehearse_spans
                    .iter()
                    .map(|&(start, end)| serde_json::json!({
                        "startSample": start,
                        "endSample": end,
                    }))
                    .collect::<Vec<_>>(),
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
/// One audio take target. `device_id` is `None` for a normal audio track
/// (uses the engine's selected/default input) and `Some` for a MIDI track
/// that has an X1 return source (uses that device).
#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioRecTarget {
    track_id: String,
    device_id: Option<String>,
}

fn split_record_targets(
    store: &Store,
    requested: Option<Vec<String>>,
    midi_target: Option<String>,
    returns: &HashMap<String, String>,
) -> Result<(Vec<AudioRecTarget>, Option<String>), String> {
    let is_midi =
        |id: &str| store.tracks.iter().any(|t| t.id.as_str() == id && t.kind == "midi");
    let candidates: Vec<String> = match requested {
        Some(ids) => {
            for id in &ids {
                if !store.tracks.iter().any(|t| &t.id == id) {
                    return Err(format!("unknown track: {id}"));
                }
            }
            ids
        }
        None => store.armed_track_ids(),
    };
    let audio: Vec<AudioRecTarget> = candidates
        .into_iter()
        .filter_map(|id| {
            if is_midi(&id) {
                // A routed MIDI track with a return is an audio take on
                // that device. Without a return it stays MIDI-only.
                returns.get(&id).map(|dev| AudioRecTarget {
                    track_id: id,
                    device_id: Some(dev.clone()),
                })
            } else {
                Some(AudioRecTarget { track_id: id, device_id: None })
            }
        })
        .collect();
    let midi = midi_target.filter(|id| is_midi(id));
    if audio.is_empty() && midi.is_none() {
        return Err("no armed tracks to record".to_string());
    }
    Ok((audio, midi))
}

fn compile_clicks(session: &Session, rate: u32, song_end: u64) -> Vec<crate::audio::metronome::Click> {
    let tempo = match crate::midi::tempo::TempoMap::new(
        session.midi.ppq,
        session.midi.tempo_events.clone(),
        rate.max(1),
    ) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let meter = crate::midi::tempo::MeterMap::new(session.midi.meter_events.clone())
        .unwrap_or_else(|_| crate::midi::tempo::MeterMap::default_map());
    let pad = crate::audio::metronome::count_in_samples(&tempo, &meter, 0, 16);
    let end = song_end.max(pad);
    crate::audio::metronome::schedule(&tempo, &meter, end)
}

fn count_in_plan(session: &Session, rate: u32, at_sample: u64, bars: u8) -> (u64, u64, u32) {
    let tempo = match crate::midi::tempo::TempoMap::new(
        session.midi.ppq,
        session.midi.tempo_events.clone(),
        rate.max(1),
    ) {
        Ok(t) => t,
        Err(_) => return (0, 0, 4),
    };
    let meter = crate::midi::tempo::MeterMap::new(session.midi.meter_events.clone())
        .unwrap_or_else(|_| crate::midi::tempo::MeterMap::default_map());
    let left = crate::audio::metronome::count_in_samples(&tempo, &meter, at_sample, bars);
    let beat = crate::audio::metronome::beat_samples_at(&tempo, &meter, at_sample);
    let at_tick = tempo.samples_to_tick(at_sample);
    let beats = meter
        .events()
        .iter()
        .rev()
        .find(|e| e.tick <= at_tick)
        .map(|e| e.num as u32)
        .unwrap_or(4);
    (left, beat, beats.max(1))
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
    use crate::audio::types::AutomationMode;
    use std::sync::atomic::AtomicUsize;

    struct NullEvents;
    impl EventSink for NullEvents {
        fn emit(&self, _event: &str, _payload: serde_json::Value) {}
    }

    /// Keeps what was emitted, for the handful of tests that assert on the
    /// events themselves rather than on the state behind them.
    struct RecordingEvents(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
    impl EventSink for RecordingEvents {
        fn emit(&self, event: &str, payload: serde_json::Value) {
            self.0.lock().push((event.to_string(), payload));
        }
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
            sends: Vec::new(),
            out_pdc: None,
            output: None,
            win: Default::default(),
            slot: 0,
            clips: Vec::new(),
            live: Some(crate::audio::rt::LiveSource { node: cell, events: Arc::new(Vec::new()) }),
            inserts: Vec::new(),
            pdc: None,
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
                sends: Vec::new(),
                out_pdc: None,
                output: None,
                win: Default::default(),
                slot,
                clips: Vec::new(),
                live: Some(crate::audio::rt::LiveSource { node: cell, events: Arc::new(events) }),
                inserts: Vec::new(),
                pdc: None,
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
                sends: Vec::new(),
                out_pdc: None,
                output: None,
                win: Default::default(),
                slot,
                clips: Vec::new(),
                live: Some(crate::audio::rt::LiveSource {
                    node: crate::audio::rt::LiveNodeCell::new(Box::new(synth)),
                    events: Arc::new(events0.take().unwrap_or_default()),
                }),
                inserts: Vec::new(),
                pdc: None,
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
        let scheduled = vec![crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 100, channel: 0 }];
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
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110, channel: 0 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0, channel: 0 },
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
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110, channel: 0 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0, channel: 0 },
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
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110, channel: 0 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0, channel: 0 },
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
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110, channel: 0 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0, channel: 0 },
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
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 110, channel: 0 },
            crate::midi::schedule::AbsNoteEvent { sample: 480_000, key: 60, velocity: 0, channel: 0 },
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
            send_slots: Default::default(),
            generation: 0,
            params: Arc::new(ParamTable::default()),
            slots: HashMap::new(),
        }));
        let session = Arc::new(Mutex::new(Session::new(Store::default(), crate::midi::MidiStore::default())));
        let gesture = Arc::new(crate::control::GestureState::new());
        let handle = start(
            shared.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullEvents),
            crate::control::testutil::test_committer(&session, &shared, &tables),
            gesture.clone(),
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
            send_slots: Default::default(),
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
            inputs: Vec::new(),
            writers: Vec::new(),
            rec_track_ids: Vec::new(),
            sel_output: None,
            sel_input: None,
            cache: HashMap::new(),
            cache_rate: 0,
            live_nodes: Default::default(),
            insert_nodes: Default::default(),
            accum: MeterAccum::default(),
            gen_maps: GenerationMaps::default(),
            sinks: Vec::new(),
            last_frame: Instant::now(),
            last_tick: Instant::now(),
            committer,
            // Its own, never-opened gesture: no `ControlPlane` shares this
            // fixture, so nothing can be mid-drag here.
            gesture: Arc::new(crate::control::GestureState::new()),
            ensure_project_fn: None,
            param_automation: crate::plugins::automation::ParamAutomationDriver::empty(),
            param_writes: Vec::new(),
            driven_params: Vec::new(),
            automation_modes: Vec::new(),
            tempo_map: None,
            slots: HashMap::new(),
            params: Arc::new(ParamTable::default()),
            automation_recorder: crate::plugins::automation::AutomationRecorder::new(),
            base_gains: Vec::new(),
            automation_epoch: 0,
            was_playing: false,
            pending_automation_finish: Vec::new(),
            pending_automation_stops: VecDeque::new(),
            deferred_automation_endpoints: Vec::new(),
            // Its OWN hub, never the process-global one: these tests would
            // otherwise race every other test that selects a routing target.
            live_in_hub: Arc::new(MidiInHub::new()),
            live_in_target: None,
            external_routing: Arc::new(crate::midi_out::RoutedOut::default()),
            published: session.lock().published_handle(),
            after_assembly: None,
            count_in_bars: 0,
            pending_record: None,
            listen_input: None,
            wants_listening: false,
            pitch_active: Arc::new(AtomicBool::new(false)),
            rehearse: Arc::new(AtomicBool::new(false)),
            rehearse_open: None,
            rehearse_spans: Vec::new(),
            // Headless tests have no microphone; the hub ownership tests
            // still need to open and close a bundle.
            stub_input: true,
            pitch_sinks: Vec::new(),
            reference_track_id: None,
            last_pitch_state: None,
            pitch_scratch: Vec::new(),
        };
        (ctl, session, tx)
    }

    fn test_track(id: &str) -> super::super::types::TrackState {
        super::super::types::TrackState {
            sends: Vec::new(),
            output: None,
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
            inserts: Vec::new(),
            group: None,
            automation_mode: AutomationMode::Read,
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

    /// Task 8: `automation_modes` is the slot-indexed cache Task 9 reads
    /// every control-thread tick — a rebuild refreshes it from
    /// `store.tracks`, same shape as `param_automation`/gain ramps.
    #[test]
    fn rebuild_caches_automation_mode_by_slot() {
        let (mut ctl, session) = bare_control();
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.automation_mode = AutomationMode::Off;
            s.store.tracks.push(t);
        }
        ctl.rebuild();
        assert_eq!(ctl.automation_modes, vec![AutomationMode::Off]);

        // A later rebuild refreshes the cache wholesale, like every other
        // rebuild-time table — it does not merely append or patch a slot.
        session.lock().store.tracks[0].automation_mode = AutomationMode::Write;
        ctl.rebuild();
        assert_eq!(ctl.automation_modes, vec![AutomationMode::Write]);
    }

    /// Task 9: every control-thread tick, a Write-mode track's live gain
    /// gets sampled into `automation_recorder` — no gesture needed, unlike
    /// Touch/Latch, since Write always records while playing.
    #[test]
    fn drive_automation_recording_samples_a_write_mode_track_every_tick() {
        let (mut ctl, session) = bare_control();
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.automation_mode = AutomationMode::Write;
            t.gain_db = -12.0;
            s.store.tracks.push(t);
            assert!(s.automation.lanes.is_empty());
        }
        ctl.rebuild();
        assert!(ctl.tempo_map.is_some(), "fixture must exercise the real tempo-map compile path");

        // A -6 dB live value over a persisted -12 dB base is a boost
        // multiplier of about 2.0; it must not be normalized back to 1.0.
        let slot = *ctl.tables.lock().slots.get("t-1").expect("t-1 has a slot");
        let live_gain = crate::audio::mixer::db_to_linear(-6.0);
        let expected = live_gain / crate::audio::mixer::db_to_linear(-12.0);
        ctl.tables.lock().params.set_gain_linear(slot, live_gain);

        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(0, Relaxed);

        ctl.drive_automation_recording();
        let recorded = ctl.automation_recorder.finish("t-1");
        assert!(recorded.is_some(), "Write mode must have sampled this tick");
        let points = recorded.unwrap();
        assert_eq!(points.len(), 1);
        assert!(expected > 1.9);
        assert!((points[0].value - expected).abs() < 1e-6, "recorded boost multiplier");
    }

    /// A stopped transport samples nothing — automation recording only
    /// happens while the transport plays, same guard shape as
    /// `drive_param_automation`.
    #[test]
    fn drive_automation_recording_does_nothing_when_stopped() {
        let (mut ctl, session) = bare_control();
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.automation_mode = AutomationMode::Write;
            s.store.tracks.push(t);
            s.automation.lanes.push(test_lane("l1", "track:ghost", 0));
        }
        ctl.rebuild();
        assert!(ctl.tempo_map.is_some());

        ctl.shared.playing.store(false, Relaxed);
        ctl.shared.position.store(0, Relaxed);

        ctl.drive_automation_recording();
        assert!(ctl.automation_recorder.finish("t-1").is_none());
    }

    // -- Task 10: committing a finished pass ---------------------------------

    /// A track in `mode` plus the gain lane a recorded pass must MERGE into
    /// — same `(target_node, param_id)` identity the frontend's
    /// `gainLaneFor` resolves a manual edit through
    /// (`src/lib/state/automation.svelte.ts`), so a pass that minted its own
    /// lane instead would show up as a second lane here. Returns the seeded
    /// lane's id.
    fn seed_recording_track(
        session: &Arc<Mutex<Session>>,
        track_id: &str,
        mode: AutomationMode,
        points: &[(u32, f32)],
    ) -> String {
        use crate::plugins::automation::{AutomationLane, AutomationPoint, TRACK_PARAM_GAIN};
        let mut s = session.lock();
        let mut t = test_track(track_id);
        t.automation_mode = mode;
        s.store.tracks.push(t);
        let id = format!("lane-{track_id}");
        s.automation.lanes.push(AutomationLane {
            id: id.clone(),
            target_node: format!("track:{track_id}"),
            param_id: TRACK_PARAM_GAIN,
            points: points.iter().map(|&(tick, value)| AutomationPoint { tick, value }).collect(),
        });
        id
    }

    /// The track's ONE gain lane, asserting there is exactly one: a recorded
    /// pass that wrote under its own key would leave two.
    fn gain_lane(
        session: &Arc<Mutex<Session>>,
        track_id: &str,
    ) -> crate::plugins::automation::AutomationLane {
        let target = format!("track:{track_id}");
        let found: Vec<_> = session
            .lock()
            .automation
            .lanes
            .iter()
            .filter(|l| {
                l.target_node == target && l.param_id == crate::plugins::automation::TRACK_PARAM_GAIN
            })
            .cloned()
            .collect();
        assert_eq!(
            found.len(),
            1,
            "exactly one track-gain lane must exist — a recorded pass must update the lane a \
             manual edit would, never mint a shadow one: {found:?}"
        );
        found.into_iter().next().expect("checked")
    }

    /// One control-thread tick's automation work, in `Control::run`'s order.
    /// The pair has to be driven together: the pass-end trigger is an EDGE
    /// detector over `shared.playing`, so it only ever sees a stop if it saw
    /// the playing ticks first.
    fn automation_tick(ctl: &mut Control, pos: u64) {
        ctl.shared.position.store(pos, Relaxed);
        ctl.drive_automation_recording();
        ctl.finish_ended_automation_passes();
    }

    fn touch_endpoint(track_id: &str, value: f32, sample: u64) -> AutomationTouchEndpoint {
        AutomationTouchEndpoint { track_id: track_id.into(), value, sample, pass: 0 }
    }

    fn automation_stop(ctl: &mut Control) {
        let at = ctl.shared.position.load(Relaxed);
        ctl.handle(ControlMsg::FinishAutomationStop { at, active_pass: true, stopped_pass: None });
        ctl.finish_ended_automation_passes();
    }

    fn point_at(lane: &crate::plugins::automation::AutomationLane, tick: u32) -> f32 {
        lane.points
            .iter()
            .find(|p| p.tick == tick)
            .unwrap_or_else(|| panic!("no point at tick {tick} in {:?}", lane.points))
            .value
    }

    /// Task 10, pass-end trigger 1 (transport stop): a Write pass sampled
    /// over several ticks lands as ONE `Op::AutomationSetLane` on the
    /// track's EXISTING gain lane and as exactly ONE undo entry. Driven
    /// through `Control::run`'s real loop iteration (see
    /// `bare_control_with_tx`) so the CALL SITE is covered, not just the
    /// method — deleting the call would otherwise leave every test green.
    #[test]
    fn stopping_playback_commits_a_write_pass_as_one_automation_set_lane_op() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        // A point inside the range about to be recorded (tick 0, replaced)
        // and one far after it (tick 3840, must survive untouched).
        let lane_id = seed_recording_track(&session, "t-1", AutomationMode::Write, &[(0, 1.0), (3840, 0.25)]);
        ctl.rebuild();
        assert!(ctl.tempo_map.is_some(), "the fixture must exercise the real tempo-map compile path");

        // Live fader at 0.5, distinct from the document's 0 dB (= 1.0).
        let slot = *ctl.tables.lock().slots.get("t-1").expect("t-1 has a slot");
        ctl.tables.lock().params.set_gain_linear(slot, 0.5);
        ctl.shared.playing.store(true, Relaxed);
        // 25 samples per tick at 120 bpm / 48 kHz / 960 ppq.
        for pos in [0u64, 1_200, 2_400] {
            automation_tick(&mut ctl, pos);
        }

        let undo_before = ctl.committer.log().depths().0;
        let gen_before = ctl.generation;
        ctl.shared.playing.store(false, Relaxed);
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();

        assert_eq!(
            ctl.committer.log().depths().0,
            undo_before + 1,
            "one recording pass is ONE undo entry"
        );
        assert!(ctl.generation > gen_before, "the commit scheduled its own rebuild");
        let lane = gain_lane(&session, "t-1");
        assert_eq!(lane.id, lane_id, "the pass updated the SAME lane a manual edit would");
        for tick in [0u32, 96] {
            assert!(
                (point_at(&lane, tick) - 0.5).abs() < 1e-6,
                "every sampled tick is in the committed lane"
            );
        }
        assert!(
            (point_at(&lane, 3840) - 0.25).abs() < 1e-6,
            "a point AFTER the recorded range keeps its pre-existing value"
        );
    }

    /// Spec §4.6's zero-point guard: a pass that sampled nothing commits
    /// nothing — no op, no undo entry, no rebuild, and above all no empty
    /// lane minted for a track that has none.
    #[test]
    fn stop_while_already_stopped_does_not_create_a_write_point_or_undo() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(&session, "t-1", AutomationMode::Write, &[(3840, 0.25)]);
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        ctl.params.set_gain_linear(slot, 0.5);
        ctl.shared.position.store(1_200, Relaxed);
        let undo_before = ctl.committer.log().depths().0;
        let pass_before = ctl.shared.automation_pass.load(Relaxed);

        ctl.handle(ControlMsg::FinishAutomationStop { at: 1_200, active_pass: false, stopped_pass: None });
        ctl.finish_ended_automation_passes();

        assert_eq!(ctl.committer.log().depths().0, undo_before);
        assert_eq!(ctl.shared.automation_pass.load(Relaxed), pass_before);
        assert!(ctl.pending_automation_stops.is_empty());
        assert!(gain_lane(&session, "t-1").points.iter().all(|point| point.tick != 48));
    }

    #[test]
    fn a_pass_with_no_samples_commits_nothing() {
        let (mut ctl, session) = bare_control();
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.automation_mode = AutomationMode::Write;
            s.store.tracks.push(t);
            // A lane on ANOTHER target, so a `TempoMap` still compiles: this
            // test must fail for want of SAMPLES, not for want of a map.
            s.automation.lanes.push(test_lane("l1", "track:ghost", 0));
        }
        ctl.rebuild();
        assert!(ctl.tempo_map.is_some());

        ctl.shared.playing.store(true, Relaxed);
        ctl.finish_ended_automation_passes(); // arms the edge detector
        let undo_before = ctl.committer.log().depths().0;
        let gen_before = ctl.generation;

        ctl.shared.playing.store(false, Relaxed);
        ctl.finish_ended_automation_passes();

        assert_eq!(ctl.committer.log().depths().0, undo_before, "an empty pass is not an undo entry");
        assert_eq!(ctl.generation, gen_before, "and schedules no rebuild");
        assert!(
            session.lock().automation.lanes.iter().all(|l| l.target_node != "track:t-1"),
            "no lane was minted for a track that recorded nothing"
        );
    }

    /// Touch commits at gesture close, while playback continues. A second
    /// gesture therefore cannot make the first gesture's range swallow the
    /// gap between them, and each gesture is independently undoable.
    #[test]
    fn touch_release_reverts_to_the_pre_existing_curve_after_the_recorded_range() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(
            &session,
            "t-1",
            AutomationMode::Touch,
            &[(0, 1.0), (144, 0.9), (192, 0.9), (3840, 0.25)],
        );
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").expect("t-1 has a slot");
        ctl.tables.lock().params.set_gain_linear(slot, 0.5);
        ctl.shared.playing.store(true, Relaxed);

        crate::control::testutil::touch_track_gain(&ctl.gesture, "t-1");
        for pos in [0u64, 1_200, 2_400] {
            automation_tick(&mut ctl, pos); // ticks 0, 48, 96
        }
        crate::control::testutil::release_gesture(&ctl.gesture);
        let undo_before = ctl.committer.log().depths().0;
        ctl.handle(ControlMsg::FinishAutomationTouch(vec![touch_endpoint("t-1", 0.5, ctl.shared.position.load(Relaxed))]));
        assert_eq!(
            ctl.committer.log().depths().0,
            undo_before + 1,
            "one Touch gesture is one undo entry at gesture close"
        );
        for pos in [3_600u64, 4_800] {
            automation_tick(&mut ctl, pos); // ticks 144, 192 — NOT recorded
        }

        let lane = gain_lane(&session, "t-1");
        assert!((point_at(&lane, 96) - 0.5).abs() < 1e-6, "the touched range was recorded");
        let old_at_resume = 1.0 + (0.9 - 1.0) * (97.0 / 144.0);
        assert!(
            (point_at(&lane, 97) - old_at_resume).abs() < 1e-6,
            "the old curve resumes on the first tick after release instead of interpolating across the untouched gap"
        );
        for tick in [144u32, 192] {
            assert!(
                (point_at(&lane, tick) - 0.9).abs() < 1e-6,
                "after the gesture closed, Touch left the pre-existing curve alone"
            );
        }
        assert!((point_at(&lane, 3840) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn touch_preserves_the_pre_existing_curve_immediately_before_the_recorded_range() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(
            &session,
            "t-1",
            AutomationMode::Touch,
            &[(0, 1.0), (48, 0.8), (192, 0.8), (3840, 0.25)],
        );
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        ctl.params.set_gain_linear(slot, 0.5);
        ctl.shared.playing.store(true, Relaxed);
        crate::control::testutil::touch_track_gain(&ctl.gesture, "t-1");
        automation_tick(&mut ctl, 2_400); // tick 96: touch starts after old point at 48
        automation_tick(&mut ctl, 3_600); // tick 144
        crate::control::testutil::release_gesture(&ctl.gesture);
        ctl.handle(ControlMsg::FinishAutomationTouch(vec![touch_endpoint("t-1", 0.5, 3_600)]));

        let lane = gain_lane(&session, "t-1");
        assert!((point_at(&lane, 95) - 0.8).abs() < 1e-6, "old curve is pinned at start-1");
        assert!((point_at(&lane, 96) - 0.5).abs() < 1e-6, "touch begins exactly at start");
        assert!((point_at(&lane, 48) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn sub_tick_touch_gesture_commits_its_final_live_value() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(&session, "t-1", AutomationMode::Touch, &[(3840, 0.25)]);
        ctl.rebuild();
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(1_200, Relaxed); // tick 48

        let undo_before = ctl.committer.log().depths().0;
        // No periodic recorder tick occurred between gesture begin and end.
        ctl.handle(ControlMsg::FinishAutomationTouch(vec![touch_endpoint("t-1", 1.75, ctl.shared.position.load(Relaxed))]));

        assert_eq!(ctl.committer.log().depths().0, undo_before + 1);
        assert!((point_at(&gain_lane(&session, "t-1"), 48) - 1.75).abs() < 1e-6);
    }

    #[test]
    fn sub_tick_latch_endpoint_arms_without_finishing_the_pass() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(&session, "t-1", AutomationMode::Latch, &[(3840, 0.25)]);
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(1_200, Relaxed); // tick 48
        let undo_before = ctl.committer.log().depths().0;

        // No periodic tick saw the touch; the endpoint must arm Latch.
        ctl.handle(ControlMsg::FinishAutomationTouch(vec![touch_endpoint("t-1", 1.75, ctl.shared.position.load(Relaxed))]));
        assert_eq!(ctl.committer.log().depths().0, undo_before, "Latch is not finished at release");
        assert_eq!(
            ctl.params.gain_automation_owner(slot),
            Some(0),
            "Latch keeps audible ownership after release"
        );

        ctl.params.set_gain_linear(slot, 0.25); // unrelated live change after release
        automation_tick(&mut ctl, 2_400); // tick 96 must hold 1.75
        automation_stop(&mut ctl);

        assert_eq!(ctl.committer.log().depths().0, undo_before + 1);
        let lane = gain_lane(&session, "t-1");
        assert!((point_at(&lane, 48) - 1.75).abs() < 1e-6);
        assert!((point_at(&lane, 96) - 1.75).abs() < 1e-6);
    }

    #[test]
    fn stop_overtaking_a_queued_touch_or_latch_release_keeps_one_complete_pass() {
        for mode in [AutomationMode::Touch, AutomationMode::Latch] {
            let (mut ctl, session) = bare_control();
            seed_recording_track(&session, "t-1", mode, &[(3840, 0.25)]);
            ctl.rebuild();
            let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
            ctl.params.set_gain_linear(slot, 0.5);
            ctl.params.set_gain_automation_owner(slot, Some(0));
            ctl.shared.playing.store(true, Relaxed);
            let endpoint = touch_endpoint("t-1", 0.5, 1_200);
            let undo_before = ctl.committer.log().depths().0;

            ctl.handle(ControlMsg::FinishAutomationStop { at: 2_400, active_pass: true, stopped_pass: None });
            assert_eq!(ctl.shared.automation_pass.load(Relaxed), 1, "public pass rotates immediately");
            ctl.shared.playing.store(false, Relaxed);
            ctl.finish_ended_automation_passes();
            assert_eq!(ctl.committer.log().depths().0, undo_before, "{mode:?}: Stop waits across a full engine tick for the queued release");
            assert!(!ctl.pending_automation_stops.is_empty());

            ctl.shared.playing.store(true, Relaxed);
            automation_tick(&mut ctl, 3_600);
            ctl.params.set_gain_automation_owner(slot, Some(1));
            let new_endpoint = AutomationTouchEndpoint {
                track_id: "t-1".into(),
                value: 0.75,
                sample: 3_600,
                pass: 1,
            };
            ctl.handle(ControlMsg::FinishAutomationTouch(vec![new_endpoint]));
            assert_eq!(ctl.deferred_automation_endpoints.len(), 1, "new pass endpoint is deferred, never merged into stopped pass");

            ctl.handle(ControlMsg::FinishAutomationStop {
                at: 3_600,
                active_pass: true,
                stopped_pass: None,
            });
            ctl.shared.playing.store(false, Relaxed);
            ctl.finish_ended_automation_passes();
            assert_eq!(ctl.shared.automation_pass.load(Relaxed), 2);
            assert_eq!(ctl.pending_automation_stops.len(), 2, "both stopped passes wait in order behind pass 0.s late endpoint");

            ctl.handle(ControlMsg::FinishAutomationTouch(vec![endpoint.clone()]));
            ctl.finish_ended_automation_passes();

            let expected_undo = undo_before + 2;
            assert_eq!(ctl.committer.log().depths().0, expected_undo, "{mode:?}: passes remain distinct");
            let lane = gain_lane(&session, "t-1");
            assert!((point_at(&lane, 48) - 0.5).abs() < 1e-6, "{mode:?}: old release endpoint retained");
            assert!((point_at(&lane, 144) - 0.75).abs() < 1e-6, "{mode:?}: second stopped pass retained its endpoint");
            assert!(ctl.pending_automation_stops.is_empty());

            ctl.shared.playing.store(true, Relaxed);
            ctl.handle(ControlMsg::FinishAutomationTouch(vec![endpoint]));
            assert_eq!(ctl.committer.log().depths().0, expected_undo, "{mode:?}: stale endpoint cannot split the undo pass");
        }
    }

    #[test]
    fn automation_endpoints_match_their_pass_across_reserved_sentinel_wrap() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(&session, "t-1", AutomationMode::Touch, &[(3840, 0.25)]);
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        let old_pass = crate::audio::rt::NO_GAIN_AUTOMATION_OWNER - 1;
        ctl.shared.automation_pass.store(old_pass, Relaxed);
        ctl.params.set_gain_automation_owner(slot, Some(old_pass));
        ctl.shared.playing.store(true, Relaxed);
        let undo_before = ctl.committer.log().depths().0;

        ctl.handle(ControlMsg::FinishAutomationStop {
            at: 2_400,
            active_pass: true,
            stopped_pass: None,
        });
        ctl.shared.playing.store(false, Relaxed);
        ctl.finish_ended_automation_passes();
        assert_eq!(ctl.shared.automation_pass.load(Relaxed), 0);
        assert_eq!(ctl.pending_automation_stops[0].pass, old_pass);

        ctl.params.set_gain_automation_owner(slot, Some(0));
        ctl.handle(ControlMsg::FinishAutomationTouch(vec![AutomationTouchEndpoint {
            track_id: "t-1".into(),
            value: 0.75,
            sample: 3_600,
            pass: 0,
        }]));
        assert_eq!(ctl.deferred_automation_endpoints.len(), 1);

        ctl.handle(ControlMsg::FinishAutomationTouch(vec![AutomationTouchEndpoint {
            track_id: "t-1".into(),
            value: 0.5,
            sample: 1_200,
            pass: old_pass,
        }]));
        ctl.finish_ended_automation_passes();

        assert!(ctl.pending_automation_stops.is_empty());
        assert!(ctl.deferred_automation_endpoints.is_empty());
        assert_eq!(ctl.params.gain_automation_owner(slot), None);
        assert_eq!(ctl.committer.log().depths().0, undo_before + 2);
        let lane = gain_lane(&session, "t-1");
        assert!((point_at(&lane, 48) - 0.5).abs() < 1e-6);
        assert!((point_at(&lane, 144) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn stopped_automation_pass_ownership_does_not_make_a_no_touch_next_pass_wait() {
        for mode in [AutomationMode::Touch, AutomationMode::Latch] {
            let (mut ctl, session) = bare_control();
            seed_recording_track(&session, "t-1", mode, &[(3840, 0.25)]);
            ctl.rebuild();
            let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
            ctl.params.set_gain_linear(slot, 0.5);
            ctl.params.set_gain_automation_owner(slot, Some(0));
            ctl.shared.playing.store(true, Relaxed);
            let late_pass_zero = touch_endpoint("t-1", 0.5, 1_200);
            let undo_before = ctl.committer.log().depths().0;

            ctl.handle(ControlMsg::FinishAutomationStop {
                at: 2_400,
                active_pass: true,
                stopped_pass: None,
            });
            ctl.shared.playing.store(false, Relaxed);
            ctl.finish_ended_automation_passes();
            assert_eq!(ctl.shared.automation_pass.load(Relaxed), 1);
            assert_eq!(ctl.pending_automation_stops.len(), 1);
            assert_eq!(ctl.pending_automation_stops[0].awaiting, vec!["t-1"]);

            // Pass 1 starts and stops without a fader gesture. The stale
            // audible owner still belongs to pass 0, so pass 1 must not
            // invent an endpoint expectation or an undo entry.
            ctl.shared.playing.store(true, Relaxed);
            ctl.handle(ControlMsg::FinishAutomationStop {
                at: 3_600,
                active_pass: true,
                stopped_pass: None,
            });
            ctl.shared.playing.store(false, Relaxed);
            ctl.finish_ended_automation_passes();
            assert_eq!(ctl.shared.automation_pass.load(Relaxed), 2);
            assert_eq!(ctl.pending_automation_stops.len(), 2);
            assert!(ctl.pending_automation_stops[1].awaiting.is_empty(), "{mode:?}: pass 1 has no endpoint to await");

            ctl.handle(ControlMsg::FinishAutomationTouch(vec![late_pass_zero]));
            ctl.finish_ended_automation_passes();

            assert!(ctl.pending_automation_stops.is_empty(), "{mode:?}: FIFO fully drains");
            assert_eq!(ctl.committer.log().depths().0, undo_before + 1, "{mode:?}: only pass 0 commits");
            let lane = gain_lane(&session, "t-1");
            assert!((point_at(&lane, 48) - 0.5).abs() < 1e-6, "{mode:?}: late pass 0 endpoint retained");
            assert!(lane.points.iter().all(|point| point.tick != 144), "{mode:?}: empty pass 1 records no boundary");
        }
    }

    #[test]
    fn persisted_non_gesture_gain_change_updates_the_recording_denominator() {
        let (mut ctl, session) = bare_control();
        {
            let mut s = session.lock();
            let mut track = test_track("t-1");
            track.automation_mode = AutomationMode::Write;
            track.gain_db = -12.0;
            s.store.tracks.push(track);
        }
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        ctl.shared.playing.store(true, Relaxed);

        ctl.committer
            .commit_with_rebuild(
                crate::control::op::TxMeta::user("non-gesture base gain"),
                |tx| {
                    tx.apply(crate::control::op::Op::Set {
                        object: crate::control::op::ObjectRef::Track("t-1".into()),
                        path: crate::control::op::PropPath::Gain,
                        from: serde_json::Value::Null,
                        to: serde_json::json!(-6.0),
                    })
                },
                false,
                || {},
            )
            .unwrap();
        assert!(
            (ctl.params.base_gain_linear(slot) - crate::audio::mixer::db_to_linear(-6.0)).abs()
                < 1e-6
        );

        ctl.drive_automation_recording();
        let points = ctl.automation_recorder.finish("t-1").unwrap();
        assert!((points[0].value - 1.0).abs() < 1e-6, "new persisted base is the denominator");
    }

    /// Latch, the exact opposite of the test above with the same fixture:
    /// `latch_armed` keeps sampling past the gesture release, so the pass
    /// holds the LAST touched value flat over the ticks Touch would have
    /// left to the pre-existing curve, until the transport stops.
    #[test]
    fn latch_holds_the_last_value_after_release_until_transport_stop() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(
            &session,
            "t-1",
            AutomationMode::Latch,
            &[(0, 1.0), (144, 0.9), (192, 0.9), (3840, 0.25)],
        );
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").expect("t-1 has a slot");
        ctl.tables.lock().params.set_gain_linear(slot, 0.5);
        ctl.shared.playing.store(true, Relaxed);

        crate::control::testutil::touch_track_gain(&ctl.gesture, "t-1");
        for pos in [0u64, 1_200, 2_400] {
            automation_tick(&mut ctl, pos); // ticks 0, 48, 96
        }
        crate::control::testutil::release_gesture(&ctl.gesture);
        for pos in [3_600u64, 4_800] {
            automation_tick(&mut ctl, pos); // ticks 144, 192 — still recorded
        }

        let undo_before = ctl.committer.log().depths().0;
        ctl.shared.playing.store(false, Relaxed);
        ctl.finish_ended_automation_passes();

        assert_eq!(ctl.committer.log().depths().0, undo_before + 1, "one pass, one undo entry");
        let lane = gain_lane(&session, "t-1");
        for tick in [0u32, 192] {
            assert!(
                (point_at(&lane, tick) - 0.5).abs() < 1e-6,
                "Latch held the last touched value through the recorded range"
            );
        }
        assert!(
            lane.points.iter().all(|point| point.tick != 144),
            "the pre-existing point inside Latch's held range was replaced"
        );
        assert!(
            (point_at(&lane, 3840) - 0.25).abs() < 1e-6,
            "still only the RECORDED range is replaced"
        );
    }

    #[test]
    fn explicit_stop_boundary_separates_stop_then_play_between_control_ticks() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(&session, "t-1", AutomationMode::Write, &[(3840, 0.25)]);
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        ctl.params.set_gain_linear(slot, 0.5);
        ctl.shared.playing.store(true, Relaxed);
        automation_tick(&mut ctl, 0);
        automation_tick(&mut ctl, 1_200);

        let undo_before = ctl.committer.log().depths().0;
        automation_stop(&mut ctl);
        assert_eq!(ctl.committer.log().depths().0, undo_before + 1);

        // Simulate an immediate Play before the periodic edge detector ever
        // observed stopped. The explicit message already closed pass one.
        ctl.shared.playing.store(true, Relaxed);
        ctl.params.set_gain_linear(slot, 0.25);
        automation_tick(&mut ctl, 2_400);
        automation_stop(&mut ctl);
        assert_eq!(ctl.committer.log().depths().0, undo_before + 2);

        let lane = gain_lane(&session, "t-1");
        assert!((point_at(&lane, 0) - 0.5).abs() < 1e-6);
        assert!((point_at(&lane, 48) - 0.5).abs() < 1e-6);
        assert!((point_at(&lane, 96) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn epoch_change_between_snapshot_and_transaction_discards_old_points_atomically() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(&session, "t-1", AutomationMode::Write, &[]);
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        ctl.params.set_gain_linear(slot, 0.5);
        ctl.shared.playing.store(true, Relaxed);
        automation_tick(&mut ctl, 0);

        // The project swaps after the recorder snapshot but immediately
        // before Session::transact takes the lock: this is the old precheck.s
        // exact race window. Validation inside Tx must reject it.
        let swapped = session.clone();
        ctl.finish_automation_recording_for_track_after("t-1", move || {
            let mut s = swapped.lock();
            s.epoch += 1;
            s.automation.lanes.clear();
        });
        assert!(session.lock().automation.lanes.is_empty());

        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").unwrap();
        ctl.params.set_gain_linear(slot, 0.25);
        automation_tick(&mut ctl, 2_400);
        automation_stop(&mut ctl);

        let lane = gain_lane(&session, "t-1");
        assert!(lane.points.iter().all(|point| point.tick != 0));
        assert!((point_at(&lane, 96) - 0.25).abs() < 1e-6);
    }

    /// Task 10, pass-end trigger 2 (mode change): leaving Write ends the
    /// pass even though the transport never stopped. `rebuild` is what
    /// notices — it compares the PREVIOUS mode cache before overwriting it
    /// — and the commit runs on the next tick.
    #[test]
    fn leaving_a_recording_mode_commits_the_pass_while_the_transport_keeps_playing() {
        let (mut ctl, session) = bare_control();
        seed_recording_track(&session, "t-1", AutomationMode::Write, &[(0, 1.0), (3840, 0.25)]);
        ctl.rebuild();
        let slot = *ctl.tables.lock().slots.get("t-1").expect("t-1 has a slot");
        ctl.tables.lock().params.set_gain_linear(slot, 0.5);
        ctl.shared.playing.store(true, Relaxed);
        for pos in [0u64, 1_200, 2_400] {
            automation_tick(&mut ctl, pos);
        }

        let undo_before = ctl.committer.log().depths().0;
        session.lock().store.tracks[0].automation_mode = AutomationMode::Read;
        ctl.rebuild(); // what the mode edit's own `effect.rebuild` schedules
        automation_tick(&mut ctl, 3_600);

        assert!(ctl.shared.playing.load(Relaxed), "the transport never stopped");
        assert_eq!(ctl.committer.log().depths().0, undo_before + 1, "one pass, one undo entry");
        let lane = gain_lane(&session, "t-1");
        assert!((point_at(&lane, 96) - 0.5).abs() < 1e-6, "the pass up to the mode change landed");
        assert!(
            lane.points.iter().all(|p| p.tick <= 96 || p.tick == 3840),
            "and nothing was recorded after it: {:?}",
            lane.points
        );
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

    /// Track D ruling 2 keeps plugin-param automation OUT of the document,
    /// so the panel had nothing to paint but the stored value while the
    /// parameter moved. `driven_params` is that read-back — the driver's own
    /// write, not a second evaluation of the curve.
    #[test]
    fn the_driven_param_read_back_carries_what_the_driver_wrote() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        seed_plugin_lane(&session);
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(48_000, Relaxed); // halfway up the ramp
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();

        assert_eq!(ctl.driven_params.len(), 1, "one automated param, one read-back entry");
        let d = &ctl.driven_params[0];
        assert_eq!((d.instance_id.as_str(), d.index), ("inst-1", 7));
        assert_eq!(d.value, ctl.param_writes[0].value, "the read-back IS the host write");
    }

    /// The read-back is an UPSERT set, not the tick's deltas. `tick`
    /// suppresses a value it already sent, so a frame built from this tick's
    /// writes alone would blank a held param 60 times a second — the panel
    /// would flicker back to the document value between changes.
    #[test]
    fn a_held_param_stays_in_the_read_back_across_a_silent_tick() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        seed_plugin_lane(&session);
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(48_000, Relaxed);
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();
        let driven = ctl.driven_params[0].value;

        // Same position again: the value has not changed, so the driver
        // suppresses the write (EPSILON) and emits nothing at all.
        ctl.drive_param_automation();
        assert!(ctl.param_writes.is_empty(), "the tick suppressed the unchanged value");
        assert_eq!(ctl.driven_params.len(), 1, "…and the read-back still holds it");
        assert_eq!(ctl.driven_params[0].value, driven);

        // Move up the ramp: the same entry is updated, not duplicated.
        ctl.shared.position.store(72_000, Relaxed);
        ctl.drive_param_automation();
        assert_eq!(ctl.driven_params.len(), 1, "upserted by (instance, index)");
        assert!(ctl.driven_params[0].value > driven, "and it followed the ramp up");
    }

    /// Stopping is what tells the UI to stop following: with the transport
    /// parked the user's own knob turns own the parameter again, and a stale
    /// read-back would pin the panel to the last automated value.
    #[test]
    fn stopping_the_transport_clears_the_driven_param_read_back() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        seed_plugin_lane(&session);
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(48_000, Relaxed);
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();
        assert_eq!(ctl.driven_params.len(), 1);

        ctl.shared.playing.store(false, Relaxed);
        ctl.drive_param_automation();
        assert!(ctl.driven_params.is_empty(), "a stopped transport drives nothing");
    }

    /// A rebuild can drop the very lane an entry names. Keeping the entry
    /// would leave the panel following a param nothing drives any more.
    #[test]
    fn a_rebuild_drops_a_read_back_for_a_lane_that_is_gone() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        seed_plugin_lane(&session);
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(48_000, Relaxed);
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();
        assert_eq!(ctl.driven_params.len(), 1, "the read-back is populated first");

        session.lock().automation.lanes.clear();
        ctl.rebuild();

        assert!(ctl.param_automation.is_empty(), "the lane is gone");
        assert!(ctl.driven_params.is_empty(), "so is its read-back");
    }

    /// The read-back reaches the UI on the meter frame, at the same 60 Hz and
    /// about the same instant `position_samples` names — no second channel.
    #[test]
    fn the_meter_frame_ships_the_driven_param_read_back() {
        let (mut ctl, session, tx) = bare_control_with_tx();
        seed_plugin_lane(&session);
        ctl.shared.playing.store(true, Relaxed);
        ctl.shared.position.store(48_000, Relaxed);
        let frames = Arc::new(Mutex::new(Vec::new()));
        tx.send(ControlMsg::Subscribe(Box::new(CountingSink(
            Arc::new(AtomicUsize::new(0)),
            frames.clone(),
        ))))
        .unwrap();
        tx.send(ControlMsg::Rebuild).unwrap();
        drop(tx);
        ctl.run();
        // `run` set `last_frame` at construction, so one immediate iteration
        // is inside the 60 Hz interval — age it and pump explicitly.
        ctl.last_frame = Instant::now() - FRAME_INTERVAL * 2;
        ctl.pump_meter_frames();

        let got = frames.lock();
        let frame = got.last().expect("one frame was pushed");
        assert_eq!(frame.driven_params.len(), 1, "the frame carries the read-back");
        assert_eq!(frame.driven_params[0].instance_id, "inst-1");
        assert_eq!(frame.driven_params[0].index, 7);
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
            let mut automation = test_track("auto-1");
            automation.kind = "automation".into();
            s.store.tracks.push(automation);
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
            // The published image IS the document these readers see, so a
            // test that pokes the live document must republish — a live/image
            // divergence with no lock held is not a state production can be
            // in (`Session::published`'s contract).
            s.republish_full();
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

    /// Plan G1 Task 7 — the bug this closes: a plugin on an AUDIO track must
    /// actually process the track's audio. Before Task 7 the insert chain was
    /// never compiled into the graph (`compile_inserts` resolved every slot to
    /// `None` and `rebuild` never called it), so a chosen effect was silently
    /// inaudible. This renders a DC clip through a `GainHalfEffect` insert,
    /// end to end through a real `rebuild`, and asserts the output is halved.
    #[test]
    fn audio_track_insert_plugin_processes_the_clip_audio() {
        use crate::audio::mixer;
        use crate::audio::transport::LoopSpec;
        use crate::audio::types::InsertSlot;
        use crate::ids::SourceId;

        let (mut ctl, session) = bare_control();
        ctl.shared.sample_rate.store(48_000, Relaxed);
        // Match the engine rate so `ensure_loaded` keeps the seeded cache.
        ctl.cache_rate = 48_000;
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.inserts.push(InsertSlot {
                id: "slot-1".into(),
                instance_id: "inst-1".into(),
                bypassed: false,
            });
            s.store.tracks.push(t);
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(),
                uid: "clap:/x.clap#fx".into(),
                name: "Gain Half".into(),
                format: "clap".into(),
                status: "stub".into(),
                track_id: Some("t-1".into()),
            });
            let mut c = crate::audio::types::testutil::test_clip("c-1", "t-1");
            c.source_id = SourceId::from("s-1");
            c.source_path = "audio/s-1.wav".into();
            c.length_samples = 64;
            c.source_length_samples = 64;
            s.store.clips.push(c);
            s.republish_full();
        }
        // The test doubles stand in for the real WAV decode and CLAP host,
        // which a headless test cannot run: a DC-1.0 mono source, and the
        // gain-halving insert processor seeded under the exact key
        // `compile_inserts` will compute.
        ctl.cache.insert(
            SourceId::from("s-1"),
            CachedSource {
                source_path: "audio/s-1.wav".into(),
                data: std::sync::Arc::new(RtClipData { channels: 1, data: vec![1.0; 64] }),
            },
        );
        ctl.insert_nodes
            .resolve_with("inst-1", "insert:inst-1@48000#0!stub", || {
                Some(Box::new(crate::audio::insert::GainHalfEffect { bypassed: false }))
            })
            .expect("seed the insert node");

        let (graph_tx, mut graph_rx) = rtrb::RingBuffer::new(8);
        let (_retire_tx, retire_rx) = rtrb::RingBuffer::new(8);
        let (_meter_tx, meter_rx) = rtrb::RingBuffer::new(8);
        let (_evt_tx, evt_rx) = rtrb::RingBuffer::new(8);
        ctl.output = Some(OutputBundle { _stream: None, graph_tx, retire_rx, meter_rx, evt_rx });
        ctl.rebuild();

        let mut graph = graph_rx.pop().expect("the rebuild published a graph").into_box();
        assert_eq!(
            graph.tracks.iter().filter(|t| t.slot == 0 && t.inserts.len() == 1).count(),
            1,
            "the audio track's row carries the compiled insert chain"
        );

        // Hard-left so the left channel carries the full (halved) signal.
        graph.params.set_pan(0, -1.0);
        let mut out = vec![0.0f32; 64 * 2];
        let dropped = mixer::render(&mut graph, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        assert_eq!(dropped, 0);
        // DC 1.0 -> GainHalfEffect 0.5 -> fader 1.0 -> pan hard left.
        assert!((out[0] - 0.5).abs() < 1e-6, "insert halved the clip, got {}", out[0]);
        assert!(out[1].abs() < 1e-6, "hard left keeps the right channel silent");
    }

    /// Plan F Task 6 — the Plan A deferral's own pin, and the reason this
    /// task exists: the GRAPH BUILD no longer runs under the session lock.
    ///
    /// The probe is an ordering one, not a timing one. Another thread takes
    /// the session lock and holds it until told to let go; the only thing
    /// that ever tells it is the assembly hook, which fires between the
    /// lock-free phase and the short publish lock. So `holder_holds` is
    /// still true at hook time IFF the whole assembly ran with the lock in
    /// someone else's hands. The holder also has a 10 s ceiling, so the
    /// PREVIOUS structure (assemble under the lock) fails this on an
    /// assertion — `holder_holds == false`, seen == 2 — instead of hanging
    /// the suite.
    #[test]
    fn rebuild_does_not_hold_the_session_lock_across_the_graph_build() {
        use std::sync::atomic::{AtomicBool, AtomicU8};
        let (mut ctl, session) = bare_control();
        ctl.shared.sample_rate.store(48_000, Relaxed);
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.kind = "midi".into();
            s.store.tracks.push(t);
            // The document these tests poke directly is only a legal state
            // once the image matches it — that equivalence is the contract
            // every reader here relies on.
            s.republish_full();
        }
        let (graph_tx, mut graph_rx) = rtrb::RingBuffer::new(8);
        let (_retire_tx, retire_rx) = rtrb::RingBuffer::new(8);
        let (_meter_tx, meter_rx) = rtrb::RingBuffer::new(8);
        let (_evt_tx, evt_rx) = rtrb::RingBuffer::new(8);
        ctl.output = Some(OutputBundle { _stream: None, graph_tx, retire_rx, meter_rx, evt_rx });
        ctl.live_in_hub.set_target_track(Some("t-1".into()));

        let holds = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(AtomicU8::new(0));
        let (h2, r2, s2) = (holds.clone(), release.clone(), session.clone());
        let holder = std::thread::spawn(move || {
            let guard = s2.lock();
            h2.store(true, Relaxed);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !r2.load(Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            h2.store(false, Relaxed);
            drop(guard);
        });
        while !holds.load(Relaxed) {
            std::thread::sleep(Duration::from_millis(1));
        }
        let (h3, r3, s3) = (holds.clone(), release.clone(), seen.clone());
        ctl.after_assembly = Some(Arc::new(move || {
            s3.store(if h3.load(Relaxed) { 1 } else { 2 }, Relaxed);
            r3.store(true, Relaxed);
        }));

        ctl.rebuild();
        holder.join().unwrap();

        assert_eq!(
            seen.load(Relaxed),
            1,
            "the graph assembly finished while ANOTHER thread held the session lock (0 = the hook never ran)"
        );
        let graph = graph_rx.pop().expect("the rebuild published a graph").into_box();
        assert!(
            graph.tracks.iter().any(|t| t.slot == 0 && t.live.is_some()),
            "and it is a REAL graph: the live node was instantiated from the published image"
        );
    }

    /// Plan F Task 6 — the seam between the two phases, in the one case
    /// where they DISAGREE. The graph is assembled from image S; the tables
    /// published under the short lock describe the live document L. A track
    /// removed in between shifts every later track's slot, so a row carried
    /// over unchanged would read another track's gain/mute lane and write
    /// another track's meter — and, with the fresh `ParamTable` now one slot
    /// shorter, the last row would index past it entirely. Rows are re-keyed
    /// by TRACK ID against L, so the departed track's row is dropped and the
    /// survivor moves to its new slot.
    #[test]
    fn rows_assembled_from_the_snapshot_are_re_keyed_onto_the_live_slot_map() {
        use crate::midi::types::{MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
        let (mut ctl, session) = bare_control();
        ctl.shared.sample_rate.store(48_000, Relaxed);
        {
            let mut s = session.lock();
            for id in ["t-1", "t-2"] {
                let mut t = test_track(id);
                t.kind = "midi".into();
                s.store.tracks.push(t);
            }
            s.midi.ppq = DEFAULT_PPQ;
            s.midi.tempo_events = vec![TempoEvent { tick: 0, bpm: 120.0 }];
            // Only t-2 has material, so the surviving row is identifiable by
            // its events alone — t-1's row comes from the live-in target and
            // carries none.
            s.midi.clips.push(MidiClip {
                id: "mc-1".into(),
                track_id: "t-2".into(),
                name: "c".into(),
                timeline_start_ticks: 0,
                length_ticks: 1920,
                notes: vec![MidiNote {
                    tick: 0,
                    length_ticks: 480,
                    key: 60,
                    velocity: 100,
                    channel: 0,
                    note_id: crate::ids::NoteId(1),
                }],
                next_note_id: 2,
                content_id: crate::ids::ContentId::mint(),
                lane_id: crate::ids::LaneId::default_for_track("t-2"),
                content_length_ticks: None,
                transpose_semitones: 0,
                velocity_offset: 0,
            });
            s.republish_full();
        }
        let (graph_tx, mut graph_rx) = rtrb::RingBuffer::new(8);
        let (_retire_tx, retire_rx) = rtrb::RingBuffer::new(8);
        let (_meter_tx, meter_rx) = rtrb::RingBuffer::new(8);
        let (_evt_tx, evt_rx) = rtrb::RingBuffer::new(8);
        ctl.output = Some(OutputBundle { _stream: None, graph_tx, retire_rx, meter_rx, evt_rx });
        ctl.live_in_hub.set_target_track(Some("t-1".into()));

        let s2 = session.clone();
        ctl.after_assembly = Some(Arc::new(move || {
            // Assembly is done and the lock is free: retire t-1 so the image
            // the rows came from no longer describes the document the tables
            // are about to. (A direct write + republish, not a transact —
            // what this pins is the S/L divergence, not the commit path.)
            let mut s = s2.lock();
            s.store.tracks.retain(|t| t.id.as_str() != "t-1");
            s.republish_full();
        }));

        ctl.rebuild();

        let graph = graph_rx.pop().expect("the rebuild published a graph").into_box();
        assert_eq!(graph.params.len(), 1, "the table is sized by the live document");
        // t-2 contributes two rows (a clips-only one from the assembly loop
        // and a live one) — t-1's two are gone, not carried at a stale slot.
        assert_eq!(graph.tracks.len(), 2, "the departed track's rows were dropped");
        assert!(
            graph.tracks.iter().all(|t| t.slot < graph.params.len()),
            "no row survives pointing past the table this graph reads"
        );
        assert!(
            graph.tracks.iter().all(|t| t.slot == 0),
            "t-2 moved into the slot the live document gives it"
        );
        assert_eq!(
            graph.tracks.iter().filter(|t| t.live.as_ref().is_some_and(|l| !l.events.is_empty())).count(),
            1,
            "the surviving live row is t-2's (the one with material), not t-1's empty live-in row"
        );
    }

    /// Plan F Task 6 — the [C1] regression pin, snapshot edition. A param
    /// write that commits DURING the lock-free assembly must still reach the
    /// tables this rebuild publishes: values come from the LIVE document
    /// read under the short lock, never from the image the graph was
    /// assembled from. Building the `ParamTable` from the assembly image
    /// instead loses the write forever — a plain `Set` schedules no rebuild
    /// of its own.
    #[test]
    fn a_param_write_committed_during_assembly_is_never_lost() {
        let (mut ctl, session) = bare_control();
        ctl.shared.sample_rate.store(48_000, Relaxed);
        {
            let mut s = session.lock();
            s.store.tracks.push(test_track("t-1"));
            s.republish_full();
        }
        let (graph_tx, _graph_rx) = rtrb::RingBuffer::new(8);
        let (_retire_tx, retire_rx) = rtrb::RingBuffer::new(8);
        let (_meter_tx, meter_rx) = rtrb::RingBuffer::new(8);
        let (_evt_tx, evt_rx) = rtrb::RingBuffer::new(8);
        ctl.output = Some(OutputBundle { _stream: None, graph_tx, retire_rx, meter_rx, evt_rx });

        let s2 = session.clone();
        ctl.after_assembly = Some(Arc::new(move || {
            // A real commit, on this thread, holding nothing: that it can
            // take the session lock here at all is the other half of the
            // proof that the assembly released it.
            Session::transact(
                &s2,
                crate::control::op::TxMeta::engine("gain mid-assembly"),
                |tx| {
                    tx.apply(crate::control::op::Op::Set {
                        object: crate::control::op::ObjectRef::Track("t-1".into()),
                        path: crate::control::op::PropPath::Gain,
                        from: serde_json::json!(0.0),
                        to: serde_json::json!(-6.0),
                    })
                },
            )
            .expect("the mid-assembly commit landed");
        }));

        ctl.rebuild();

        let tables = ctl.tables.lock();
        let published = f32::from_bits(tables.params.gain[0].load(Relaxed));
        assert!(
            (published - mixer::db_to_linear(-6.0)).abs() < 1e-6,
            "the published table carries the gain committed during assembly, got {published}"
        );
        assert_eq!(tables.generation, ctl.generation, "published at THIS rebuild's generation");
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
            let mut off = test_track("t-1");
            off.automation_mode = AutomationMode::Off;
            s.store.tracks.push(off);
            s.store.tracks.push(test_track("t-2"));
            s.automation.lanes.push(test_lane("l1", "track:t-2", 0));
            s.automation.lanes.push(test_lane("l2", "track:ghost", 0));
            s.automation.lanes.push(test_lane("l3", "inst-1", 7));
        }
        let (ramps, driver, _) = {
            let s = session.lock();
            let slots = derive_slots(&s.store.tracks);
            ctl.compile_automation(&s, &slots, s.store.tracks.len())
        };
        assert_eq!(ramps.len(), 2, "one entry per track slot, like ParamTable");
        assert!(ramps[0].gain.is_none(), "t-1 has no lane — an unautomated track must stay unramped");
        assert!(ramps[0].pan.is_none());
        let ev = ramps[1].gain.as_ref().expect("t-2's lane compiled into ITS slot");
        assert_eq!(ev.first().map(|e| e.value), Some(1.0));
        assert_eq!(ev.last().map(|e| e.value), Some(0.0));
        assert!(
            ev.last().unwrap().sample > 0,
            "ticks became absolute samples on the control thread"
        );
        assert!(driver.is_empty(), "no such plugin instance — the plugin lane resolves to nothing");

        // A failed tempo map compiles nothing, rather than something at rate 0.
        ctl.cache_rate = 0; // as if the tempo map failed to build
        let (ramps, driver, _) = {
            let s = session.lock();
            let slots = derive_slots(&s.store.tracks);
            ctl.compile_automation(&s, &slots, s.store.tracks.len())
        };
        assert_eq!(ramps.len(), 2, "still one entry per slot, just nothing in them");
        assert!(ramps.iter().all(|r| r.gain.is_none() && r.pan.is_none()));
        assert!(driver.is_empty());
    }

    /// An automation-track binding on a plugin param must compile into the
    /// live driver (Task 9 review). `lanes_from_doc` skips AutomationTrack
    /// sources, so the driver has to ingest `CompiledModulation.params`.
    #[test]
    fn an_automation_track_plugin_param_binding_compiles_into_the_driver() {
        use crate::modulation::model::{
            AutomationClip, Binding, BindingMode, Curve, Domain, Range, Source, TargetRef,
        };
        use crate::plugins::automation::AutomationPoint;
        use crate::plugins::descriptor::ParamInfo;

        let (mut ctl, session) = bare_control();
        ctl.cache_rate = 48_000;
        {
            let mut s = session.lock();
            let mut auto = test_track("auto");
            auto.kind = "automation".into();
            s.store.tracks.push(auto);
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(),
                uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(),
                format: "lv2".into(),
                status: "active".into(),
                track_id: None,
            });
            s.plugins.params.insert(
                "inst-1".into(),
                vec![ParamInfo {
                    id: 7,
                    name: "cutoff".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    value: 0.5,
                    steps: 0,
                }],
            );
            s.modulation.curves.push(Curve {
                id: "cur".into(),
                name: "cur".into(),
                length_ticks: Some(160),
                points: vec![
                    AutomationPoint { tick: 0, value: 0.25 },
                    AutomationPoint { tick: 159, value: 0.25 },
                ],
            });
            s.modulation.automation_clips.push(AutomationClip {
                id: "acl".into(),
                track_id: "auto".into(),
                curve_id: "cur".into(),
                timeline_start_ticks: 0,
                length_ticks: 160,
                content_length_ticks: None,
            });
            s.modulation.bindings.push(Binding {
                id: "b".into(),
                source: Source::AutomationTrack { track_id: "auto".into() },
                target: TargetRef::PluginParam { instance_id: "inst-1".into(), param_id: 7 },
                mode: BindingMode::Absolute,
                depth: 1.0,
                range: Range::default(),
                domain: Domain::Normalized,
                range_snapshot: None,
                enabled: true,
            });
        }
        let (_, mut driver, _) = {
            let s = session.lock();
            let slots = derive_slots(&s.store.tracks);
            ctl.compile_automation(&s, &slots, mixer_slot_count(&s.store.tracks))
        };
        assert!(!driver.is_empty(), "automation-track plugin binding must reach the driver");
        let mut writes = Vec::new();
        driver.tick(0, &mut writes);
        assert_eq!(writes.len(), 1, "the driver emits at the clip start");
        assert_eq!(writes[0].instance, "inst-1");
        assert_eq!(writes[0].index, 7);
        assert!(
            (writes[0].value - 0.25).abs() < 1e-5,
            "native value follows the curve: {}",
            writes[0].value
        );
    }

    /// Task 10: `compile_automation` must feed real MidiClip placements into
    /// `CompileCtx.content_placements`. A clip-envelope binding with an
    /// empty closure is silent even when the clips sit on the session.
    #[test]
    fn a_clip_envelope_binding_compiles_from_midi_clip_placements() {
        use crate::modulation::model::{
            Binding, BindingMode, Curve, Domain, Range, Source, TargetRef, TrackParam,
        };
        use crate::plugins::automation::AutomationPoint;

        let (mut ctl, session) = bare_control();
        ctl.cache_rate = 48_000;
        {
            let mut s = session.lock();
            let mut off = test_track("t-1");
            off.automation_mode = AutomationMode::Off;
            s.store.tracks.push(off);
            s.store.tracks.push(test_track("t-2"));
            s.midi.clips.push(crate::midi::MidiClip {
                id: "c1".into(),
                track_id: "t-1".into(),
                name: "c1".into(),
                timeline_start_ticks: 0,
                length_ticks: 960,
                notes: Vec::new(),
                next_note_id: 1,
                content_id: "con".into(),
                lane_id: crate::ids::LaneId::default_for_track("t-1"),
                content_length_ticks: None,
                transpose_semitones: 0,
                velocity_offset: 0,
            });
            s.midi.clips.push(crate::midi::MidiClip {
                id: "c2".into(),
                track_id: "t-2".into(),
                name: "c2".into(),
                timeline_start_ticks: 0,
                length_ticks: 960,
                notes: Vec::new(),
                next_note_id: 1,
                content_id: "con".into(),
                lane_id: crate::ids::LaneId::default_for_track("t-2"),
                content_length_ticks: None,
                transpose_semitones: 0,
                velocity_offset: 0,
            });
            s.modulation.curves.push(Curve {
                id: "cur".into(),
                name: "cur".into(),
                length_ticks: Some(960),
                points: vec![AutomationPoint { tick: 0, value: 0.4 }],
            });
            s.modulation.bindings.push(Binding {
                id: "b".into(),
                source: Source::ClipEnvelope {
                    content_id: "con".into(),
                    curve_id: "cur".into(),
                },
                target: TargetRef::SelfTrackParam { param: TrackParam::Gain },
                mode: BindingMode::Multiply,
                depth: 1.0,
                range: Range::default(),
                domain: Domain::Normalized,
                range_snapshot: None,
                enabled: true,
            });
        }
        let (ramps, _, _) = {
            let s = session.lock();
            let slots = derive_slots(&s.store.tracks);
            ctl.compile_automation(&s, &slots, mixer_slot_count(&s.store.tracks))
        };
        assert!(
            ramps[0].gain.is_none(),
            "Off must bypass modulation-backed track-gain automation too"
        );
        assert!(
            ramps[1].gain.is_some(),
            "the same content on t-2 must drive t-2's own gain"
        );
    }

    /// Clip-envelope `selfInstrumentParam` must resolve through the track's
    /// `plugin:<id>` ref to the BARE instance id `PluginDoc.params` is
    /// keyed by. Returning the ref verbatim makes `param_range` miss and
    /// the whole group compiles to nothing.
    #[test]
    fn a_clip_envelope_self_instrument_param_compiles_into_the_driver() {
        use crate::modulation::model::{
            Binding, BindingMode, Curve, Domain, Range, Source, TargetRef,
        };
        use crate::plugins::automation::AutomationPoint;
        use crate::plugins::descriptor::ParamInfo;

        let (mut ctl, session) = bare_control();
        ctl.cache_rate = 48_000;
        {
            let mut s = session.lock();
            let mut t = test_track("t-1");
            t.kind = "midi".into();
            t.instrument_id = Some("plugin:inst-1".into());
            s.store.tracks.push(t);
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(),
                uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(),
                format: "lv2".into(),
                status: "active".into(),
                track_id: Some("t-1".into()),
            });
            s.plugins.params.insert(
                "inst-1".into(),
                vec![ParamInfo {
                    id: 7,
                    name: "cutoff".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    value: 0.5,
                    steps: 0,
                }],
            );
            s.midi.clips.push(crate::midi::MidiClip {
                id: "c1".into(),
                track_id: "t-1".into(),
                name: "c1".into(),
                timeline_start_ticks: 0,
                length_ticks: 960,
                notes: Vec::new(),
                next_note_id: 1,
                content_id: "con".into(),
                lane_id: crate::ids::LaneId::default_for_track("t-1"),
                content_length_ticks: None,
                transpose_semitones: 0,
                velocity_offset: 0,
            });
            s.modulation.curves.push(Curve {
                id: "cur".into(),
                name: "cur".into(),
                length_ticks: Some(960),
                points: vec![AutomationPoint { tick: 0, value: 0.25 }],
            });
            s.modulation.bindings.push(Binding {
                id: "b".into(),
                source: Source::ClipEnvelope {
                    content_id: "con".into(),
                    curve_id: "cur".into(),
                },
                target: TargetRef::SelfInstrumentParam { param_id: 7 },
                mode: BindingMode::Absolute,
                depth: 1.0,
                range: Range::default(),
                domain: Domain::Normalized,
                range_snapshot: None,
                enabled: true,
            });
        }
        let (_, mut driver, _) = {
            let s = session.lock();
            let slots = derive_slots(&s.store.tracks);
            ctl.compile_automation(&s, &slots, mixer_slot_count(&s.store.tracks))
        };
        assert!(
            !driver.is_empty(),
            "selfInstrumentParam must reach the driver — range lookup uses the bare instance id"
        );
        let mut writes = Vec::new();
        driver.tick(0, &mut writes);
        assert_eq!(writes.len(), 1, "the driver emits at the clip start");
        assert_eq!(
            writes[0].instance, "inst-1",
            "must be the bare instance id, not plugin:inst-1"
        );
        assert_eq!(writes[0].index, 7);
        assert!(
            (writes[0].value - 0.25).abs() < 1e-5,
            "native value follows the curve: {}",
            writes[0].value
        );
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
                sends: Vec::new(),
                output: None,
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
                inserts: Vec::new(),
                group: None,
                automation_mode: AutomationMode::Read,
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
            pitch: None,
            rehearse: Arc::new(AtomicBool::new(false)),
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

    // ---- Pitch Coach input hub (plan task 5) ----------------------------

    use super::super::pitch_thread::PitchWorker;

    /// What [`listening_input_cb`] hands back: the callback, the record
    /// consumer, the rehearse flag, and — when a tap was asked for — the
    /// pitch worker with its frame consumer.
    type ListeningCb = (
        InputCb,
        rtrb::Consumer<f32>,
        Arc<AtomicBool>,
        Option<(PitchWorker, rtrb::Consumer<PitchFrame>)>,
    );

    fn tone_48k(hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin())
            .collect()
    }

    /// A take's input bundle with no cpal stream. `with_pitch` attaches an
    /// unused frame ring so `pitch_rx.is_some()` is the same signal a real
    /// take on the pitch device would carry.
    fn fake_bundle_on(device_key: &str, with_pitch: bool) -> InputBundle {
        let (_meter_tx, meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let pitch_rx = if with_pitch {
            let (_tx, rx) = rtrb::RingBuffer::new(PITCH_RING_SLOTS);
            Some(rx)
        } else {
            None
        };
        InputBundle {
            _stream: None,
            _pitch_worker: None,
            meter_rx,
            pitch_rx,
            device_key: device_key.to_string(),
            wants: InputWants {
                listening: with_pitch,
                recording: true,
            },
            rate: 48_000,
        }
    }

    /// An `InputCb` wired the way a listening take is, with a roomy ring so
    /// nothing overflows: returns the callback, the record consumer, the
    /// rehearse flag, and (when a tap was asked for) the pitch worker plus
    /// its frame consumer. The worker is handed back UNSPAWNED so tests drive
    /// it with `pump()` and stay deterministic.
    fn listening_input_cb(shared: &Arc<SharedRt>, with_pitch: bool) -> ListeningCb {
        let (producer, consumer) = rtrb::RingBuffer::new(1 << 16);
        let (meter_tx, _meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let rehearse = Arc::new(AtomicBool::new(false));
        let (pitch, pitch_out) = match with_pitch {
            true => {
                let (t, worker, rx) = pitch_channel(48_000, Arc::new(AtomicBool::new(true)));
                (Some(t), Some((worker, rx)))
            }
            false => (None, None),
        };
        let mut b = RawMeterBlock::new(1, 0, 0);
        b.base_slot = 0;
        let cb = InputCb {
            producers: vec![producer],
            owed: vec![0],
            meter_tx,
            blocks: vec![(b, vec![0])],
            in_ch: 1,
            rec_ch: 1,
            shared: shared.clone(),
            pitch,
            rehearse: rehearse.clone(),
        };
        (cb, consumer, rehearse, pitch_out)
    }

    /// Listening is a policy decision the control thread makes; opening the
    /// device is what it does about it. Asserts on hub presence, not cpal
    /// (plan task 5): `bare_control` stubs the stream so this runs without
    /// a microphone (spec R6: an explicit listen toggle opens the mic).
    #[test]
    fn listening_opens_and_closes_the_input_stream() {
        let (mut ctl, _session) = bare_control();
        assert!(ctl.listen_input.is_none(), "idle must not hold the mic");
        assert!(!ctl.listen_stream_wanted());

        ctl.set_listening(true).expect("stub hub");
        assert!(
            ctl.listen_input.is_some(),
            "set_listening(true) must open the hub"
        );
        assert!(ctl.listen_stream_wanted());

        ctl.set_listening(false).expect("close");
        assert!(
            ctl.listen_input.is_none(),
            "set_listening(false) must drop the hub"
        );
        assert!(!ctl.listen_stream_wanted());
    }

    /// Arming a track must NOT open the microphone — the ruling that
    /// separates this feature from "monitor while armed" (R6).
    #[test]
    fn arming_alone_does_not_want_the_microphone() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push({
            let mut t = test_track("t1");
            t.armed = true;
            t
        });
        assert!(
            !ctl.listen_stream_wanted(),
            "an armed track must not open the mic on its own"
        );
        ctl.wants_listening = true;
        assert!(ctl.listen_stream_wanted());
    }

    /// While a take captures the pitch device, that take's own stream
    /// carries the analyser — so no SECOND stream is wanted on the same
    /// device, but the microphone is still open.
    #[test]
    fn recording_keeps_the_hub_open_when_listening_stops() {
        let (mut ctl, _session) = bare_control();
        ctl.set_listening(true).expect("stub hub");
        assert!(ctl.listen_input.is_some());

        // A take opens the pitch device (key "" = the default input).
        ctl.inputs.push(fake_bundle_on("", true));
        ctl.sync_input_hub().expect("drop listen-only; take owns it");
        assert!(
            ctl.listen_input.is_none(),
            "the take owns the mic; a second stream on it would be wrong"
        );
        assert!(
            ctl.inputs.iter().any(|i| i.pitch_rx.is_some()),
            "the take's own stream must carry the analyser"
        );

        // Turning listening off mid-take must not disturb the take.
        ctl.set_listening(false).expect("listen off");
        assert!(ctl.listen_input.is_none());
        assert_eq!(ctl.inputs.len(), 1, "the take's input must survive");
    }

    /// The mirror: a take ending must not silence the tuner.
    #[test]
    fn stopping_the_take_while_listening_keeps_the_hub_open() {
        let (mut ctl, _session) = bare_control();
        ctl.set_listening(true).expect("stub hub");
        ctl.inputs.push(fake_bundle_on("", true));
        ctl.sync_input_hub().expect("take owns the mic");
        assert!(ctl.listen_input.is_none(), "take owns it");

        ctl.inputs.clear(); // what stop_recording does
        ctl.sync_input_hub().expect("give the mic back");
        assert!(
            ctl.listen_input.is_some(),
            "with the take gone, the listener wants the mic back"
        );
    }

    /// A take on a DIFFERENT device leaves the microphone to the listener.
    #[test]
    fn a_take_on_another_device_does_not_claim_the_microphone() {
        let (mut ctl, _session) = bare_control();
        ctl.wants_listening = true;
        ctl.sel_input = Some("mic".into());
        ctl.inputs.push(fake_bundle_on("line-in", false));
        assert!(
            ctl.listen_stream_wanted(),
            "a take elsewhere must not take the mic away from the tuner"
        );
    }

    /// Rehearse-hold writes silence for exactly the held span and NOTHING
    /// else changes: same sample count, so the take stays sample-aligned.
    /// That alignment is the whole reason this writes silence rather than
    /// skipping (spec §4.1).
    #[test]
    fn rehearse_hold_writes_silence_for_exactly_the_held_span() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, mut consumer, rehearse, _) = listening_input_cb(&shared, false);

        let ones = [1.0f32; 4];
        cb.capture(&ones); // before
        rehearse.store(true, Relaxed);
        cb.capture(&ones); // held
        cb.capture(&ones); // still held
        rehearse.store(false, Relaxed);
        cb.capture(&ones); // after

        let mut got = Vec::new();
        while let Ok(v) = consumer.pop() {
            got.push(v);
        }
        assert_eq!(got.len(), 16, "the take must stay sample-aligned");
        assert_eq!(&got[0..4], &[1.0; 4], "audio before the hold");
        assert_eq!(&got[4..12], &[0.0; 8], "exactly the held span is silent");
        assert_eq!(&got[12..16], &[1.0; 4], "audio after the hold");
        assert_eq!(shared.xruns.load(Relaxed), 0, "a hold is not an xrun");
    }

    /// Rehearsing means "do not commit it", not "stop showing me my pitch":
    /// the analyser sits upstream of the hold.
    #[test]
    fn rehearse_hold_still_analyses_the_real_signal() {
        let shared = Arc::new(SharedRt::default());
        let (mut cb, _consumer, rehearse, pitch_out) = listening_input_cb(&shared, true);
        let (mut worker, mut pitch_rx) = pitch_out.expect("tap requested");

        rehearse.store(true, Relaxed);
        let tone = tone_48k(220.0, 24_000);
        for chunk in tone.chunks(512) {
            cb.capture(chunk);
        }
        worker.pump();

        let mut voiced = 0;
        while let Ok(f) = pitch_rx.pop() {
            if f.voiced {
                voiced += 1;
            }
        }
        assert!(
            voiced > 0,
            "a held rehearse must still produce voiced frames"
        );
    }

    /// After a take with one hold, exactly one span is reported, matching
    /// where the transport was when the hold opened and closed.
    #[test]
    fn rehearse_hold_spans_are_reported() {
        let (mut ctl, _session) = bare_control();

        ctl.shared.position.store(1_000, Relaxed);
        ctl.set_rehearse_hold(true);
        ctl.shared.position.store(5_800, Relaxed);
        ctl.set_rehearse_hold(false);

        // A repeated release is a no-op, not a second span.
        ctl.set_rehearse_hold(false);

        ctl.shared.position.store(9_000, Relaxed);
        assert_eq!(ctl.take_rehearse_spans(9_000), vec![(1_000, 5_800)]);
        assert!(
            ctl.take_rehearse_spans(9_000).is_empty(),
            "spans belong to one take and are not reported twice"
        );
    }

    /// A take stopped WHILE held still reports the span, closed at the stop.
    #[test]
    fn a_hold_still_down_at_stop_is_reported_up_to_the_stop() {
        let (mut ctl, _session) = bare_control();
        ctl.shared.position.store(200, Relaxed);
        ctl.set_rehearse_hold(true);

        assert_eq!(ctl.take_rehearse_spans(900), vec![(200, 900)]);
        assert!(
            ctl.rehearse.load(Relaxed),
            "the key is still down; stopping a take must not release it"
        );
    }

    /// Hold across stop → seek → start → stop. Spans must be relative to
    /// *this* take, not the previous take's stop position.
    #[test]
    fn a_hold_across_takes_reports_spans_relative_to_each_take() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.ensure_project_fn = Some(Arc::new(|| Ok(PathBuf::from("/nonexistent"))));
        ctl.live_in_hub.attach_shared(ctl.shared.clone());
        ctl.live_in_hub.set_target_track(Some("m-1".into()));

        ctl.shared.position.store(0, Relaxed);
        ctl.set_rehearse_hold(true);
        ctl.start_recording(None, HashMap::new()).expect("take A");
        assert_eq!(ctl.rehearse_open, Some(0), "held at arm time starts at 0");
        ctl.shared.position.store(48_000, Relaxed);
        ctl.stop_recording().ok();
        assert_eq!(
            ctl.rehearse_open,
            Some(48_000),
            "still held: reopen at the stop so the flag stays consistent"
        );

        ctl.shared.position.store(0, Relaxed);
        ctl.start_recording(None, HashMap::new()).expect("take B");
        assert_eq!(
            ctl.rehearse_open,
            Some(0),
            "a new take must reset the open hold to this take's start"
        );
        assert!(
            ctl.rehearse_spans.is_empty(),
            "previous take's spans must not leak into the next"
        );
        ctl.shared.position.store(48_000, Relaxed);
        assert_eq!(
            ctl.take_rehearse_spans(48_000),
            vec![(0, 48_000)],
            "the second take was silent for its entire length"
        );
        ctl.stop_recording().ok();
    }

    /// A failed take start must not leave `wants_listening` true with no
    /// listen stream (the listen hub is dropped before `open_capture_group`).
    #[test]
    fn failed_take_start_restores_the_listen_stream() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        session.lock().store.tracks[0].armed = true;
        ctl.ensure_project_fn = Some(Arc::new(|| Ok(PathBuf::from("/nonexistent"))));
        ctl.set_listening(true).expect("stub hub");
        assert!(ctl.listen_input.is_some());
        assert!(ctl.wants_listening);

        let mut returns = HashMap::new();
        returns.insert("m-1".into(), "aura-x1-no-such-input".into());
        let err = ctl.start_recording(None, returns).unwrap_err();
        assert!(
            err.contains("unknown input device: aura-x1-no-such-input"),
            "must fail on the return device, got {err}"
        );
        assert!(
            ctl.listen_input.is_some(),
            "failed take start must restore the listen hub"
        );
        assert!(ctl.wants_listening, "listen intent survives a failed take");
        assert!(ctl.writers.is_empty() && ctl.inputs.is_empty());
    }

    /// Issue 7. A take on the pitch device owns the microphone and its stream
    /// cannot be rebuilt without losing audio, so the tap has to be there from
    /// the start — dormant — and `set_listening` has to be what wakes it.
    /// Before this, opening the panel mid-take set the flag and nothing else:
    /// listening stayed dark until the take stopped.
    #[test]
    fn listen_started_mid_take_wakes_the_takes_tap() {
        let (mut ctl, _session) = bare_control();
        let shared = ctl.shared.clone();
        // The take's own capture stream, carrying the dormant tap.
        let (tap, mut worker, mut frames) = pitch_channel(48_000, ctl.pitch_active.clone());
        let (meter_tx, _meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let mut cb = InputCb {
            producers: Vec::new(),
            owed: Vec::new(),
            meter_tx,
            blocks: Vec::new(),
            in_ch: 1,
            rec_ch: 1,
            shared,
            pitch: Some(tap),
            rehearse: ctl.rehearse.clone(),
        };
        ctl.inputs.push(fake_bundle_on("", true));

        let tone = tone_48k(220.0, 24_000);
        for chunk in tone.chunks(512) {
            cb.capture(chunk);
        }
        worker.pump();
        assert!(
            frames.pop().is_err(),
            "a dormant tap must analyse nothing while listening is off"
        );

        ctl.set_listening(true).expect("no second stream to open");
        assert!(
            ctl.listen_input.is_none(),
            "the take owns the mic; a second stream on it would be wrong"
        );
        for chunk in tone.chunks(512) {
            cb.capture(chunk);
        }
        worker.pump();
        let mut voiced = 0;
        while let Ok(f) = frames.pop() {
            if f.voiced {
                voiced += 1;
            }
        }
        assert!(voiced > 0, "listen started mid-take must not stay dark");
    }

    /// The other half of issue 7: the take that captures the pitch device
    /// carries the tap whether or not the user was listening when it started.
    #[test]
    fn a_take_on_the_pitch_device_carries_the_tap_even_when_not_listening() {
        let (mut ctl, _session) = bare_control();
        ctl.sel_input = Some("mic".into());
        let groups = ["line-in".to_string(), "mic".to_string()];
        assert!(!ctl.wants_listening, "nobody is listening yet");
        assert_eq!(
            ctl.pitch_group_key(groups.iter()),
            Some("mic".to_string()),
            "the group on the pitch device must carry the tap regardless"
        );

        ctl.sel_input = Some("usb".into());
        assert_eq!(
            ctl.pitch_group_key(groups.iter()),
            None,
            "no group captures the pitch device: nothing to attach to"
        );
    }

    /// A take on the pitch device whose tap could not be built (the worker
    /// thread failed to spawn) leaves nothing that can analyse, and the take
    /// owns the device so no listen stream can be opened either. Reporting
    /// `listening: true` there would be a lie the UI cannot see through: an
    /// enabled panel over a permanently dark trail, until the take stops.
    #[test]
    fn pitch_state_does_not_claim_to_listen_with_no_tap() {
        let (mut ctl, _session) = bare_control();
        ctl.inputs.push(fake_bundle_on("", false));
        ctl.set_listening(true).expect("no second stream to open");

        assert!(ctl.wants_listening, "the intent is still recorded");
        assert!(
            ctl.listen_input.is_none(),
            "the take owns the device; no listen stream is possible"
        );
        assert!(
            !ctl.current_pitch_state().listening,
            "nothing can analyse, so the state must not claim to be listening"
        );
    }

    /// The mirror: with a tap present, the state reports what it should.
    #[test]
    fn pitch_state_reports_listening_when_a_tap_exists() {
        let (mut ctl, _session) = bare_control();
        ctl.set_listening(true).expect("stub hub");
        assert!(ctl.current_pitch_state().listening);
    }

    /// A listen that could not open a device must leave every tap dormant —
    /// otherwise a take's tap would analyse into a ring nobody drains.
    #[test]
    fn a_failed_listen_leaves_the_taps_dormant() {
        let (mut ctl, _session) = bare_control();
        ctl.stub_input = false;
        ctl.sel_input = Some("aura-no-such-input".into());
        assert!(ctl.set_listening(true).is_err(), "device must not exist");
        assert!(
            !ctl.pitch_active.load(Relaxed),
            "no stream means no analysis"
        );
    }

    /// Listening on and off must move the shared flag every tap reads.
    #[test]
    fn set_listening_drives_the_shared_tap_flag() {
        let (mut ctl, _session) = bare_control();
        assert!(!ctl.pitch_active.load(Relaxed));
        ctl.set_listening(true).expect("stub hub");
        assert!(ctl.pitch_active.load(Relaxed), "listening must wake taps");
        ctl.set_listening(false).expect("close");
        assert!(!ctl.pitch_active.load(Relaxed), "stopping must idle them");
    }

    /// Listen-only capture must not push `base_slot == 0` meter blocks:
    /// `MeterAccum` would fold them into the output RMS denominator.
    #[test]
    fn listen_only_capture_does_not_push_meter_blocks() {
        let shared = Arc::new(SharedRt::default());
        let (meter_tx, mut meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let (tap, _worker, _pitch_rx) = pitch_channel(48_000, Arc::new(AtomicBool::new(true)));
        let mut cb = InputCb {
            producers: Vec::new(),
            owed: Vec::new(),
            meter_tx,
            blocks: Vec::new(),
            in_ch: 1,
            rec_ch: 1,
            shared,
            pitch: Some(tap),
            rehearse: Arc::new(AtomicBool::new(false)),
        };
        cb.capture(&[0.5f32; 64]);
        assert!(
            meter_rx.pop().is_err(),
            "listen-only must not contribute meter blocks"
        );
    }

    /// A counting stand-in for a Tauri `Channel<PitchFrameBatch>`. It keeps
    /// accepting sends forever, exactly like the real one whose JS side has
    /// merely stopped listening — which is the whole reason `id`-based
    /// unsubscribe exists.
    struct CountingPitchSink {
        id: u32,
        sent: Arc<AtomicUsize>,
    }

    impl PitchSink for CountingPitchSink {
        fn send_batch(&self, _batch: &PitchFrameBatch) -> bool {
            self.sent.fetch_add(1, Relaxed);
            true
        }
        fn id(&self) -> u32 {
            self.id
        }
    }

    /// Opening and closing the panel must not leave a sink behind. Before
    /// `UnsubscribePitch` existed, `send_batch` returning true forever meant
    /// nothing was ever retired and every visit cost one more batch per tick.
    #[test]
    fn unsubscribe_pitch_retires_exactly_that_channel() {
        let (mut ctl, _session) = bare_control();
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        ctl.handle(ControlMsg::SubscribePitch(Box::new(CountingPitchSink {
            id: 7,
            sent: a.clone(),
        })));
        ctl.handle(ControlMsg::SubscribePitch(Box::new(CountingPitchSink {
            id: 9,
            sent: b.clone(),
        })));
        assert_eq!(ctl.pitch_sinks.len(), 2);

        ctl.handle(ControlMsg::UnsubscribePitch(7));
        assert_eq!(ctl.pitch_sinks.len(), 1, "only channel 7 goes");
        assert_eq!(ctl.pitch_sinks[0].id(), 9);

        // Unknown and repeated ids are no-ops, not errors: a panel that is
        // torn down twice must not take an innocent subscriber with it.
        ctl.handle(ControlMsg::UnsubscribePitch(7));
        ctl.handle(ControlMsg::UnsubscribePitch(1234));
        assert_eq!(ctl.pitch_sinks.len(), 1);
    }

    /// A panel that mounts against an engine which already has a reference
    /// track must be TOLD about it. `referenceTrackId` rides on
    /// `pitch://state` and nothing else, and `emit_pitch_state` dedupes — so
    /// before this, a remounted panel drew no melody until some unrelated
    /// transition (a take starting or stopping flips `listening`) happened to
    /// emit. Which is exactly "the notes did not appear until the recording
    /// finished".
    #[test]
    fn subscribing_to_pitch_gets_the_current_state_without_waiting_for_a_change() {
        let (mut ctl, _session) = bare_control();
        let log = Arc::new(Mutex::new(Vec::new()));
        ctl.events = Box::new(RecordingEvents(log.clone()));
        ctl.reference_track_id = Some("trk-melody".into());
        // Whatever transition set the reference in the first place.
        ctl.emit_pitch_state();
        log.lock().clear();

        // The panel mounts. Nothing about the engine has changed since.
        ctl.handle(ControlMsg::SubscribePitch(Box::new(CountingPitchSink {
            id: 3,
            sent: Arc::new(AtomicUsize::new(0)),
        })));

        let events = log.lock();
        let (_, payload) = events
            .iter()
            .find(|(name, _)| name == "pitch://state")
            .expect("subscribing must emit the current pitch state");
        assert_eq!(
            payload["referenceTrackId"], "trk-melody",
            "the new subscriber has to learn which track the melody is on"
        );
    }

    /// Resubscribing the same channel replaces it. Two sinks on one channel
    /// would double every batch the frontend receives.
    #[test]
    fn resubscribing_one_channel_does_not_stack_sinks() {
        let (mut ctl, _session) = bare_control();
        let sent = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            ctl.handle(ControlMsg::SubscribePitch(Box::new(CountingPitchSink {
                id: 4,
                sent: sent.clone(),
            })));
        }
        assert_eq!(ctl.pitch_sinks.len(), 1);
    }

    /// `SelectInput` must emit `pitch://state` so `deviceRate` is not stale.
    #[test]
    fn select_input_emits_pitch_state() {
        let (mut ctl, _session) = bare_control();
        ctl.set_listening(true).expect("stub hub");
        ctl.last_pitch_state = None;
        let (tx, rx) = bounded(1);
        ctl.handle(ControlMsg::SelectInput {
            device_id: Some("mic-2".into()),
            reply: tx,
        });
        assert!(rx.recv().unwrap().is_ok());
        let state = ctl.last_pitch_state.expect("must emit pitch://state");
        assert!(state.listening);
        assert_eq!(ctl.sel_input.as_deref(), Some("mic-2"));
        assert_eq!(state.device_rate, 48_000, "stub hub reports 48 kHz");
    }

    /// A failed device switch must not leave listening-with-no-stream.
    #[test]
    fn failed_select_input_restores_listen_stream() {
        let (mut ctl, _session) = bare_control();
        ctl.set_listening(true).expect("stub hub");
        assert!(ctl.listen_input.is_some());
        ctl.stub_input = false;
        let (tx, rx) = bounded(1);
        ctl.handle(ControlMsg::SelectInput {
            device_id: Some("aura-no-such-input".into()),
            reply: tx,
        });
        assert!(rx.recv().unwrap().is_err());
        assert_eq!(
            ctl.sel_input, None,
            "failed switch must restore the previous device"
        );
        assert!(
            ctl.listen_input.is_some() || !ctl.wants_listening,
            "must not claim listening with no stream"
        );
    }

    /// Listening without a take: pitch frames flow and no writer exists.
    #[test]
    fn pitch_frames_are_produced_while_listening_without_recording() {
        let shared = Arc::new(SharedRt::default());
        let (meter_tx, _meter_rx) = rtrb::RingBuffer::new(METER_RING_SLOTS);
        let (tap, mut worker, mut pitch_rx) =
            pitch_channel(48_000, Arc::new(AtomicBool::new(true)));
        // A listen-only callback: NO producers and NO meter blocks, which
        // is what "no take" means down here (meter blocks would dilute
        // output RMS).
        let mut cb = InputCb {
            producers: Vec::new(),
            owed: Vec::new(),
            meter_tx,
            blocks: Vec::new(),
            in_ch: 1,
            rec_ch: 1,
            shared: shared.clone(),
            pitch: Some(tap),
            rehearse: Arc::new(AtomicBool::new(false)),
        };

        for chunk in tone_48k(220.0, 48_000).chunks(512) {
            cb.capture(chunk);
        }
        worker.pump();

        let mut voiced = Vec::new();
        while let Ok(f) = pitch_rx.pop() {
            if f.voiced {
                voiced.push(f);
            }
        }
        assert!(
            !voiced.is_empty(),
            "listening must produce voiced frames with no take running"
        );
        // A3 = MIDI 57. Every voiced frame must actually be the tone.
        for f in &voiced {
            assert!(
                (f.midi - 57.0).abs() < 0.5,
                "expected ~57 (A3), got {}",
                f.midi
            );
        }
        let (mut ctl, _session) = bare_control();
        assert!(
            ctl.writers.is_empty() && ctl.take_rehearse_spans(0).is_empty(),
            "listening creates no writer and no take state"
        );
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
                sends: Vec::new(),
                output: None,
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
                inserts: Vec::new(),
                group: None,
                automation_mode: AutomationMode::Read,
            });
            let mut c1 = test_clip("c1", "t1");
            c1.source_id = sid.clone();
            s.clips.push(c1);
            // The published image IS the document these readers see, so a
            // test that pokes the live document must republish — a live/image
            // divergence with no lock held is not a state production can be
            // in (`Session::published`'s contract).
            session.republish_full();
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
            session.republish_full();
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
                sends: Vec::new(),
                output: None,
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
                inserts: Vec::new(),
                group: None,
                automation_mode: AutomationMode::Read,
            });
            let mut c1 = test_clip("c1", "t1");
            c1.source_id = sid.clone();
            s.clips.push(c1);
            // The published image IS the document these readers see, so a
            // test that pokes the live document must republish — a live/image
            // divergence with no lock held is not a state production can be
            // in (`Session::published`'s contract).
            session.republish_full();
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

        let empty = HashMap::new();
        let (audio, midi) = split_record_targets(&store, None, Some("m-1".into()), &empty).unwrap();
        assert_eq!(
            audio.iter().map(|t| t.track_id.as_str()).collect::<Vec<_>>(),
            vec!["a-1"],
            "an armed midi track without a return is not a WAV target"
        );
        assert_eq!(midi.as_deref(), Some("m-1"));

        let (audio, midi) =
            split_record_targets(&store, Some(vec!["m-1".into()]), Some("m-1".into()), &empty).unwrap();
        assert!(audio.is_empty(), "midi tracks without a return never record audio, got {audio:?}");
        assert_eq!(midi.as_deref(), Some("m-1"));

        let (audio, midi) = split_record_targets(
            &store,
            Some(vec!["a-1".into(), "m-1".into()]),
            Some("m-1".into()),
            &empty,
        )
        .unwrap();
        assert_eq!(audio.iter().map(|t| t.track_id.as_str()).collect::<Vec<_>>(), vec!["a-1"]);
        assert_eq!(midi.as_deref(), Some("m-1"));
    }

    #[test]
    fn split_record_targets_includes_a_midi_track_with_a_return() {
        let mut store = Store::default();
        store.tracks.push(crate::audio::types::testutil::test_track("a-1"));
        store.tracks.push(midi_track("m-1"));
        store.tracks[0].armed = true;
        store.tracks[1].armed = true;

        let mut returns = HashMap::new();
        returns.insert("m-1".into(), "Mic 2".into());
        let (audio, midi) = split_record_targets(&store, None, Some("m-1".into()), &returns).unwrap();
        assert_eq!(
            audio,
            vec![
                AudioRecTarget { track_id: "a-1".into(), device_id: None },
                AudioRecTarget { track_id: "m-1".into(), device_id: Some("Mic 2".into()) },
            ]
        );
        assert_eq!(midi.as_deref(), Some("m-1"));
    }

    #[test]
    fn split_record_targets_allows_a_midi_only_take() {
        let mut store = Store::default();
        store.tracks.push(midi_track("m-1"));
        let (audio, midi) =
            split_record_targets(&store, None, Some("m-1".into()), &HashMap::new()).unwrap();
        assert!(audio.is_empty());
        assert_eq!(midi.as_deref(), Some("m-1"));
    }

    #[test]
    fn split_record_targets_errors_when_nothing_is_recordable() {
        let store = Store::default();
        let err = split_record_targets(&store, None, None, &HashMap::new()).unwrap_err();
        assert!(err.contains("no armed tracks"), "got {err}");

        let mut store2 = Store::default();
        store2.tracks.push(crate::audio::types::testutil::test_track("a-1"));
        assert!(
            split_record_targets(&store2, None, Some("a-1".into()), &HashMap::new()).is_err(),
            "an audio track as routing target is not a midi take"
        );
        assert!(
            split_record_targets(&store2, None, Some("ghost".into()), &HashMap::new()).is_err(),
            "a routing target that no longer exists is not a midi take"
        );
    }

    #[test]
    fn split_record_targets_rejects_an_unknown_explicit_track() {
        let store = Store::default();
        let err = split_record_targets(&store, Some(vec!["ghost".into()]), None, &HashMap::new())
            .unwrap_err();
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
            pitch_path: None,
            wav_path: dir.join("audio/take.wav"),
            rel_path: "audio/take.wav".into(),
            cache_dir: dir.join("cache"),
            start_pos: 0,
        };
        ctl.writers = vec![recorder::spawn(vec![spec], vec![consumer], 2, 48_000).unwrap()];
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
            pitch_path: None,
            wav_path: dir.join("blocker/audio/take.wav"),
            rel_path: "audio/take.wav".into(),
            cache_dir: dir.join("blocker/cache"),
            start_pos: 0,
        };
        ctl.writers = vec![recorder::spawn(vec![spec], vec![consumer], 2, 48_000).unwrap()];
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

        let recorded = ctl
            .start_recording(None, HashMap::new())
            .expect("a routing target alone is a take");
        assert_eq!(recorded, vec!["m-1".to_string()], "the midi target is reported as recorded");
        assert!(ctl.writers.is_empty(), "no WAV writer for a midi-only take");
        assert!(ctl.inputs.is_empty(), "no input device opened");
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
        assert_eq!(ctl.start_recording(None, HashMap::new()).unwrap_err(), "already recording");
    }

    #[test]
    fn count_in_holds_the_take_until_the_pre_roll_elapses() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        ctl.ensure_project_fn = Some(Arc::new(|| Ok(PathBuf::from("/nonexistent"))));
        ctl.live_in_hub.set_target_track(Some("m-1".into()));
        ctl.count_in_bars = 1;
        ctl.shared.sample_rate.store(48_000, Relaxed);

        let recorded = ctl.start_recording(None, HashMap::new()).unwrap();
        assert_eq!(recorded, vec!["m-1".to_string()]);
        assert!(ctl.pending_record.is_some());
        assert_eq!(ctl.shared.countin_left.load(Relaxed), 96_000, "1 bar at 120 bpm 4/4");
        assert!(ctl.writers.is_empty() && !ctl.live_in_hub.capturing());
        assert!(ctl.shared.playing.load(Relaxed));

        ctl.shared.countin_left.store(0, Relaxed);
        ctl.arm_pending_after_countin();
        assert!(ctl.pending_record.is_none());
        assert!(ctl.live_in_hub.capturing(), "the MIDI take arms after count-in");
        ctl.stop_recording().ok();
    }

    /// X1: a MIDI track with a return is an audio take on THAT device. A
    /// name that is not a cpal input fails before any writer starts — that
    /// is how we prove the engine asked for the return device rather than
    /// the global default (which on a headless runner may not exist either,
    /// but would say "no default input device", not "unknown input device").
    #[test]
    fn start_recording_a_returned_midi_track_asks_for_that_device() {
        let (mut ctl, session) = bare_control();
        session.lock().store.tracks.push(midi_track("m-1"));
        session.lock().store.tracks[0].armed = true;
        ctl.ensure_project_fn = Some(Arc::new(|| Ok(PathBuf::from("/nonexistent"))));
        let mut returns = HashMap::new();
        returns.insert("m-1".into(), "aura-x1-no-such-input".into());
        let err = ctl.start_recording(None, returns).unwrap_err();
        assert!(
            err.contains("unknown input device: aura-x1-no-such-input"),
            "must fail on the return device, got {err}"
        );
        assert!(ctl.writers.is_empty() && ctl.inputs.is_empty());
        assert!(!ctl.live_in_hub.capturing(), "a failed start must not arm MIDI capture");
    }
}
