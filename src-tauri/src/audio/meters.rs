//! Meter data flow: fixed-size POD blocks pushed from the RT callbacks
//! through an rtrb SPSC queue, folded control-side into 60 Hz `MeterFrame`s.

use std::collections::{BTreeMap, HashMap};

use super::types::{MeterFrame, TrackMeter};
use crate::ids::TrackId;

/// Linear level at/above which a channel is reported as clipped.
pub const CLIP_THRESHOLD: f32 = 0.999;

/// Slots covered by one meter chunk — Task 7: replaces the old
/// `MAX_TRACKS`-wide single block. A graph wider than this emits several
/// chunks per callback (`⌈slots / METER_CHUNK_SLOTS⌉`), still fixed-size POD
/// through rtrb; per-graph sizing (`ParamTable::with_slots`) has no cap, so
/// the meter path can't have one either.
pub const METER_CHUNK_SLOTS: usize = 64;

/// One per-buffer meter CHUNK, POD, fixed size (~2 KiB) so pushing it
/// through rtrb is a memcpy + two atomic ops. `mask` bit N marks LOCAL lane
/// N (i.e. slot `base_slot + N`) as present within this chunk.
#[derive(Clone, Copy)]
pub struct RawMeterBlock {
    /// The `RtGraph` generation this chunk's slots were resolved against
    /// (round-2 §2.4 / Task 6) — the fold must resolve `(generation, slot)`
    /// under the SAME slot map that produced the chunk, never the current
    /// one, or a per-rebuild renumbering shows one track's level on another.
    pub generation: u64,
    pub position: u64,
    pub frames: u32,
    /// First slot this chunk covers; lane i = slot `base_slot + i`.
    pub base_slot: u32,
    /// Presence within THIS chunk (lane-relative, not slot-relative).
    pub mask: u64,
    /// [lane][channel] max(|sample|) over the buffer.
    pub peak: [[f32; 2]; METER_CHUNK_SLOTS],
    /// [lane][channel] sum of squared samples over the buffer.
    pub sumsq: [[f32; 2]; METER_CHUNK_SLOTS],
    /// Master bus meters — carried ONLY on the chunk with `base_slot == 0`;
    /// the fold reads master from there and ignores it on other chunks.
    pub master_peak: [f32; 2],
    pub master_sumsq: [f32; 2],
}

impl RawMeterBlock {
    pub fn new(generation: u64, position: u64, frames: u32) -> Self {
        Self {
            generation,
            position,
            frames,
            base_slot: 0,
            mask: 0,
            peak: [[0.0; 2]; METER_CHUNK_SLOTS],
            sumsq: [[0.0; 2]; METER_CHUNK_SLOTS],
            master_peak: [0.0; 2],
            master_sumsq: [0.0; 2],
        }
    }

    /// Set LANE `lane` (local to this chunk — the global slot is
    /// `base_slot + lane`) to the given peak/sum-of-squares.
    #[inline]
    pub fn set_slot_local(&mut self, lane: usize, peak_l: f32, peak_r: f32, ss_l: f32, ss_r: f32) {
        if lane < METER_CHUNK_SLOTS {
            self.mask |= 1 << lane;
            self.peak[lane] = [peak_l, peak_r];
            self.sumsq[lane] = [ss_l, ss_r];
        }
    }
}

/// generation -> (slot -> track id), kept for the adoption window.
///
/// Entries are pruned to the last [`GenerationMaps::KEPT_GENERATIONS`] on
/// insert — a block older than that is dropped by the fold (stale beyond the
/// window). `KEPT_GENERATIONS = 4` deliberately tolerates a command-burst
/// publishing several generations before the meter ring drains (e.g. several
/// structural commits land back-to-back — each schedules its own `rebuild`,
/// and the RT callback keeps pushing blocks stamped with whichever
/// generation was current when it rendered): the result is at most one blank
/// meter frame while the window catches up — self-healing on the very next
/// frame — do not "fix" it down [design-attack M3].
///
/// PINNING [design-attack I2]: `pin(generation)` exempts a generation from
/// pruning; `unpin()` releases it. `start_recording` pins the generation its
/// `InputCb` slots were resolved against, `stop_recording` unpins —
/// otherwise a take spanning more than `KEPT_GENERATIONS` rebuilds (e.g.
/// dropping four clips mid-take) would lose its input meters for the rest of
/// the recording once the pinned generation aged out of the plain window.
#[derive(Default)]
pub struct GenerationMaps {
    maps: BTreeMap<u64, HashMap<usize, TrackId>>,
    pinned: Option<u64>,
}

impl GenerationMaps {
    pub const KEPT_GENERATIONS: usize = 4;

    /// Publish a fresh generation's slot map, pruning the oldest
    /// non-pinned entries once more than `KEPT_GENERATIONS` (+1 for a
    /// pinned entry that has aged out of the plain window) are held.
    pub fn publish(&mut self, generation: u64, slots: &HashMap<TrackId, usize>) {
        let by_slot: HashMap<usize, TrackId> =
            slots.iter().map(|(id, &slot)| (slot, id.clone())).collect();
        self.maps.insert(generation, by_slot);
        let cap = Self::KEPT_GENERATIONS + if self.pinned.is_some() { 1 } else { 0 };
        while self.maps.len() > cap {
            let Some(&oldest_prunable) =
                self.maps.keys().find(|&&k| Some(k) != self.pinned)
            else {
                break; // everything left is pinned (can't happen: only one key can be)
            };
            self.maps.remove(&oldest_prunable);
        }
    }

    fn slot_map(&self, generation: u64) -> Option<&HashMap<usize, TrackId>> {
        self.maps.get(&generation)
    }

    pub fn pin(&mut self, generation: u64) {
        self.pinned = Some(generation);
    }

    pub fn unpin(&mut self) {
        self.pinned = None;
    }
}

#[derive(Default, Clone, Copy)]
struct Lanes {
    peak: [f32; 2],
    sumsq: [f32; 2],
}

/// Control-side aggregator: folds every block that arrived since the last UI
/// frame (peak = max, RMS = sqrt(sum_sq / frames)).
#[derive(Default)]
pub struct MeterAccum {
    lanes: HashMap<TrackId, Lanes>,
    master_peak: [f32; 2],
    master_sumsq: [f32; 2],
    frames: u64,
    position: u64,
}

impl MeterAccum {
    /// Fold a chunk into the accumulator, resolving each set lane under the
    /// slot map published for `b.generation` — NOT the current one. A chunk
    /// whose generation isn't in the window at all is dropped wholesale
    /// (stale beyond `GenerationMaps::KEPT_GENERATIONS`, or a pinned
    /// recording generation that was never published); a lane that IS
    /// covered by a known generation but has no track (a mid-rebuild
    /// mismatch) is skipped individually.
    ///
    /// Frame/position/master accounting [I3] happens ONLY on the chunk with
    /// `base_slot == 0` — one per callback per generation, regardless of how
    /// many chunks a wide graph emits. Counting every chunk would inflate
    /// the RMS denominator by the chunk count (a real bug this design
    /// avoids: meters would read low, silently, the wider the graph gets).
    pub fn fold(&mut self, b: &RawMeterBlock, maps: &GenerationMaps) {
        let Some(slot_map) = maps.slot_map(b.generation) else { return };
        if b.base_slot == 0 {
            self.frames += b.frames as u64;
            self.position = b.position;
            for c in 0..2 {
                self.master_peak[c] = self.master_peak[c].max(b.master_peak[c]);
                self.master_sumsq[c] += b.master_sumsq[c];
            }
        }
        for lane in 0..METER_CHUNK_SLOTS {
            if b.mask & (1 << lane) != 0 {
                let slot = b.base_slot as usize + lane;
                if let Some(track_id) = slot_map.get(&slot) {
                    let lanes = self.lanes.entry(track_id.clone()).or_default();
                    lanes.peak[0] = lanes.peak[0].max(b.peak[lane][0]);
                    lanes.peak[1] = lanes.peak[1].max(b.peak[lane][1]);
                    lanes.sumsq[0] += b.sumsq[lane][0];
                    lanes.sumsq[1] += b.sumsq[lane][1];
                }
            }
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

    fn track_meter(&self, id: &TrackId) -> TrackMeter {
        match self.lanes.get(id) {
            None => TrackMeter { track_id: id.to_string(), ..Default::default() },
            Some(l) => {
                let [pl, pr] = l.peak;
                TrackMeter {
                    track_id: id.to_string(),
                    peak_l: pl,
                    peak_r: pr,
                    rms_l: self.rms(l.sumsq[0]),
                    rms_r: self.rms(l.sumsq[1]),
                    clipped: pl >= CLIP_THRESHOLD || pr >= CLIP_THRESHOLD,
                }
            }
        }
    }

    /// Build one UI frame from everything folded so far, then reset.
    /// `order` is display-order track ids for the frame; `position` falls
    /// back to the supplied playhead when no audio blocks arrived (idle).
    pub fn take_frame(&mut self, seq: u64, order: &[TrackId], idle_position: u64) -> MeterFrame {
        let position = if self.is_empty() { idle_position } else { self.position };
        let frame = MeterFrame {
            seq,
            position_samples: position,
            tracks: order.iter().map(|id| self.track_meter(id)).collect(),
            master: TrackMeter {
                track_id: "master".into(),
                peak_l: self.master_peak[0],
                peak_r: self.master_peak[1],
                rms_l: self.rms(self.master_sumsq[0]),
                rms_r: self.rms(self.master_sumsq[1]),
                clipped: self.master_peak[0] >= CLIP_THRESHOLD
                    || self.master_peak[1] >= CLIP_THRESHOLD,
            },
            // The accumulator only knows about audio. The driven-param
            // read-back is the control thread's business, so `pump_meter_frames`
            // fills it in on the way out.
            driven_params: Vec::new(),
        };
        *self = MeterAccum::default();
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps_with(generation: u64, entries: &[(&str, usize)]) -> GenerationMaps {
        let mut maps = GenerationMaps::default();
        maps.publish(generation, &entries.iter().map(|&(id, s)| (id.into(), s)).collect());
        maps
    }

    fn block(gen: u64, pos: u64, frames: u32, slot: usize, peak: f32, sumsq: f32) -> RawMeterBlock {
        let mut b = RawMeterBlock::new(gen, pos, frames);
        b.set_slot_local(slot, peak, peak / 2.0, sumsq, sumsq / 4.0);
        b.master_peak = [peak, peak];
        b.master_sumsq = [sumsq, sumsq];
        b
    }

    #[test]
    fn fold_takes_max_peak_and_sums_energy() {
        let maps = maps_with(1, &[("t3", 3)]);
        let mut acc = MeterAccum::default();
        acc.fold(&block(1, 0, 100, 3, 0.5, 10.0), &maps);
        acc.fold(&block(1, 100, 100, 3, 0.8, 6.0), &maps);
        let f = acc.take_frame(7, &["t3".into()], 0);
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
        let maps = maps_with(1, &[("a", 0), ("b", 5)]);
        let mut acc = MeterAccum::default();
        acc.fold(&block(1, 0, 128, 0, 0.9, 1.0), &maps);
        let f = acc.take_frame(0, &["a".into(), "b".into()], 0);
        assert!(f.tracks[1].peak_l == 0.0 && f.tracks[1].rms_r == 0.0);
        assert!(!f.tracks[1].clipped);
    }

    #[test]
    fn clipping_is_flagged_at_full_scale() {
        let maps = maps_with(1, &[("hot", 1)]);
        let mut acc = MeterAccum::default();
        acc.fold(&block(1, 0, 10, 1, 1.0, 10.0), &maps);
        let f = acc.take_frame(0, &["hot".into()], 0);
        assert!(f.tracks[0].clipped);
        assert!(f.master.clipped);
    }

    #[test]
    fn idle_frame_uses_fallback_position() {
        let mut acc = MeterAccum::default();
        let f = acc.take_frame(1, &["a".into()], 4242);
        assert_eq!(f.position_samples, 4242);
        assert_eq!(f.master.peak_l, 0.0);
    }

    #[test]
    fn blocks_fold_under_the_slot_map_of_their_own_generation() {
        // gen 1: slot 0 = "a", slot 1 = "b". gen 2 (after removing "a"):
        // slot 0 = "b". A gen-1 block reporting slot 0 and a gen-2 block
        // reporting slot 0 must land on DIFFERENT tracks.
        let mut maps = GenerationMaps::default();
        maps.publish(1, &[("a".into(), 0), ("b".into(), 1)].into_iter().collect());
        maps.publish(2, &[("b".into(), 0)].into_iter().collect());
        let mut acc = MeterAccum::default();
        let mut b1 = RawMeterBlock::new(1, 0, 100);
        b1.set_slot_local(0, 0.5, 0.5, 1.0, 1.0); // "a" under gen 1
        let mut b2 = RawMeterBlock::new(2, 100, 100);
        b2.set_slot_local(0, 0.9, 0.9, 2.0, 2.0); // "b" under gen 2
        acc.fold(&b1, &maps);
        acc.fold(&b2, &maps);
        let f = acc.take_frame(0, &["a".into(), "b".into()], 0);
        assert!((f.tracks[0].peak_l - 0.5).abs() < 1e-6, "a keeps its gen-1 level");
        assert!((f.tracks[1].peak_l - 0.9).abs() < 1e-6, "b gets the gen-2 level");
    }

    #[test]
    fn blocks_from_unknown_generations_are_dropped() {
        let mut maps = GenerationMaps::default();
        for g in 1..=6u64 {
            maps.publish(g, &[("t".into(), 0)].into_iter().collect());
        }
        let mut acc = MeterAccum::default();
        let mut stale = RawMeterBlock::new(1, 0, 100); // pruned (only recent kept)
        stale.set_slot_local(0, 1.0, 1.0, 1.0, 1.0);
        acc.fold(&stale, &maps);
        assert!(acc.is_empty(), "stale-generation blocks contribute nothing");
    }

    #[test]
    fn pinned_generation_survives_many_rebuilds() {
        let mut maps = GenerationMaps::default();
        maps.pin(1);
        for g in 1..=6u64 {
            maps.publish(g, &[("t".into(), 0)].into_iter().collect());
        }
        let mut acc = MeterAccum::default();
        let mut b = RawMeterBlock::new(1, 0, 100);
        b.set_slot_local(0, 0.7, 0.7, 1.0, 1.0);
        acc.fold(&b, &maps);
        let f = acc.take_frame(0, &["t".into()], 0);
        assert!((f.tracks[0].peak_l - 0.7).abs() < 1e-6, "pinned gen-1 still resolves");
    }

    #[test]
    fn chunked_blocks_cover_slots_past_sixty_four() {
        let mut maps = GenerationMaps::default();
        maps.publish(
            1,
            &(0..100)
                .map(|i| (TrackId::from(format!("t{i}").as_str()), i))
                .collect(),
        );
        let mut acc = MeterAccum::default();
        let mut hi = RawMeterBlock::new(1, 0, 100);
        hi.base_slot = 64;
        hi.set_slot_local(99 - 64, 0.7, 0.7, 1.0, 1.0); // slot 99, lane 35
        acc.fold(&hi, &maps);
        let order: Vec<TrackId> = (0..100).map(|i| TrackId::from(format!("t{i}").as_str())).collect();
        let f = acc.take_frame(0, &order, 0);
        assert!((f.tracks[99].peak_l - 0.7).abs() < 1e-6);
    }

    /// [I3]: frame/RMS accounting must come ONLY from the `base_slot == 0`
    /// chunk. Folding a second chunk (base 64) from the SAME callback must
    /// not double the RMS denominator — the bug this design avoids reads
    /// meters low, silently, the wider the graph gets.
    #[test]
    fn frame_accounting_ignores_non_base_chunks() {
        let maps = maps_with(1, &[("t", 0)]);
        // Single-chunk baseline: one block, 100 frames, sumsq 8.0 -> rms = sqrt(8/100).
        let mut single = MeterAccum::default();
        single.fold(&block(1, 0, 100, 0, 1.0, 8.0), &maps);
        let f_single = single.take_frame(0, &["t".into()], 0);

        // Two chunks from ONE callback: base 0 (frames=100) and base 64
        // (also frames=100, as `render` stamps every chunk the same way) —
        // the second must not add to the frame/RMS denominator.
        let mut two = MeterAccum::default();
        two.fold(&block(1, 0, 100, 0, 1.0, 8.0), &maps);
        let mut extra = RawMeterBlock::new(1, 0, 100);
        extra.base_slot = 64;
        two.fold(&extra, &maps);
        let f_two = two.take_frame(0, &["t".into()], 0);

        assert!((f_two.tracks[0].rms_l - f_single.tracks[0].rms_l).abs() < 1e-6);
    }
}
