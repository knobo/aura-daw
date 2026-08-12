//! Meter data flow: fixed-size POD blocks pushed from the RT callbacks
//! through an rtrb SPSC queue, folded control-side into 60 Hz `MeterFrame`s.

use super::types::{MeterFrame, TrackMeter, MAX_TRACKS};

/// Linear level at/above which a channel is reported as clipped.
pub const CLIP_THRESHOLD: f32 = 0.999;

/// One per-buffer meter block, POD, fixed size (~2 KiB) so pushing it through
/// rtrb is a memcpy + two atomic ops. `mask` bit N marks slot N as present.
#[derive(Clone, Copy)]
pub struct RawMeterBlock {
    pub position: u64,
    pub frames: u32,
    pub mask: u64,
    /// [slot][channel] max(|sample|) over the buffer.
    pub peak: [[f32; 2]; MAX_TRACKS],
    /// [slot][channel] sum of squared samples over the buffer.
    pub sumsq: [[f32; 2]; MAX_TRACKS],
    pub master_peak: [f32; 2],
    pub master_sumsq: [f32; 2],
}

impl RawMeterBlock {
    pub fn new(position: u64, frames: u32) -> Self {
        Self {
            position,
            frames,
            mask: 0,
            peak: [[0.0; 2]; MAX_TRACKS],
            sumsq: [[0.0; 2]; MAX_TRACKS],
            master_peak: [0.0; 2],
            master_sumsq: [0.0; 2],
        }
    }

    #[inline]
    pub fn set_slot(&mut self, slot: usize, peak_l: f32, peak_r: f32, ss_l: f32, ss_r: f32) {
        if slot < MAX_TRACKS {
            self.mask |= 1 << slot;
            self.peak[slot] = [peak_l, peak_r];
            self.sumsq[slot] = [ss_l, ss_r];
        }
    }
}

/// Control-side aggregator: folds every block that arrived since the last UI
/// frame (peak = max, RMS = sqrt(sum_sq / frames)).
pub struct MeterAccum {
    mask: u64,
    peak: [[f32; 2]; MAX_TRACKS],
    sumsq: [[f32; 2]; MAX_TRACKS],
    master_peak: [f32; 2],
    master_sumsq: [f32; 2],
    frames: u64,
    position: u64,
}

impl Default for MeterAccum {
    fn default() -> Self {
        Self {
            mask: 0,
            peak: [[0.0; 2]; MAX_TRACKS],
            sumsq: [[0.0; 2]; MAX_TRACKS],
            master_peak: [0.0; 2],
            master_sumsq: [0.0; 2],
            frames: 0,
            position: 0,
        }
    }
}

impl MeterAccum {
    pub fn fold(&mut self, b: &RawMeterBlock) {
        self.mask |= b.mask;
        self.frames += b.frames as u64;
        self.position = b.position;
        for slot in 0..MAX_TRACKS {
            if b.mask & (1 << slot) != 0 {
                for c in 0..2 {
                    self.peak[slot][c] = self.peak[slot][c].max(b.peak[slot][c]);
                    self.sumsq[slot][c] += b.sumsq[slot][c];
                }
            }
        }
        for c in 0..2 {
            self.master_peak[c] = self.master_peak[c].max(b.master_peak[c]);
            self.master_sumsq[c] += b.master_sumsq[c];
        }
    }

    pub fn is_empty(&self) -> bool {
        self.frames == 0
    }

    #[inline]
    fn rms(&self, sumsq: f32) -> f32 {
        if self.frames == 0 {
            0.0
        } else {
            (sumsq / self.frames as f32).sqrt()
        }
    }

    fn slot_meter(&self, slot: usize, track_id: &str) -> TrackMeter {
        let present = slot < MAX_TRACKS && self.mask & (1 << slot) != 0;
        if !present {
            return TrackMeter { track_id: track_id.to_string(), ..Default::default() };
        }
        let [pl, pr] = self.peak[slot];
        TrackMeter {
            track_id: track_id.to_string(),
            peak_l: pl,
            peak_r: pr,
            rms_l: self.rms(self.sumsq[slot][0]),
            rms_r: self.rms(self.sumsq[slot][1]),
            clipped: pl >= CLIP_THRESHOLD || pr >= CLIP_THRESHOLD,
        }
    }

    /// Build one UI frame from everything folded so far, then reset.
    /// `tracks` is (slot, track_id) in display order; `position` falls back to
    /// the supplied playhead when no audio blocks arrived (idle).
    pub fn take_frame(&mut self, seq: u64, tracks: &[(usize, String)], idle_position: u64) -> MeterFrame {
        let position = if self.is_empty() { idle_position } else { self.position };
        let frame = MeterFrame {
            seq,
            position_samples: position,
            tracks: tracks.iter().map(|(slot, id)| self.slot_meter(*slot, id)).collect(),
            master: TrackMeter {
                track_id: "master".into(),
                peak_l: self.master_peak[0],
                peak_r: self.master_peak[1],
                rms_l: self.rms(self.master_sumsq[0]),
                rms_r: self.rms(self.master_sumsq[1]),
                clipped: self.master_peak[0] >= CLIP_THRESHOLD
                    || self.master_peak[1] >= CLIP_THRESHOLD,
            },
        };
        *self = MeterAccum::default();
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(pos: u64, frames: u32, slot: usize, peak: f32, sumsq: f32) -> RawMeterBlock {
        let mut b = RawMeterBlock::new(pos, frames);
        b.set_slot(slot, peak, peak / 2.0, sumsq, sumsq / 4.0);
        b.master_peak = [peak, peak];
        b.master_sumsq = [sumsq, sumsq];
        b
    }

    #[test]
    fn fold_takes_max_peak_and_sums_energy() {
        let mut acc = MeterAccum::default();
        acc.fold(&block(0, 100, 3, 0.5, 10.0));
        acc.fold(&block(100, 100, 3, 0.8, 6.0));
        let f = acc.take_frame(7, &[(3, "t3".into())], 0);
        assert_eq!(f.seq, 7);
        assert_eq!(f.position_samples, 100); // last block position
        let m = &f.tracks[0];
        assert_eq!(m.track_id, "t3");
        assert!((m.peak_l - 0.8).abs() < 1e-6);
        assert!((m.peak_r - 0.4).abs() < 1e-6);
        // rms over 200 frames: sqrt(16 / 200)
        assert!((m.rms_l - (16.0f32 / 200.0).sqrt()).abs() < 1e-6);
        assert!(!m.clipped);
        // reset after take
        assert!(acc.is_empty());
    }

    #[test]
    fn absent_slots_report_silence() {
        let mut acc = MeterAccum::default();
        acc.fold(&block(0, 128, 0, 0.9, 1.0));
        let f = acc.take_frame(0, &[(0, "a".into()), (5, "b".into())], 0);
        assert!(f.tracks[1].peak_l == 0.0 && f.tracks[1].rms_r == 0.0);
        assert!(!f.tracks[1].clipped);
    }

    #[test]
    fn clipping_is_flagged_at_full_scale() {
        let mut acc = MeterAccum::default();
        acc.fold(&block(0, 10, 1, 1.0, 10.0));
        let f = acc.take_frame(0, &[(1, "hot".into())], 0);
        assert!(f.tracks[0].clipped);
        assert!(f.master.clipped);
    }

    #[test]
    fn idle_frame_uses_fallback_position() {
        let mut acc = MeterAccum::default();
        let f = acc.take_frame(1, &[(0, "a".into())], 4242);
        assert_eq!(f.position_samples, 4242);
        assert_eq!(f.master.peak_l, 0.0);
    }
}
