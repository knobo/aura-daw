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
    /// Task 8 fills these; declared here so the wire shape is final.
    pub note_track_id: Option<String>,
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
    inner: PlMutex<Inner>,
}

impl Default for MidiOut {
    fn default() -> Self {
        Self {
            session: OnceLock::new(),
            shared: OnceLock::new(),
            clock_enabled: Arc::new(AtomicBool::new(true)),
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

        let handle = std::thread::Builder::new()
            .name("aura-midi-out".to_string())
            .spawn(move || {
                run_thread(sink, ts_for_thread, session_for_thread, shared_for_thread, clock_enabled)
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

    /// Cheap live snapshot for polling.
    pub fn status(&self) -> MidiOutputStatus {
        let inner = self.inner.lock();
        let clock_enabled = self.clock_enabled.load(Relaxed);
        match inner.active.as_ref() {
            Some(active) => MidiOutputStatus {
                selected: Some(active.port.clone()),
                clock_enabled,
                running: active.thread_shared.running.load(Relaxed),
                pulses_sent: active.thread_shared.pulses_sent.load(Relaxed),
                resyncs: active.thread_shared.resyncs.load(Relaxed),
                note_track_id: None,
                notes_sent: 0,
            },
            None => MidiOutputStatus {
                selected: None,
                clock_enabled,
                running: false,
                pulses_sent: 0,
                resyncs: 0,
                note_track_id: None,
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
) {
    let mut engine = ClockEngine::new();
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
                    drop(guard);
                    if snapshot_key.as_ref() != Some(&key) {
                        if let Ok(new_map) = TempoMap::new(key.0, key.1.clone(), key.2) {
                            map = new_map;
                            snapshot_key = Some(key);
                        }
                    }
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
        if let Err(e) = out_tick(&mut engine, &mut sink, input, enabled) {
            log::warn!("aura-midi-out: send failed: {e}");
        }

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
/// `clock_enabled == false` suppresses clock/transport bytes entirely (the
/// engine is not even stepped, so its internal Start/Stop/pulse state stays
/// frozen exactly as it was), leaving the caller free to keep the thread and
/// the port alive for Task 8's note-out.
pub(crate) fn out_tick(
    engine: &mut ClockEngine,
    sink: &mut dyn ClockSink,
    input: ClockInput<'_>,
    clock_enabled: bool,
) -> Result<(), String> {
    if !clock_enabled {
        return Ok(());
    }
    let mut out = Vec::new();
    engine.step(input, &mut out);
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
        let mut sink = RecordingSink::default();
        out_tick(&mut engine, &mut sink, ClockInput { now_micros: 0, playing: true, position: 0, rate: 48_000, map: &map }, true).unwrap();
        assert_eq!(sink.0, vec![vec![0xFA]]);
        for ms in 1..=25u64 {
            out_tick(&mut engine, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, true).unwrap();
        }
        assert!(sink.0.iter().any(|b| b == &vec![0xF8]), "clock bytes reached the sink");
    }

    #[test]
    fn out_tick_writes_nothing_while_the_clock_is_disabled() {
        let map = map120();
        let mut engine = ClockEngine::new();
        let mut sink = RecordingSink::default();
        for ms in 0..=25u64 {
            out_tick(&mut engine, &mut sink, ClockInput { now_micros: ms * 1_000, playing: true, position: ms * 48, rate: 48_000, map: &map }, false).unwrap();
        }
        assert!(sink.0.is_empty(), "clock disabled means no bytes");
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
