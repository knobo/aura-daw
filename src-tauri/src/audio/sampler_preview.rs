//! Sampler preview/audition path — OWNED BY THE SAMPLER AGENT (phase 2,
//! zone D).
//!
//! `sampler_preview_note` must make a loaded instrument AUDIBLE without a
//! project, a midi clip, or zone C's note scheduling. The engine's render
//! graph only carries audio clips today (the midi-source seam in
//! `engine.rs::rebuild` is zone C's named integration point), so preview runs
//! a small dedicated output stream instead:
//!
//! * A control thread (spawned lazily on first preview) owns the cpal stream
//!   — mirroring `engine.rs`: streams live and die on a control thread only.
//! * The RT callback owns a [`SamplerNode`] and obeys the §2.1 contract. It
//!   receives new nodes by pointer over a wait-free rtrb queue and ships the
//!   retired node back on a second queue for control-side deallocation (the
//!   same RCU discipline as the engine graph swap).
//! * Notes travel over the node's [`NoteCmd`] queue with an auto-release
//!   hold, so one command plays a complete, envelope-shaped audition note.
//! * [`PreviewHandle::note_on`]/`note_off`/`all_off` (added for the
//!   `midi_input` live-monitoring path, slice 1b) are the sustained-note
//!   siblings of `play`: fire-and-forget sends (no reply channel, so a
//!   caller — including a non-RT MIDI input callback thread — never
//!   blocks), holding until an explicit release instead of auto-releasing.
//!
//! When zone C's midi-track sources land, previews can additionally route
//! through the master bus by installing the same `SamplerNode` there; this
//! module keeps working unchanged as the "no project open" audition path.

use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};

use super::dsp::{AudioProcessor, ProcessBlock};
use super::sampler_engine::{CompiledInstrument, NoteCmd, SamplerNode};

/// Audition note length before auto-release, seconds (release tail follows).
const PREVIEW_HOLD_SECS: f64 = 0.6;
/// Capacity of the note-command queue (bursts of previews between blocks).
const NOTE_QUEUE: usize = 64;

// ---------------------------------------------------------------------------
// Handle (control-plane facade)
// ---------------------------------------------------------------------------

enum Msg {
    Play {
        instrument: Arc<CompiledInstrument>,
        key: u8,
        velocity: u8,
        reply: Sender<Result<(), String>>,
    },
    /// Sustained audition with a reply channel: `note_on`'s hold semantics
    /// (no auto-release — the voice ends itself at the end of the sample
    /// data) with `play`'s error reporting, for user-initiated commands that
    /// have a result to surface. Used by `library_audition`.
    PlayHeld {
        instrument: Arc<CompiledInstrument>,
        key: u8,
        velocity: u8,
        reply: Sender<Result<(), String>>,
    },
    /// Fire-and-forget sustained note-on (no auto-release hold; waits for an
    /// explicit `NoteOff`). No reply channel — the caller never blocks. Added
    /// for live-monitoring callers (e.g. `midi_input`) that manage their own
    /// note-off timing and must not stall on the control thread.
    NoteOn { instrument: Arc<CompiledInstrument>, key: u8, velocity: u8 },
    /// Fire-and-forget release. A no-op if nothing is installed/playing.
    NoteOff { key: u8 },
    /// Fire-and-forget release-all (e.g. monitor toggled off, port changed).
    AllOff,
    #[allow(dead_code)]
    Shutdown,
}

/// Cheap clonable handle; owns nothing but the channel to the preview thread.
#[derive(Clone)]
pub struct PreviewHandle {
    tx: Sender<Msg>,
}

impl PreviewHandle {
    /// Audition `key` on `instrument`. Blocks briefly for the thread's reply
    /// (stream open errors surface here); the note itself plays async.
    pub fn play(
        &self,
        instrument: Arc<CompiledInstrument>,
        key: u8,
        velocity: u8,
    ) -> Result<(), String> {
        let (reply, rx) = bounded(1);
        self.tx
            .send(Msg::Play { instrument, key, velocity, reply })
            .map_err(|_| "preview thread is gone".to_string())?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "preview thread did not respond".to_string())?
    }

    /// Audition a whole sample: a sustained note that holds until the sample
    /// data runs out (or an [`PreviewHandle::all_off`]). Blocks briefly for
    /// the thread's reply, so stream-open and install errors reach the caller
    /// — unlike [`PreviewHandle::note_on`], which is fire-and-forget for
    /// non-RT callback threads.
    pub fn play_held(
        &self,
        instrument: Arc<CompiledInstrument>,
        key: u8,
        velocity: u8,
    ) -> Result<(), String> {
        let (reply, rx) = bounded(1);
        self.tx
            .send(Msg::PlayHeld { instrument, key, velocity, reply })
            .map_err(|_| "preview thread is gone".to_string())?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "preview thread did not respond".to_string())?
    }

    /// Sustained note-on for live monitoring: no auto-release, holds until a
    /// matching [`PreviewHandle::note_off`]. Non-blocking — only enqueues on
    /// the control channel (a plain crossbeam send), so it is safe to call
    /// from a non-RT callback thread (e.g. a `midir` input callback) without
    /// risking a stall there. Errors (stream/queue trouble) are NOT reported
    /// back to the caller by design — this is preview-grade monitoring, not
    /// a user-initiated command with a result to surface; the control thread
    /// logs failures itself.
    pub fn note_on(&self, instrument: Arc<CompiledInstrument>, key: u8, velocity: u8) -> Result<(), String> {
        self.tx
            .send(Msg::NoteOn { instrument, key, velocity })
            .map_err(|_| "preview thread is gone".to_string())
    }

    /// Release a sustained note started with [`PreviewHandle::note_on`].
    /// Non-blocking, same rationale as `note_on`.
    pub fn note_off(&self, key: u8) -> Result<(), String> {
        self.tx.send(Msg::NoteOff { key }).map_err(|_| "preview thread is gone".to_string())
    }

    /// Release every currently-sounding note. Non-blocking.
    pub fn all_off(&self) -> Result<(), String> {
        self.tx.send(Msg::AllOff).map_err(|_| "preview thread is gone".to_string())
    }
}

/// Spawn the preview control thread (call once, lazily). `name_suffix`
/// distinguishes independent preview streams in thread dumps/logs when more
/// than one caller lazily starts its own (e.g. `sampler_preview_note`'s
/// "ui" stream vs. `midi_input`'s "midi" live-monitoring stream) — each
/// gets its own thread named `aura-sampler-preview-<name_suffix>`.
pub fn start(name_suffix: &str) -> PreviewHandle {
    let (tx, rx) = unbounded();
    std::thread::Builder::new()
        .name(format!("aura-sampler-preview-{name_suffix}"))
        .spawn(move || preview_thread(rx))
        .expect("spawn sampler preview thread");
    PreviewHandle { tx }
}

// ---------------------------------------------------------------------------
// RT plumbing (RCU node swap + note queue)
// ---------------------------------------------------------------------------

/// Raw `SamplerNode` pointer shipped through rtrb queues; ownership moves
/// with it (exactly the engine's `GraphPtr` pattern — dealloc control-side).
pub(crate) struct NodePtr(pub *mut SamplerNode);
// SAFETY: exactly one side owns the pointee at any time; SamplerNode is Send.
unsafe impl Send for NodePtr {}

/// State owned by the preview output callback. RT-safe by construction.
pub(crate) struct PreviewCb {
    pub swap_rx: rtrb::Consumer<NodePtr>,
    pub retire_tx: rtrb::Producer<NodePtr>,
    pub node: Option<Box<SamplerNode>>,
    pub channels: usize,
    pub rate: u32,
}

impl PreviewCb {
    pub fn render(&mut self, out: &mut [f32]) {
        // Adopt a fresh node only when the retired one has a slot to go to.
        while self.retire_tx.slots() > 0 {
            match self.swap_rx.pop() {
                Ok(p) => {
                    let fresh = unsafe { Box::from_raw(p.0) };
                    if let Some(old) = self.node.replace(fresh) {
                        // slots() > 0 checked above: push cannot fail.
                        let _ = self.retire_tx.push(NodePtr(Box::into_raw(old)));
                    }
                }
                Err(_) => break,
            }
        }
        out.fill(0.0);
        if let Some(node) = self.node.as_mut() {
            let mut io = ProcessBlock {
                samples: out,
                channels: self.channels,
                sample_rate: self.rate,
                // Sample preview, not the live engine graph — no CLAP node
                // ever sits behind this seam, so there is no steady_time to
                // carry.
                steady: None,
            };
            node.process(&mut io);
        }
    }
}

impl Drop for PreviewCb {
    fn drop(&mut self) {
        // The callback closure is dropped with the stream on the CONTROL
        // thread (cpal contract) — freeing the node here is off-RT.
        while let Ok(p) = self.swap_rx.pop() {
            drop(unsafe { Box::from_raw(p.0) });
        }
        self.node = None;
    }
}

// ---------------------------------------------------------------------------
// Preview control thread
// ---------------------------------------------------------------------------

struct Bundle {
    _stream: cpal::Stream,
    swap_tx: rtrb::Producer<NodePtr>,
    retire_rx: rtrb::Consumer<NodePtr>,
    note_tx: Option<rtrb::Producer<NoteCmd>>,
    rate: u32,
    /// Id of the instrument currently installed in the callback.
    instrument_id: Option<String>,
}

impl Drop for Bundle {
    fn drop(&mut self) {
        while let Ok(p) = self.retire_rx.pop() {
            drop(unsafe { Box::from_raw(p.0) });
        }
    }
}

fn preview_thread(rx: Receiver<Msg>) {
    let mut bundle: Option<Bundle> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Msg::Play { instrument, key, velocity, reply }) => {
                let _ = reply.send(handle_play(&mut bundle, instrument, key, velocity));
            }
            Ok(Msg::PlayHeld { instrument, key, velocity, reply }) => {
                let _ = reply.send(handle_note_on(&mut bundle, instrument, key, velocity));
            }
            Ok(Msg::NoteOn { instrument, key, velocity }) => {
                if let Err(e) = handle_note_on(&mut bundle, instrument, key, velocity) {
                    log::warn!("live monitor note-on failed: {e}");
                }
            }
            Ok(Msg::NoteOff { key }) => {
                if let Some(b) = bundle.as_mut() {
                    if let Some(tx) = b.note_tx.as_mut() {
                        let _ = tx.push(NoteCmd::Off { key });
                    }
                }
            }
            Ok(Msg::AllOff) => {
                if let Some(b) = bundle.as_mut() {
                    if let Some(tx) = b.note_tx.as_mut() {
                        let _ = tx.push(NoteCmd::AllOff);
                    }
                }
            }
            Ok(Msg::Shutdown) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
        }
        // Free retired nodes control-side.
        if let Some(b) = bundle.as_mut() {
            while let Ok(p) = b.retire_rx.pop() {
                drop(unsafe { Box::from_raw(p.0) });
            }
        }
    }
    drop(bundle);
}

fn handle_play(
    bundle: &mut Option<Bundle>,
    instrument: Arc<CompiledInstrument>,
    key: u8,
    velocity: u8,
) -> Result<(), String> {
    let b = ensure_instrument_installed(bundle, instrument)?;
    let hold = (b.rate as f64 * PREVIEW_HOLD_SECS) as u64;
    let tx = b.note_tx.as_mut().expect("note queue installed above");
    tx.push(NoteCmd::On { key, velocity: velocity.clamp(1, 127), hold_frames: hold })
        .map_err(|_| "preview note queue full — too many overlapping previews".to_string())
}

/// Sustained (no auto-release) note-on, for live monitoring — the note holds
/// until a matching `Msg::NoteOff` releases it.
fn handle_note_on(
    bundle: &mut Option<Bundle>,
    instrument: Arc<CompiledInstrument>,
    key: u8,
    velocity: u8,
) -> Result<(), String> {
    let b = ensure_instrument_installed(bundle, instrument)?;
    let tx = b.note_tx.as_mut().expect("note queue installed above");
    // hold_frames: 0 => SamplerNode maps this to HOLD_MANUAL (sustain until
    // an explicit NoteCmd::Off), not "zero-length note".
    tx.push(NoteCmd::On { key, velocity: velocity.clamp(1, 127), hold_frames: 0 })
        .map_err(|_| "preview note queue full — too many overlapping notes".to_string())
}

/// Lazily open the preview output stream and (re)install `instrument` in the
/// callback's node if it isn't already the one installed. Shared by both the
/// auto-release `Play` path and the sustained `NoteOn` monitoring path.
fn ensure_instrument_installed(
    bundle: &mut Option<Bundle>,
    instrument: Arc<CompiledInstrument>,
) -> Result<&mut Bundle, String> {
    if bundle.is_none() {
        *bundle = Some(open_stream()?);
    }
    let b = bundle.as_mut().expect("bundle just ensured");

    if b.instrument_id.as_deref() != Some(instrument.id.as_str()) || b.note_tx.is_none() {
        let mut node = Box::new(SamplerNode::new((*instrument).clone()));
        let note_tx = node.command_queue(NOTE_QUEUE);
        node.prepare(b.rate, 4096);
        if let Err(rtrb::PushError::Full(p)) = b.swap_tx.push(NodePtr(Box::into_raw(node))) {
            // Callback stalled — free here and report.
            drop(unsafe { Box::from_raw(p.0) });
            return Err("preview stream is not consuming (swap queue full)".into());
        }
        b.note_tx = Some(note_tx);
        b.instrument_id = Some(instrument.id.clone());
    }
    Ok(b)
}

fn open_stream() -> Result<Bundle, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no output device available for preview".to_string())?;
    let cfg = device.default_output_config().map_err(|e| e.to_string())?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!(
            "unsupported output sample format {:?} (prototype supports f32)",
            cfg.sample_format()
        ));
    }
    let rate = cfg.sample_rate().0;
    let channels = cfg.channels().max(1) as usize;

    let (swap_tx, swap_rx) = rtrb::RingBuffer::new(4);
    let (retire_tx, retire_rx) = rtrb::RingBuffer::new(4);
    let mut cb = PreviewCb { swap_rx, retire_tx, node: None, channels, rate };
    let stream = device
        .build_output_stream(
            &cfg.into(),
            move |data: &mut [f32], _| cb.render(data),
            |e| log::warn!("preview stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    log::info!("sampler preview: output stream open ({channels} ch @ {rate} Hz)");
    Ok(Bundle {
        _stream: stream,
        swap_tx,
        retire_rx,
        note_tx: None,
        rate,
        instrument_id: None,
    })
}

// ---------------------------------------------------------------------------
// Tests (headless: exercise the RT callback + RCU swap without a device)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::sampler_voice::{CompiledRegion, SampleData};
    use super::*;

    const RATE: u32 = 48_000;

    fn compiled(id: &str, freq: f64) -> CompiledInstrument {
        let n = RATE as usize; // 1 s
        let data: Vec<f32> = (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / RATE as f64).sin() as f32 * 0.8
            })
            .collect();
        let sample = Arc::new(SampleData { channels: 1, rate: RATE, data });
        CompiledInstrument {
            id: id.into(),
            name: id.into(),
            rate: RATE,
            regions: Arc::new(vec![CompiledRegion {
                lokey: 0,
                hikey: 127,
                lovel: 1,
                hivel: 127,
                pitch_keycenter: 69,
                tune: 0,
                transpose: 0,
                gain_l: 1.0,
                gain_r: 1.0,
                offset: 0,
                looped: false,
                loop_start: 0,
                loop_end: 0,
                ampeg_attack: 0.001,
                ampeg_release: 0.05,
                sample,
            }]),
        }
    }

    /// The full preview path minus the device: node swap over the RCU
    /// queues, note command in, audible audio out, old node retired.
    #[test]
    fn preview_callback_swaps_nodes_and_renders_notes() {
        let (mut swap_tx, swap_rx) = rtrb::RingBuffer::new(4);
        let (retire_tx, mut retire_rx) = rtrb::RingBuffer::new(4);
        let mut cb = PreviewCb { swap_rx, retire_tx, node: None, channels: 2, rate: RATE };

        // No node yet: renders silence, doesn't crash.
        let mut out = vec![1.0f32; 256 * 2];
        cb.render(&mut out);
        assert!(out.iter().all(|s| *s == 0.0), "silence with no instrument");

        // Install instrument A and play a note through the command queue.
        let mut node_a = Box::new(SamplerNode::new(compiled("a", 440.0)));
        let mut note_tx = node_a.command_queue(8);
        node_a.prepare(RATE, 512);
        swap_tx.push(NodePtr(Box::into_raw(node_a))).unwrap();
        note_tx
            .push(NoteCmd::On { key: 69, velocity: 120, hold_frames: 20_000 })
            .unwrap();
        let mut out2 = vec![0.0f32; 1024 * 2];
        cb.render(&mut out2);
        assert!(out2.iter().any(|s| s.abs() > 0.05), "preview note is audible");

        // Swap to instrument B: A comes back on the retire queue (dealloc
        // control-side — never in the callback).
        let mut node_b = Box::new(SamplerNode::new(compiled("b", 220.0)));
        let _tx_b = node_b.command_queue(8);
        node_b.prepare(RATE, 512);
        swap_tx.push(NodePtr(Box::into_raw(node_b))).unwrap();
        let mut out3 = vec![0.0f32; 256 * 2];
        cb.render(&mut out3);
        assert!(out3.iter().all(|s| *s == 0.0), "fresh node, no note yet");
        let retired = retire_rx.pop().expect("old node retired to control side");
        let old = unsafe { Box::from_raw(retired.0) };
        assert_eq!(old.instrument().id, "a");
    }

    /// `handle_play` on a machine without an output device must fail cleanly
    /// (never panic). With a device it must play. Either way: no hang.
    #[test]
    fn play_reports_cleanly_with_or_without_device() {
        let handle = start("test");
        let res = handle.play(Arc::new(compiled("x", 330.0)), 60, 100);
        match res {
            Ok(()) => {
                // Device present: a second preview on the SAME instrument
                // reuses the installed node (no swap) and must also succeed.
                handle.play(Arc::new(compiled("x", 330.0)), 64, 100).unwrap();
            }
            Err(e) => {
                assert!(!e.is_empty(), "error message is informative: {e}");
            }
        }
        let _ = handle.tx.send(Msg::Shutdown);
    }

    /// `play_held`'s wire behaviour at the callback: a note pushed with
    /// `hold_frames: 0` (HOLD_MANUAL) is still sounding well past
    /// PREVIEW_HOLD_SECS, and `NoteCmd::AllOff` releases it to silence.
    #[test]
    fn a_held_preview_note_outlives_the_auto_release_window_until_all_off() {
        let (mut swap_tx, swap_rx) = rtrb::RingBuffer::new(4);
        let (retire_tx, _retire_rx) = rtrb::RingBuffer::new(4);
        let mut cb = PreviewCb { swap_rx, retire_tx, node: None, channels: 2, rate: RATE };

        let mut node = Box::new(SamplerNode::new(compiled("held", 440.0)));
        let mut note_tx = node.command_queue(8);
        node.prepare(RATE, 512);
        swap_tx.push(NodePtr(Box::into_raw(node))).unwrap();
        note_tx.push(NoteCmd::On { key: 69, velocity: 110, hold_frames: 0 }).unwrap();

        // Render past PREVIEW_HOLD_SECS (0.6 s) — `compiled()` is a 1 s sample.
        let block = 512usize;
        let blocks = ((RATE as f64 * 0.8) / block as f64) as usize;
        let mut last = vec![0.0f32; block * 2];
        for _ in 0..blocks {
            last.fill(0.0);
            cb.render(&mut last);
        }
        assert!(last.iter().any(|s| s.abs() > 0.01), "still sounding after 0.8 s");

        note_tx.push(NoteCmd::AllOff).unwrap();
        for _ in 0..20 {
            last.fill(0.0);
            cb.render(&mut last);
        }
        assert!(last.iter().all(|s| s.abs() < 1.0e-4), "AllOff released it to silence");
    }
}
