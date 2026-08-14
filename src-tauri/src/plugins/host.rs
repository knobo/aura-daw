//! Plugin instance hosting (phase 3 scaffold).
//!
//! ## Architecture (binding for zones P1/P2 — ARCHITECTURE §15)
//!
//! * **Plugin main thread.** CLAP's `[main-thread]` contract and lilv's
//!   thread-affinity both demand that instantiation, activation, state and
//!   GUI calls happen on ONE dedicated thread. Zones spawn
//!   `aura-plugin-host` (a std thread mirroring `engine::start`'s pattern:
//!   crossbeam request channel + `Reply` oneshots) owning the `livi::World`
//!   and every CLAP `PluginInstance`. Tauri commands never touch plugin FFI
//!   directly.
//! * **RT processing.** An activated instance yields an RT-half object
//!   (CLAP: `StartedPluginAudioProcessor`; LV2: `livi::Instance`) that is
//!   moved into a node implementing [`LiveInstrument`] and installed in the
//!   graph via the same `LiveNodeCell` seam as PolySynth/SamplerNode. Note
//!   events arrive as `BlockNoteEvent`s (velocity 0 = off) and are
//!   translated inside the node (CLAP `NoteOnEvent`/`NoteOffEvent`; LV2
//!   atom-sequence MIDI via a preallocated `LV2AtomSequence`).
//! * **Parameters** bridge through a wait-free ring (control -> node), same
//!   pattern as `sampler_engine::NoteCmd`; values mirror into the registry
//!   for the generic UI. Automation joins via the event-port design
//!   (SCALABILITY §2).
//! * **Isolation.** In-process for v1 (documented risk); the node only ever
//!   talks to an internal handle so an out-of-process bridge (shared-memory
//!   audio ring + event pipe, yabridge-style) can replace the in-process
//!   implementation per instance without touching the graph.
//!
//! What ships in THIS scaffold: the registry data model, uid validation and
//! [`StubPluginNode`] — a silent, RT-safe `LiveInstrument` that occupies the
//! seam so track binding, persistence and UI can be built and tested before
//! the real hosts land. A `plugin:`-bound track therefore renders SILENCE
//! (status "stub"), never a misleading fallback sound.

use std::any::Any;
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use crossbeam_channel::{bounded, unbounded, Sender};

use crate::audio::dsp::{AudioProcessor, LiveInstrument, ProcessBlock};
use crate::midi::synth::BlockNoteEvent;

// ---------------------------------------------------------------------------
// The plugin main thread (cross-zone contract 2, PHASE3-PLAN §2)
// ---------------------------------------------------------------------------
//
// ONE std thread (`aura-plugin-host`) owns every piece of thread-affine
// plugin FFI state: all CLAP entries + `PluginInstance`s (zone P1,
// `clap_host.rs`) and the `livi::World` + LV2 instances (zone P2, which
// registers its state into the same thread via [`MainCtx::slot_mut`] — never
// a second FFI thread). Tauri commands and the engine control thread talk to
// it exclusively through [`PluginMainThread::run`]/[`post`] (crossbeam
// request channel + `Reply` oneshots, the `engine::start` pattern).

/// State container owned by the plugin main thread. Zones keep their world
/// in a named slot (`"clap"`, `"lv2"`); tickers run periodically (~30 ms)
/// for main-thread callbacks (CLAP `request_callback`, LV2 workers).
pub struct MainCtx {
    slots: HashMap<&'static str, Box<dyn Any>>,
    tickers: Vec<(&'static str, Ticker)>,
}

type Ticker = Box<dyn FnMut(&mut MainCtx)>;

impl MainCtx {
    fn new() -> Self {
        Self { slots: HashMap::new(), tickers: Vec::new() }
    }

    /// The zone's state slot, created on first access. The type per tag must
    /// be stable (each zone owns its tag).
    pub fn slot_mut<T: Default + 'static>(&mut self, tag: &'static str) -> &mut T {
        self.slots
            .entry(tag)
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("plugin main thread: slot type is stable per tag")
    }

    /// Register a periodic main-thread ticker (idempotent per tag).
    pub fn add_ticker(&mut self, tag: &'static str, f: Ticker) {
        if !self.tickers.iter().any(|(t, _)| *t == tag) {
            self.tickers.push((tag, f));
        }
    }

    fn tick(&mut self) {
        // Tickers are moved out while running so they can borrow the ctx.
        let mut tickers = std::mem::take(&mut self.tickers);
        for (_, t) in tickers.iter_mut() {
            t(self);
        }
        // Preserve tickers registered DURING the tick (rare, but cheap).
        tickers.append(&mut self.tickers);
        self.tickers = tickers;
    }
}

enum MainMsg {
    Run(Box<dyn FnOnce(&mut MainCtx) + Send>),
    Shutdown,
}

/// Handle to the plugin main thread (cheap to clone via the static).
pub struct PluginMainThread {
    tx: Sender<MainMsg>,
}

impl PluginMainThread {
    /// Run `f` on the plugin main thread and wait for its result. Callable
    /// from Tauri command threads and the engine control thread — NEVER from
    /// the RT thread (it blocks).
    pub fn run<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut MainCtx) -> R + Send + 'static,
    ) -> Result<R, String> {
        let (rtx, rrx) = bounded(1);
        self.tx
            .send(MainMsg::Run(Box::new(move |ctx| {
                let _ = rtx.send(f(ctx));
            })))
            .map_err(|_| "plugin main thread is not running".to_string())?;
        rrx.recv_timeout(Duration::from_secs(30))
            .map_err(|_| "plugin main thread did not respond".to_string())
    }

    /// Fire-and-forget variant (used by node teardown on the engine control
    /// thread — must not block or fail loudly during graph swaps).
    pub fn post(&self, f: impl FnOnce(&mut MainCtx) + Send + 'static) {
        let _ = self.tx.send(MainMsg::Run(Box::new(f)));
    }

    /// Request shutdown (tests/teardown only; the app keeps it for life).
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        let _ = self.tx.send(MainMsg::Shutdown);
    }
}

static MAIN_THREAD: OnceLock<PluginMainThread> = OnceLock::new();

/// The app-wide plugin main thread, spawned lazily on first use.
pub fn plugin_main() -> &'static PluginMainThread {
    MAIN_THREAD.get_or_init(|| {
        let (tx, rx) = unbounded::<MainMsg>();
        std::thread::Builder::new()
            .name("aura-plugin-host".into())
            .spawn(move || {
                let mut ctx = MainCtx::new();
                loop {
                    match rx.recv_timeout(Duration::from_millis(30)) {
                        Ok(MainMsg::Run(f)) => f(&mut ctx),
                        Ok(MainMsg::Shutdown) => break,
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => ctx.tick(),
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn plugin main thread");
        PluginMainThread { tx }
    })
}

/// Silent placeholder node for registered plugin instances. RT-safe by
/// construction: no state, no allocation, adds nothing to the (pre-zeroed)
/// scratch it is handed.
pub struct StubPluginNode {
    /// Events accepted since the last process call (diagnostics only).
    events_seen: u64,
}

impl StubPluginNode {
    pub fn new() -> Self {
        Self { events_seen: 0 }
    }

    pub fn events_seen(&self) -> u64 {
        self.events_seen
    }
}

impl Default for StubPluginNode {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioProcessor for StubPluginNode {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}
    fn process(&mut self, _io: &mut ProcessBlock<'_>) {
        // Silence: the scratch buffer arrives zeroed and stays zeroed.
    }
    fn reset(&mut self) {}
}

impl LiveInstrument for StubPluginNode {
    fn queue_event(&mut self, _ev: BlockNoteEvent) -> bool {
        self.events_seen += 1;
        true
    }
    fn all_notes_off(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stub occupies the live seam safely: consumes events, renders
    /// silence, never panics.
    #[test]
    fn stub_node_is_silent_and_event_tolerant() {
        let mut node = StubPluginNode::new();
        node.prepare(48_000, 512);
        for i in 0..600u32 {
            node.queue_event(BlockNoteEvent {
                offset: i % 512,
                key: (i % 128) as u8,
                velocity: (i % 2 * 100) as u8,
            });
        }
        assert_eq!(node.events_seen(), 600);
        let mut buf = vec![0.0f32; 512 * 2];
        let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: 48_000, steady: 0 };
        node.process(&mut io);
        node.all_notes_off();
        assert!(buf.iter().all(|s| *s == 0.0), "stub renders silence");
    }

    /// The plugin main thread serves requests, keeps per-tag slot state
    /// across requests, and runs registered tickers.
    #[test]
    fn plugin_main_thread_slots_and_tickers() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let mt = plugin_main();
        let n = mt.run(|ctx| {
            let v: &mut Vec<u32> = ctx.slot_mut("test-zone");
            v.push(7);
            v.len()
        });
        assert_eq!(n, Ok(1));
        let n = mt.run(|ctx| {
            let v: &mut Vec<u32> = ctx.slot_mut("test-zone");
            v.push(8);
            v.iter().sum::<u32>()
        });
        assert_eq!(n, Ok(15), "slot state survives across requests");

        let ticks = Arc::new(AtomicU32::new(0));
        let t = ticks.clone();
        mt.run(move |ctx| {
            ctx.add_ticker(
                "test-zone",
                Box::new(move |_| {
                    t.fetch_add(1, Ordering::Relaxed);
                }),
            );
        })
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ticks.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ticks.load(Ordering::Relaxed) > 0, "ticker ran on idle");
    }
}
