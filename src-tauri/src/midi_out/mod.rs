//! MIDI output: transport sync (clock/Start/Stop/Continue/SPP) and note-out
//! to external gear, with per-track and per-clip routing across any number
//! of simultaneously open hardware ports. Runs entirely off `SharedRt`
//! atomics + a document snapshot on its own non-RT threads — see ruling 3:
//! NOTHING here touches `audio/engine.rs`.
//!
//! ## Routing model
//!
//! A [`RouteScope`] (a track, or one specific clip) maps to a
//! [`RouteTarget`] (a port id + MIDI channel) in `MidiOut::routes` — a flat
//! table shared by every open port's thread, each of which filters it down
//! to just the entries that target its own port. A clip-level route always
//! wins over its track's route: a track route's events are built by
//! [`crate::midi::playback::track_events_excluding`] with every
//! clip-routed clip subtracted out, so a note is never sent twice.
//! Routing is app config, not document state (ruling 10, extended from
//! slice 2's single-track carve-out): no `Op`, no `commit`, never written
//! into `project.json` — see [`persist`] for the per-machine, per-project
//! file it DOES live in.
//!
//! ## One `aura-midi-out-<n>` thread per open port
//!
//! Opening a port spawns a thread exactly like slice 2's single-port
//! design did; opening a SECOND port spawns a second, independent thread
//! rather than tearing down the first. Each thread:
//!
//! * runs its own [`ClockEngine`] and its own `clock_enabled` flag (a
//!   device is slaved to the clock independently of any other open port),
//! * every 250 ms (never a blocking `session.try_lock()` — see below),
//!   re-derives ONLY the routes whose `port_id` is its own, converts each
//!   into a [`NoteOutSnapshot`], and keeps one [`NoteOutEngine`] per
//!   [`RouteScope`] (own cursor + sounding array, so a clip override's
//!   cursor never fights its track route's cursor, or another port's),
//! * self-heals: routes whose track/clip no longer exists in the document
//!   are dropped from the shared table during that same window (this is
//!   how a deleted clip's routing gets cleaned up — there is no
//!   clip-delete hook to retrofit, see `docs/midi-output.md`).
//!
//! Tempo snapshot: at most every 250 ms the thread attempts
//! `session.try_lock()` — **never** a blocking lock, because
//! `engine::rebuild` holds the session lock across a full graph build and a
//! blocking lock here would stall clock output for that whole window. A
//! contended try_lock is not an error: the thread simply keeps whatever it
//! already has and tries again on the next 250 ms boundary.
//!
//! Lock order: these threads take the session lock and the `routes` lock
//! only — never `EngineHandle::request` — so the Committer deadlock
//! invariant ("no thread may hold the session lock across a `request`")
//! holds trivially, by construction.
//!
//! On stop (close, app exit): a thread releases every sounding note across
//! every route it owns and then sends an explicit `ClockMsg::Stop`, both
//! before the sink (and the underlying `midir` connection) is dropped, so a
//! hardware slave synced to MIDI clock does not keep running forever after
//! AURA lets go of the port, and no note is left hanging on it.
//!
//! `Drop` is NOT the app-exit path and must not be relied on as one: Tauri
//! runs its event loop to a `process::exit`, so managed state is never
//! dropped. That is what `release_on_exit` exists for.

pub mod persist;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex as PlMutex;
use serde::Serialize;

use crate::audio::rt::SharedRt;
use crate::control::Session;
use crate::midi::tempo::TempoMap;
use crate::midi::{TempoEvent, DEFAULT_PPQ};
use crate::midi_input::MidiPortInfo;

/// One outbound MIDI message. POD so the clock and the note scheduler share
/// one output buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutMsg {
    pub bytes: [u8; 3],
    pub len: usize,
}

impl OutMsg {
    pub fn one(b: u8) -> Self {
        Self { bytes: [b, 0, 0], len: 1 }
    }

    pub fn three(a: u8, b: u8, c: u8) -> Self {
        Self { bytes: [a, b, c], len: 3 }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMsg {
    Clock,
    Start,
    Continue,
    Stop,
    SongPosition(u16),
}

impl ClockMsg {
    /// 0xF8 clock, 0xFA start, 0xFB continue, 0xFC stop,
    /// 0xF2 lsb msb song position (14-bit, 7 bits per byte).
    pub fn to_out(self) -> OutMsg {
        match self {
            ClockMsg::Clock => OutMsg::one(0xF8),
            ClockMsg::Start => OutMsg::one(0xFA),
            ClockMsg::Continue => OutMsg::one(0xFB),
            ClockMsg::Stop => OutMsg::one(0xFC),
            ClockMsg::SongPosition(spp) => {
                let lsb = (spp & 0x7F) as u8;
                let msb = ((spp >> 7) & 0x7F) as u8;
                OutMsg::three(0xF2, lsb, msb)
            }
        }
    }
}

/// MIDI clock is 24 pulses per quarter note, always — independent of ppq.
pub const PULSES_PER_QUARTER: u64 = 24;
/// Most clock pulses emitted in one tick; a bigger gap means the thread was
/// starved or the transport jumped, and is re-anchored instead of flooded.
pub const MAX_PULSE_BURST: u64 = 24;
/// Position deviation (in samples) that counts as a transport JUMP rather
/// than ordinary block quantization: 20 ms at the engine rate.
pub fn drift_tolerance(rate: u32) -> u64 {
    (rate as u64) / 50
}

pub fn pulse_of_tick(tick: u64, ppq: u32) -> u64 {
    (tick * PULSES_PER_QUARTER) / (ppq as u64)
}

/// SPP counts MIDI beats (sixteenth notes) = 6 clock pulses; 14-bit range.
pub fn song_position_of_pulse(pulse: u64) -> u16 {
    let spp = pulse / 6;
    spp.min(16_383) as u16
}

/// What the driver reads each tick.
pub struct ClockInput<'a> {
    pub now_micros: u64,
    pub playing: bool,
    pub position: u64,
    pub rate: u32,
    pub map: &'a TempoMap,
}

/// Anchor point: the (sample, wall-clock-micros) pair interpolation is
/// computed from between observed position updates.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    sample: u64,
    micros: u64,
}

#[derive(Default)]
pub struct ClockEngine {
    running: bool,
    anchor: Option<Anchor>,
    last_pulse: u64,
    pulses_sent: u64,
    resyncs: u64,
    estimated_sample: u64,
}

impl ClockEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append this tick's messages. Pure with respect to `now_micros` — the
    /// caller owns the clock, so tests drive it deterministically.
    pub fn step(&mut self, input: ClockInput<'_>, out: &mut Vec<ClockMsg>) {
        let ClockInput { now_micros, playing, position, rate, map } = input;

        // Clause 1: not playing.
        if !playing {
            if self.running {
                out.push(ClockMsg::Stop);
                self.anchor = None;
                self.running = false;
            }
            return;
        }

        // Clause 2: transport start.
        if !self.running {
            self.anchor = Some(Anchor { sample: position, micros: now_micros });
            self.estimated_sample = position;
            let pulse = pulse_of_tick(map.samples_to_tick(position), map.ppq());
            self.last_pulse = pulse;
            if pulse == 0 {
                out.push(ClockMsg::Start);
            } else {
                out.push(ClockMsg::SongPosition(song_position_of_pulse(pulse)));
                out.push(ClockMsg::Continue);
            }
            self.running = true;
            return;
        }

        // Clause 3: running — interpolate, detect jumps.
        let anchor = self.anchor.expect("running implies an anchor");
        let elapsed_us = now_micros.saturating_sub(anchor.micros);
        let est = anchor.sample + (elapsed_us * rate as u64) / 1_000_000;

        if position.abs_diff(est) > drift_tolerance(rate) {
            out.push(ClockMsg::Stop);
            self.anchor = Some(Anchor { sample: position, micros: now_micros });
            self.estimated_sample = position;
            let pulse = pulse_of_tick(map.samples_to_tick(position), map.ppq());
            self.last_pulse = pulse;
            out.push(ClockMsg::SongPosition(song_position_of_pulse(pulse)));
            out.push(ClockMsg::Continue);
            self.resyncs += 1;
            return;
        }

        self.estimated_sample = est;

        // Clause 4: pulse emission with burst clamp.
        let pulse = pulse_of_tick(map.samples_to_tick(est), map.ppq());
        let n = pulse.saturating_sub(self.last_pulse);
        if n > MAX_PULSE_BURST {
            self.last_pulse = pulse;
        } else {
            for _ in 0..n {
                out.push(ClockMsg::Clock);
            }
            self.last_pulse = pulse;
            self.pulses_sent += n;
        }
    }

    /// The interpolated sample position this tick resolved to (the note
    /// scheduler advances over the same window, from ONE anchor).
    pub fn estimated_sample(&self) -> u64 {
        self.estimated_sample
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn pulses_sent(&self) -> u64 {
        self.pulses_sent
    }

    pub fn resyncs(&self) -> u64 {
        self.resyncs
    }
}

// ---------------------------------------------------------------------------
// The sink abstraction and port enumeration.
// ---------------------------------------------------------------------------

/// Anything that can accept outbound MIDI bytes. `MidiOutSink` wraps a real
/// port; tests inject [`RecordingSink`].
pub trait ClockSink: Send {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
}

/// A real `midir` output connection.
pub struct MidiOutSink(midir::MidiOutputConnection);

impl ClockSink for MidiOutSink {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.send(bytes).map_err(|e| e.to_string())
    }
}

/// Recording sink for tests.
#[derive(Default)]
pub struct RecordingSink(pub Vec<Vec<u8>>);

impl ClockSink for RecordingSink {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Routing table.
// ---------------------------------------------------------------------------

/// What a route addresses: an entire track's notes, or one clip's (a clip
/// route always wins over its track's — see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RouteScope {
    Track(String),
    Clip(String),
}

/// Where a route sends its notes: a port id (as returned by
/// [`list_output_ports`]) and a MIDI channel (0-based on the wire, 0-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTarget {
    pub port_id: String,
    pub channel: u8,
}

// ---------------------------------------------------------------------------
// Note-out: the routed events, the per-route scheduler, and the shared
// snapshot type both consume.
// ---------------------------------------------------------------------------

/// One route's scheduled events, absolute engine samples, sorted — exactly
/// what `midi::playback`/`midi::schedule` hand the internal live node, so
/// external and internal timing come from ONE conversion.
#[derive(Clone, Default)]
pub struct NoteOutSnapshot {
    pub events: Arc<Vec<crate::midi::schedule::AbsNoteEvent>>,
    /// MIDI channel for outgoing notes (0-based on the wire).
    pub channel: u8,
}

/// Only `AbsNoteEvent` indices this module ever assumed sorted, ascending
/// by `sample`, get near either `advance` or `reseek` — both binary-search
/// or forward-scan the slice on that assumption. Cheap insurance against a
/// future builder producing out-of-order events, which would otherwise
/// stop the cursor forever, silently.
fn debug_assert_events_sorted(events: &[crate::midi::schedule::AbsNoteEvent]) {
    debug_assert!(
        events.windows(2).all(|w| w[0].sample <= w[1].sample),
        "NoteOutSnapshot::events must be sorted ascending by sample"
    );
}

/// Scheduler state for ONE route: a cursor into `NoteOutSnapshot::events`
/// (advanced forward by `advance`, but repositioned in EITHER direction by
/// `reseek`) plus a per-key sounding flag so `all_off`/`reseek` release
/// exactly what is actually sounding — never more, never less. Every
/// `RouteScope` gets its own, so a clip override and its track's route (or
/// two independent routes on the same port) never share cursor state.
pub struct NoteOutEngine {
    cursor: usize,
    sounding: [bool; 128],
    notes_sent: u64,
}

impl Default for NoteOutEngine {
    fn default() -> Self {
        Self { cursor: 0, sounding: [false; 128], notes_sent: 0 }
    }
}

impl NoteOutEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emit note messages for the half-open sample window `[from, to)`. The
    /// cursor is monotonic — it only ever moves forward through `snap.
    /// events` — so `from` is documentation of intent (the caller's last
    /// window edge) rather than a filter this method re-checks; `to` is the
    /// only bound that actually gates emission.
    pub fn advance(&mut self, snap: &NoteOutSnapshot, from: u64, to: u64, out: &mut Vec<OutMsg>) {
        debug_assert!(from <= to, "advance window must not go backward");
        debug_assert_events_sorted(&snap.events);
        while self.cursor < snap.events.len() && snap.events[self.cursor].sample < to {
            let ev = snap.events[self.cursor];
            if ev.velocity == 0 {
                out.push(OutMsg::three(0x80 | snap.channel, ev.key, 0));
                self.sounding[ev.key as usize] = false;
            } else {
                out.push(OutMsg::three(0x90 | snap.channel, ev.key, ev.velocity));
                self.sounding[ev.key as usize] = true;
            }
            self.notes_sent += 1;
            self.cursor += 1;
        }
    }

    /// Release everything currently sounding (transport stop, resync, route
    /// removed/changed, port close). Emits one Note Off per sounding key —
    /// explicit offs, not just CC 123, because not every device honors All
    /// Notes Off.
    pub fn all_off(&mut self, channel: u8, out: &mut Vec<OutMsg>) {
        for key in 0..128u8 {
            if self.sounding[key as usize] {
                out.push(OutMsg::three(0x80 | channel, key, 0));
                self.sounding[key as usize] = false;
                self.notes_sent += 1;
            }
        }
    }

    /// `all_off` + reposition the cursor to the first event at or after
    /// `to` (a seek / loop wrap / fresh start / route swap). A binary
    /// search over the FULL slice, from scratch every call: it never reads
    /// `self.cursor` going in, so a rewind, a loop wrap, or an entirely
    /// different (shorter or longer) events array under the same
    /// `NoteOutEngine` are all handled uniformly.
    pub fn reseek(&mut self, snap: &NoteOutSnapshot, to: u64, out: &mut Vec<OutMsg>) {
        debug_assert_events_sorted(&snap.events);
        self.all_off(snap.channel, out);
        self.cursor = snap.events.partition_point(|e| e.sample < to);
    }

    pub fn notes_sent(&self) -> u64 {
        self.notes_sent
    }
}

/// One route's live scheduler state, keyed by `RouteScope` in a port
/// thread's local map — this is what persists (cursor + sounding array)
/// across 250 ms snapshot windows, so a content-unchanged refresh never
/// disturbs an in-flight note.
pub struct RouteNoteState {
    pub engine: NoteOutEngine,
    pub snapshot: NoteOutSnapshot,
}

/// Enumerate MIDI OUTPUT ports currently visible to the platform backend
/// (ALSA-seq on Linux). Mirrors `midi_input::list_ports`: same `"<name>#
/// <index>"` id scheme, same skip-on-vanish rule (a port that disappears
/// between `.ports()` and `.port_name()` is silently dropped rather than
/// failing the whole call), and creates/drops a throwaway `midir::
/// MidiOutput` client — no connection is held.
pub fn list_output_ports() -> Result<Vec<MidiPortInfo>, String> {
    let midi_out = midir::MidiOutput::new("aura-midi-output-enum").map_err(|e| e.to_string())?;
    let ports = midi_out.ports();
    let mut out = Vec::with_capacity(ports.len());
    for (i, p) in ports.iter().enumerate() {
        if let Ok(name) = midi_out.port_name(p) {
            out.push(MidiPortInfo { id: format!("{name}#{i}"), name });
        }
    }
    Ok(out)
}

/// Live status of one open port.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortStatus {
    pub port: MidiPortInfo,
    pub clock_enabled: bool,
    /// The clock engine currently considers the transport running.
    pub running: bool,
    pub pulses_sent: u64,
    pub resyncs: u64,
    pub notes_sent: u64,
}

/// One entry of the routing table, serialized for the frontend.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteStatus {
    /// `"track"` or `"clip"`.
    pub scope: &'static str,
    pub id: String,
    pub port_id: String,
    pub channel: u8,
}

/// Live status snapshot returned by [`MidiOut::status`].
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiOutputStatus {
    pub outputs: Vec<PortStatus>,
    pub routes: Vec<RouteStatus>,
}

/// State a port's `aura-midi-out-<n>` thread updates every tick and
/// `MidiOut::status` reads back — no lock needed since it's all atomics.
#[derive(Default)]
struct ThreadShared {
    stop: AtomicBool,
    pulses_sent: AtomicU64,
    resyncs: AtomicU64,
    running: AtomicBool,
    notes_sent: AtomicU64,
}

struct ActiveOutput {
    port: MidiPortInfo,
    thread_shared: Arc<ThreadShared>,
    /// Per-port: whether THIS device is slaved to AURA's clock. Defaults to
    /// enabled so plugging a device in and pressing play "just works"
    /// without an extra step (slice 2's precedent).
    clock_enabled: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Inner {
    active: HashMap<String, ActiveOutput>,
}

/// Owns every currently open MIDI output connection and the
/// `aura-midi-out-<n>` threads driving them, plus the routing table shared
/// across all of them. See the module doc for the full contract.
pub struct MidiOut {
    /// Set once by lib.rs setup via [`MidiOut::attach`]. `None` in every
    /// unit test that builds a bare `MidiOut::default()` — a thread then
    /// falls back to "transport never plays", which is exactly what the
    /// port-enumeration/error-path tests below need and nothing more.
    session: OnceLock<Arc<PlMutex<Session>>>,
    shared: OnceLock<Arc<SharedRt>>,
    /// Track/clip -> port+channel, shared by every open port's thread (each
    /// filters to its own port id). Survives a port close/reopen — routing
    /// is independent of which ports happen to be open right now, same as
    /// slice 2's single `note_track` field was.
    routes: Arc<PlMutex<HashMap<RouteScope, RouteTarget>>>,
    /// Override for the per-machine routing file's path. `None` (the
    /// production default) means "use `persist::default_path()`" — every
    /// real app run. Tests set this to a throwaway temp file via
    /// [`MidiOut::set_routing_path_for_test`] so `persist`/`adopt_project`
    /// never touch the real developer machine's config.
    routing_path: PlMutex<Option<std::path::PathBuf>>,
    inner: PlMutex<Inner>,
}

impl Default for MidiOut {
    fn default() -> Self {
        Self {
            session: OnceLock::new(),
            shared: OnceLock::new(),
            routes: Arc::new(PlMutex::new(HashMap::new())),
            routing_path: PlMutex::new(None),
            inner: PlMutex::new(Inner::default()),
        }
    }
}

impl MidiOut {
    /// lib.rs setup, once: threads need the transport atomics and a way to
    /// snapshot tempo. A second call is silently ignored (first attach
    /// wins).
    pub fn attach(&self, session: Arc<PlMutex<Session>>, shared: Arc<SharedRt>) {
        let _ = self.session.set(session);
        let _ = self.shared.set(shared);
    }

    /// Open one more output port, additively — does not touch any other
    /// port already open. Re-opening an already-open port is a no-op (fast
    /// path, mirrors slice 2's `select_port`).
    pub fn open_port(&self, port_id: String) -> Result<(), String> {
        let mut inner = self.inner.lock();
        if inner.active.contains_key(&port_id) {
            return Ok(());
        }

        let midi_out = midir::MidiOutput::new("aura-midi-output").map_err(|e| e.to_string())?;
        let ports = midi_out.ports();
        let mut found = None;
        for (i, p) in ports.iter().enumerate() {
            let name = match midi_out.port_name(p) {
                Ok(n) => n,
                Err(_) => continue, // vanished mid-enumeration; skip
            };
            if format!("{name}#{i}") == port_id {
                found = Some((p.clone(), MidiPortInfo { id: port_id.clone(), name }));
                break;
            }
        }
        let (port, info) = found.ok_or_else(|| format!("MIDI output port not found: {port_id}"))?;

        let conn = midi_out
            .connect(&port, "aura-midi-output")
            .map_err(|e| e.to_string())?;
        let sink = MidiOutSink(conn);

        let thread_shared = Arc::new(ThreadShared::default());
        let clock_enabled = Arc::new(AtomicBool::new(true));
        let ts_for_thread = thread_shared.clone();
        let clock_for_thread = clock_enabled.clone();
        let session_for_thread = self.session.get().cloned();
        let shared_for_thread = self.shared.get().cloned();
        let routes_for_thread = self.routes.clone();
        let port_id_for_thread = port_id.clone();

        let handle = std::thread::Builder::new()
            .name(format!("aura-midi-out-{port_id}"))
            .spawn(move || {
                run_thread(
                    sink,
                    ts_for_thread,
                    session_for_thread,
                    shared_for_thread,
                    clock_for_thread,
                    routes_for_thread,
                    port_id_for_thread,
                )
            })
            .map_err(|e| e.to_string())?;

        inner.active.insert(
            port_id,
            ActiveOutput { port: info, thread_shared, clock_enabled, handle: Some(handle) },
        );
        Ok(())
    }

    /// Close one open port (Stop-then-drop, see module doc). Closing a port
    /// that isn't open is a no-op.
    pub fn close_port(&self, port_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock();
        if let Some(mut active) = inner.active.remove(port_id) {
            active.thread_shared.stop.store(true, Relaxed);
            if let Some(handle) = active.handle.take() {
                let _ = handle.join();
            }
        }
        Ok(())
    }

    /// Close every open port (app exit, or a from-scratch reset).
    fn close_all(&self) {
        let ids: Vec<String> = self.inner.lock().active.keys().cloned().collect();
        for id in ids {
            let _ = self.close_port(&id);
        }
    }

    /// Flip a port's live clock-enable flag. Never touches the thread/
    /// connection — safe to call any time the port is open. Errors if the
    /// port isn't currently open (there is no persistent per-port state to
    /// flip until it is).
    pub fn set_clock_enabled(&self, port_id: &str, enabled: bool) -> Result<(), String> {
        let inner = self.inner.lock();
        let active = inner
            .active
            .get(port_id)
            .ok_or_else(|| format!("MIDI output port not open: {port_id}"))?;
        active.clock_enabled.store(enabled, Relaxed);
        Ok(())
    }

    /// Route (or, on `None`, un-route) a track or clip's notes to external
    /// gear. Never touches any thread/connection directly — safe to call
    /// whether or not the target port is currently open (a route to a
    /// closed port simply produces nothing until it opens). Each port
    /// thread picks up the change on its next 250 ms refresh window and
    /// releases whatever the PREVIOUS routing left sounding before adopting
    /// the new one.
    pub fn set_route(&self, scope: RouteScope, target: Option<RouteTarget>) {
        let mut routes = self.routes.lock();
        match target {
            Some(t) => {
                routes.insert(scope, t);
            }
            None => {
                routes.remove(&scope);
            }
        }
    }

    /// Every route currently targeting `port_id` — removed as a group. A
    /// port being retired should not leave routes silently pointing at
    /// nothing forever; the user re-routes explicitly if they meant to
    /// swap devices instead of closing one for good.
    pub fn clear_routes_for_port(&self, port_id: &str) {
        self.routes.lock().retain(|_, t| t.port_id != port_id);
    }

    /// Drop every route addressing this track (its own track-level route,
    /// AND any of its clips' overrides) — called after a track is removed
    /// from the document (ruling 10: nothing in the document model retires
    /// these on its own).
    pub fn clear_routes_for_track(&self, track_id: &str, clip_ids: &[String]) {
        let mut routes = self.routes.lock();
        routes.remove(&RouteScope::Track(track_id.to_string()));
        for id in clip_ids {
            routes.remove(&RouteScope::Clip(id.clone()));
        }
    }

    pub fn routes(&self) -> HashMap<RouteScope, RouteTarget> {
        self.routes.lock().clone()
    }

    /// Cheap live snapshot for polling.
    pub fn status(&self) -> MidiOutputStatus {
        let inner = self.inner.lock();
        let outputs = inner
            .active
            .values()
            .map(|active| PortStatus {
                port: active.port.clone(),
                clock_enabled: active.clock_enabled.load(Relaxed),
                running: active.thread_shared.running.load(Relaxed),
                pulses_sent: active.thread_shared.pulses_sent.load(Relaxed),
                resyncs: active.thread_shared.resyncs.load(Relaxed),
                notes_sent: active.thread_shared.notes_sent.load(Relaxed),
            })
            .collect();
        drop(inner);
        let routes = self
            .routes()
            .into_iter()
            .map(|(scope, target)| {
                let (kind, id) = match scope {
                    RouteScope::Track(id) => ("track", id),
                    RouteScope::Clip(id) => ("clip", id),
                };
                RouteStatus { scope: kind, id, port_id: target.port_id, channel: target.channel }
            })
            .collect();
        MidiOutputStatus { outputs, routes }
    }

    /// Called when a project finishes opening (or is freshly created): read
    /// the per-machine routing file, resolve this project's persisted
    /// routes (by track/clip id) and remembered ports (by NAME) against
    /// whatever is visible on this machine right now, open what resolves,
    /// and adopt each port's clock-enabled preference. Best-effort
    /// throughout — missing hardware, or a track/clip id the project no
    /// longer has, is logged and skipped, never an error.
    pub fn adopt_project(&self, project_dir: &Path) {
        let file = self.load_routing();
        let key = persist::project_key(project_dir);
        let Some(proj) = file.projects.get(&key) else { return };

        let available = list_output_ports().unwrap_or_default();
        let name_to_id: HashMap<&str, &str> =
            available.iter().map(|p| (p.name.as_str(), p.id.as_str())).collect();

        let mut new_routes = HashMap::new();
        for r in &proj.routes {
            let Some(&port_id) = name_to_id.get(r.port_name.as_str()) else {
                log::info!(
                    "midi routing: port '{}' not found on this machine; skipping a persisted route",
                    r.port_name
                );
                continue;
            };
            if !self.inner.lock().active.contains_key(port_id) {
                if let Err(e) = self.open_port(port_id.to_string()) {
                    log::warn!("midi routing: failed to open persisted port {port_id}: {e}");
                    continue;
                }
            }
            let scope = match r.scope.as_str() {
                "track" => RouteScope::Track(r.id.clone()),
                "clip" => RouteScope::Clip(r.id.clone()),
                other => {
                    log::warn!("midi routing: unknown persisted scope '{other}'; skipping");
                    continue;
                }
            };
            new_routes.insert(scope, RouteTarget { port_id: port_id.to_string(), channel: r.channel });
        }
        *self.routes.lock() = new_routes;

        let inner = self.inner.lock();
        for active in inner.active.values() {
            if let Some(prefs) = file.ports.get(&active.port.name) {
                active.clock_enabled.store(prefs.clock_enabled, Relaxed);
            }
        }
    }

    /// Save the current port-clock prefs (always) and, if `project_dir` is
    /// `Some`, this project's current routing table (keyed by that
    /// directory) to the per-machine file. `project_dir` is `None` for an
    /// unsaved project — its routes simply aren't written, so they fall
    /// back to session-only, same as before persistence existed at all.
    pub fn persist(&self, project_dir: Option<&Path>) {
        let mut file = self.load_routing();
        let inner = self.inner.lock();
        for active in inner.active.values() {
            file.ports.insert(
                active.port.name.clone(),
                persist::PortPrefs { clock_enabled: active.clock_enabled.load(Relaxed) },
            );
        }
        if let Some(dir) = project_dir {
            let id_to_name: HashMap<&str, &str> =
                inner.active.values().map(|a| (a.port.id.as_str(), a.port.name.as_str())).collect();
            let routes = self.routes.lock();
            let persisted: Vec<persist::PersistedRoute> = routes
                .iter()
                .filter_map(|(scope, target)| {
                    let port_name = (*id_to_name.get(target.port_id.as_str())?).to_string();
                    let (kind, id) = match scope {
                        RouteScope::Track(id) => ("track", id.clone()),
                        RouteScope::Clip(id) => ("clip", id.clone()),
                    };
                    Some(persist::PersistedRoute {
                        scope: kind.into(),
                        id,
                        port_name,
                        channel: target.channel,
                    })
                })
                .collect();
            file.projects.insert(persist::project_key(dir), persist::ProjectRouting { routes: persisted });
        }
        drop(inner);
        self.save_routing(&file);
    }

    fn load_routing(&self) -> persist::RoutingFile {
        match &*self.routing_path.lock() {
            Some(p) => persist::load_from_path(p),
            None => persist::load(),
        }
    }

    fn save_routing(&self, file: &persist::RoutingFile) {
        match &*self.routing_path.lock() {
            Some(p) => persist::save_to_path(p, file),
            None => persist::save(file),
        }
    }

    /// Redirect `persist`/`adopt_project` to a throwaway path instead of the
    /// real per-machine config file — every test that exercises routing
    /// persistence (directly, or indirectly through `ControlPlane`'s
    /// route/port/clock methods) MUST call this right after construction,
    /// or it will read and overwrite the real developer machine's MIDI
    /// routing file.
    #[cfg(test)]
    pub(crate) fn set_routing_path_for_test(&self, path: std::path::PathBuf) {
        *self.routing_path.lock() = Some(path);
    }
}

impl Drop for MidiOut {
    /// Belt and suspenders: normal teardown already happens in
    /// `close_port`, but if a `MidiOut` is simply dropped with connections
    /// open (e.g. process exit), stop every thread rather than leaking it —
    /// each thread itself still performs the Stop-then-drop sequence before
    /// it returns.
    fn drop(&mut self) {
        let ids: Vec<String> = self.inner.lock().active.keys().cloned().collect();
        for id in ids {
            let mut inner = self.inner.lock();
            if let Some(mut active) = inner.active.remove(&id) {
                active.thread_shared.stop.store(true, Relaxed);
                drop(inner);
                if let Some(handle) = active.handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }
}

/// Release the hardware on app exit: closes every open port, which joins
/// each `aura-midi-out-<n>` thread and runs its proven shutdown sequence
/// (every sounding note released, then `Stop`, both while the connection is
/// still alive). Wired to `RunEvent::Exit` in `lib.rs::run`.
///
/// `MidiOut::drop` cannot serve this purpose — Tauri's event loop ends in
/// `process::exit`, so managed state is never dropped, and before this
/// existed, closing the window while a note sounded left it hanging on the
/// external synth forever while it free-ran with no `Stop`.
pub fn release_on_exit(out: &MidiOut) {
    out.close_all();
}

/// A 120 bpm, one-event tempo map at the given rate — the fallback used
/// before the first successful tempo snapshot (or permanently, in tests
/// that never call `attach`).
fn default_tempo_map(rate: u32) -> TempoMap {
    TempoMap::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: 120.0 }], rate)
        .expect("a single tick-0 tempo event is always a valid TempoMap")
}

/// Everything routed to THIS port, addressed by the SAME clock tick and
/// (per-route) engine as `run_thread` maintains between 250 ms windows.
type RouteNoteStates = HashMap<RouteScope, RouteNoteState>;

/// The `aura-midi-out-<n>` thread body — see the module doc for the full
/// contract (500 µs loop, 250 ms try_lock tempo snapshot, Stop-on-exit,
/// session-lock-only, no `EngineHandle::request`). One instance of this
/// runs per open port; `port_id` is this thread's own identity into the
/// shared `routes` table.
fn run_thread(
    mut sink: MidiOutSink,
    thread_shared: Arc<ThreadShared>,
    session: Option<Arc<PlMutex<Session>>>,
    shared_rt: Option<Arc<SharedRt>>,
    clock_enabled: Arc<AtomicBool>,
    routes: Arc<PlMutex<HashMap<RouteScope, RouteTarget>>>,
    port_id: String,
) {
    let mut engine = ClockEngine::new();
    let mut route_states: RouteNoteStates = HashMap::new();
    let fallback_rate = shared_rt
        .as_ref()
        .map(|s| s.sample_rate.load(Relaxed))
        .filter(|&r| r > 0)
        .unwrap_or(48_000);
    let mut map = default_tempo_map(fallback_rate);
    // The last (ppq, tempo_events, rate) triple the map was built from —
    // rebuilding is skipped when nothing in this triple changed.
    let mut snapshot_key: Option<(u32, Vec<TempoEvent>, u32)> = None;
    // Backdated so the very first loop iteration attempts a snapshot
    // immediately, rather than waiting a full 250 ms to see a project's
    // real tempo for the first time.
    let mut last_snapshot = Instant::now() - Duration::from_millis(250);
    let start = Instant::now();

    loop {
        if thread_shared.stop.load(Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_micros(500));

        if last_snapshot.elapsed() >= Duration::from_millis(250) {
            if let Some(session) = &session {
                // NEVER a blocking lock — `engine::rebuild` holds the
                // session across a graph build, and this thread must never
                // stall waiting for it out.
                if let Some(guard) = session.try_lock() {
                    let rate = shared_rt
                        .as_ref()
                        .map(|s| s.sample_rate.load(Relaxed))
                        .filter(|&r| r > 0)
                        .unwrap_or(fallback_rate);
                    let key = (guard.midi.ppq, guard.midi.tempo_events.clone(), rate);
                    if snapshot_key.as_ref() != Some(&key) {
                        if let Ok(new_map) = TempoMap::new(key.0, key.1.clone(), key.2) {
                            map = new_map;
                            snapshot_key = Some(key);
                        }
                    }

                    // Self-heal: drop this port's own routes whose track/
                    // clip no longer exists in the document, then take a
                    // snapshot of the (now-clean) full table.
                    let all_routes: HashMap<RouteScope, RouteTarget> = {
                        let mut all = routes.lock();
                        all.retain(|scope, target| {
                            if target.port_id != port_id {
                                return true; // not this thread's to judge
                            }
                            match scope {
                                RouteScope::Track(id) => {
                                    guard.store.tracks.iter().any(|t| t.id.as_str() == id.as_str())
                                }
                                RouteScope::Clip(id) => {
                                    guard.midi.clips.iter().any(|c| c.id.as_str() == id.as_str())
                                }
                            }
                        });
                        all.clone()
                    };

                    // Any clip with its OWN route (on any port) is excluded
                    // from its track's route — a clip override always wins.
                    let overridden_clips: HashSet<String> = all_routes
                        .keys()
                        .filter_map(|s| match s {
                            RouteScope::Clip(id) => Some(id.clone()),
                            RouteScope::Track(_) => None,
                        })
                        .collect();

                    let mine: Vec<(RouteScope, RouteTarget)> = all_routes
                        .iter()
                        .filter(|(_, t)| t.port_id == port_id)
                        .map(|(s, t)| (s.clone(), t.clone()))
                        .collect();

                    let mut new_snapshots: HashMap<RouteScope, NoteOutSnapshot> = HashMap::new();
                    for (scope, target) in &mine {
                        let events = match scope {
                            RouteScope::Track(track_id) => crate::midi::playback::track_events_excluding(
                                &guard.midi,
                                track_id,
                                &overridden_clips,
                                &map,
                            ),
                            RouteScope::Clip(clip_id) => guard
                                .midi
                                .clips
                                .iter()
                                .find(|c| c.id.as_str() == clip_id.as_str())
                                .map(|c| crate::midi::schedule::clip_events(c, &map))
                                .unwrap_or_default(),
                        };
                        new_snapshots.insert(
                            scope.clone(),
                            NoteOutSnapshot { events: Arc::new(events), channel: target.channel },
                        );
                    }
                    drop(guard);

                    let pos_now = shared_rt.as_ref().map(|s| s.position.load(Relaxed)).unwrap_or(0);

                    // Routes that vanished (deleted, or re-pointed at a
                    // different port): release whatever they left sounding
                    // and drop their engine.
                    let gone: Vec<RouteScope> = route_states
                        .keys()
                        .filter(|k| !new_snapshots.contains_key(k))
                        .cloned()
                        .collect();
                    for scope in gone {
                        if let Some(mut state) = route_states.remove(&scope) {
                            let mut edge = Vec::new();
                            state.engine.all_off(state.snapshot.channel, &mut edge);
                            for m in &edge {
                                let _ = sink.send(m.as_slice());
                            }
                        }
                    }

                    // New or changed routes: whole-track review, Critical
                    // 1's lesson generalized — compare CONTENT (events +
                    // channel), not just presence, because a route whose
                    // scope id is unchanged can still have its underlying
                    // clip/track edited out from under it. A same-content
                    // refresh must not touch a live cursor.
                    for (scope, snap) in new_snapshots {
                        match route_states.get_mut(&scope) {
                            Some(existing)
                                if existing.snapshot.events == snap.events
                                    && existing.snapshot.channel == snap.channel => {}
                            Some(existing) => {
                                let mut edge = Vec::new();
                                existing.engine.all_off(existing.snapshot.channel, &mut edge);
                                existing.engine.reseek(&snap, pos_now, &mut edge);
                                for m in &edge {
                                    let _ = sink.send(m.as_slice());
                                }
                                existing.snapshot = snap;
                            }
                            None => {
                                let mut engine = NoteOutEngine::new();
                                let mut edge = Vec::new();
                                engine.reseek(&snap, pos_now, &mut edge);
                                for m in &edge {
                                    let _ = sink.send(m.as_slice());
                                }
                                route_states.insert(scope, RouteNoteState { engine, snapshot: snap });
                            }
                        }
                    }

                    last_snapshot = Instant::now();
                }
                // Contended try_lock: keep the previous state and simply
                // try again on the next iteration where the 250ms window
                // has reopened (last_snapshot is left untouched).
            } else {
                // Unattached (every unit test that never calls `attach`):
                // nothing to snapshot from, so there is nothing to retry —
                // avoid re-checking on every single tick.
                last_snapshot = Instant::now();
            }
        }

        let (playing, position, rate) = match &shared_rt {
            Some(s) => (
                s.playing.load(Relaxed),
                s.position.load(Relaxed),
                s.sample_rate.load(Relaxed),
            ),
            None => (false, 0, fallback_rate),
        };
        let rate = if rate > 0 { rate } else { fallback_rate };
        let now_micros = start.elapsed().as_micros() as u64;
        let enabled = clock_enabled.load(Relaxed);

        let input = ClockInput { now_micros, playing, position, rate, map: &map };
        if let Err(e) = out_tick(&mut engine, &mut route_states, &mut sink, input, enabled) {
            log::warn!("aura-midi-out: send failed: {e}");
        }
        let notes_sent: u64 = route_states.values().map(|s| s.engine.notes_sent()).sum();
        thread_shared.notes_sent.store(notes_sent, Relaxed);

        thread_shared.running.store(engine.running(), Relaxed);
        thread_shared.pulses_sent.store(engine.pulses_sent(), Relaxed);
        thread_shared.resyncs.store(engine.resyncs(), Relaxed);
    }

    // Release whatever is still sounding, across EVERY route this port was
    // driving, BEFORE the explicit Stop and BEFORE `sink` (and its `midir`
    // connection) is dropped at the end of this function — otherwise a
    // note started shortly before a port close/app-exit hangs on the
    // external device forever.
    shutdown_release_and_stop(&mut sink, &mut route_states);
}

/// All-off (for every route, if anything is sounding) + the transport
/// Stop, sent in that order to `sink` while it is still alive — the caller
/// drops `sink` immediately after this returns, so both sends must
/// complete here, not be left to a buffer that dies with the connection.
fn shutdown_release_and_stop(sink: &mut dyn ClockSink, route_states: &mut RouteNoteStates) {
    let mut edge = Vec::new();
    for state in route_states.values_mut() {
        state.engine.all_off(state.snapshot.channel, &mut edge);
    }
    for msg in &edge {
        let _ = sink.send(msg.as_slice());
    }
    let _ = sink.send(ClockMsg::Stop.to_out().as_slice());
}

/// The per-tick body, extracted so it is testable with an injected sink and
/// injected time — no thread, no `SharedRt`, no `Session`.
/// `clock_enabled == false` suppresses clock/transport BYTES only — the
/// engine still steps every tick, so its anchor/`estimated_sample()` keeps
/// advancing in lockstep with real time (every route's note scheduler
/// advances over that SAME window, so the anchor must stay live even with
/// clock output off).
pub(crate) fn out_tick(
    engine: &mut ClockEngine,
    routes: &mut RouteNoteStates,
    sink: &mut dyn ClockSink,
    input: ClockInput<'_>,
    clock_enabled: bool,
) -> Result<(), String> {
    let playing = input.playing;
    let was_running = engine.running();
    let prev_estimated = engine.estimated_sample();
    let mut out = Vec::new();
    engine.step(input, &mut out);
    let resynced = out.iter().any(|m| matches!(m, ClockMsg::SongPosition(_)));

    for state in routes.values_mut() {
        let snap = &state.snapshot;
        let notes = &mut state.engine;
        let mut note_out = Vec::new();
        if !playing {
            if was_running {
                notes.all_off(snap.channel, &mut note_out);
            }
        } else if !was_running {
            // Fresh start: reposition past anything already behind us, then
            // catch the boundary sample itself — `reseek`'s "at or after"
            // cursor deliberately leaves an event exactly AT the landing
            // point unprocessed for the very next `advance`, so a start
            // from a position that lands exactly on a note needs that
            // immediate follow-up advance in the SAME tick.
            let est = engine.estimated_sample();
            notes.reseek(snap, est, &mut note_out);
            notes.advance(snap, est, est.saturating_add(1), &mut note_out);
        } else if was_running && resynced {
            // Resync (backward jump / large forward seek): same
            // "reseek + catch the boundary" shape as a fresh start.
            let est = engine.estimated_sample();
            notes.reseek(snap, est, &mut note_out);
            notes.advance(snap, est, est.saturating_add(1), &mut note_out);
        } else {
            notes.advance(snap, prev_estimated, engine.estimated_sample(), &mut note_out);
        }
        for msg in &note_out {
            sink.send(msg.as_slice())?;
        }
    }

    if !clock_enabled {
        return Ok(());
    }
    for msg in out {
        sink.send(msg.to_out().as_slice())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri commands — thin wrappers, additive only (registered from lib.rs).
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn midi_list_output_ports() -> Result<Vec<MidiPortInfo>, String> {
    list_output_ports()
}

#[tauri::command]
pub fn midi_open_output_port(
    port_id: String,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.open_midi_output_port(port_id, crate::control::op::TxMeta::user("open midi output port"))
}

#[tauri::command]
pub fn midi_close_output_port(
    port_id: String,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.close_midi_output_port(port_id, crate::control::op::TxMeta::user("close midi output port"))
}

#[tauri::command]
pub fn midi_set_output_clock_enabled(
    port_id: String,
    enabled: bool,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.set_midi_output_clock_enabled(
        port_id,
        enabled,
        crate::control::op::TxMeta::user("set midi output clock enabled"),
    )
}

#[tauri::command]
pub fn midi_output_status(state: tauri::State<'_, Arc<MidiOut>>) -> Result<MidiOutputStatus, String> {
    Ok(state.status())
}

#[tauri::command]
pub fn midi_set_track_route(
    track_id: String,
    port_id: Option<String>,
    channel: Option<u8>,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.set_midi_track_route(
        track_id,
        port_id,
        channel.unwrap_or(0),
        crate::control::op::TxMeta::user("set midi track route"),
    )
}

#[tauri::command]
pub fn midi_set_clip_route(
    clip_id: String,
    port_id: Option<String>,
    channel: Option<u8>,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.set_midi_clip_route(
        clip_id,
        port_id,
        channel.unwrap_or(0),
        crate::control::op::TxMeta::user("set midi clip route"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::types::{DEFAULT_PPQ, TempoEvent};

    fn map120() -> TempoMap {
        TempoMap::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: 120.0 }], 48_000).unwrap()
    }

    fn step(e: &mut ClockEngine, now_us: u64, playing: bool, pos: u64, map: &TempoMap) -> Vec<ClockMsg> {
        let mut out = Vec::new();
        e.step(ClockInput { now_micros: now_us, playing, position: pos, rate: 48_000, map }, &mut out);
        out
    }

    #[test]
    fn wire_bytes_match_the_midi_spec() {
        assert_eq!(ClockMsg::Clock.to_out().as_slice(), &[0xF8]);
        assert_eq!(ClockMsg::Start.to_out().as_slice(), &[0xFA]);
        assert_eq!(ClockMsg::Continue.to_out().as_slice(), &[0xFB]);
        assert_eq!(ClockMsg::Stop.to_out().as_slice(), &[0xFC]);
        // 14-bit SPP, lsb first: 200 = 0x0C8 -> lsb 0x48, msb 0x01
        assert_eq!(ClockMsg::SongPosition(200).to_out().as_slice(), &[0xF2, 0x48, 0x01]);
    }

    #[test]
    fn pulses_are_24_per_quarter_and_spp_counts_sixteenths() {
        assert_eq!(pulse_of_tick(0, 960), 0);
        assert_eq!(pulse_of_tick(960, 960), 24);
        assert_eq!(pulse_of_tick(480, 960), 12);
        assert_eq!(song_position_of_pulse(0), 0);
        assert_eq!(song_position_of_pulse(6), 1);
        assert_eq!(song_position_of_pulse(24), 4, "one beat = four sixteenths");
        assert_eq!(song_position_of_pulse(u64::MAX), 16_383, "clamped to 14 bits");
    }

    #[test]
    fn play_from_zero_emits_start_not_continue() {
        let (m, mut e) = (map120(), ClockEngine::new());
        assert_eq!(step(&mut e, 0, true, 0, &m), vec![ClockMsg::Start]);
        assert!(e.running());
    }

    #[test]
    fn play_from_mid_song_emits_song_position_then_continue() {
        let (m, mut e) = (map120(), ClockEngine::new());
        // Two beats in: 48 000 samples -> tick 1920 -> pulse 48 -> SPP 8.
        assert_eq!(
            step(&mut e, 0, true, 48_000, &m),
            vec![ClockMsg::SongPosition(8), ClockMsg::Continue]
        );
    }

    #[test]
    fn stop_emits_stop_exactly_once() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        assert_eq!(step(&mut e, 1_000, false, 0, &m), vec![ClockMsg::Stop]);
        assert_eq!(step(&mut e, 2_000, false, 0, &m), vec![], "idle emits nothing");
        assert!(!e.running());
    }

    #[test]
    fn one_beat_of_wall_time_emits_exactly_24_clocks_at_120bpm() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        let mut clocks = 0;
        for ms in 1..=500u64 {
            let pos = ms * 48; // 48 samples per ms
            clocks += step(&mut e, ms * 1_000, true, pos, &m).iter()
                .filter(|m| **m == ClockMsg::Clock).count();
        }
        assert_eq!(clocks, 24, "24 PPQN over one beat");
        assert_eq!(e.resyncs(), 0);
    }

    #[test]
    fn block_quantized_position_does_not_trigger_a_resync() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        let mut clocks = 0;
        for ms in 1..=100u64 {
            let pos = (ms / 10) * 480; // one 10 ms block = 480 samples
            clocks += step(&mut e, ms * 1_000, true, pos, &m).iter()
                .filter(|m| **m == ClockMsg::Clock).count();
        }
        assert_eq!(e.resyncs(), 0, "block quantization is not a jump");
        assert!(clocks >= 4, "clocks kept flowing: {clocks}");
    }

    #[test]
    fn a_backward_jump_resyncs_with_stop_spp_continue() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 48_000, &m);
        let out = step(&mut e, 10_000, true, 0, &m);
        assert_eq!(out, vec![ClockMsg::Stop, ClockMsg::SongPosition(0), ClockMsg::Continue]);
        assert_eq!(e.resyncs(), 1);
    }

    #[test]
    fn a_large_forward_seek_resyncs_instead_of_flooding_clocks() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        let out = step(&mut e, 10_000, true, 48_000 * 20, &m);
        assert_eq!(out[0], ClockMsg::Stop);
        assert!(matches!(out[1], ClockMsg::SongPosition(_)));
        assert_eq!(out[2], ClockMsg::Continue);
        assert!(!out.contains(&ClockMsg::Clock), "no burst of catch-up clocks");
    }

    #[test]
    fn a_starved_tick_clamps_the_burst() {
        let (m, mut e) = (map120(), ClockEngine::new());
        step(&mut e, 0, true, 0, &m);
        let out = step(&mut e, 2_000_000, true, 96_000, &m);
        let clocks = out.iter().filter(|m| **m == ClockMsg::Clock).count();
        assert!(clocks <= MAX_PULSE_BURST as usize, "burst clamped, got {clocks}");
    }

    // -----------------------------------------------------------------
    // Sink/port enumeration, `MidiOut`, and `out_tick`.
    // -----------------------------------------------------------------

    #[test]
    fn list_output_ports_never_panics() {
        match list_output_ports() {
            Ok(ports) => for p in &ports { assert!(!p.id.is_empty()); },
            Err(e) => assert!(!e.is_empty()),
        }
    }

    #[test]
    fn opening_a_nonexistent_output_port_is_a_graceful_error() {
        let out = MidiOut::default();
        let err = out.open_port("definitely-not-a-port#99".into()).unwrap_err();
        assert!(err.contains("not found"), "got {err}");
        assert!(out.status().outputs.is_empty(), "a failed open leaves nothing half-open");
    }

    fn no_routes() -> RouteNoteStates {
        HashMap::new()
    }

    fn one_route(scope: RouteScope, snapshot: NoteOutSnapshot) -> RouteNoteStates {
        let mut m = HashMap::new();
        m.insert(scope, RouteNoteState { engine: NoteOutEngine::new(), snapshot });
        m
    }

    #[test]
    fn out_tick_writes_start_then_clocks_to_the_sink() {
        let map = map120();
        let mut engine = ClockEngine::new();
        let mut routes = no_routes();
        let mut sink = RecordingSink::default();
        out_tick(&mut engine, &mut routes, &mut sink, ClockInput { now_micros: 0, playing: true, position: 0, rate: 48_000, map: &map }, true).unwrap();
        assert_eq!(sink.0, vec![vec![0xFA]]);
        for ms in 1..=25u64 {
            out_tick(&mut engine, &mut routes, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, true).unwrap();
        }
        assert!(sink.0.iter().any(|b| b == &vec![0xF8]), "clock bytes reached the sink");
    }

    #[test]
    fn out_tick_writes_nothing_while_the_clock_is_disabled() {
        let map = map120();
        let mut engine = ClockEngine::new();
        let mut routes = no_routes();
        let mut sink = RecordingSink::default();
        for ms in 0..=25u64 {
            out_tick(&mut engine, &mut routes, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, false).unwrap();
        }
        assert!(sink.0.is_empty(), "clock disabled means no bytes");
    }

    /// `clock_enabled == false` must suppress bytes only, NOT freeze the
    /// engine — every route's note scheduler advances over the SAME
    /// anchor/`estimated_sample()` window `step()` maintains, so the anchor
    /// has to keep living while clock output happens to be off.
    #[test]
    fn out_tick_still_steps_the_engine_while_the_clock_is_disabled() {
        let map = map120();
        let mut engine = ClockEngine::new();
        let mut routes = no_routes();
        let mut sink = RecordingSink::default();
        for ms in 0..=25u64 {
            out_tick(&mut engine, &mut routes, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, false).unwrap();
        }
        assert!(sink.0.is_empty(), "clock disabled means no bytes");
        assert!(engine.running(), "the transport still registers as running");
        assert_eq!(engine.estimated_sample(), 25 * 48, "the anchor kept advancing with real time");
    }

    // -----------------------------------------------------------------
    // Note-out.
    // -----------------------------------------------------------------

    fn snap(events: Vec<crate::midi::schedule::AbsNoteEvent>) -> NoteOutSnapshot {
        NoteOutSnapshot { events: Arc::new(events), channel: 0 }
    }

    #[test]
    fn advance_emits_on_and_off_inside_the_window_only() {
        use crate::midi::schedule::AbsNoteEvent;
        let s = snap(vec![
            AbsNoteEvent { sample: 100, key: 60, velocity: 100 },
            AbsNoteEvent { sample: 500, key: 60, velocity: 0 },
        ]);
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&s, 0, 200, &mut out);
        assert_eq!(out.iter().map(|m| m.as_slice().to_vec()).collect::<Vec<_>>(), vec![vec![0x90, 60, 100]]);
        out.clear();
        e.advance(&s, 200, 400, &mut out);
        assert!(out.is_empty(), "nothing due in this window");
        e.advance(&s, 400, 600, &mut out);
        assert_eq!(out[0].as_slice(), &[0x80, 60, 0]);
        assert_eq!(e.notes_sent(), 2);
    }

    #[test]
    fn advance_never_replays_an_event_across_windows() {
        use crate::midi::schedule::AbsNoteEvent;
        let s = snap(vec![AbsNoteEvent { sample: 10, key: 64, velocity: 90 }]);
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&s, 0, 100, &mut out);
        let first = out.len();
        e.advance(&s, 100, 200, &mut out);
        assert_eq!(out.len(), first, "the cursor never rewinds on its own");
    }

    #[test]
    fn all_off_releases_exactly_the_sounding_keys() {
        use crate::midi::schedule::AbsNoteEvent;
        let s = snap(vec![
            AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
            AbsNoteEvent { sample: 0, key: 67, velocity: 100 },
            AbsNoteEvent { sample: 10, key: 60, velocity: 0 },
        ]);
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&s, 0, 20, &mut out);
        out.clear();
        e.all_off(0, &mut out);
        assert_eq!(out.len(), 1, "only key 67 is still sounding: {:?}", out);
        assert_eq!(out[0].as_slice(), &[0x80, 67, 0]);
        out.clear();
        e.all_off(0, &mut out);
        assert!(out.is_empty(), "all_off is idempotent");
    }

    #[test]
    fn reseek_releases_and_repositions_the_cursor() {
        use crate::midi::schedule::AbsNoteEvent;
        let s = snap(vec![
            AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
            AbsNoteEvent { sample: 1_000, key: 62, velocity: 100 },
        ]);
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&s, 0, 10, &mut out);
        out.clear();
        e.reseek(&s, 900, &mut out);
        assert_eq!(out[0].as_slice(), &[0x80, 60, 0], "the sounding note is released");
        out.clear();
        e.advance(&s, 900, 1_100, &mut out);
        assert_eq!(out[0].as_slice(), &[0x90, 62, 100], "cursor landed before the next event");
    }

    /// `reseek` is the ONLY repositioning entry point (`out_tick`'s own
    /// fresh-start/resync path AND `run_thread`'s content-changed path both
    /// funnel through it) so it must be able to move the cursor BACKWARD —
    /// a loop wrap or an ordinary rewind-then-replay is exactly that.
    #[test]
    fn reseek_moves_the_cursor_backward_and_replays_the_intervening_note() {
        use crate::midi::schedule::AbsNoteEvent;
        let s = snap(vec![
            AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
            AbsNoteEvent { sample: 1_000, key: 62, velocity: 100 },
            AbsNoteEvent { sample: 2_000, key: 64, velocity: 100 },
        ]);
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&s, 0, 3_000, &mut out);
        assert_eq!(e.notes_sent(), 3, "cursor is now past every event");

        out.clear();
        e.reseek(&s, 500, &mut out);

        out.clear();
        e.advance(&s, 500, 1_500, &mut out);
        assert_eq!(
            out.get(0).map(|m| m.as_slice()),
            Some(&[0x90, 62, 100][..]),
            "the note at 1000 sounds again after the rewind: {:?}", out
        );
    }

    /// `reseek` recomputes the cursor from scratch against whatever array it
    /// is given — a swap to a SHORTER array (a route's content shrinking
    /// under an edit) must not permanently stall.
    #[test]
    fn a_content_swap_with_a_shorter_array_still_emits() {
        use crate::midi::schedule::AbsNoteEvent;
        let long = snap(vec![
            AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
            AbsNoteEvent { sample: 1_000, key: 60, velocity: 0 },
            AbsNoteEvent { sample: 2_000, key: 62, velocity: 100 },
            AbsNoteEvent { sample: 3_000, key: 62, velocity: 0 },
            AbsNoteEvent { sample: 4_000, key: 64, velocity: 100 },
        ]);
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&long, 0, 4_500, &mut out); // cursor now past every event (index 5)

        let short = snap(vec![AbsNoteEvent { sample: 100, key: 67, velocity: 90 }]);

        out.clear();
        e.reseek(&short, 0, &mut out);
        out.clear();
        e.advance(&short, 0, 200, &mut out);
        assert_eq!(
            out.get(0).map(|m| m.as_slice()),
            Some(&[0x90, 67, 90][..]),
            "the newly swapped-in (shorter) content still emits: {:?}", out
        );
    }

    #[test]
    #[should_panic(expected = "sorted ascending")]
    fn advance_panics_in_debug_on_an_out_of_order_snapshot() {
        use crate::midi::schedule::AbsNoteEvent;
        let s = snap(vec![
            AbsNoteEvent { sample: 500, key: 60, velocity: 100 },
            AbsNoteEvent { sample: 100, key: 62, velocity: 100 }, // out of order
        ]);
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&s, 0, 600, &mut out);
    }

    #[test]
    fn channel_is_encoded_in_the_status_byte() {
        use crate::midi::schedule::AbsNoteEvent;
        let mut s = snap(vec![AbsNoteEvent { sample: 0, key: 60, velocity: 100 }]);
        s.channel = 9; // GM drums
        let mut e = NoteOutEngine::new();
        let mut out = Vec::new();
        e.advance(&s, 0, 10, &mut out);
        assert_eq!(out[0].as_slice(), &[0x99, 60, 100]);
    }

    #[test]
    fn out_tick_sends_clock_and_notes_from_one_anchor() {
        use crate::midi::schedule::AbsNoteEvent;
        let map = map120();
        let s = snap(vec![AbsNoteEvent { sample: 4_800, key: 60, velocity: 100 }]); // 100 ms in
        let mut clock = ClockEngine::new();
        let mut routes = one_route(RouteScope::Track("t-1".into()), s);
        let mut sink = RecordingSink::default();
        for ms in 0..=150u64 {
            out_tick(&mut clock, &mut routes, &mut sink,
                ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map },
                true).unwrap();
        }
        assert!(sink.0.iter().any(|b| b == &vec![0xFA]), "Start went out");
        assert!(sink.0.iter().any(|b| b == &vec![0xF8]), "clock went out");
        assert!(sink.0.iter().any(|b| b == &vec![0x90, 60, 100]), "the note went out: {:?}", sink.0);
    }

    #[test]
    fn stopping_the_transport_releases_outgoing_notes() {
        use crate::midi::schedule::AbsNoteEvent;
        let map = map120();
        let s = snap(vec![AbsNoteEvent { sample: 0, key: 60, velocity: 100 }]);
        let mut clock = ClockEngine::new();
        let mut routes = one_route(RouteScope::Track("t-1".into()), s);
        let mut sink = RecordingSink::default();
        out_tick(&mut clock, &mut routes, &mut sink, ClockInput { now_micros: 0, playing: true, position: 0, rate: 48_000, map: &map }, true).unwrap();
        sink.0.clear();
        out_tick(&mut clock, &mut routes, &mut sink, ClockInput { now_micros: 10_000, playing: false, position: 480, rate: 48_000, map: &map }, true).unwrap();
        assert!(sink.0.iter().any(|b| b == &vec![0x80, 60, 0]), "no hanging note after stop: {:?}", sink.0);
        assert!(sink.0.iter().any(|b| b == &vec![0xFC]), "and the slave was told to stop");
    }

    #[test]
    fn two_routes_on_the_same_tick_stay_independent() {
        use crate::midi::schedule::AbsNoteEvent;
        let map = map120();
        let track_snap = snap(vec![AbsNoteEvent { sample: 0, key: 60, velocity: 100 }]);
        let clip_snap = snap(vec![AbsNoteEvent { sample: 0, key: 72, velocity: 90 }]);
        let mut clock = ClockEngine::new();
        let mut routes: RouteNoteStates = HashMap::new();
        routes.insert(RouteScope::Track("t-1".into()), RouteNoteState { engine: NoteOutEngine::new(), snapshot: track_snap });
        routes.insert(RouteScope::Clip("c-1".into()), RouteNoteState { engine: NoteOutEngine::new(), snapshot: clip_snap });
        let mut sink = RecordingSink::default();
        out_tick(&mut clock, &mut routes, &mut sink, ClockInput { now_micros: 0, playing: true, position: 0, rate: 48_000, map: &map }, true).unwrap();
        assert!(sink.0.iter().any(|b| b == &vec![0x90, 60, 100]), "track route's note went out");
        assert!(sink.0.iter().any(|b| b == &vec![0x90, 72, 90]), "clip route's note went out independently");
    }

    /// Real `midir` loopback: a virtual INPUT port receives what a real
    /// `MidiOutSink` sends. Skips cleanly where ALSA seq is unavailable.
    #[test]
    fn a_real_output_connection_delivers_bytes_to_a_virtual_port() {
        use midir::os::unix::VirtualInput;
        let Ok(midi_in) = midir::MidiInput::new("aura-midi-out-test-in") else {
            eprintln!("skipping: ALSA seq unavailable"); return;
        };
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<Vec<u8>>::new()));
        let sink_seen = seen.clone();
        let Ok(_conn) = midi_in.create_virtual("aura-out-loopback", move |_, msg, _: &mut ()| {
            sink_seen.lock().push(msg.to_vec());
        }, ()) else { eprintln!("skipping: virtual port unavailable"); return; };

        let Ok(ports) = list_output_ports() else { eprintln!("skipping: no output enumeration"); return };
        let Some(target) = ports.into_iter().find(|p| p.name.contains("aura-out-loopback")) else {
            eprintln!("skipping: loopback port not visible"); return;
        };
        let out = MidiOut::default();
        out.open_port(target.id.clone()).expect("open the loopback port");
        // Give the ALSA delivery a moment, then assert SOMETHING arrived once a
        // Stop is sent on close.
        out.close_port(&target.id).expect("close sends Stop");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(seen.lock().iter().any(|m| m.as_slice() == [0xFC]), "Stop reached the port: {:?}", seen.lock());
    }

    /// Editing the ROUTED track while a note is sounding must not hang that
    /// note on the hardware. The thread rebuilds events every 250 ms from
    /// the live session; comparing snapshot CONTENT (not just scope
    /// presence) is what catches a same-scope, shrunk-array edit.
    ///
    /// Real ALSA loopback, asserted STRICTLY BETWEEN a pre-edit marker and
    /// the port close — a note-off emitted by the close would prove
    /// nothing. `resyncs` is asserted to stay 0 throughout: a drift resync
    /// releases and reseeks on its own, which would make this test pass
    /// for the wrong reason.
    #[test]
    fn editing_the_routed_track_releases_a_sounding_note_instead_of_hanging_it() {
        use midir::os::unix::VirtualInput;
        use crate::audio::types::{Store, TrackState};
        use crate::ids::NoteId;
        use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
        use crate::midi::MidiStore;

        let Ok(midi_in) = midir::MidiInput::new("aura-midi-out-test-in-edit") else {
            eprintln!("skipping: ALSA seq unavailable"); return;
        };
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<Vec<u8>>::new()));
        let sink_seen = seen.clone();
        let Ok(_conn) = midi_in.create_virtual("aura-out-edit-loopback", move |_, msg, _: &mut ()| {
            sink_seen.lock().push(msg.to_vec());
        }, ()) else { eprintln!("skipping: virtual port unavailable"); return; };

        let Ok(ports) = list_output_ports() else { eprintln!("skipping: no output enumeration"); return };
        let Some(target) = ports.into_iter().find(|p| p.name.contains("aura-out-edit-loopback")) else {
            eprintln!("skipping: loopback port not visible"); return;
        };

        let mut store = Store::default();
        store.tracks.push(TrackState {
            id: "t-1".into(),
            name: "t-1".into(),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        });
        let long_ticks = DEFAULT_PPQ as u64 * 8; // 4 s at 120 bpm: still sounding
        let clip_id: crate::ids::ClipId = uuid::Uuid::new_v4().to_string().into();
        let midi = MidiStore {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![MidiClip {
                id: clip_id.clone(),
                track_id: "t-1".into(),
                name: "c".into(),
                timeline_start_ticks: 0,
                length_ticks: long_ticks,
                // The short note supplies the two edges (on+off) that push
                // the cursor past where it lands once they are deleted.
                notes: vec![
                    MidiNote { tick: 0, length_ticks: 240, key: 62, velocity: 100, channel: 0, note_id: NoteId(0) },
                    MidiNote { tick: 0, length_ticks: long_ticks as u32, key: 60, velocity: 100, channel: 0, note_id: NoteId(1) },
                ],
                next_note_id: 2,
                content_id: crate::ids::ContentId::mint(),
                lane_id: crate::ids::LaneId::default_for_track("t-1"),
                content_length_ticks: None,
            }],
            loaded_dir: None,
            dirty: false,
        };
        let session = Arc::new(PlMutex::new(crate::control::Session::new(store, midi)));
        let shared = Arc::new(SharedRt::default());
        shared.playing.store(true, Relaxed);
        shared.position.store(0, Relaxed);

        let updater_running = Arc::new(AtomicBool::new(true));
        let updater = {
            let shared2 = shared.clone();
            let running2 = updater_running.clone();
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                while running2.load(Relaxed) {
                    shared2.position.store((t0.elapsed().as_secs_f64() * 48_000.0) as u64, Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            })
        };

        let out = MidiOut::default();
        out.attach(session.clone(), shared.clone());
        out.set_route(RouteScope::Track("t-1".into()), Some(RouteTarget { port_id: target.id.clone(), channel: 0 }));
        out.open_port(target.id.clone()).expect("open the loopback port");

        // Past the short note's own note-off (125 ms) and at least one
        // snapshot window, so the cursor is genuinely past both its edges.
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(
            seen.lock().iter().any(|m| m.as_slice() == [0x90, 60, 100]),
            "the long note is actually sounding before the edit: {:?}", seen.lock()
        );
        let before_edit = seen.lock().len();
        assert_eq!(out.status().outputs[0].resyncs, 0, "no resync before the edit — it would release notes on its own");

        // The edit: delete the short note. Same track id, different array.
        session.lock().midi.clips[0].notes.retain(|n| n.key != 62);

        std::thread::sleep(std::time::Duration::from_millis(400));
        let after: Vec<Vec<u8>> = seen.lock()[before_edit..].to_vec();
        let resyncs = out.status().outputs[0].resyncs;

        updater_running.store(false, Relaxed);
        let _ = updater.join();
        out.close_port(&target.id).ok();

        assert_eq!(resyncs, 0, "the release came from the content change, not a drift resync");
        assert!(
            after.iter().any(|m| m.as_slice() == [0x80, 60, 0]),
            "the sounding note was released after the edit instead of hanging: {:?}", after
        );
    }

    /// A clip with its own route always wins over its track's route: two
    /// clips on the SAME routed track, one with a clip-level override to a
    /// different channel on the SAME port, produce exactly one note-on per
    /// clip's key, each on its OWN channel — never the overridden clip's
    /// key on the track's channel too.
    #[test]
    fn a_clip_override_wins_over_its_tracks_route() {
        use midir::os::unix::VirtualInput;
        use crate::audio::types::{Store, TrackState};
        use crate::ids::NoteId;
        use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
        use crate::midi::MidiStore;

        let Ok(midi_in) = midir::MidiInput::new("aura-midi-out-test-in-override") else {
            eprintln!("skipping: ALSA seq unavailable"); return;
        };
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<Vec<u8>>::new()));
        let sink_seen = seen.clone();
        let Ok(_conn) = midi_in.create_virtual("aura-out-override-loopback", move |_, msg, _: &mut ()| {
            sink_seen.lock().push(msg.to_vec());
        }, ()) else { eprintln!("skipping: virtual port unavailable"); return; };

        let Ok(ports) = list_output_ports() else { eprintln!("skipping: no output enumeration"); return };
        let Some(target) = ports.into_iter().find(|p| p.name.contains("aura-out-override-loopback")) else {
            eprintln!("skipping: loopback port not visible"); return;
        };

        let mut store = Store::default();
        store.tracks.push(TrackState {
            id: "t-1".into(),
            name: "t-1".into(),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        });
        let long_ticks = DEFAULT_PPQ as u64 * 8;
        let track_clip_id: crate::ids::ClipId = uuid::Uuid::new_v4().to_string().into();
        let overridden_clip_id: crate::ids::ClipId = uuid::Uuid::new_v4().to_string().into();
        let midi = MidiStore {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![
                MidiClip {
                    id: track_clip_id,
                    track_id: "t-1".into(),
                    name: "track-routed".into(),
                    timeline_start_ticks: 0,
                    length_ticks: long_ticks,
                    notes: vec![MidiNote { tick: 0, length_ticks: long_ticks as u32, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) }],
                    next_note_id: 1,
                    content_id: crate::ids::ContentId::mint(),
                    lane_id: crate::ids::LaneId::default_for_track("t-1"),
                    content_length_ticks: None,
                },
                MidiClip {
                    id: overridden_clip_id.clone(),
                    track_id: "t-1".into(),
                    name: "clip-routed".into(),
                    timeline_start_ticks: 0,
                    length_ticks: long_ticks,
                    notes: vec![MidiNote { tick: 0, length_ticks: long_ticks as u32, key: 72, velocity: 100, channel: 0, note_id: NoteId(0) }],
                    next_note_id: 1,
                    content_id: crate::ids::ContentId::mint(),
                    lane_id: crate::ids::LaneId::default_for_track("t-1"),
                    content_length_ticks: None,
                },
            ],
            loaded_dir: None,
            dirty: false,
        };
        let session = Arc::new(PlMutex::new(crate::control::Session::new(store, midi)));
        let shared = Arc::new(SharedRt::default());
        shared.playing.store(true, Relaxed);
        shared.position.store(0, Relaxed);

        let updater_running = Arc::new(AtomicBool::new(true));
        let updater = {
            let shared2 = shared.clone();
            let running2 = updater_running.clone();
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                while running2.load(Relaxed) {
                    shared2.position.store((t0.elapsed().as_secs_f64() * 48_000.0) as u64, Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            })
        };

        let out = MidiOut::default();
        out.attach(session, shared.clone());
        // Track routed on channel 0; the second clip overrides to channel 5
        // on the SAME port.
        out.set_route(RouteScope::Track("t-1".into()), Some(RouteTarget { port_id: target.id.clone(), channel: 0 }));
        out.set_route(RouteScope::Clip(overridden_clip_id.to_string()), Some(RouteTarget { port_id: target.id.clone(), channel: 5 }));
        out.open_port(target.id.clone()).expect("open the loopback port");

        std::thread::sleep(std::time::Duration::from_millis(400));
        updater_running.store(false, Relaxed);
        let _ = updater.join();
        out.close_port(&target.id).ok();

        let seen = seen.lock();
        assert!(seen.iter().any(|m| m.as_slice() == [0x90, 60, 100]), "the track-routed clip's note went out on channel 0: {:?}", *seen);
        assert!(seen.iter().any(|m| m.as_slice() == [0x95, 72, 100]), "the overridden clip's note went out on channel 5: {:?}", *seen);
        assert!(!seen.iter().any(|m| m.as_slice() == [0x90, 72, 100]), "the overridden clip's note must NOT also go out on the track's channel: {:?}", *seen);
    }

    /// A route whose track/clip no longer exists in the document self-heals
    /// away — there is no clip-delete hook to retrofit (see the module
    /// doc), so `run_thread`'s periodic refresh is the only thing that
    /// cleans this up.
    #[test]
    fn a_route_to_a_deleted_clip_self_heals_away() {
        use midir::os::unix::VirtualInput;
        use crate::audio::types::{Store, TrackState};
        use crate::ids::NoteId;
        use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
        use crate::midi::MidiStore;

        let mut store = Store::default();
        store.tracks.push(TrackState {
            id: "t-1".into(),
            name: "t-1".into(),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        });
        let clip_id: crate::ids::ClipId = uuid::Uuid::new_v4().to_string().into();
        let midi = MidiStore {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![MidiClip {
                id: clip_id.clone(),
                track_id: "t-1".into(),
                name: "c".into(),
                timeline_start_ticks: 0,
                length_ticks: 960,
                notes: vec![MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) }],
                next_note_id: 1,
                content_id: crate::ids::ContentId::mint(),
                lane_id: crate::ids::LaneId::default_for_track("t-1"),
                content_length_ticks: None,
            }],
            loaded_dir: None,
            dirty: false,
        };
        let session = Arc::new(PlMutex::new(crate::control::Session::new(store, midi)));
        let shared = Arc::new(SharedRt::default());

        let out = MidiOut::default();
        out.attach(session.clone(), shared);
        // Self-heal only runs inside an open port's thread (it is the
        // 250 ms refresh window that re-validates routes against the live
        // document), so a real loopback port is needed even though no
        // bytes from it are asserted on here.
        let Ok(midi_in) = midir::MidiInput::new("aura-midi-out-test-in-selfheal") else {
            eprintln!("skipping: ALSA seq unavailable"); return;
        };
        let Ok(_conn) = midi_in.create_virtual("aura-out-selfheal-loopback", |_, _msg, _: &mut ()| {}, ()) else {
            eprintln!("skipping: virtual port unavailable"); return;
        };
        let Ok(ports) = list_output_ports() else { eprintln!("skipping: no output enumeration"); return };
        let Some(target) = ports.into_iter().find(|p| p.name.contains("aura-out-selfheal-loopback")) else {
            eprintln!("skipping: loopback port not visible"); return;
        };

        out.set_route(RouteScope::Clip(clip_id.to_string()), Some(RouteTarget { port_id: target.id.clone(), channel: 0 }));
        out.open_port(target.id.clone()).expect("open the loopback port");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            out.routes().contains_key(&RouteScope::Clip(clip_id.to_string())),
            "the route is live before the delete"
        );

        session.lock().midi.clips.clear();
        std::thread::sleep(std::time::Duration::from_millis(400));
        out.close_port(&target.id).ok();

        assert!(
            !out.routes().contains_key(&RouteScope::Clip(clip_id.to_string())),
            "a route to a deleted clip self-heals away instead of lingering forever"
        );
    }

    /// End-to-end persistence: routing set on one `MidiOut` instance
    /// survives a simulated app restart (a FRESH `MidiOut` sharing the same
    /// per-machine file) — ports are re-resolved by NAME and reopened, and
    /// the project's track route is reapplied.
    #[test]
    fn adopt_project_restores_routing_after_a_simulated_restart() {
        use midir::os::unix::VirtualInput;
        let Ok(midi_in) = midir::MidiInput::new("aura-midi-out-test-in-persist") else {
            eprintln!("skipping: ALSA seq unavailable"); return;
        };
        let Ok(_conn) = midi_in.create_virtual("aura-out-persist-loopback", |_, _msg, _: &mut ()| {}, ()) else {
            eprintln!("skipping: virtual port unavailable"); return;
        };
        let Ok(ports) = list_output_ports() else { eprintln!("skipping: no output enumeration"); return };
        let Some(target) = ports.into_iter().find(|p| p.name.contains("aura-out-persist-loopback")) else {
            eprintln!("skipping: loopback port not visible"); return;
        };

        let routing_path = std::env::temp_dir().join(format!(
            "aura-midi-routing-restart-test-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let project_dir = std::env::temp_dir().join(format!(
            "aura-midi-routing-restart-project-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));

        let before = MidiOut::default();
        before.set_routing_path_for_test(routing_path.clone());
        before.open_port(target.id.clone()).expect("open the loopback port");
        before.set_clock_enabled(&target.id, false).unwrap();
        before.set_route(RouteScope::Track("t-1".into()), Some(RouteTarget { port_id: target.id.clone(), channel: 3 }));
        before.persist(Some(&project_dir));
        before.close_port(&target.id).ok();
        assert!(before.status().outputs.is_empty(), "the OLD instance's port is closed, simulating app exit");

        // A brand new `MidiOut` — nothing open, no routes — standing in for
        // the next app launch.
        let after = MidiOut::default();
        after.set_routing_path_for_test(routing_path);
        assert!(after.status().outputs.is_empty(), "nothing open yet");
        after.adopt_project(&project_dir);

        let status = after.status();
        assert_eq!(status.outputs.len(), 1, "the persisted port was reopened by name");
        assert_eq!(status.outputs[0].port.name, target.name);
        assert!(!status.outputs[0].clock_enabled, "the persisted clock-enabled preference was restored");
        assert!(
            after.routes().get(&RouteScope::Track("t-1".into()))
                == Some(&RouteTarget { port_id: status.outputs[0].port.id.clone(), channel: 3 }),
            "the persisted track route was reapplied, re-pointed at the freshly resolved port id: {:?}",
            after.routes()
        );

        after.close_port(&status.outputs[0].port.id).ok();
        let _ = std::fs::remove_dir_all(&project_dir);
    }

    /// On shutdown the thread must release whatever is sounding BEFORE the
    /// connection is dropped — a hanging note on real gear is silent and
    /// permanent otherwise. Proven end-to-end through the REAL thread and a
    /// REAL ALSA virtual loopback port: `close_port` joins the thread
    /// before returning, so if `seen` has the note-off ahead of the final
    /// Stop, the bytes provably left the sink before its `midir` connection
    /// was dropped at the end of `run_thread`.
    #[test]
    fn shutdown_flushes_note_off_before_stop_and_before_the_connection_drops() {
        use midir::os::unix::VirtualInput;
        use crate::audio::types::{Store, TrackState};
        use crate::ids::NoteId;
        use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
        use crate::midi::MidiStore;

        let Ok(midi_in) = midir::MidiInput::new("aura-midi-out-test-in-shutdown") else {
            eprintln!("skipping: ALSA seq unavailable"); return;
        };
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<Vec<u8>>::new()));
        let sink_seen = seen.clone();
        let Ok(_conn) = midi_in.create_virtual("aura-out-shutdown-loopback", move |_, msg, _: &mut ()| {
            sink_seen.lock().push(msg.to_vec());
        }, ()) else { eprintln!("skipping: virtual port unavailable"); return; };

        let Ok(ports) = list_output_ports() else { eprintln!("skipping: no output enumeration"); return };
        let Some(target) = ports.into_iter().find(|p| p.name.contains("aura-out-shutdown-loopback")) else {
            eprintln!("skipping: loopback port not visible"); return;
        };

        let mut store = Store::default();
        store.tracks.push(TrackState {
            id: "t-1".into(),
            name: "t-1".into(),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        });
        // 8 beats at 120 bpm = 4 s — well past the ~400 ms this test runs
        // before forcing shutdown, so the note is still genuinely sounding.
        let long_ticks = DEFAULT_PPQ as u64 * 8;
        let midi = MidiStore {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![MidiClip {
                id: uuid::Uuid::new_v4().to_string().into(),
                track_id: "t-1".into(),
                name: "c".into(),
                timeline_start_ticks: 0,
                length_ticks: long_ticks,
                notes: vec![MidiNote { tick: 0, length_ticks: long_ticks as u32, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) }],
                next_note_id: 1,
                content_id: crate::ids::ContentId::mint(),
                lane_id: crate::ids::LaneId::default_for_track("t-1"),
                content_length_ticks: None,
            }],
            loaded_dir: None,
            dirty: false,
        };
        let session = Arc::new(PlMutex::new(crate::control::Session::new(store, midi)));
        let shared = Arc::new(SharedRt::default());
        shared.playing.store(true, Relaxed);
        shared.position.store(0, Relaxed);

        // Keep `position` tracking real wall-clock time (nothing else here
        // runs an audio engine to advance it), so the clock never sees a
        // drift-sized gap and resyncs on its own.
        let updater_running = Arc::new(AtomicBool::new(true));
        let updater = {
            let shared2 = shared.clone();
            let running2 = updater_running.clone();
            std::thread::spawn(move || {
                let t0 = std::time::Instant::now();
                while running2.load(Relaxed) {
                    let samples = (t0.elapsed().as_secs_f64() * 48_000.0) as u64;
                    shared2.position.store(samples, Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            })
        };

        let out = MidiOut::default();
        out.attach(session, shared.clone());
        out.set_route(RouteScope::Track("t-1".into()), Some(RouteTarget { port_id: target.id.clone(), channel: 0 }));
        out.open_port(target.id.clone()).expect("open the loopback port");

        // The thread's 250 ms try_lock snapshot window has to pick up the
        // routed track before we close — give it headroom for more than one
        // window plus the fresh-start note-on.
        std::thread::sleep(std::time::Duration::from_millis(400));
        let before_shutdown = seen.lock().len();

        // Driven through `release_on_exit` — the EXACT function `lib.rs`'s
        // `RunEvent::Exit` handler calls — rather than `close_port`
        // directly, so the app-exit entry point is what these assertions
        // pin. What this test cannot cover is whether Tauri actually fires
        // `RunEvent::Exit`; that needs a windowed run.
        release_on_exit(&out);
        std::thread::sleep(std::time::Duration::from_millis(50));

        updater_running.store(false, Relaxed);
        let _ = updater.join();

        let seen = seen.lock();
        let after = &seen[before_shutdown..];
        let off_idx = after.iter().position(|m| m.as_slice() == [0x80, 60, 0]);
        let stop_idx = after.iter().position(|m| m.as_slice() == [0xFC]);
        assert!(off_idx.is_some(), "note-off reached the real port DURING shutdown (not before): {:?}", after);
        assert!(stop_idx.is_some(), "Stop reached the real port: {:?}", after);
        assert!(
            off_idx.unwrap() < stop_idx.unwrap(),
            "note-off precedes Stop, both delivered before the connection dropped: {:?}", after
        );
    }
}
