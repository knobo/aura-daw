//! The live-pitch chain, split across the real-time boundary.
//!
//! Spec §3.2: the capture callback must not run YIN. What lands here is the
//! split the spec asks for —
//!
//! ```text
//! capture (RT) --decimate--> rtrb<f32> + rtrb<PitchChunk> --> worker --> rtrb<PitchFrame> --> control
//! ```
//!
//! * [`PitchTap`] is the RT half. It averages channels, band-limits and
//!   resamples to 8 kHz (all bounded, allocation-free work on buffers
//!   reserved at open) and hands the samples over two SPSC rings.
//! * [`PitchWorker`] is the non-RT half: it owns the [`PitchAnalyzer`] — YIN,
//!   the RMS gate, the median and the jump limiter — and turns chunks into
//!   frames. [`PitchWorker::pump`] is synchronous so the whole chain is
//!   testable without a thread; [`spawn_pitch_worker`] is what production
//!   uses.
//!
//! **Why two rings and not one.** The analyser timestamps frames against the
//! transport position of the samples it is given, so each chunk of samples
//! needs its device position and rate travelling with it. Samples go into
//! their ring FIRST and the descriptor second, so a descriptor the worker can
//! see always has its samples already behind it. A chunk that does not fit in
//! either ring is dropped WHOLE — never half-written — so the two rings can
//! not drift out of step.

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::decimate::Decimator;
use super::pitch::{PitchAnalyzer, PitchFrame};

/// Input frames a [`PitchTap`]'s scratch buffers are sized for. Well above any
/// plausible cpal buffer, so a large period never forces a reallocation on
/// the RT thread.
pub const PITCH_TAP_MAX_FRAMES: usize = 8192;

/// 8 kHz samples the worker may fall behind by. Sized to hold one MAXIMAL
/// chunk — the same bound `dec_buf` is reserved to — because a chunk that
/// cannot fit is dropped, and a device the decimator upsamples from would
/// then never produce a single frame rather than merely dropping a few. That
/// bound also buys ~8 seconds of slack at 8 kHz, far more than the [`POLL`]
/// interval needs.
const SAMPLE_RING_SLOTS: usize = PITCH_TAP_MAX_FRAMES * 8 + 8;

/// Chunk descriptors in flight. One per capture buffer.
const CHUNK_RING_SLOTS: usize = 512;

/// Analysed frames waiting for the 60 Hz control tick.
const FRAME_RING_SLOTS: usize = 512;

/// How long the worker sleeps when there is nothing to analyse. Frames are
/// consumed by a 60 Hz tick (16.7 ms), so this is well inside the budget and
/// costs 200 wake-ups a second when idle.
const POLL: Duration = Duration::from_millis(5);

/// Describes the samples that precede it in the sample ring.
#[derive(Clone, Copy, Debug)]
struct PitchChunk {
    /// Transport position, in DEVICE samples, of this chunk's first input
    /// frame — what [`PitchAnalyzer::push`] anchors its timestamps to.
    start_sample: u64,
    device_rate: u32,
    /// 8 kHz samples in the sample ring belonging to this chunk.
    len: u32,
    /// The analyser must start over: this is the first chunk after the tap
    /// was switched on, so nothing before it is contiguous with it.
    reset: bool,
}

/// The RT half: decimate and hand over. Owns no detector.
///
/// Every buffer is preallocated by [`pitch_channel`] on the control thread.
/// `Vec::clear` keeps capacity, so [`PitchTap::process`] reuses them forever
/// and never allocates in steady state — the `Vec`s are not an RT violation,
/// which is worth stating because they look like one.
pub struct PitchTap {
    decimator: Decimator,
    dec_buf: Vec<f32>,
    samples_tx: rtrb::Producer<f32>,
    chunks_tx: rtrb::Producer<PitchChunk>,
    /// Whether the user wants live pitch right now. Read once per buffer, so
    /// a tap can sit dormant on a take's capture stream and start producing
    /// the moment the panel is opened — without rebuilding the stream (that
    /// is what makes listen-mid-take possible at all).
    active: Arc<AtomicBool>,
    /// `active` as of the previous buffer, so the off→on edge can reset the
    /// filter state and tell the worker to reset the analyser.
    was_active: bool,
    /// A reset is owed to the worker but has not been attached to a chunk
    /// yet (the resampler can swallow a short buffer whole).
    pending_reset: bool,
    device_rate: u32,
}

impl PitchTap {
    /// Push one interleaved capture buffer. `start_sample` is the transport
    /// position of its first frame, in device samples.
    ///
    /// Does nothing at all while the tap is inactive: one relaxed atomic load
    /// is the entire cost of carrying a dormant tap on a take's stream.
    pub fn process(&mut self, input: &[f32], channels: usize, start_sample: u64) {
        if !self.active.load(Relaxed) {
            self.was_active = false;
            return;
        }
        if !self.was_active {
            // Nothing before this buffer is contiguous with it: drop the
            // filter/resampler history here and the detector history over
            // there, so a re-listen does not splice two unrelated moments.
            self.decimator.reset();
            self.pending_reset = true;
            self.was_active = true;
        }

        let channels = channels.max(1);
        let max_samples = PITCH_TAP_MAX_FRAMES.saturating_mul(channels);
        // A period larger than `PITCH_TAP_MAX_FRAMES` is not a real device —
        // drop the surplus rather than let `Vec::push` grow on the RT thread.
        let data = if input.len() > max_samples {
            &input[..max_samples]
        } else {
            input
        };

        self.dec_buf.clear();
        self.decimator.process(data, channels, &mut self.dec_buf);
        let len = self.dec_buf.len();
        if len == 0 {
            return;
        }

        // Both rings must have room, checked BEFORE anything is written: a
        // chunk written without its descriptor (or the reverse) would put the
        // two rings permanently out of step. Dropping a whole chunk costs the
        // trail a few pixels; desynchronising them would corrupt every frame
        // that follows.
        if self.chunks_tx.slots() == 0 || self.samples_tx.slots() < len {
            return;
        }
        let Ok(slot) = self.samples_tx.write_chunk_uninit(len) else {
            return;
        };
        slot.fill_from_iter(self.dec_buf.iter().copied());
        let _ = self.chunks_tx.push(PitchChunk {
            start_sample,
            device_rate: self.device_rate,
            len: len as u32,
            reset: self.pending_reset,
        });
        self.pending_reset = false;
    }
}

/// The non-RT half: owns the detector and turns chunks into frames.
pub struct PitchWorker {
    analyzer: PitchAnalyzer,
    samples_rx: rtrb::Consumer<f32>,
    chunks_rx: rtrb::Consumer<PitchChunk>,
    frames_tx: rtrb::Producer<PitchFrame>,
    /// Reused so a steady-state pump allocates nothing. Not an RT
    /// requirement here — just no reason to churn.
    scratch: Vec<f32>,
    frames: Vec<PitchFrame>,
}

impl PitchWorker {
    /// Analyse every chunk currently in the ring. Returns how many were
    /// analysed, so a caller can distinguish "there was work" from "idle".
    pub fn pump(&mut self) -> usize {
        let mut done = 0;
        while let Ok(chunk) = self.chunks_rx.pop() {
            let len = chunk.len as usize;
            let Ok(read) = self.samples_rx.read_chunk(len) else {
                // Cannot happen: samples are written before the descriptor
                // that describes them. Stop rather than analyse a short read
                // as if it were a whole chunk.
                break;
            };
            self.scratch.clear();
            let (a, b) = read.as_slices();
            self.scratch.extend_from_slice(a);
            self.scratch.extend_from_slice(b);
            read.commit_all();

            if chunk.reset {
                self.analyzer.reset();
            }
            self.frames.clear();
            self.analyzer.push(
                &self.scratch,
                chunk.start_sample,
                chunk.device_rate,
                &mut self.frames,
            );
            // A full frame ring just drops frames: the UI is a 100 Hz trail,
            // and a dropped frame is a pixel, not a corrupted take.
            for f in self.frames.iter() {
                let _ = self.frames_tx.push(*f);
            }
            done += 1;
        }
        done
    }
}

/// A running [`PitchWorker`] thread. Dropping this stops and joins it, which
/// is why an [`InputBundle`](super::engine) can own one and forget about it.
pub struct PitchWorkerHandle {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for PitchWorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Run a worker on its own thread until the returned handle is dropped.
pub fn spawn_pitch_worker(mut worker: PitchWorker) -> PitchWorkerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let join = std::thread::Builder::new()
        .name("aura-pitch".into())
        .spawn(move || {
            while !flag.load(Relaxed) {
                if worker.pump() == 0 {
                    std::thread::sleep(POLL);
                }
            }
        })
        .ok();
    PitchWorkerHandle { stop, join }
}

/// Build both halves of the chain plus the frame consumer the control thread
/// drains. `active` is shared with the engine: the tap produces only while it
/// is set.
pub fn pitch_channel(
    device_rate: u32,
    active: Arc<AtomicBool>,
) -> (PitchTap, PitchWorker, rtrb::Consumer<PitchFrame>) {
    let (samples_tx, samples_rx) = rtrb::RingBuffer::new(SAMPLE_RING_SLOTS);
    let (chunks_tx, chunks_rx) = rtrb::RingBuffer::new(CHUNK_RING_SLOTS);
    let (frames_tx, frames_rx) = rtrb::RingBuffer::new(FRAME_RING_SLOTS);
    let tap = PitchTap {
        decimator: Decimator::new(device_rate),
        // 8× covers a device as slow as 1 kHz upsampling to 8 kHz
        // (`Decimator` clamps below that). +8 is phase-accumulator slack so
        // `process` never grows this on the RT thread.
        dec_buf: Vec::with_capacity(PITCH_TAP_MAX_FRAMES * 8 + 8),
        samples_tx,
        chunks_tx,
        active,
        was_active: false,
        pending_reset: false,
        device_rate,
    };
    let worker = PitchWorker {
        analyzer: PitchAnalyzer::new(),
        samples_rx,
        chunks_rx,
        frames_tx,
        scratch: Vec::with_capacity(SAMPLE_RING_SLOTS),
        frames: Vec::with_capacity(256),
    };
    (tap, worker, frames_rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(hz: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / RATE as f32).sin())
            .collect()
    }

    fn chain(
        on: bool,
    ) -> (
        PitchTap,
        PitchWorker,
        rtrb::Consumer<PitchFrame>,
        Arc<AtomicBool>,
    ) {
        let active = Arc::new(AtomicBool::new(on));
        let (tap, worker, frames) = pitch_channel(RATE, active.clone());
        (tap, worker, frames, active)
    }

    /// Feed `secs` seconds of `hz` through the tap in 512-frame buffers,
    /// starting at device position `from`. Returns the position after it.
    fn feed(tap: &mut PitchTap, hz: f32, n: usize, from: u64) -> u64 {
        let mut pos = from;
        for chunk in tone(hz, n).chunks(512) {
            tap.process(chunk, 1, pos);
            pos += chunk.len() as u64;
        }
        pos
    }

    fn drain(rx: &mut rtrb::Consumer<PitchFrame>) -> Vec<PitchFrame> {
        let mut out = Vec::new();
        while let Ok(f) = rx.pop() {
            out.push(f);
        }
        out
    }

    /// The point of the whole module (spec §3.2): the capture callback hands
    /// samples over and nothing more. No frame exists until the worker runs.
    #[test]
    fn the_capture_side_produces_no_frames_by_itself() {
        let (mut tap, mut worker, mut frames, _active) = chain(true);
        feed(&mut tap, 220.0, RATE as usize / 2, 0);
        assert!(
            drain(&mut frames).is_empty(),
            "detection must not happen on the capture thread"
        );

        assert!(worker.pump() > 0, "the worker had chunks to analyse");
        assert!(
            !drain(&mut frames).is_empty(),
            "the worker is what produces frames"
        );
    }

    /// End to end through both rings: what was sung is what comes out.
    #[test]
    fn frames_report_the_sung_note() {
        let (mut tap, mut worker, mut frames, _active) = chain(true);
        feed(&mut tap, 220.0, RATE as usize, 0);
        worker.pump();

        let voiced: Vec<_> = drain(&mut frames)
            .into_iter()
            .filter(|f| f.voiced)
            .collect();
        assert!(!voiced.is_empty(), "a steady 220 Hz tone must be voiced");
        for f in &voiced {
            assert!((f.midi - 57.0).abs() < 0.5, "A3 is MIDI 57, got {}", f.midi);
        }
    }

    /// A dormant tap on a take's capture stream costs one atomic load: it
    /// must not decimate, must not fill the rings, and must produce nothing.
    #[test]
    fn an_inactive_tap_hands_over_nothing() {
        let (mut tap, mut worker, mut frames, _active) = chain(false);
        feed(&mut tap, 220.0, RATE as usize / 2, 0);
        assert_eq!(worker.pump(), 0, "an inactive tap must queue no chunks");
        assert!(drain(&mut frames).is_empty());
    }

    /// Issue 7's fix at this level: flipping the flag mid-stream starts the
    /// analysis without rebuilding anything.
    #[test]
    fn activating_mid_stream_starts_producing_frames() {
        let (mut tap, mut worker, mut frames, active) = chain(false);
        let pos = feed(&mut tap, 220.0, RATE as usize / 2, 0);
        assert_eq!(worker.pump(), 0);

        active.store(true, Relaxed);
        feed(&mut tap, 220.0, RATE as usize / 2, pos);
        worker.pump();
        assert!(
            drain(&mut frames).iter().any(|f| f.voiced),
            "listen turned on mid-take must produce frames"
        );
    }

    /// Deactivating stops the stream at the tap, not merely at the UI.
    #[test]
    fn deactivating_stops_the_frames() {
        let (mut tap, mut worker, mut frames, active) = chain(true);
        let pos = feed(&mut tap, 220.0, RATE as usize / 2, 0);
        worker.pump();
        assert!(!drain(&mut frames).is_empty(), "was producing");

        active.store(false, Relaxed);
        feed(&mut tap, 220.0, RATE as usize / 2, pos);
        assert_eq!(worker.pump(), 0);
        assert!(drain(&mut frames).is_empty());
    }

    /// The gap between two listens is not audio: the capture callback kept
    /// running, the tap just threw it away. Frames after a re-listen must
    /// therefore be timestamped from the position handed in, with none of the
    /// detector's history from before the gap spliced onto the front of them.
    #[test]
    fn a_relisten_is_timestamped_from_the_new_position() {
        let (mut tap, mut worker, mut frames, active) = chain(true);
        let pos = feed(&mut tap, 220.0, RATE as usize / 2, 0);
        worker.pump();
        drain(&mut frames);

        // Dormant, but the microphone is still delivering buffers.
        active.store(false, Relaxed);
        feed(&mut tap, 220.0, RATE as usize / 2, pos);
        // A minute of transport went by while the tap was off.
        let resume = 60 * RATE as u64;
        active.store(true, Relaxed);
        feed(&mut tap, 220.0, RATE as usize / 2, resume);
        worker.pump();

        let got = drain(&mut frames);
        assert!(!got.is_empty(), "frames must resume");
        for f in &got {
            assert!(
                f.sample >= resume && f.sample < resume + RATE as u64,
                "frame at {} is outside the second that was actually sung \
                 (resumed at {resume})",
                f.sample
            );
        }
    }

    /// A worker that falls far behind drops whole chunks. What must never
    /// happen is a chunk analysed against another chunk's samples — that
    /// would mistime every frame after it, silently and forever.
    #[test]
    fn a_backed_up_ring_drops_whole_chunks_and_stays_aligned() {
        let (mut tap, mut worker, mut frames, _active) = chain(true);
        // Far more than SAMPLE_RING_SLOTS holds, with nothing pumping.
        let end = feed(&mut tap, 220.0, RATE as usize * 4, 0);
        worker.pump();

        let got = drain(&mut frames);
        assert!(!got.is_empty(), "the chunks that did fit are analysed");
        let mut prev = 0u64;
        for f in &got {
            assert!(f.hz.is_finite() && f.midi.is_finite());
            assert!(
                f.sample >= prev,
                "timestamps went backwards at {}",
                f.sample
            );
            assert!(
                f.sample <= end,
                "frame at {} is past everything fed",
                f.sample
            );
            prev = f.sample;
        }
    }

    /// The sample ring has to hold one MAXIMAL chunk. If it cannot, a device
    /// whose rate the decimator upsamples from never fits — and the chain
    /// does not degrade, it goes permanently dark on that device.
    #[test]
    fn a_maximal_chunk_always_fits() {
        let active = Arc::new(AtomicBool::new(true));
        // The worst case the tap admits: a full `PITCH_TAP_MAX_FRAMES`
        // buffer from a device slow enough that 8 kHz is an UPsample.
        let slow = 4_000;
        let (mut tap, mut worker, _frames) = pitch_channel(slow, active);
        tap.process(&vec![0.5f32; PITCH_TAP_MAX_FRAMES], 1, 0);
        assert_eq!(
            worker.pump(),
            1,
            "a chunk the tap is willing to produce must fit in the ring"
        );
    }

    /// The thread is the only part `pump` tests cannot cover: it must pick
    /// work up on its own and stop when the handle is dropped.
    #[test]
    fn the_spawned_worker_analyses_without_being_pumped() {
        let (mut tap, worker, mut frames, _active) = chain(true);
        let handle = spawn_pitch_worker(worker);
        feed(&mut tap, 220.0, RATE as usize / 2, 0);

        let mut got = Vec::new();
        for _ in 0..100 {
            got.extend(drain(&mut frames));
            if got.iter().any(|f| f.voiced) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            got.iter().any(|f| f.voiced),
            "the worker thread must analyse what the tap hands it"
        );
        drop(handle); // joins; a hang here is the test failing
    }
}
