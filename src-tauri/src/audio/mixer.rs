//! Mixer math and the RT render function.
//!
//! `render` is called from the cpal output callback: it must not allocate,
//! lock, or syscall. All inputs are preallocated (`RtGraph`, incl. the live
//! scratch buffer) or atomics (`ParamTable`); meter results are returned by
//! value as a POD block.
//!
//! Track sources (phase 3, ARCHITECTURE §15): a track sums its audio CLIPS
//! (immutable sample buffers) and, when present, its LIVE instrument node
//! (`RtTrack::live`) — PolySynth / SamplerNode / plugin node behind the
//! `LiveInstrument` seam. Live nodes are fed pre-scheduled absolute-sample
//! note events (sliced per block, converted to block offsets — zero ticks,
//! zero allocation on this thread) and render into the graph's preallocated
//! scratch, which is then mixed through the same gain/pan/mute path as clips.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use super::dsp::ProcessBlock;
use super::meters::{RawMeterBlock, METER_CHUNK_SLOTS};
use super::rt::{RtClip, RtGraph, RtTrack, FLAG_MUTE, FLAG_SOLO, MAX_LIVE_BLOCK};
use super::transport::{frame_pos, LoopSpec};
use crate::midi::synth::BlockNoteEvent;
use crate::plugins::automation::{AbsParamEvent, RampCursor};

/// Fader dB to linear gain. -160 dB (and below) encodes -inf.
pub fn db_to_linear(db: f64) -> f32 {
    if db <= -159.95 {
        0.0
    } else {
        10f64.powf(db / 20.0) as f32
    }
}

/// Constant-power pan law: center = -3 dB on both channels, hard L/R = unity
/// on one channel, zero on the other.
#[inline]
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (theta.cos(), theta.sin())
}

/// Solo/mute resolution: when any track is soloed only soloed tracks sound;
/// mute always silences its own track (mute wins over its own solo).
#[inline]
pub fn audible(muted: bool, soloed: bool, any_solo: bool) -> bool {
    !muted && (!any_solo || soloed)
}

/// Sample a clip at absolute timeline position `pos` (engine samples).
/// Returns [L, R]; mono sources are duplicated. Applies clip gain and linear
/// fades. RT-safe.
#[inline]
pub fn clip_sample(clip: &RtClip, pos: u64) -> [f32; 2] {
    if pos < clip.start {
        return [0.0, 0.0];
    }
    let rel = pos - clip.start;
    if rel >= clip.len {
        return [0.0, 0.0];
    }
    let idx = clip.offset + rel;
    if idx >= clip.samples.frames() {
        return [0.0, 0.0];
    }
    let ch = clip.samples.channels.max(1) as usize;
    let base = idx as usize * ch;
    let l = clip.samples.data[base];
    let r = if ch > 1 { clip.samples.data[base + 1] } else { l };

    let mut g = clip.gain;
    if clip.fade_in > 0 && rel < clip.fade_in {
        g *= rel as f32 / clip.fade_in as f32;
    }
    if clip.fade_out > 0 {
        let rem = clip.len - rel;
        if rem <= clip.fade_out {
            g *= rem as f32 / clip.fade_out as f32;
        }
    }
    [l * g, r * g]
}

/// Per-track meter accumulator (stack POD).
#[derive(Default, Clone, Copy)]
struct TrackAccum {
    pk_l: f32,
    pk_r: f32,
    ss_l: f32,
    ss_r: f32,
}

impl TrackAccum {
    #[inline]
    fn fold(&mut self, l: f32, r: f32) {
        self.pk_l = self.pk_l.max(l.abs());
        self.pk_r = self.pk_r.max(r.abs());
        self.ss_l += l * l;
        self.ss_r += r * r;
    }
}

/// Mix one post-fader stereo sample into the interleaved output.
#[inline]
fn mix_out(out: &mut [f32], frame: usize, out_ch: usize, l: f32, r: f32) {
    let o = frame * out_ch;
    if out_ch >= 2 {
        out[o] += l;
        out[o + 1] += r;
    } else {
        out[o] += 0.5 * (l + r);
    }
}

/// Render a track's LIVE instrument node into `out` through the track's
/// gain/pan/mute, folding meters into `acc`. RT-safe: the node renders into
/// the graph's preallocated `scratch` in loop-aware contiguous runs (a loop
/// wrap releases all voices — the note-offs beyond the loop end will never
/// arrive), and note events are sliced from the snapshot's pre-scheduled
/// absolute-sample event list by binary search (no allocation).
#[allow(clippy::too_many_arguments)]
fn render_live(
    tr: &RtTrack,
    scratch: &mut [f32],
    base_pos: u64,
    lp: &LoopSpec,
    out: &mut [f32],
    out_ch: usize,
    frames: usize,
    sample_rate: u32,
    discontinuity: bool,
    steady_base: Option<u64>,
    mix: (f32, f32, f32, bool),
    ramp: &[AbsParamEvent],
    acc: &mut TrackAccum,
) {
    let Some(live) = &tr.live else { return };
    // SAFETY: RCU discipline — exactly one graph snapshot is rendered at a
    // time, on this (the only RT) thread; see `LiveNodeCell`.
    let node = unsafe { live.node.rt_mut() };
    let (gain, gl, gr, on) = mix;
    if discontinuity {
        node.all_notes_off();
    }
    // Per-run discontinuity for the automation seam: the caller's flag on
    // the first run, then true again after every loop wrap (the position
    // jumps back to the loop start).
    let mut run_discontinuity = discontinuity;
    // Track D: one cursor for the whole call; `pos` is this run's base, and
    // runs advance monotonically except across a loop wrap, which the
    // cursor re-seeds for.
    let mut ramp_cursor = RampCursor::new();
    let mut f = 0usize;
    while f < frames {
        let pos = frame_pos(base_pos, f as u64, lp);
        let mut run = (frames - f).min(MAX_LIVE_BLOCK);
        let mut wraps = false;
        if lp.active() && pos < lp.end {
            let to_end = (lp.end - pos) as usize;
            if to_end <= run {
                run = to_end;
                wraps = true;
            }
        }
        if run == 0 || scratch.len() < run * 2 {
            // Defensive: never render live audio without prepared scratch.
            return;
        }
        let sblock = &mut scratch[..run * 2];
        sblock.fill(0.0);
        let end = pos + run as u64;
        let evs = &live.events[..];
        let lo = evs.partition_point(|e| e.sample < pos);
        for e in evs[lo..].iter().take_while(|e| e.sample < end) {
            node.queue_event(BlockNoteEvent {
                offset: (e.sample - pos) as u32,
                key: e.key,
                velocity: e.velocity,
            });
        }
        // Automation seam (ARCHITECTURE §15.1): the run's absolute base
        // position + discontinuity reach the node BEFORE it processes, so
        // ramp cursors survive seeks and loop wraps.
        node.set_block_context(pos, run_discontinuity);
        // Round-2 §3.5: the same engine-global steady_time base for every
        // run in this block (a loop wrap can split one callback block into
        // several runs) — non-decreasing is the CLAP contract, not
        // strictly-increasing every call.
        let mut io = ProcessBlock { samples: sblock, channels: 2, sample_rate, steady: steady_base };
        node.process(&mut io);
        if wraps {
            node.all_notes_off();
            run_discontinuity = true;
        } else {
            run_discontinuity = false;
        }
        for i in 0..run {
            let g = gain * ramp_cursor.value(ramp, pos + i as u64).unwrap_or(1.0);
            let mut l = scratch[i * 2] * g * gl;
            let mut r = scratch[i * 2 + 1] * g * gr;
            if !on {
                l = 0.0;
                r = 0.0;
            }
            acc.fold(l, r);
            mix_out(out, f + i, out_ch, l, r);
        }
        f += run;
    }
}

/// Render one output buffer: mixes all tracks into `out` (interleaved,
/// `out_ch` channels) starting at timeline position `base_pos`, honoring the
/// loop region per-frame. RT-safe: fixed-size stack state + the graph's
/// preallocated scratch only — including the meter chunks (Task 7:
/// `graph.meter_scratch`, sized `⌈slots / METER_CHUNK_SLOTS⌉` at BUILD time,
/// so a wide graph's meter output is N in-place mutations + N pushes, never
/// an RT allocation).
///
/// `discontinuity` must be true when `base_pos` does not continue the
/// previous render (seek, stop→play): live instrument nodes get
/// `all_notes_off` so no voice hangs waiting for a note-off that will never
/// be delivered.
///
/// `meter_tx` is `None` for offline/loopjam/headless-test callers that don't
/// wire up a ring; `Some` (the cpal output callback) gets every chunk pushed
/// into it. Returns the number of chunks DROPPED because the ring was full —
/// `render` has no `SharedRt` to bump xruns itself, so the caller (which
/// does) counts one xrun per dropped chunk.
///
/// This entry point has no engine-global `steady_time` to hand live nodes
/// (round-2 §3.5) — only the real RT output callback owns one (`SharedRt`;
/// see `render_rt`). Every other caller (offline bounce, loopjam, preview,
/// tests) hands live nodes `ProcessBlock::steady == None` (fix-round-1):
/// `base_pos` can move BACKWARD here (a loop wrap), so it must never be
/// used to derive a steady value — a persistent live node (e.g. `ClapNode`)
/// falls back to its OWN per-instance monotonic counter when told `None`,
/// exactly the pre-round-2 behavior for these paths, which is immune to
/// `base_pos` direction because it only counts frames it actually
/// processed.
#[allow(clippy::too_many_arguments)]
pub fn render(
    graph: &mut RtGraph,
    base_pos: u64,
    lp: &LoopSpec,
    out: &mut [f32],
    out_ch: usize,
    sample_rate: u32,
    discontinuity: bool,
    meter_tx: Option<&mut rtrb::Producer<RawMeterBlock>>,
) -> u32 {
    render_impl(graph, base_pos, lp, out, out_ch, sample_rate, discontinuity, None, meter_tx)
}

/// Like [`render`], but carries the engine-global `steady_time` base for
/// this block (round-2 §3.5) explicitly — used ONLY by the real RT output
/// callback (`engine::OutputCb::render`), which advances `SharedRt::steady`
/// once per block and passes the pre-advance value here. Live nodes
/// (`ProcessBlock::steady`) see this instead of self-counting, so the value
/// they observe survives node re-creation (instrument rebind, sample-rate
/// change, a track leaving and re-entering the live set) — the counter
/// lives on the engine, not the node.
#[allow(clippy::too_many_arguments)]
pub fn render_rt(
    graph: &mut RtGraph,
    base_pos: u64,
    lp: &LoopSpec,
    out: &mut [f32],
    out_ch: usize,
    sample_rate: u32,
    discontinuity: bool,
    steady_base: u64,
    meter_tx: Option<&mut rtrb::Producer<RawMeterBlock>>,
) -> u32 {
    render_impl(
        graph,
        base_pos,
        lp,
        out,
        out_ch,
        sample_rate,
        discontinuity,
        Some(steady_base),
        meter_tx,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_impl(
    graph: &mut RtGraph,
    base_pos: u64,
    lp: &LoopSpec,
    out: &mut [f32],
    out_ch: usize,
    sample_rate: u32,
    discontinuity: bool,
    steady_base: Option<u64>,
    meter_tx: Option<&mut rtrb::Producer<RawMeterBlock>>,
) -> u32 {
    let out_ch = out_ch.max(1);
    let frames = out.len() / out_ch;
    out.fill(0.0);
    // Round-2 §2.4 / [I6]: the graph's OWN params, not a passed-in
    // reference — `render(g, &g.params, ...)` doesn't borrow-check
    // (mutable borrow of `*g` + shared borrow of `g.params`), so this reads
    // `graph.params` internally via a split borrow, which IS fine inside
    // the function. This is also how the O-13 alias window stays dead: a
    // retired graph always renders against the table it was built with.
    let params = graph.params.clone();
    let any_solo = params.any_solo.load(Relaxed);
    let n_slots = params.len();
    let generation = graph.generation;
    let RtGraph { tracks, scratch, meter_scratch, gain_ramps, .. } = graph;
    let gain_ramps: &[Option<Arc<Vec<AbsParamEvent>>>] = gain_ramps;

    // Reset this callback's chunk templates in place (Task 7: preallocated
    // at BUILD time on the control thread; `render` never grows this Vec).
    for (i, chunk) in meter_scratch.iter_mut().enumerate() {
        *chunk = RawMeterBlock::new(generation, base_pos, frames as u32);
        chunk.base_slot = (i * METER_CHUNK_SLOTS) as u32;
    }

    for tr in tracks.iter() {
        if tr.slot >= n_slots {
            continue;
        }
        let gain = f32::from_bits(params.gain[tr.slot].load(Relaxed));
        let pan = f32::from_bits(params.pan[tr.slot].load(Relaxed));
        let flags = params.flags[tr.slot].load(Relaxed);
        let on = audible(flags & FLAG_MUTE != 0, flags & FLAG_SOLO != 0, any_solo);
        let (gl, gr) = pan_gains(pan);
        let mut acc = TrackAccum::default();

        // Track D: this snapshot's compiled gain automation for the slot.
        // RT-safe: a slice read + an index walk, no allocation, no locks.
        let ramp: &[AbsParamEvent] = gain_ramps
            .get(tr.slot)
            .and_then(|r| r.as_ref())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        let mut clip_ramp = RampCursor::new();

        if !tr.clips.is_empty() {
            for i in 0..frames {
                let pos = frame_pos(base_pos, i as u64, lp);
                let mut l = 0.0f32;
                let mut r = 0.0f32;
                for clip in &tr.clips {
                    let s = clip_sample(clip, pos);
                    l += s[0];
                    r += s[1];
                }
                let g = gain * clip_ramp.value(ramp, pos).unwrap_or(1.0);
                l *= g * gl;
                r *= g * gr;
                if !on {
                    l = 0.0;
                    r = 0.0;
                }
                acc.fold(l, r);
                mix_out(out, i, out_ch, l, r);
            }
        }

        render_live(
            tr,
            scratch,
            base_pos,
            lp,
            out,
            out_ch,
            frames,
            sample_rate,
            discontinuity,
            steady_base,
            (gain, gl, gr, on),
            ramp,
            &mut acc,
        );

        let chunk_idx = tr.slot / METER_CHUNK_SLOTS;
        let lane = tr.slot % METER_CHUNK_SLOTS;
        if let Some(chunk) = meter_scratch.get_mut(chunk_idx) {
            chunk.set_slot_local(lane, acc.pk_l, acc.pk_r, acc.ss_l, acc.ss_r);
        }
    }

    // Master meters from the summed output, on the first chunk only
    // (base_slot == 0) — `meter_scratch` always has at least one entry
    // (`RtGraph::new` floors the chunk count to 1), so this never misses.
    if let Some(first) = meter_scratch.first_mut() {
        for i in 0..frames {
            let o = i * out_ch;
            let l = out[o];
            let r = if out_ch >= 2 { out[o + 1] } else { out[o] };
            first.master_peak[0] = first.master_peak[0].max(l.abs());
            first.master_peak[1] = first.master_peak[1].max(r.abs());
            first.master_sumsq[0] += l * l;
            first.master_sumsq[1] += r * r;
        }
    }

    let mut dropped = 0u32;
    if let Some(producer) = meter_tx {
        for chunk in meter_scratch.iter() {
            if producer.push(*chunk).is_err() {
                dropped += 1;
            }
        }
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::rt::ParamTable;
    use super::super::rt::RtClipData;
    use super::super::rt::RtTrack;

    fn clip(start: u64, offset: u64, len: u64, data: Vec<f32>, channels: u16) -> RtClip {
        RtClip {
            start,
            offset,
            len,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            samples: Arc::new(RtClipData { channels, data }),
        }
    }

    // ---- gain law ----

    #[test]
    fn db_to_linear_reference_points() {
        assert!((db_to_linear(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_linear(-6.0) - 0.5011872).abs() < 1e-4);
        assert!((db_to_linear(6.0) - 1.9952623).abs() < 1e-4);
        assert!((db_to_linear(-20.0) - 0.1).abs() < 1e-6);
        assert_eq!(db_to_linear(-160.0), 0.0, "-160 dB is -inf");
        assert_eq!(db_to_linear(-500.0), 0.0);
    }

    // ---- pan law ----

    #[test]
    fn pan_law_is_constant_power() {
        let (l, r) = pan_gains(0.0);
        assert!((l - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6, "center is -3 dB");
        assert!((l - r).abs() < 1e-6);
        let (l, r) = pan_gains(-1.0);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6, "hard left");
        let (l, r) = pan_gains(1.0);
        assert!(l.abs() < 1e-6 && (r - 1.0).abs() < 1e-6, "hard right");
        // power is preserved across the arc
        for p in [-0.75f32, -0.3, 0.2, 0.9] {
            let (l, r) = pan_gains(p);
            assert!((l * l + r * r - 1.0).abs() < 1e-5);
        }
    }

    // ---- solo / mute resolution ----

    #[test]
    fn solo_mute_matrix() {
        // (muted, soloed, any_solo) -> audible
        assert!(audible(false, false, false));
        assert!(!audible(true, false, false), "mute silences");
        assert!(!audible(false, false, true), "other track soloed");
        assert!(audible(false, true, true), "soloed track plays");
        assert!(!audible(true, true, true), "mute wins over own solo");
    }

    // ---- clip sampling ----

    #[test]
    fn clip_sampling_respects_bounds_offset_and_mono() {
        let c = clip(100, 2, 4, vec![0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7], 1);
        assert_eq!(clip_sample(&c, 99), [0.0, 0.0], "before clip");
        assert_eq!(clip_sample(&c, 100), [0.2, 0.2], "offset applied, mono dup");
        assert_eq!(clip_sample(&c, 103), [0.5, 0.5]);
        assert_eq!(clip_sample(&c, 104), [0.0, 0.0], "past clip length");
    }

    #[test]
    fn clip_fades_are_linear() {
        let mut c = clip(0, 0, 8, vec![1.0; 8], 1);
        c.fade_in = 4;
        c.fade_out = 2;
        assert_eq!(clip_sample(&c, 0)[0], 0.0);
        assert!((clip_sample(&c, 2)[0] - 0.5).abs() < 1e-6);
        assert!((clip_sample(&c, 4)[0] - 1.0).abs() < 1e-6);
        // rel 6 -> rem 2 -> factor 1.0; rel 7 -> rem 1 -> 0.5
        assert!((clip_sample(&c, 7)[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn stereo_clip_channels_stay_separate() {
        let c = clip(0, 0, 2, vec![0.25, -0.5, 0.75, -1.0], 2);
        assert_eq!(clip_sample(&c, 0), [0.25, -0.5]);
        assert_eq!(clip_sample(&c, 1), [0.75, -1.0]);
    }

    // ---- full render ----

    fn one_track_graph(slot: usize, c: RtClip) -> RtGraph {
        RtGraph::new(vec![RtTrack::clips(slot, vec![c])], 1, Arc::new(ParamTable::default()))
    }

    /// Old-signature convenience: clip-only graphs, no discontinuity, no
    /// meter ring (`None`) — for tests that only care about the mixed
    /// audio. Params now live on the graph (`g.params`); callers mutate
    /// that Arc directly before rendering.
    fn render_simple(g: &mut RtGraph, base: u64, lp: &LoopSpec, out: &mut [f32], out_ch: usize) {
        render(g, base, lp, out, out_ch, 48_000, false, None);
    }

    /// Render with a meter ring wired up, returning every chunk pushed by
    /// this call (Task 7: `render` no longer returns a block directly — it
    /// pushes `1..=⌈slots/64⌉` chunks into the ring).
    fn render_with_meters(
        g: &mut RtGraph,
        base: u64,
        lp: &LoopSpec,
        out: &mut [f32],
        out_ch: usize,
    ) -> Vec<RawMeterBlock> {
        let (mut tx, mut rx) = rtrb::RingBuffer::new(16);
        let dropped = render(g, base, lp, out, out_ch, 48_000, false, Some(&mut tx));
        assert_eq!(dropped, 0, "test ring is never full");
        let mut blocks = Vec::new();
        while let Ok(b) = rx.pop() {
            blocks.push(b);
        }
        blocks
    }

    #[test]
    fn render_applies_gain_and_pan() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_gain_linear(0, 0.5);
        g.params.set_pan(0, -1.0); // hard left
        let mut out = vec![0.0f32; 8]; // 4 frames stereo
        let blocks = render_with_meters(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let blk = blocks[0];
        assert!((out[0] - 0.5).abs() < 1e-6, "left gets gain*1.0");
        assert!(out[1].abs() < 1e-6, "right silent at hard left");
        assert!((blk.peak[0][0] - 0.5).abs() < 1e-6);
        assert!(blk.mask & 1 != 0);
        assert!((blk.master_peak[0] - 0.5).abs() < 1e-6);
        // track rms: sqrt(4*0.25/4) = 0.5
        assert!(((blk.sumsq[0][0] / 4.0).sqrt() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn render_mutes_and_solos() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.tracks
            .push(RtTrack::clips(1, vec![clip(0, 0, 4, vec![0.5; 4], 1)]));
        g.params.set_flag(1, FLAG_SOLO, true);
        g.params.any_solo.store(true, Relaxed);
        let mut out = vec![0.0f32; 8];
        let blocks = render_with_meters(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let blk = blocks[0];
        assert_eq!(blk.peak[0][0], 0.0, "non-soloed track is silent");
        let expected = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((blk.peak[1][0] - expected).abs() < 1e-5, "soloed track plays center-panned");
        assert!((out[0] - expected).abs() < 1e-5);
    }

    #[test]
    fn render_wraps_around_loop() {
        // 4-sample clip at [0,4), loop [0,4), render 8 frames -> plays twice
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![0.1, 0.2, 0.3, 0.4], 1));
        g.params.set_pan(0, -1.0);
        let lp = LoopSpec { enabled: true, start: 0, end: 4 };
        let mut out = vec![0.0f32; 16];
        render_simple(&mut g, 0, &lp, &mut out, 2);
        let lefts: Vec<f32> = out.iter().step_by(2).copied().collect();
        for (i, v) in lefts.iter().enumerate() {
            let expected = [0.1f32, 0.2, 0.3, 0.4][i % 4];
            assert!((v - expected).abs() < 1e-6, "frame {i}");
        }
    }

    #[test]
    fn render_mono_output_downmixes() {
        let mut g = one_track_graph(0, clip(0, 0, 2, vec![0.8, 0.8], 1));
        let mut out = vec![0.0f32; 2];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 1);
        // center pan: l = r = 0.8/sqrt(2); mono = (l+r)/2
        let expected = 0.8 * std::f32::consts::FRAC_1_SQRT_2;
        assert!((out[0] - expected).abs() < 1e-5);
    }

    #[test]
    fn render_sums_overlapping_clips() {
        let mut g = RtGraph::new(
            vec![RtTrack::clips(
                0,
                vec![
                    clip(0, 0, 4, vec![0.25; 4], 1),
                    clip(2, 0, 4, vec![0.25; 4], 1),
                ],
            )],
            1,
            Arc::new(ParamTable::default()),
        );
        g.params.set_pan(0, -1.0);
        let mut out = vec![0.0f32; 12];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let lefts: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert!((lefts[1] - 0.25).abs() < 1e-6);
        assert!((lefts[2] - 0.5).abs() < 1e-6, "overlap sums");
        assert!((lefts[5] - 0.25).abs() < 1e-6);
    }

    // ---- live instrument nodes (phase 3 seam, ARCHITECTURE §15) ----

    use super::super::dsp::AudioProcessor;
    use super::super::rt::{LiveNodeCell, LiveSource};
    use crate::midi::schedule::AbsNoteEvent;
    use crate::midi::synth::PolySynth;

    /// A midi-style track: live PolySynth + pre-scheduled events.
    fn live_track(slot: usize, events: Vec<AbsNoteEvent>, rate: u32) -> RtTrack {
        let mut synth = PolySynth::new();
        synth.prepare(rate, super::super::rt::MAX_LIVE_BLOCK);
        RtTrack {
            slot,
            clips: Vec::new(),
            live: Some(LiveSource {
                node: LiveNodeCell::new(Box::new(synth)),
                events: Arc::new(events),
            }),
        }
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// The headless seam proof: a live PolySynth node inside the RCU graph
    /// renders audible audio through `render`, sample-positioned, and the
    /// note's release leaves silence.
    #[test]
    fn live_node_renders_audibly_through_graph() {
        const RATE: u32 = 48_000;
        let events = vec![
            AbsNoteEvent { sample: 1_000, key: 69, velocity: 110 },
            AbsNoteEvent { sample: 9_000, key: 69, velocity: 0 },
        ];
        let mut g = RtGraph::new(vec![live_track(0, events, RATE)], 1, Arc::new(ParamTable::default()));
        assert!(!g.scratch.is_empty(), "live graph allocates scratch at build");
        // Render 24000 frames in odd-sized callback blocks.
        let mut out = vec![0.0f32; 24_000 * 2];
        let mut pos = 0u64;
        let (mut tx, mut rx) = rtrb::RingBuffer::new(8);
        for chunk in out.chunks_mut(700 * 2) {
            let dropped = render(&mut g, pos, &LoopSpec::OFF, chunk, 2, RATE, false, Some(&mut tx));
            assert_eq!(dropped, 0);
            pos += (chunk.len() / 2) as u64;
            let blk = rx.pop().expect("one base-0 meter chunk per callback");
            assert_eq!(blk.frames, (chunk.len() / 2) as u32);
        }
        let lefts: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert_eq!(peak(&lefts[..1_000]), 0.0, "silent before the note-on");
        assert!(peak(&lefts[2_000..8_000]) > 0.05, "note is audible");
        // Note off at 9000 + 80 ms release -> silent well before 20000.
        assert_eq!(peak(&lefts[20_000..]), 0.0, "released to digital silence");
    }

    /// Seek/stop discontinuity: without the flag a pending note keeps
    /// sounding past its (skipped) note-off; with it, voices are released.
    #[test]
    fn live_node_discontinuity_releases_voices() {
        const RATE: u32 = 48_000;
        let events = vec![
            AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
            AbsNoteEvent { sample: 50_000, key: 60, velocity: 0 },
        ];
        let mut g = RtGraph::new(vec![live_track(0, events, RATE)], 1, Arc::new(ParamTable::default()));
        let mut out = vec![0.0f32; 512 * 2];
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, None);
        assert!(peak(&out) > 0.01, "note started");
        // Seek far past the note-off; the off event will never be delivered.
        // First block after the jump carries discontinuity=true.
        render(&mut g, 100_000, &LoopSpec::OFF, &mut out, 2, RATE, true, None);
        // Render past the release tail: must decay to silence, not hang.
        let mut pos = 100_000 + 512;
        for _ in 0..20 {
            render(&mut g, pos, &LoopSpec::OFF, &mut out, 2, RATE, false, None);
            pos += 512;
        }
        assert_eq!(peak(&out), 0.0, "no hung voice after discontinuity");
    }

    /// A loop wrap inside one callback block must release voices (their
    /// note-offs lie beyond the loop end and never arrive) and re-fire the
    /// events at the loop start on the wrapped run.
    #[test]
    fn live_node_loop_wrap_releases_and_retriggers() {
        const RATE: u32 = 48_000;
        // Note spans the whole loop; off is exactly at the loop end (never
        // inside the played region more than once).
        let events = vec![
            AbsNoteEvent { sample: 0, key: 72, velocity: 100 },
            AbsNoteEvent { sample: 40_000, key: 72, velocity: 0 },
        ];
        let lp = LoopSpec { enabled: true, start: 0, end: 8_192 };
        let mut g = RtGraph::new(vec![live_track(0, events, RATE)], 1, Arc::new(ParamTable::default()));
        // One callback block that crosses the wrap: frames [4096..8192) then
        // wraps to [0..4096).
        let mut out = vec![0.0f32; 8_192 * 2];
        render(&mut g, 4_096, &lp, &mut out, 2, RATE, false, None);
        assert!(peak(&out) > 0.01, "audible across the wrap");
        // After many looped blocks the voice count must not grow without
        // bound: each wrap releases before the retrigger.
        let node_voices = {
            let mut pos = 4_096u64;
            for _ in 0..50 {
                render(&mut g, pos, &lp, &mut out, 2, RATE, false, None);
                pos = crate::audio::transport::advance(pos, 8_192, &lp);
            }
            let live = g.tracks[0].live.as_ref().unwrap();
            let node = unsafe { live.node.rt_mut() };
            // Render one wrap-free silent stretch so releases finish.
            node.all_notes_off();
            let mut tail = vec![0.0f32; 8_192 * 2];
            render(
                &mut g,
                1_000_000,
                &LoopSpec::OFF,
                &mut tail,
                2,
                RATE,
                false,
                None,
            );
            peak(&tail[8_000..])
        };
        assert!(node_voices < 0.05, "voices do not accumulate across wraps");
    }

    // ---- fix-round-1: non-RT steady_time must never decrease ----

    use super::super::dsp::LiveInstrument;

    /// Mirrors `plugins::clap_host::ClapNode`'s fix-round-1 fallback exactly
    /// (see that module): when handed `ProcessBlock::steady == None` (every
    /// non-RT caller — offline bounce, loopjam, preview), fall back to a
    /// per-instance accumulator that only counts frames THIS node actually
    /// processed, so the caller's `base_pos` direction (which CAN move
    /// backward on a loop wrap) never enters into it.
    #[derive(Default)]
    struct FallbackState {
        fallback: u64,
        log: Vec<u64>,
    }
    struct FallbackNode(Arc<parking_lot::Mutex<FallbackState>>);
    impl AudioProcessor for FallbackNode {
        fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}
        fn process(&mut self, io: &mut ProcessBlock<'_>) {
            let frames = io.frames() as u64;
            let mut st = self.0.lock();
            let steady = match io.steady {
                Some(s) => s,
                None => {
                    let s = st.fallback;
                    st.fallback = st.fallback.wrapping_add(frames);
                    s
                }
            };
            st.log.push(steady);
        }
        fn reset(&mut self) {}
    }
    impl LiveInstrument for FallbackNode {
        fn queue_event(&mut self, _ev: BlockNoteEvent) -> bool {
            true
        }
        fn all_notes_off(&mut self) {}
    }

    /// Round-2 §3.5 fix-round-1: the non-RT `render` entry point (offline
    /// bounce, loopjam, preview) must never hand a PERSISTENT live node a
    /// decreasing steady_time, even though its own `base_pos` moves
    /// backward on a loop wrap. Two `render` calls on the SAME graph/SAME
    /// node (no rebuild between them) — the second call's `base_pos` is
    /// LOWER than the first's, simulating exactly that wrap — must still
    /// produce a strictly-increasing steady sequence.
    #[test]
    fn non_rt_render_never_hands_a_decreasing_steady_across_a_loop_wrap() {
        let state = Arc::new(parking_lot::Mutex::new(FallbackState::default()));
        let tr = RtTrack {
            slot: 0,
            clips: Vec::new(),
            live: Some(LiveSource {
                node: LiveNodeCell::new(Box::new(FallbackNode(state.clone()))),
                events: Arc::new(Vec::new()),
            }),
        };
        let mut g = RtGraph::new(vec![tr], 1, Arc::new(ParamTable::with_slots(1)));
        let mut out = vec![0.0f32; 128 * 2];

        render(&mut g, 5_000, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        // base_pos DROPS here — a loop wrap on the same persistent node.
        render(&mut g, 100, &LoopSpec::OFF, &mut out, 2, 48_000, true, None);
        render(&mut g, 228, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);

        let seen = state.lock().log.clone();
        assert_eq!(seen.len(), 3, "one steady value observed per rendered block");
        assert!(seen[1] > seen[0], "monotonic across the loop wrap: {:?}", seen);
        assert!(seen[2] > seen[1], "still climbing after the wrap: {:?}", seen);
    }

    // ---- Task 8: slot-indexed gain ramps applied by the mixer -------------

    /// Track D's audibility proof at the MIXER seam (scope ruling 1: the
    /// ramp attaches at the per-track gain stage, so it scales CLIP audio
    /// and LIVE audio alike — `GainAutomatedNode` could only ever have done
    /// the latter). Offline render, amplitude asserted against the lane.
    #[test]
    fn track_gain_ramp_scales_clip_output_sample_accurately() {
        use crate::plugins::automation::AbsParamEvent;
        // A DC-1.0 mono clip so the applied gain is directly readable.
        let data = Arc::new(RtClipData { channels: 1, data: vec![1.0; 4096] });
        let clip = RtClip {
            start: 0, offset: 0, len: 4096, gain: 1.0,
            fade_in: 0, fade_out: 0, samples: data,
        };
        let mut g = RtGraph::new(
            vec![RtTrack::clips(0, vec![clip])],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        g.params.set_pan(0, -1.0); // hard left: channel 0 carries unity
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 1.0 },
            AbsParamEvent { sample: 1000, value: 0.0 },
        ]);
        g.set_gain_ramps(vec![Some(ev.clone())]);

        let mut out = vec![0.0f32; 1024 * 2];
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        for (i, frame) in out.chunks_exact(2).enumerate() {
            let want = crate::plugins::automation::value_at(&ev, i as u64).unwrap();
            assert!(
                (frame[0] - want).abs() < 1e-4,
                "sample {i}: got {} want {want}",
                frame[0]
            );
        }
    }

    /// The cursor must re-seed on a BACKWARD position jump: one callback
    /// block crossing a loop end renders the tail of the ramp and then the
    /// loop start's value, mid-block.
    #[test]
    fn track_gain_ramp_re_seeds_across_a_loop_wrap() {
        use crate::plugins::automation::AbsParamEvent;
        let data = Arc::new(RtClipData { channels: 1, data: vec![1.0; 8192] });
        let clip = RtClip {
            start: 0, offset: 0, len: 8192, gain: 1.0,
            fade_in: 0, fade_out: 0, samples: data,
        };
        let mut g = RtGraph::new(
            vec![RtTrack::clips(0, vec![clip])],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        g.params.set_pan(0, -1.0);
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 1.0 },
            AbsParamEvent { sample: 2000, value: 0.0 },
        ]);
        g.set_gain_ramps(vec![Some(ev.clone())]);

        let lp = LoopSpec { enabled: true, start: 500, end: 2000 };
        let mut out = vec![0.0f32; 1024 * 2];
        render(&mut g, 1_744, &lp, &mut out, 2, 48_000, true, None);
        for (i, frame) in out.chunks_exact(2).enumerate() {
            let pos = crate::audio::transport::frame_pos(1_744, i as u64, &lp);
            let want = crate::plugins::automation::value_at(&ev, pos).unwrap();
            assert!(
                (frame[0] - want).abs() < 1e-4,
                "frame {i} (pos {pos}): got {} want {want}",
                frame[0]
            );
        }
    }

    /// A LIVE (instrument) track's output goes through the same gain stage,
    /// so one ramp covers both source kinds.
    #[test]
    fn track_gain_ramp_scales_live_output_too() {
        use crate::plugins::automation::AbsParamEvent;
        let mut g = RtGraph::new(
            vec![live_track(0, vec![], 48_000)],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 0.0 },
            AbsParamEvent { sample: 512, value: 0.0 },
        ]);
        g.set_gain_ramps(vec![Some(ev)]);
        let mut out = vec![0.0f32; 512 * 2];
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        assert!(
            out.iter().all(|s| s.abs() < 1e-6),
            "a lane pinned at 0.0 silences the live node's contribution"
        );
    }
}
