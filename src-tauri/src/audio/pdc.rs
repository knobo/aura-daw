//! Plugin-delay compensation (PDC) — Plan G1 Task 6.
//!
//! Insert plugins can report processing latency (`AudioProcessor::latency_samples`).
//! A track whose insert chain is heavier than a sibling's arrives at the
//! master EARLY relative to it unless the shorter path is delayed by the
//! difference. [`compile_pdc`] computes that per-track delay from each
//! track's total path latency; [`DelayLine`] is the RT primitive that
//! applies it on the mixer strip (`mixer::process_inserts`, after inserts,
//! before the fader — see the hook Task 5 left on `RtTrack::pdc`).
//!
//! This module only computes numbers and offers the primitive. Wiring
//! `compile_pdc`'s output into an actual graph rebuild (attaching
//! `DelayLine`s to `RtTrack::pdc`) is Task 7 — out of scope here.
//!
//! KNOWN GAP FOR TASK 7: `mixer::process_inserts` applies `RtTrack::pdc`
//! AFTER inserts, shifting the audio buffer `delay` samples into the past —
//! but the fader/pan automation ramps applied right after in `apply_fader`
//! are still keyed to the un-shifted "now". Once a real `DelayLine` is
//! attached, a track with nonzero PDC will read automation values for the
//! wrong playhead position (off by `delay` samples), a timing error that
//! grows with plugin latency. Not reachable today — no production `RtTrack`
//! sets `pdc` to anything but `None` — but Task 7 needs to either offset
//! the ramp lookup by the track's own PDC delay or accept the drift as a
//! documented divergence.

/// A fixed-length delay: shifts an interleaved stream by exactly `delay`
/// samples. RT-safe — `buf` is sized once in [`DelayLine::new`] and never
/// grows; `process` does no allocation, no locking, no tick math.
pub struct DelayLine {
    /// Ring buffer, `(delay + max_block) * channels` samples, sized once.
    buf: Vec<f32>,
    /// Next frame index to write (ring position, in FRAMES not samples).
    w: usize,
    delay: usize,
    channels: usize,
}

impl DelayLine {
    /// `delay_samples` is the shift in frames (not interleaved samples).
    /// The buffer is sized to hold `delay_samples + max_block` frames so a
    /// full block's worth of writes never laps the still-unread tail.
    pub fn new(delay_samples: usize, max_block: usize, channels: usize) -> Self {
        let channels = channels.max(1);
        let buf_frames = delay_samples + max_block;
        Self {
            buf: vec![0.0; buf_frames * channels],
            w: 0,
            delay: delay_samples,
            channels,
        }
    }

    pub fn delay(&self) -> usize {
        self.delay
    }

    /// Sized at `new()`, never grows — RT-safety pin (G-2).
    pub fn buf_len(&self) -> usize {
        self.buf.len()
    }

    /// RT-safe. `io` is interleaved `frames * channels`. Does not allocate.
    /// A zero delay is a no-op (nothing to shift, nothing to preroll).
    ///
    /// Debug-only shape asserts (never on the release RT path): `io` must be
    /// a whole number of frames at `self.channels`, and the block must not
    /// exceed the `max_block` this line was sized for at `new()` — either
    /// violation is a caller bug (a stray/misconverted sample count, or a
    /// run larger than the graph's `MAX_LIVE_BLOCK`), not something this
    /// method should silently paper over by dropping the tail or collapsing
    /// toward passthrough.
    pub fn process(&mut self, io: &mut [f32]) {
        debug_assert!(
            io.len() % self.channels == 0,
            "DelayLine::process: io.len()={} is not a whole number of frames at {} channels",
            io.len(),
            self.channels
        );
        if self.delay == 0 {
            return;
        }
        let buf_frames = self.buf.len() / self.channels;
        if buf_frames == 0 {
            return;
        }
        let frames = io.len() / self.channels;
        debug_assert!(
            frames + self.delay <= buf_frames,
            "DelayLine::process: block of {frames} frames + delay {} exceeds the {buf_frames}-frame \
             buffer sized at new() for max_block={}",
            self.delay,
            buf_frames - self.delay
        );
        for i in 0..frames {
            // Equivalent to `(self.w + buf_frames - self.delay) % buf_frames`
            // without a division on the RT hot path — `self.w < buf_frames`
            // and `self.delay <= buf_frames` (sized at `new()`) keep both
            // branches in range.
            let read_frame = if self.w >= self.delay {
                self.w - self.delay
            } else {
                self.w + buf_frames - self.delay
            };
            let write_base = self.w * self.channels;
            let read_base = read_frame * self.channels;
            let io_base = i * self.channels;
            for c in 0..self.channels {
                let incoming = io[io_base + c];
                io[io_base + c] = self.buf[read_base + c];
                self.buf[write_base + c] = incoming;
            }
            self.w += 1;
            if self.w == buf_frames {
                self.w = 0;
            }
        }
    }
}

/// A track's total path latency: its instrument/source latency plus every
/// insert's reported latency, in document order (sum — inserts run in
/// series, each adding its own).
pub fn track_latency(instrument: usize, insert_latencies: &[usize]) -> usize {
    instrument + insert_latencies.iter().sum::<usize>()
}

/// Per-track compensating delay: every track is padded up to the slowest
/// DECLARED path (`max` of `declared`), then topped up by what its OWN
/// signal is not already delayed by (`applied`).
///
/// `declared[i]` is `track_latency` counting every insert regardless of
/// bypass — it sets the alignment target so a bypassed insert elsewhere
/// still holds the target where it would be if active. `applied[i]` is
/// `track_latency` counting only NON-bypassed inserts — the delay actually
/// present in that track's signal today, since `mixer::process_inserts`
/// skips `process()` (and therefore the delay) on a bypassed insert. The
/// two differ exactly on a track with a bypassed insert: its declared
/// latency still counts toward `max`, but none of it is real, so the
/// DelayLine must supply the WHOLE gap itself (G-5 — toggling bypass must
/// not jump the mix: the track waits the same amount whether the plugin is
/// running or skipped). `declared` and `applied` are paired by index, one
/// entry per track; empty input yields empty output.
pub fn compile_pdc(declared: &[usize], applied: &[usize]) -> Vec<usize> {
    debug_assert_eq!(declared.len(), applied.len(), "compile_pdc: declared/applied must pair 1:1 per track");
    let max = declared.iter().copied().max().unwrap_or(0);
    applied.iter().map(|&a| max.saturating_sub(a)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_pdc_delays_the_shorter_paths() {
        assert_eq!(compile_pdc(&[256, 0, 64], &[256, 0, 64]), vec![0, 256, 192]);
        assert_eq!(compile_pdc(&[0, 0], &[0, 0]), vec![0, 0]);
        assert_eq!(compile_pdc(&[], &[]), Vec::<usize>::new());
    }

    /// G-5: a bypassed insert's declared latency still sets the alignment
    /// target, but its DelayLine must supply the WHOLE gap — the plugin
    /// isn't actually delaying anything (`process()` is skipped), so
    /// `applied` is 0 for that track even though `declared` is 256. Mirrors
    /// `mixer::tests::bypassed_insert_still_contributes_latency`, which
    /// exercises the same numbers end-to-end through a real render.
    #[test]
    fn compile_pdc_gives_a_bypassed_insert_s_full_latency_as_pdc() {
        assert_eq!(compile_pdc(&[256, 0], &[0, 0]), vec![256, 256]);
    }

    #[test]
    fn track_latency_sums_instrument_and_inserts() {
        assert_eq!(track_latency(10, &[20, 30]), 60);
        assert_eq!(track_latency(0, &[]), 0);
    }

    #[test]
    fn delay_line_shifts_an_impulse_by_exactly_n_and_does_not_allocate() {
        let mut d = DelayLine::new(4, 8, 2);
        let cap = d.buf_len();
        let mut block = vec![1.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]; // 4 frames stereo
        d.process(&mut block);
        assert_eq!(block, vec![0.0; 8], "first block is the pre-roll of silence");
        let mut block2 = vec![0.0f32; 8];
        d.process(&mut block2);
        assert!((block2[0] - 1.0).abs() < 1e-9 && (block2[1] - 1.0).abs() < 1e-9);
        assert_eq!(d.buf_len(), cap, "G-2: process must not grow the buffer");
    }
}
