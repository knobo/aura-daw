//! MIDI output: transport sync (clock/Start/Stop/Continue/SPP) and note-out
//! to external gear. Runs entirely off `SharedRt` atomics + a document
//! snapshot on its own non-RT thread — see ruling 3: NOTHING here touches
//! `audio/engine.rs`.
//!
//! ## The `aura-midi-out` thread (Task 7)
//!
//! Named `aura-midi-out`, spawned on the first `MidiOut::select_port(Some
//! (..))` and stopped by an `Arc<AtomicBool>` flag on close/re-select/
//! `Drop`. **Not an RT thread**: unlike the audio callback it is free to
//! allocate and take locks — it is driven by `std::thread::sleep`, not the
//! audio device.
//!
//! Loop body: `std::thread::sleep(Duration::from_micros(500))`, then one
//! `out_tick(now_micros, shared.playing, shared.position, shared.
//! sample_rate)`. 500 µs gives sub-millisecond clock jitter at any musical
//! tempo — at 120 bpm a single 24-PPQN pulse is 20.8 ms apart, so the loop
//! is roughly 40× oversampled relative to the fastest thing it needs to
//! emit on time.
//!
//! Tempo snapshot: at most every **250 ms** the thread attempts
//! `session.try_lock()` — **never** a blocking lock, because
//! `engine::rebuild` holds the session lock across a full graph build and a
//! blocking lock here would stall clock output for that whole window. On a
//! successful try_lock the thread rebuilds its `TempoMap` from
//! `session.midi.ppq` + `session.midi.tempo_events` + the engine's current
//! sample rate, but ONLY if that triple changed since the last snapshot —
//! otherwise the existing `TempoMap` (and its internal caches) is reused
//! as-is. A contended try_lock is not an error: the thread simply keeps
//! whatever `TempoMap` it already has and tries again on the next 250 ms
//! boundary. Documented consequence: a tempo edit made in the UI can take
//! up to ~250 ms to reach external gear.
//!
//! Lock order: this thread takes the session lock and NOTHING else — it
//! never calls `EngineHandle::request`. So the Committer deadlock invariant
//! ("no thread may hold the session lock across a `request`") holds here
//! trivially, by construction, with no cross-thread ordering argument
//! needed.
//!
//! On stop (close, re-select to a different port, or `Drop`): the thread
//! sends an explicit `ClockMsg::Stop` to its sink before the sink (and the
//! underlying `midir` connection) is dropped, so a hardware slave synced to
//! MIDI clock does not keep running forever after AURA lets go of the port.
//!
//! `MidiOut::set_clock_enabled(false)` suppresses clock/transport bytes
//! (the thread stops emitting `ClockMsg`s to the sink) but leaves the
//! thread — and the open port — alive; Task 8's note-out keeps working
//! through the very same connection.
//!
//! The per-tick body is pulled out as the free function [`out_tick`] so it
//! is testable with an injected [`ClockSink`] and injected time, with no
//! thread, no `SharedRt`, and no `Session` involved.

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

    /// The interpolated sample position this tick resolved to (Task 8's note
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
// Task 7: the sink abstraction, port enumeration, the `aura-midi-out`
// thread, and the four Tauri commands.
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

/// Recording sink for tests (and, in spirit, for the status readout's byte
/// counter — Task 8 wires notes through the same trait).
#[derive(Default)]
pub struct RecordingSink(pub Vec<Vec<u8>>);

impl ClockSink for RecordingSink {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Task 8: note-out. The routed track's notes are scheduled off the SAME
// anchor the clock uses (`ClockEngine::estimated_sample`) — one conversion
// (`midi::playback::track_events`), two consumers (the internal live node
// and this scheduler). Ruling 10: a routing carve-out — app config, no
// document field, no `Op`.
// ---------------------------------------------------------------------------

/// The routed track's scheduled events, absolute engine samples, sorted —
/// exactly what `midi::playback` hands the internal live node, so external
/// and internal timing come from ONE conversion.
#[derive(Clone, Default)]
pub struct NoteOutSnapshot {
    pub track_id: String,
    pub events: Arc<Vec<crate::midi::schedule::AbsNoteEvent>>,
    /// MIDI channel for outgoing notes (0-based on the wire). Fixed at 0 in
    /// this slice.
    pub channel: u8,
}

/// Scheduler state: a monotonic cursor into `NoteOutSnapshot::events` plus a
/// per-key sounding flag so `all_off`/`reseek` release exactly what is
/// actually sounding — never more, never less.
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

    /// Release everything currently sounding (transport stop, resync, track
    /// change, port close). Emits one Note Off per sounding key — explicit
    /// offs, not just CC 123, because not every device honors All Notes Off.
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
    /// `to` (a seek / loop wrap / fresh start).
    pub fn reseek(&mut self, snap: &NoteOutSnapshot, to: u64, out: &mut Vec<OutMsg>) {
        self.all_off(snap.channel, out);
        while self.cursor < snap.events.len() && snap.events[self.cursor].sample < to {
            self.cursor += 1;
        }
    }

    pub fn notes_sent(&self) -> u64 {
        self.notes_sent
    }
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

/// Live status snapshot returned by [`MidiOut::status`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MidiOutputStatus {
    pub selected: Option<MidiPortInfo>,
    pub clock_enabled: bool,
    /// The clock engine currently considers the transport running.
    pub running: bool,
    pub pulses_sent: u64,
    pub resyncs: u64,
    /// The track routed to external gear (ruling 10: app config, not
    /// document state) — read from `MidiOut::note_track`, so this is correct
    /// even with no output port selected (same "hub-sourced fields are
    /// correct even idle" precedent as `midi_input::MidiInputStatus`).
    pub note_track_id: Option<String>,
    /// Zero whenever no `aura-midi-out` thread is running (no port
    /// selected) — mirrors `pulses_sent`'s "idle when nothing selected".
    pub notes_sent: u64,
}

/// State the `aura-midi-out` thread updates every tick and `MidiOut::status`
/// reads back — no lock needed since it's all atomics.
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
    handle: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct Inner {
    active: Option<ActiveOutput>,
}

/// Owns the (at most one) open MIDI output connection and the `aura-midi-out`
/// thread driving it. See the module doc for the full thread contract.
pub struct MidiOut {
    /// Set once by lib.rs setup via [`MidiOut::attach`]. `None` in every
    /// unit test that builds a bare `MidiOut::default()` — the thread then
    /// falls back to "transport never plays" (see `run_thread`), which is
    /// exactly what the port-enumeration/error-path tests below need and
    /// nothing more.
    session: OnceLock<Arc<PlMutex<Session>>>,
    shared: OnceLock<Arc<SharedRt>>,
    /// Live flag the thread reads every tick; flipping it never tears down
    /// or restarts the thread/connection (Task 7 contract: "leaves the
    /// thread and port alive"). Defaults to enabled so plugging a device in
    /// and pressing play "just works" without an extra step, mirroring
    /// `midi_input`'s "default ON when a port is selected" precedent.
    clock_enabled: Arc<AtomicBool>,
    /// The track routed to external gear (Task 8, ruling 10: app config —
    /// no `Op`, no document field). Top-level like `clock_enabled` so a
    /// routing choice survives a port re-select and is readable even with
    /// no port open. `None` = no track routed, notes stay internal-only.
    note_track: Arc<PlMutex<Option<String>>>,
    /// The routed track's converted events, refreshed by the thread in the
    /// same 250 ms window as the `TempoMap` — set here (rather than kept
    /// purely thread-local) so a future status/debug readout can see what
    /// the thread is actually scheduling from.
    snapshot: Arc<PlMutex<NoteOutSnapshot>>,
    inner: PlMutex<Inner>,
}

impl Default for MidiOut {
    fn default() -> Self {
        Self {
            session: OnceLock::new(),
            shared: OnceLock::new(),
            clock_enabled: Arc::new(AtomicBool::new(true)),
            note_track: Arc::new(PlMutex::new(None)),
            snapshot: Arc::new(PlMutex::new(NoteOutSnapshot::default())),
            inner: PlMutex::new(Inner::default()),
        }
    }
}

impl MidiOut {
    /// lib.rs setup, once: the thread needs the transport atomics and a way
    /// to snapshot tempo. A second call is silently ignored (first attach
    /// wins) — lib.rs calls this exactly once, same shape as `midi_input`'s
    /// `attach_midi_input`.
    pub fn attach(&self, session: Arc<PlMutex<Session>>, shared: Arc<SharedRt>) {
        let _ = self.session.set(session);
        let _ = self.shared.set(shared);
    }

    /// Open (or close, on `None`) the output port. Starts the
    /// `aura-midi-out` thread on first open; stops it (Stop-then-drop, see
    /// module doc) on close. Re-selecting the SAME port is a no-op —
    /// mirrors `MidiInputManager::select_port`'s fast path: no teardown, no
    /// counter reset.
    pub fn select_port(&self, port_id: Option<String>) -> Result<(), String> {
        let mut inner = self.inner.lock();

        if let Some(active) = inner.active.as_ref() {
            if port_id.as_deref() == Some(active.port.id.as_str()) {
                return Ok(()); // fast path: same port, nothing to do.
            }
        }

        // Slow path: an actual port change (including closing to `None`).
        // Tear down whatever is currently open first.
        if let Some(mut active) = inner.active.take() {
            active.thread_shared.stop.store(true, Relaxed);
            if let Some(handle) = active.handle.take() {
                let _ = handle.join();
            }
        }

        let Some(id) = port_id else {
            return Ok(());
        };

        let midi_out = midir::MidiOutput::new("aura-midi-output").map_err(|e| e.to_string())?;
        let ports = midi_out.ports();
        let mut found = None;
        for (i, p) in ports.iter().enumerate() {
            let name = match midi_out.port_name(p) {
                Ok(n) => n,
                Err(_) => continue, // vanished mid-enumeration; skip
            };
            if format!("{name}#{i}") == id {
                found = Some((p.clone(), MidiPortInfo { id: id.clone(), name }));
                break;
            }
        }
        let (port, info) = found.ok_or_else(|| format!("MIDI output port not found: {id}"))?;

        let conn = midi_out
            .connect(&port, "aura-midi-output")
            .map_err(|e| e.to_string())?;
        let sink = MidiOutSink(conn);

        let thread_shared = Arc::new(ThreadShared::default());
        let ts_for_thread = thread_shared.clone();
        let session_for_thread = self.session.get().cloned();
        let shared_for_thread = self.shared.get().cloned();
        let clock_enabled = self.clock_enabled.clone();
        let note_track = self.note_track.clone();
        let snapshot = self.snapshot.clone();

        let handle = std::thread::Builder::new()
            .name("aura-midi-out".to_string())
            .spawn(move || {
                run_thread(
                    sink,
                    ts_for_thread,
                    session_for_thread,
                    shared_for_thread,
                    clock_enabled,
                    note_track,
                    snapshot,
                )
            })
            .map_err(|e| e.to_string())?;

        inner.active = Some(ActiveOutput { port: info, thread_shared, handle: Some(handle) });
        Ok(())
    }

    /// Flip the live clock-enable flag. Never touches the thread/connection
    /// — safe to call whether or not a port is currently open.
    pub fn set_clock_enabled(&self, enabled: bool) {
        self.clock_enabled.store(enabled, Relaxed);
    }

    /// Route (or, on `None`, un-route) a track's notes to external gear.
    /// Never touches the thread/connection directly — safe to call whether
    /// or not a port is currently open. The `aura-midi-out` thread picks up
    /// the change on its next 250 ms refresh window and releases whatever
    /// the PREVIOUS routing left sounding before adopting the new one (same
    /// "all_off first" rule as a port close/change).
    pub fn select_note_track(&self, track_id: Option<String>) -> Result<(), String> {
        *self.note_track.lock() = track_id;
        Ok(())
    }

    /// Cheap live snapshot for polling.
    pub fn status(&self) -> MidiOutputStatus {
        let inner = self.inner.lock();
        let clock_enabled = self.clock_enabled.load(Relaxed);
        // Hub-sourced (well, MidiOut-field-sourced): correct even idle.
        let note_track_id = self.note_track.lock().clone();
        match inner.active.as_ref() {
            Some(active) => MidiOutputStatus {
                selected: Some(active.port.clone()),
                clock_enabled,
                running: active.thread_shared.running.load(Relaxed),
                pulses_sent: active.thread_shared.pulses_sent.load(Relaxed),
                resyncs: active.thread_shared.resyncs.load(Relaxed),
                note_track_id,
                notes_sent: active.thread_shared.notes_sent.load(Relaxed),
            },
            None => MidiOutputStatus {
                selected: None,
                clock_enabled,
                running: false,
                pulses_sent: 0,
                resyncs: 0,
                note_track_id,
                notes_sent: 0,
            },
        }
    }
}

impl Drop for MidiOut {
    /// Belt and suspenders: normal teardown already happens in
    /// `select_port`'s slow path (re-select to `None`/a different port), but
    /// if a `MidiOut` is simply dropped with a connection open (e.g. process
    /// exit), stop the thread rather than leaking it — the thread itself
    /// still performs the Stop-then-drop sequence before it returns.
    fn drop(&mut self) {
        let mut inner = self.inner.lock();
        if let Some(mut active) = inner.active.take() {
            active.thread_shared.stop.store(true, Relaxed);
            if let Some(handle) = active.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

/// A 120 bpm, one-event tempo map at the given rate — the fallback used
/// before the first successful tempo snapshot (or permanently, in tests
/// that never call `attach`).
fn default_tempo_map(rate: u32) -> TempoMap {
    TempoMap::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: 120.0 }], rate)
        .expect("a single tick-0 tempo event is always a valid TempoMap")
}

/// The `aura-midi-out` thread body — see the module doc for the full
/// contract (500 µs loop, 250 ms try_lock tempo snapshot, Stop-on-exit,
/// session-lock-only, no `EngineHandle::request`).
fn run_thread(
    mut sink: MidiOutSink,
    thread_shared: Arc<ThreadShared>,
    session: Option<Arc<PlMutex<Session>>>,
    shared_rt: Option<Arc<SharedRt>>,
    clock_enabled: Arc<AtomicBool>,
    note_track: Arc<PlMutex<Option<String>>>,
    snapshot: Arc<PlMutex<NoteOutSnapshot>>,
) {
    let mut engine = ClockEngine::new();
    let mut notes = NoteOutEngine::new();
    let fallback_rate = shared_rt
        .as_ref()
        .map(|s| s.sample_rate.load(Relaxed))
        .filter(|&r| r > 0)
        .unwrap_or(48_000);
    let mut map = default_tempo_map(fallback_rate);
    // The last (ppq, tempo_events, rate) triple the map was built from —
    // rebuilding is skipped when nothing in this triple changed.
    let mut snapshot_key: Option<(u32, Vec<TempoEvent>, u32)> = None;
    // The routed track's current events snapshot (Task 8) — rebuilt every
    // 250 ms window alongside the `TempoMap`, from the SAME `session.midi`
    // read, so external and internal timing come from one conversion.
    let mut current_snapshot = NoteOutSnapshot::default();
    let mut last_note_track: Option<String> = None;
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

                    // Task 8: note_track + its events snapshot, refreshed in
                    // this SAME try_lock window (and off the SAME map) as
                    // the tempo — one conversion, two consumers.
                    let track_now = note_track.lock().clone();
                    let events = match &track_now {
                        Some(id) => crate::midi::playback::track_events(&guard.midi, id, &map),
                        None => Vec::new(),
                    };
                    drop(guard);

                    let new_snapshot = NoteOutSnapshot {
                        track_id: track_now.clone().unwrap_or_default(),
                        events: Arc::new(events),
                        channel: 0,
                    };

                    if track_now != last_note_track {
                        // Routing changed (select_output_track / a track
                        // swap detected here): release whatever the OLD
                        // routing left sounding, then reposition the cursor
                        // into the new track at the current playhead —
                        // "all_off first", same rule as a port close.
                        let mut edge = Vec::new();
                        notes.all_off(current_snapshot.channel, &mut edge);
                        let pos_now = shared_rt.as_ref().map(|s| s.position.load(Relaxed)).unwrap_or(0);
                        notes.reseek(&new_snapshot, pos_now, &mut edge);
                        for m in &edge {
                            let _ = sink.send(m.as_slice());
                        }
                        last_note_track = track_now;
                    }

                    current_snapshot = new_snapshot;
                    *snapshot.lock() = current_snapshot.clone();
                    last_snapshot = Instant::now();
                }
                // Contended try_lock: keep the previous snapshot and simply
                // try again on the next iteration where the 250ms window has
                // reopened (last_snapshot is left untouched).
            } else {
                // Unattached (every unit test, and the ALSA loopback test):
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
        if let Err(e) = out_tick(&mut engine, &mut notes, &mut sink, input, enabled, Some(&current_snapshot)) {
            log::warn!("aura-midi-out: send failed: {e}");
        }
        thread_shared.notes_sent.store(notes.notes_sent(), Relaxed);

        thread_shared.running.store(engine.running(), Relaxed);
        thread_shared.pulses_sent.store(engine.pulses_sent(), Relaxed);
        thread_shared.resyncs.store(engine.resyncs(), Relaxed);
    }

    // Stop-on-close: an explicit Stop reaches the sink before the
    // connection is dropped, so a hardware slave synced to MIDI clock does
    // not keep running forever after AURA lets go of the port.
    let _ = sink.send(ClockMsg::Stop.to_out().as_slice());
}

/// The per-tick body, extracted so it is testable with an injected sink and
/// injected time — no thread, no `SharedRt`, no `Session`.
/// `clock_enabled == false` suppresses clock/transport BYTES only — the
/// engine still steps every tick, so its anchor/`estimated_sample()` keeps
/// advancing in lockstep with real time (fix round 1: Task 8's note
/// scheduler advances over that SAME window, so the anchor must stay live
/// even with clock output off), leaving the caller free to keep the thread
/// and the port alive for Task 8's note-out.
pub(crate) fn out_tick(
    engine: &mut ClockEngine,
    notes: &mut NoteOutEngine,
    sink: &mut dyn ClockSink,
    input: ClockInput<'_>,
    clock_enabled: bool,
    snap: Option<&NoteOutSnapshot>,
) -> Result<(), String> {
    // The engine steps UNCONDITIONALLY every tick — its anchor/
    // `estimated_sample()` is the SAME window Task 8's note scheduler
    // advances over, so it must keep living even while clock output is
    // disabled (fix round 1: it previously froze here, which would have
    // frozen note-out in lockstep). `clock_enabled` gates ONLY the byte
    // emission below, not whether the transport state advances.
    let playing = input.playing;
    let was_running = engine.running();
    let prev_estimated = engine.estimated_sample();
    let mut out = Vec::new();
    engine.step(input, &mut out);

    if let Some(snap) = snap {
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
            // from a position that lands exactly on a note (the common
            // "start from bar 1" case) needs that immediate follow-up
            // advance in the SAME tick, not the next one.
            let est = engine.estimated_sample();
            notes.reseek(snap, est, &mut note_out);
            notes.advance(snap, est, est.saturating_add(1), &mut note_out);
        } else if was_running && out.iter().any(|m| matches!(m, ClockMsg::SongPosition(_))) {
            // Resync (backward jump / large forward seek): same "reseek +
            // catch the boundary" shape as a fresh start.
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
pub fn midi_select_output_port(
    port_id: Option<String>,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.select_midi_output_port(
        port_id,
        crate::control::op::TxMeta::user("select midi output port"),
    )
}

#[tauri::command]
pub fn midi_set_clock_enabled(
    enabled: bool,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.set_midi_clock_enabled(enabled, crate::control::op::TxMeta::user("set midi clock enabled"))
}

#[tauri::command]
pub fn midi_output_status(state: tauri::State<'_, Arc<MidiOut>>) -> Result<MidiOutputStatus, String> {
    Ok(state.status())
}

#[tauri::command]
pub fn midi_select_output_track(
    track_id: Option<String>,
    control: tauri::State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.select_midi_output_track(
        track_id,
        crate::control::op::TxMeta::user("select midi output track"),
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
        // 500 ms = one beat at 120 bpm, driven in 1 ms ticks with the position
        // advancing exactly as the audio device would.
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
        // The playhead only moves once per ~10 ms audio block; wall time moves
        // every ms. This must interpolate, not resync.
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
        // A loop wrap back to 0, 10 ms later.
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
        // 2 s of wall time in one tick, with the position keeping up (so it is
        // NOT a jump): 96 pulses due, clamped.
        let out = step(&mut e, 2_000_000, true, 96_000, &m);
        let clocks = out.iter().filter(|m| **m == ClockMsg::Clock).count();
        assert!(clocks <= MAX_PULSE_BURST as usize, "burst clamped, got {clocks}");
    }

    // -----------------------------------------------------------------
    // Task 7: sink/port enumeration, `MidiOut`, and `out_tick`.
    // -----------------------------------------------------------------

    #[test]
    fn list_output_ports_never_panics() {
        match list_output_ports() {
            Ok(ports) => for p in &ports { assert!(!p.id.is_empty()); },
            Err(e) => assert!(!e.is_empty()),
        }
    }

    #[test]
    fn selecting_a_nonexistent_output_port_is_a_graceful_error() {
        let out = MidiOut::default();
        let err = out.select_port(Some("definitely-not-a-port#99".into())).unwrap_err();
        assert!(err.contains("not found"), "got {err}");
        assert!(out.status().selected.is_none(), "a failed open leaves nothing half-open");
    }

    #[test]
    fn out_tick_writes_start_then_clocks_to_the_sink() {
        let map = map120();
        let mut engine = ClockEngine::new();
        let mut notes = NoteOutEngine::new();
        let mut sink = RecordingSink::default();
        out_tick(&mut engine, &mut notes, &mut sink, ClockInput { now_micros: 0, playing: true, position: 0, rate: 48_000, map: &map }, true, None).unwrap();
        assert_eq!(sink.0, vec![vec![0xFA]]);
        for ms in 1..=25u64 {
            out_tick(&mut engine, &mut notes, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, true, None).unwrap();
        }
        assert!(sink.0.iter().any(|b| b == &vec![0xF8]), "clock bytes reached the sink");
    }

    #[test]
    fn out_tick_writes_nothing_while_the_clock_is_disabled() {
        let map = map120();
        let mut engine = ClockEngine::new();
        let mut notes = NoteOutEngine::new();
        let mut sink = RecordingSink::default();
        for ms in 0..=25u64 {
            out_tick(&mut engine, &mut notes, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, false, None).unwrap();
        }
        assert!(sink.0.is_empty(), "clock disabled means no bytes");
    }

    /// Fix round 1: `clock_enabled == false` must suppress bytes only, NOT
    /// freeze the engine — Task 8's note scheduler advances over the SAME
    /// anchor/`estimated_sample()` window `step()` maintains, so the anchor
    /// has to keep living while clock output happens to be off.
    #[test]
    fn out_tick_still_steps_the_engine_while_the_clock_is_disabled() {
        let map = map120();
        let mut engine = ClockEngine::new();
        let mut notes = NoteOutEngine::new();
        let mut sink = RecordingSink::default();
        for ms in 0..=25u64 {
            out_tick(&mut engine, &mut notes, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, false, None).unwrap();
        }
        assert!(sink.0.is_empty(), "clock disabled means no bytes");
        assert!(engine.running(), "the transport still registers as running");
        assert_eq!(engine.estimated_sample(), 25 * 48, "the anchor kept advancing with real time");
    }

    // -----------------------------------------------------------------
    // Task 8: note-out.
    // -----------------------------------------------------------------

    fn snap(events: Vec<crate::midi::schedule::AbsNoteEvent>) -> NoteOutSnapshot {
        NoteOutSnapshot { track_id: "t-1".into(), events: Arc::new(events), channel: 0 }
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
        let (mut clock, mut notes) = (ClockEngine::new(), NoteOutEngine::new());
        let mut sink = RecordingSink::default();
        for ms in 0..=150u64 {
            out_tick(&mut clock, &mut notes, &mut sink,
                ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map },
                true, Some(&s)).unwrap();
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
        let (mut clock, mut notes) = (ClockEngine::new(), NoteOutEngine::new());
        let mut sink = RecordingSink::default();
        out_tick(&mut clock, &mut notes, &mut sink, ClockInput { now_micros: 0, playing: true, position: 0, rate: 48_000, map: &map }, true, Some(&s)).unwrap();
        sink.0.clear();
        out_tick(&mut clock, &mut notes, &mut sink, ClockInput { now_micros: 10_000, playing: false, position: 480, rate: 48_000, map: &map }, true, Some(&s)).unwrap();
        assert!(sink.0.iter().any(|b| b == &vec![0x80, 60, 0]), "no hanging note after stop: {:?}", sink.0);
        assert!(sink.0.iter().any(|b| b == &vec![0xFC]), "and the slave was told to stop");
    }

    /// Real `midir` loopback: a virtual INPUT port receives what a real
    /// `MidiOutSink` sends. Skips cleanly where ALSA seq is unavailable — the
    /// same pattern slice 1's `monitor_toggle_via_manager_does_not_reset_
    /// activity_counters` uses.
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
        out.select_port(Some(target.id)).expect("open the loopback port");
        // Give the ALSA delivery a moment, then assert SOMETHING arrived once a
        // Stop is sent on close.
        out.select_port(None).expect("close sends Stop");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(seen.lock().iter().any(|m| m.as_slice() == [0xFC]), "Stop reached the port: {:?}", seen.lock());
    }
}
