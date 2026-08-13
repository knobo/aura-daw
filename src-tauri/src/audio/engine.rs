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

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use parking_lot::Mutex;

use super::dsp::linear_resample;
use super::meters::{MeterAccum, RawMeterBlock};
use super::mixer;
use super::offline;
use super::recorder::{self, DiskWriter, RecSpec};
use super::rt::{GraphPtr, ParamTable, RtClip, RtClipData, RtGraph, RtTrack, SharedRt, NO_PARK};
use super::transport;
use super::types::{Clip, MeterFrame, Store};
use super::waveform::{pyramid_exists, Pyramid};
use super::project;
use crate::control::Session;
use crate::ids::SourceId;

/// Meter frame cadence (~60 Hz).
const FRAME_INTERVAL: Duration = Duration::from_micros(16_600);
/// Recording ring headroom, seconds of audio per track.
const REC_RING_SECS: usize = 2;

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
    params: Arc<ParamTable>,
    session: Arc<Mutex<Session>>,
    events: Box<dyn EventSink>,
) -> EngineHandle {
    let (tx, rx) = unbounded();
    std::thread::Builder::new()
        .name("aura-engine-control".into())
        .spawn(move || {
            let mut ctl = Control {
                shared,
                params,
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
                sinks: Vec::new(),
                last_frame: Instant::now(),
                last_tick: Instant::now(),
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
    params: Arc<ParamTable>,
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

        match (&mut self.graph, playing) {
            (Some(g), true) => {
                let blk = mixer::render(
                    g,
                    &self.params,
                    base,
                    &lp,
                    out,
                    self.channels,
                    self.rate,
                    discontinuity,
                );
                if self.meter_tx.push(blk).is_err() {
                    // Meter queue overflow: telemetry, not data — drop it.
                    self.shared.xruns.fetch_add(1, Relaxed);
                }
            }
            _ => out.fill(0.0),
        }

        if playing {
            let frames = (out.len() / self.channels.max(1)) as u64;
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
    /// RT param slots of the recorded tracks (same input feeds all of them).
    slots: Vec<usize>,
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
        let mut blk =
            RawMeterBlock::new(self.shared.position.load(Relaxed), frames as u32);
        for &slot in &self.slots {
            blk.set_slot(slot, pk_l, pk_r, ss_l, ss_r);
        }
        let _ = self.meter_tx.push(blk);

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
/// * a clip whose `source_id` is EMPTY is skipped (with a loud warning and a
///   debug-build assertion — the store boundary, `assign_source_ids` /
///   the production minting sites, should make this unreachable): it
///   renders silent rather than risk playing another clip's audio through
///   a shared empty-sentinel bucket.
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
            debug_assert!(
                false,
                "clip {} reached the engine cache with an empty source_id — \
                 the store boundary (assign_source_ids / minting sites) should make this unreachable",
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
    params: Arc<ParamTable>,
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
    sinks: Vec<(Box<dyn MeterSink>, u64)>,
    last_frame: Instant,
    last_tick: Instant,
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
            self.drain_meters();
            self.drain_rt_events();
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
        let (meter_tx, meter_rx) = rtrb::RingBuffer::new(64);
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
            params: self.params.clone(),
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
        {
            let mut session = self.session.lock();
            session.store.transport.sample_rate = rate;
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

    // -- graph lifecycle (prepare on control thread, swap by pointer) -------

    fn rebuild(&mut self) {
        self.ensure_loaded();
        let Some(out) = self.output.as_mut() else { return };
        // Free anything the callback already retired before queueing more.
        while let Ok(gp) = out.retire_rx.pop() {
            drop(gp);
        }
        let graph = {
            let session = self.session.lock();
            let store = &session.store;
            let mut tracks = Vec::with_capacity(store.tracks.len());
            for t in &store.tracks {
                let Some(&slot) = store.slots.get(&t.id) else { continue };
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
            // LIVE instrument tracks (phase 3, ARCHITECTURE §15): midi tracks
            // become RtTracks carrying a live node (SamplerNode when the
            // track's `instrument_id` resolves, plugin node for `plugin:`
            // refs — stub until zones P1/P2 land —, PolySynth fallback) plus
            // this snapshot's pre-scheduled events (ticks -> absolute samples
            // via TempoMap, HERE on the control thread; the RT thread only
            // slices sample-offset events). Nodes come from `live_nodes` so
            // voice/plugin state SURVIVES rebuilds; brand-new nodes are
            // prepared before the snapshot is published (RCU discipline).
            // Store and midi share one guard now, so this reads `session.midi`
            // directly instead of re-locking through the registered global.
            let bank = crate::audio::sampler::registered_bank().map(|b| b.lock());
            crate::midi::playback::append_from(
                &session.midi,
                store,
                self.cache_rate,
                bank.as_deref(),
                &mut self.live_nodes,
                &mut tracks,
            );
            // The timeline boundary belongs to the material, so it is derived
            // exactly where the material is assembled — same helper the
            // offline bounce uses, so live and export agree on where the song
            // ends (clip ends AND the final scheduled note-off).
            self.shared
                .song_end
                .store(offline::song_end(&tracks), Relaxed);
            Box::new(RtGraph::new(tracks))
        };
        if let Err(rtrb::PushError::Full(_gp)) = out.graph_tx.push(GraphPtr::new(graph)) {
            // Queue full (callback stalled?) — the returned GraphPtr frees
            // the fresh graph on drop, here on the control thread; the next
            // Rebuild retries.
            log::warn!("audio: graph queue full, rebuild dropped");
        }
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
            let session = self.session.lock();
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
        // (grouped by source), building from the cached (resampled) source
        // data — the pyramid is a peak/RMS visual overview, not
        // audio-critical, so reusing the already-decoded cache entry here
        // (rather than re-reading the file) is exact enough and avoids a
        // second decode of every source on every call.
        for (source_id, clip_ids) in &clips_by_source {
            let Some(cached) = self.cache.get(source_id) else { continue };
            let missing: Vec<&String> = clip_ids
                .iter()
                .filter(|clip_id| !pyramid_exists(&Store::cache_dir_for(&project_dir, clip_id)))
                .collect();
            if missing.is_empty() {
                continue;
            }
            let pyr = Pyramid::from_interleaved(&cached.data.data, cached.data.channels as usize);
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
                self.accum.fold(&blk);
            }
        }
        if let Some(inp) = self.input.as_mut() {
            while let Ok(blk) = inp.meter_rx.pop() {
                self.accum.fold(&blk);
            }
        }
    }

    fn pump_meter_frames(&mut self) {
        if self.sinks.is_empty() || self.last_frame.elapsed() < FRAME_INTERVAL {
            return;
        }
        self.last_frame = Instant::now();
        let tracks = self.session.lock().store.track_slots();
        let position = self.shared.position.load(Relaxed);
        let frame = self.accum.take_frame(0, &tracks, position);
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
        let snap = {
            let mut session = self.session.lock();
            session.store.transport.state = "stopped".into();
            session.store.transport.position_samples = at;
            crate::control::ops::transport_snapshot(&session.store, &self.shared)
        };
        if let Ok(v) = serde_json::to_value(&snap) {
            self.events.emit("transport://state", v);
        }
    }

    // -- recording ----------------------------------------------------------

    fn start_recording(&mut self, track_ids: Option<Vec<String>>) -> Result<Vec<String>, String> {
        if self.writer.is_some() {
            return Err("already recording".to_string());
        }

        // Resolve target tracks (explicit list or armed flags).
        let targets: Vec<String> = {
            let session = self.session.lock();
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

        // Rings + writer specs.
        let (project_dir, take_no, slots) = {
            let session = self.session.lock();
            let store = &session.store;
            let dir = store.project_dir.clone().ok_or("no project open")?;
            let slots: Vec<usize> = targets
                .iter()
                .filter_map(|id| store.slots.get(id.as_str()).copied())
                .collect();
            (dir, store.clips.len() + 1, slots)
        };
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

        let (meter_tx, meter_rx) = rtrb::RingBuffer::new(64);
        let n_producers = producers.len();
        let mut cb = InputCb {
            producers,
            owed: vec![0; n_producers],
            meter_tx,
            slots,
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

        self.input = Some(InputBundle { _stream: stream, meter_rx });
        self.writer = Some(writer);
        self.rec_track_ids = targets.clone();
        self.shared.recording.store(true, Relaxed);
        self.shared.playing.store(true, Relaxed);
        {
            let mut session = self.session.lock();
            session.store.transport.state = "recording".into();
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

    fn stop_recording(&mut self) -> Result<Vec<Clip>, String> {
        let writer = self.writer.take().ok_or("not recording")?;
        // Drop the input stream FIRST so the ring producers close and the
        // writer can drain to empty.
        self.input = None;
        let clips = writer.finish(Duration::from_secs(15))?;

        self.shared.recording.store(false, Relaxed);
        self.shared.playing.store(false, Relaxed);
        let track_ids = std::mem::take(&mut self.rec_track_ids);
        {
            let mut session = self.session.lock();
            session.store.transport.state = "stopped".into();
            session.store.clips.extend(clips.iter().cloned());
        }
        self.rebuild();
        self.events.emit(
            "recording://state",
            serde_json::json!({
                "recording": false,
                "trackIds": track_ids,
                "xruns": self.shared.xruns.load(Relaxed),
                "clips": clips,
            }),
        );
        // Persist the take into project.json right away.
        let snapshot = {
            let session = self.session.lock();
            project::from_store(
                &session.store,
                self.shared.position.load(Relaxed),
                self.engine_rate(),
            )
            .ok()
            .map(|p| (session.store.project_dir.clone().unwrap(), p))
        };
        if let Some((dir, p)) = snapshot {
            if let Err(e) = project::save(&dir, &p) {
                log::warn!("auto-save after recording failed: {e}");
            }
        }
        Ok(clips)
    }

    /// Auto-create a default project when recording starts with none open.
    fn ensure_project(&mut self) -> Result<(), String> {
        if self.session.lock().store.project_dir.is_some() {
            return Ok(());
        }
        let parent = dirs::audio_dir()
            .or_else(dirs::home_dir)
            .ok_or("cannot determine a directory for the default project")?
            .join("AURA");
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        let mut name = "Untitled".to_string();
        let mut n = 1;
        while parent.join(format!("{name}.aura")).exists() {
            n += 1;
            name = format!("Untitled-{n}");
        }
        let (project, dir) =
            project::create(&parent, &name, self.engine_rate(), 120.0)?;
        {
            let mut session = self.session.lock();
            session.store.project_dir = Some(dir);
            session.store.project_name = Some(project.name.clone());
            session.store.created_at = project.created_at.clone();
        }
        self.events.emit(
            "project://changed",
            serde_json::to_value(&project).unwrap_or_default(),
        );
        log::info!("audio: auto-created project {name}");
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
                params: Arc::new(ParamTable::default()),
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

    fn spin_up() -> (EngineHandle, Arc<SharedRt>, Arc<ParamTable>, Arc<Mutex<Session>>) {
        let shared = Arc::new(SharedRt::default());
        let params = Arc::new(ParamTable::default());
        let session = Arc::new(Mutex::new(Session::new(Store::default(), crate::midi::MidiStore::default())));
        let handle = start(
            shared.clone(),
            params.clone(),
            session.clone(),
            Box::new(NullEvents),
        );
        (handle, shared, params, session)
    }

    /// Runs with or without a real audio device: the engine falls back to
    /// headless mode, so meter frames must flow either way.
    #[test]
    fn engine_pumps_meter_frames_at_60hz() {
        let (handle, _shared, _params, session) = spin_up();
        {
            let mut session = session.lock();
            let s = &mut session.store;
            let slot = s.alloc_slot("t1").unwrap();
            assert_eq!(slot, 0);
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
        let (handle, shared, _params, _store) = spin_up();
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
        let mut cb = InputCb {
            producers: vec![producer],
            owed: vec![0],
            meter_tx,
            slots: vec![0],
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
        let graph = Box::new(RtGraph::new(vec![RtTrack::clips(
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
        )]));
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

    /// Same shape as `control::mod`'s `test_clip` helper.
    fn test_clip(id: &str, track_id: &str) -> Clip {
        Clip {
            id: id.into(),
            track_id: track_id.into(),
            name: "clip".into(),
            source_path: "audio/x.wav".into(),
            source_id: SourceId::default(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 48_000,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: 48_000,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
        }
    }

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
        // Default SourceId is the empty-string sentinel — must never reach
        // the cache (H-3). `stale_sources` both warns AND `debug_assert!`s on
        // this (the store boundary should make it unreachable), so a debug
        // build panics here — verify the assertion fires rather than calling
        // straight through (which would abort this test binary).
        let c = test_clip("c-1", "t-1");
        assert!(c.source_id.as_str().is_empty());
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // silence the expected panic's default print
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            stale_sources(std::slice::from_ref(&c), &cache)
        }));
        std::panic::set_hook(hook);
        assert!(
            result.is_err(),
            "debug_assert! catches an empty source_id reaching the cache boundary (H-3)"
        );
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

        let (handle, _shared, _params, session) = spin_up();
        let sid = SourceId::mint();
        {
            let mut session = session.lock();
            let s = &mut session.store;
            s.project_dir = Some(dir.clone());
            s.alloc_slot("t1");
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
}
