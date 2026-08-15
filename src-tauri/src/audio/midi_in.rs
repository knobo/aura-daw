//! Hardware MIDI-in seam (slice 2). Owns the wait-free path from the midir
//! callback thread to the RT audio callback, the resolved target slot, and
//! the control-side take capture buffer. Knows nothing about midir, ports,
//! ops or the document.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use super::rt::{SharedGraphTables, SharedRt};

pub const EV_NOTE_OFF: u8 = 0;
pub const EV_NOTE_ON: u8 = 1;
pub const EV_ALL_OFF: u8 = 2;

/// POD, `Copy`, 3 bytes — this crosses an rtrb ring into the RT callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveMidiEvent {
    pub kind: u8,
    pub key: u8,
    pub velocity: u8,
}

impl LiveMidiEvent {
    pub fn note_on(key: u8, velocity: u8) -> Self {
        Self { kind: EV_NOTE_ON, key, velocity }
    }
    pub fn note_off(key: u8) -> Self {
        Self { kind: EV_NOTE_OFF, key, velocity: 0 }
    }
    pub fn all_off() -> Self {
        Self { kind: EV_ALL_OFF, key: 0, velocity: 0 }
    }
}

/// One captured incoming edge, stamped with the transport position observed
/// when it ARRIVED (ruling 9). Control-side only — never crosses to RT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturedEvent {
    pub sample: u64,
    pub on: bool,
    pub key: u8,
    pub velocity: u8,
}

/// A finished take's raw capture (see `midi::capture` for the conversion).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub track_id: String,
    pub start_sample: u64,
    pub end_sample: u64,
    pub events: Vec<CapturedEvent>,
}

/// Ring depth: one block of dense playing is a handful of events; 256 is
/// ~5 s of very fast playing at 60 Hz drain, so overflow means the audio
/// callback stopped, not that the player is fast.
pub const LIVE_IN_RING_SLOTS: usize = 256;
/// Most events one RT block will take from the ring (preallocated array on
/// the callback side — never an allocation).
pub const MAX_LIVE_IN_PER_BLOCK: usize = 64;
/// `target_slot` sentinel: nothing routed.
pub const NO_SLOT: u32 = u32::MAX;

/// In-progress take capture state (control side only).
struct CaptureState {
    track_id: String,
    start_sample: u64,
    events: Vec<CapturedEvent>,
}

#[derive(Default)]
pub struct MidiInHub {
    tx: Mutex<Option<rtrb::Producer<LiveMidiEvent>>>,
    dropped: AtomicU64,
    target: Mutex<Option<String>>,
    target_dirty: AtomicBool,
    target_slot: AtomicU32,
    resolved_generation: AtomicU64,
    capture: Mutex<Option<CaptureState>>,
    shared: OnceLock<Arc<SharedRt>>,
}

impl MidiInHub {
    pub fn new() -> Self {
        Self { target_slot: AtomicU32::new(NO_SLOT), ..Default::default() }
    }

    /// Called once by `audio::init`; used only to stamp captured events.
    pub fn attach_shared(&self, shared: Arc<SharedRt>) {
        let _ = self.shared.set(shared);
    }

    // -- producer side (midir callback thread; non-RT, may lock) ----------

    pub fn install_producer(&self, tx: rtrb::Producer<LiveMidiEvent>) {
        *self.tx.lock() = Some(tx);
    }

    pub fn clear_producer(&self) {
        *self.tx.lock() = None;
    }

    /// Never blocks on the RT side; returns false (and bumps `dropped`) when
    /// no producer is installed or the ring is full.
    pub fn push(&self, ev: LiveMidiEvent) -> bool {
        let mut guard = self.tx.lock();
        let ok = match guard.as_mut() {
            Some(tx) => tx.push(ev).is_ok(),
            None => false,
        };
        if !ok {
            self.dropped.fetch_add(1, Relaxed);
        }
        ok
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }

    // -- target (control side) --------------------------------------------

    /// Push an `all_off` for the OUTGOING target first, then switch. Marks
    /// the resolution dirty so the next `refresh_target` re-resolves.
    pub fn set_target_track(&self, track_id: Option<String>) {
        let mut target = self.target.lock();
        if target.is_some() && *target != track_id {
            self.push(LiveMidiEvent::all_off());
        }
        *target = track_id;
        drop(target);
        self.target_dirty.store(true, Relaxed);
    }

    pub fn target_track(&self) -> Option<String> {
        self.target.lock().clone()
    }

    pub fn has_target(&self) -> bool {
        self.target.lock().is_some()
    }

    /// Engine control thread, once per loop tick. Re-resolves only when the
    /// target changed or the graph generation moved. Takes ONLY the tables
    /// lock (never the session lock) — [C1] order is preserved trivially.
    pub fn refresh_target(&self, generation: u64, tables: &SharedGraphTables) {
        let dirty = self.target_dirty.swap(false, Relaxed);
        if !dirty && self.resolved_generation.load(Relaxed) == generation {
            return;
        }
        let want = self.target.lock().clone();
        let slot = match &want {
            None => NO_SLOT,
            Some(id) => tables.lock().slots.get(id.as_str()).copied().map(|s| s as u32).unwrap_or(NO_SLOT),
        };
        self.target_slot.store(slot, Relaxed);
        self.resolved_generation.store(generation, Relaxed);
    }

    /// RT-side read: a single relaxed atomic load, no lock, no allocation.
    pub fn target_slot(&self) -> Option<usize> {
        let slot = self.target_slot.load(Relaxed);
        if slot == NO_SLOT {
            None
        } else {
            Some(slot as usize)
        }
    }

    // -- take capture (control side) ---------------------------------------

    pub fn begin_capture(&self, track_id: String, start_sample: u64) {
        *self.capture.lock() = Some(CaptureState { track_id, start_sample, events: Vec::new() });
    }

    pub fn capturing(&self) -> bool {
        self.capture.lock().is_some()
    }

    /// Called from the midir callback thread; stamps with `SharedRt::position`
    /// (0 when no `SharedRt` is attached, i.e. in unit tests). No-op when not
    /// capturing.
    pub fn capture_event(&self, on: bool, key: u8, velocity: u8) {
        let mut guard = self.capture.lock();
        if let Some(state) = guard.as_mut() {
            let sample = self.shared.get().map(|s| s.position.load(Relaxed)).unwrap_or(0);
            state.events.push(CapturedEvent { sample, on, key, velocity });
        }
    }

    /// Disarms and returns the take. `end_sample` is the position now.
    pub fn end_capture(&self) -> Option<Capture> {
        let state = self.capture.lock().take()?;
        let end_sample = self.shared.get().map(|s| s.position.load(Relaxed)).unwrap_or(0);
        Some(Capture {
            track_id: state.track_id,
            start_sample: state.start_sample,
            end_sample,
            events: state.events,
        })
    }
}

/// Process-global hub (same shape as `sampler::registered_bank` and
/// `midi::playback::registered_store`): lazily created, never replaced.
pub fn hub() -> &'static Arc<MidiInHub> {
    static HUB: OnceLock<Arc<MidiInHub>> = OnceLock::new();
    HUB.get_or_init(|| Arc::new(MidiInHub::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_without_a_producer_is_dropped_not_a_panic() {
        let hub = MidiInHub::new();
        assert!(!hub.push(LiveMidiEvent::note_on(60, 100)));
        assert_eq!(hub.dropped(), 1);
    }

    #[test]
    fn push_and_pop_round_trip_through_the_ring() {
        let hub = MidiInHub::new();
        let (tx, mut rx) = rtrb::RingBuffer::new(LIVE_IN_RING_SLOTS);
        hub.install_producer(tx);
        assert!(hub.push(LiveMidiEvent::note_on(60, 100)));
        assert!(hub.push(LiveMidiEvent::note_off(60)));
        assert_eq!(rx.pop().unwrap(), LiveMidiEvent { kind: EV_NOTE_ON, key: 60, velocity: 100 });
        assert_eq!(rx.pop().unwrap(), LiveMidiEvent { kind: EV_NOTE_OFF, key: 60, velocity: 0 });
        assert_eq!(hub.dropped(), 0);
    }

    #[test]
    fn ring_overflow_counts_drops_and_never_blocks() {
        let hub = MidiInHub::new();
        let (tx, _rx) = rtrb::RingBuffer::new(4);
        hub.install_producer(tx);
        for i in 0..10u8 { hub.push(LiveMidiEvent::note_on(i, 100)); }
        assert!(hub.dropped() >= 6, "dropped {} of 10 into a 4-slot ring", hub.dropped());
    }

    #[test]
    fn switching_target_emits_an_all_off_for_the_old_node() {
        let hub = MidiInHub::new();
        let (tx, mut rx) = rtrb::RingBuffer::new(8);
        hub.install_producer(tx);
        hub.set_target_track(Some("t-1".into()));
        assert!(rx.pop().is_err(), "no all-off when nothing was routed yet");
        hub.set_target_track(Some("t-2".into()));
        assert_eq!(rx.pop().unwrap().kind, EV_ALL_OFF);
        hub.set_target_track(None);
        assert_eq!(rx.pop().unwrap().kind, EV_ALL_OFF);
        assert!(!hub.has_target());
    }

    #[test]
    fn refresh_target_resolves_the_slot_from_the_current_tables() {
        let hub = MidiInHub::new();
        let tables = crate::audio::rt::GraphTables::empty();
        tables.lock().slots.insert("t-1".into(), 3);
        tables.lock().generation = 1;
        hub.set_target_track(Some("t-1".into()));
        hub.refresh_target(1, &tables);
        assert_eq!(hub.target_slot(), Some(3));
    }

    #[test]
    fn refresh_target_is_a_noop_within_the_same_generation() {
        let hub = MidiInHub::new();
        let tables = crate::audio::rt::GraphTables::empty();
        tables.lock().slots.insert("t-1".into(), 3);
        hub.set_target_track(Some("t-1".into()));
        hub.refresh_target(7, &tables);
        assert_eq!(hub.target_slot(), Some(3));
        // Slot map changes without a generation bump: NOT re-read (the engine
        // bumps `generation` on every rebuild, so this cannot go stale in
        // production — this pins that the fast path really is a fast path).
        tables.lock().slots.insert("t-1".into(), 9);
        hub.refresh_target(7, &tables);
        assert_eq!(hub.target_slot(), Some(3));
        // A new generation re-resolves.
        hub.refresh_target(8, &tables);
        assert_eq!(hub.target_slot(), Some(9));
    }

    #[test]
    fn a_target_with_no_slot_resolves_to_nothing_routed() {
        let hub = MidiInHub::new();
        let tables = crate::audio::rt::GraphTables::empty();
        hub.set_target_track(Some("ghost".into()));
        hub.refresh_target(1, &tables);
        assert_eq!(hub.target_slot(), None);
    }

    #[test]
    fn capture_records_only_while_armed() {
        let hub = MidiInHub::new();
        hub.capture_event(true, 60, 100);
        assert!(!hub.capturing());
        hub.begin_capture("t-1".into(), 480);
        assert!(hub.capturing());
        hub.capture_event(true, 60, 100);
        hub.capture_event(false, 60, 0);
        let take = hub.end_capture().expect("a take was armed");
        assert_eq!(take.track_id, "t-1");
        assert_eq!(take.start_sample, 480);
        assert_eq!(take.events.len(), 2);
        assert!(take.events[0].on && take.events[0].key == 60);
        assert!(!hub.capturing());
        assert!(hub.end_capture().is_none(), "end_capture disarms");
    }
}
