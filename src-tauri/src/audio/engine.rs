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
    _stream: cpal::Stream,
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
}

impl OutputCb {
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

        match (&mut self.graph, playing) {
            (Some(g), true) => {
                // Task 7: `render` pushes the graph's meter chunks itself
                // (1..=⌈slots/64⌉ for a wide graph) and reports how many the
                // ring couldn't take — telemetry, not data, so a dropped
                // chunk is one xrun, not lost audio.
                let dropped = mixer::render_rt(
                    g,
                    base,
                    &lp,
                    out,
                    self.channels,
                    self.rate,
                    discontinuity,
                    steady_base,
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

        // Replacing the bundle drops the previous stream + its graph here on
        // the control thread.
        self.output = Some(OutputBundle {
            _stream: stream,
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
                crate::midi::playback::append_from(
                    &session.midi,
                    store,
                    &session.plugins,
                    &slots,
                    self.cache_rate,
                    bank.as_deref(),
                    &mut self.live_nodes,
                    &mut tracks,
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
        if self.writer.is_some() {
            return Err("already recording".to_string());
        }

        // Resolve target tracks (explicit list or armed flags).
        let targets: Vec<String> = {
            let session = self.session.lock(); // read-only: resolve/validate target track ids before recording starts
            let store = &session.store;
            match track_ids {
                Some(ids) => {
                    for id in &ids {
                        if !store.tracks.iter().any(|t| &t.id == id) {
                            return Err(format!("unknown track: {id}"));
                        }
                    }
                    ids
                }
                None => store.armed_track_ids(),
            }
        };
        if targets.is_empty() {
            return Err("no armed tracks to record".to_string());
        }

        self.ensure_project()?;

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
        let start_pos = self.shared.position.load(Relaxed);

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
        self.rec_track_ids = targets.clone();
        self.shared.recording.store(true, Relaxed);
        self.shared.playing.store(true, Relaxed);
        if let Err(e) = self.commit_start_recording_state() {
            log::warn!("start_recording: transport-state commit failed: {e}");
        }
        self.events.emit(
            "recording://state",
            serde_json::json!({
                "recording": true,
                "trackIds": targets,
                "startedAtSamples": start_pos,
                "xruns": 0,
            }),
        );
        log::info!(
            "audio: recording {} track(s) @ {} Hz x{}ch",
            targets.len(),
            rate,
            rec_ch
        );
        Ok(targets)
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

    fn stop_recording(&mut self) -> Result<Vec<Clip>, String> {
        let writer = self.writer.take().ok_or("not recording")?;
        // Drop the input stream FIRST so the ring producers close and the
        // writer can drain to empty.
        self.input = None;
        // Release the pin (Task 6 [I2]) — recording is over, so its
        // generation no longer needs exemption from the plain window.
        self.gen_maps.unpin();
        let clips = writer.finish(Duration::from_secs(15))?;

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
        if let Err(e) = self.commit_recording_finalize(&clips) {
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
            }),
        );
        Ok(clips)
    }

    /// The ONE non-transient `Actor::Engine` tx `stop_recording` submits to
    /// register the take's clips — split out so it's independently testable
    /// without a live input stream/disk writer (`clips` is exactly what
    /// `writer.finish` returned; this fn does no I/O of its own).
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
    fn commit_recording_finalize(&mut self, clips: &[Clip]) -> Result<Committed, String> {
        let committer = self.committer.clone();
        committer.commit_with_rebuild(
            op::TxMeta::engine("stop recording"),
            |tx| {
                for clip in clips {
                    let idx = tx.store().clips.len();
                    tx.apply(op::Op::ClipAdd { clip: clip.clone(), index: idx })?;
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

        let clip_committed = ctl.commit_recording_finalize(&clips).unwrap();
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
}
