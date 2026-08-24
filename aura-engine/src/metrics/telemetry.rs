//! Per-block latency and xrun telemetry.
//!
//! The engine already counts one class of xrun (`SharedRt::xruns`, bumped
//! when a meter chunk cannot be pushed). That is a *ring* overflow. This
//! counts the other, more important one: **the callback took longer than the
//! audio it produced**, which is what the user hears as a click.
//!
//! The deadline is the block's own duration, `frames / sample_rate`. A block
//! of 512 frames at 48 kHz has 10.67 ms to be rendered in; spend more and the
//! device has already run dry. Recording the ratio rather than the raw
//! duration is what makes numbers comparable across buffer sizes — 4 ms is
//! fine for a 512-frame block and a hard overrun for a 128-frame one.
//!
//! Nothing here allocates or locks, so `Block::start`/`Block::finish` are
//! callable from the callback. It is deliberately NOT atomic-shared: one
//! instance lives in the callback's own state, and its `Snapshot` is what
//! crosses to the UI (through, for instance, [`crate::sync::triple_buffer`]).

use std::time::{Duration, Instant};

/// Rolling per-block telemetry, owned by the audio callback.
#[derive(Clone, Copy, Debug)]
pub struct Telemetry {
    sample_rate: u32,
    blocks: u64,
    xruns: u64,
    total: Duration,
    worst: Duration,
    worst_load: f32,
    last_load: f32,
}

/// An in-flight block measurement. `finish` is what folds it in; dropping it
/// without finishing records nothing, so an early `return` out of the
/// callback cannot corrupt the statistics.
#[must_use = "a started block that is never finished records nothing"]
pub struct Block {
    started: Instant,
    frames: u32,
}

/// Telemetry as it crosses to a non-RT reader. POD and `Copy` — publishable
/// through a triple buffer with no allocation on either side.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub blocks: u64,
    /// Blocks whose render took longer than the audio they produced.
    pub xruns: u64,
    /// Fraction of the deadline used by the most recent block. 1.0 = exactly
    /// on time; > 1.0 = late.
    pub last_load: f32,
    /// The worst such fraction seen since the last reset.
    pub worst_load: f32,
    pub mean_us: f32,
    pub worst_us: f32,
}

impl Telemetry {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate: sample_rate.max(1),
            blocks: 0,
            xruns: 0,
            total: Duration::ZERO,
            worst: Duration::ZERO,
            worst_load: 0.0,
            last_load: 0.0,
        }
    }

    /// The device's sample rate changed (stream reopened); the deadline moved
    /// with it, so previous loads are no longer comparable.
    pub fn retune(&mut self, sample_rate: u32) {
        *self = Self::new(sample_rate);
    }

    /// Begin timing a block of `frames` frames.
    #[inline]
    pub fn start(&self, frames: u32) -> Block {
        Block { started: Instant::now(), frames }
    }

    /// Fold a finished block in. Returns true if it was late (an xrun).
    #[inline]
    pub fn finish(&mut self, block: Block) -> bool {
        let elapsed = block.started.elapsed();
        let deadline = self.deadline(block.frames);
        self.blocks += 1;
        self.total += elapsed;
        if elapsed > self.worst {
            self.worst = elapsed;
        }
        let load = if deadline.is_zero() {
            0.0
        } else {
            elapsed.as_secs_f32() / deadline.as_secs_f32()
        };
        self.last_load = load;
        if load > self.worst_load {
            self.worst_load = load;
        }
        let late = elapsed > deadline;
        if late {
            self.xruns += 1;
        }
        late
    }

    /// How long a block of `frames` frames has to be rendered in.
    #[inline]
    pub fn deadline(&self, frames: u32) -> Duration {
        Duration::from_secs_f64(f64::from(frames) / f64::from(self.sample_rate))
    }

    pub fn snapshot(&self) -> Snapshot {
        let blocks = self.blocks.max(1) as f64;
        Snapshot {
            blocks: self.blocks,
            xruns: self.xruns,
            last_load: self.last_load,
            worst_load: self.worst_load,
            mean_us: (self.total.as_secs_f64() * 1e6 / blocks) as f32,
            worst_us: (self.worst.as_secs_f64() * 1e6) as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_is_the_blocks_own_duration() {
        let t = Telemetry::new(48_000);
        // 512 frames at 48 kHz = 10.666 ms.
        let d = t.deadline(512);
        assert!((d.as_secs_f64() - 512.0 / 48_000.0).abs() < 1e-9, "{d:?}");
    }

    #[test]
    fn a_block_inside_its_deadline_is_not_an_xrun() {
        let mut t = Telemetry::new(48_000);
        let b = t.start(4096); // 85 ms of headroom — no sleep needed.
        assert!(!t.finish(b));
        let s = t.snapshot();
        assert_eq!((s.blocks, s.xruns), (1, 0));
        assert!(s.last_load < 1.0, "load {}", s.last_load);
    }

    #[test]
    fn a_late_block_is_counted_and_the_load_exceeds_one() {
        // One frame at 48 kHz is 20 µs; the sleep guarantees the overrun
        // without depending on how fast this machine is.
        let mut t = Telemetry::new(48_000);
        let b = t.start(1);
        std::thread::sleep(Duration::from_millis(2));
        assert!(t.finish(b));
        let s = t.snapshot();
        assert_eq!((s.blocks, s.xruns), (1, 1));
        assert!(s.worst_load > 1.0, "load {}", s.worst_load);
    }

    #[test]
    fn measuring_a_block_does_not_allocate() {
        // Registered by tests/no_alloc.rs; vacuous here, asserted there.
        let mut t = Telemetry::new(48_000);
        let (_, stats) = crate::metrics::measure(|| {
            let b = t.start(512);
            t.finish(b)
        });
        assert_eq!(stats.allocs, 0);
    }
}
