//! Mixer math and the RT render function.
//!
//! `render` is called from the cpal output callback: it must not allocate,
//! lock, or syscall. All inputs are preallocated (`RtGraph`, incl. the live
//! scratch buffer) or atomics (`ParamTable`); meter results are returned by
//! value as a POD block.
//!
//! Track strip (Plan G1): per run of `MAX_LIVE_BLOCK`, a track zeros
//! `track_buf`, mixes clips (no fader), ADDs its LIVE instrument when
//! present, walks inserts REPLACE in document order, then applies the
//! shared gain/pan/mute fader into `out`. Live nodes are fed pre-scheduled
//! absolute-sample note events (sliced per run, converted to block offsets
//! — zero ticks, zero allocation on this thread).

use std::sync::atomic::Ordering::Relaxed;

use super::dsp::ProcessBlock;
use super::meters::{RawMeterBlock, METER_CHUNK_SLOTS};
use super::midi_in::{LiveMidiEvent, EV_ALL_OFF, EV_NOTE_ON};
use super::rt::{
    LaunchPlayhead, RtClip, RtGraph, RtTrack, FLAG_LAUNCH, FLAG_MUTE, FLAG_SOLO, MAX_LIVE_BLOCK,
};
use super::transport::{frame_pos, LoopSpec};
use crate::midi::synth::BlockNoteEvent;
use crate::plugins::automation::{value_at, AbsParamEvent, RampCursor};

/// Hardware MIDI-in events for THIS block, already drained from the hub ring
/// by the RT output callback. Delivered to the node at block offset 0
/// (ruling 9: no sub-block placement in this slice).
#[derive(Clone, Copy)]
pub struct LiveInBlock<'a> {
    pub slot: usize,
    pub events: &'a [LiveMidiEvent],
}

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
/// `launch` is a live launch target — it stays audible through both.
#[inline]
pub fn audible(muted: bool, soloed: bool, any_solo: bool) -> bool {
    audible_with_launch(muted, soloed, any_solo, false)
}

#[inline]
pub fn audible_with_launch(muted: bool, soloed: bool, any_solo: bool, launch: bool) -> bool {
    launch || (!muted && (!any_solo || soloed))
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

/// Lerp a block-boundary `(gl, gr)` pair. `last` is `frames - 1` (or 0).
#[inline]
fn lerp_pan(gl0: f32, gr0: f32, gl1: f32, gr1: f32, i: usize, last: usize) -> (f32, f32) {
    if last == 0 {
        return (gl0, gr0);
    }
    let t = i as f32 / last as f32;
    (gl0 + (gl1 - gl0) * t, gr0 + (gr1 - gr0) * t)
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

/// The pan-gain quad for one block: `(gl0, gr0)` at the block's first frame,
/// `(gl1, gr1)` at its last — [`lerp_pan`] interpolates between them per
/// sample. Bundled (not four loose floats) so the two identically-typed
/// pairs can't be transposed at a call site by accident.
#[derive(Clone, Copy)]
struct PanGains {
    gl0: f32,
    gr0: f32,
    gl1: f32,
    gr1: f32,
}

/// Resolve a block's pan-gain quad from the compiled pan ramp (or the flat
/// atomic pan when there is none / it's empty). `first_pos`/`last_pos` are
/// the absolute sample positions of the block's first and last frame —
/// loop-aware in `render_impl`, a flat `base_pos..base_pos+frames` in
/// `render_live_input_only` (monitoring never advances a position).
/// `pan_gains` is two trig calls; per-sample evaluation would cost ~3M
/// sin/cos per second at 32 tracks for a difference nobody can hear (design
/// §6.1) — deliberate divergence from gain's per-sample `RampCursor`.
fn pan_gain_quad(
    pan_ramp: Option<&[AbsParamEvent]>,
    pan: f32,
    atomic: (f32, f32),
    first_pos: u64,
    last_pos: u64,
) -> PanGains {
    match pan_ramp {
        Some(events) if !events.is_empty() => {
            let p0 = value_at(events, first_pos).unwrap_or(pan);
            let p1 = value_at(events, last_pos).unwrap_or(pan);
            let (a, b) = (pan_gains(p0), pan_gains(p1));
            PanGains { gl0: a.0, gr0: a.1, gl1: b.0, gr1: b.1 }
        }
        _ => PanGains { gl0: atomic.0, gr0: atomic.1, gl1: atomic.0, gr1: atomic.1 },
    }
}

/// Per-block fader inputs shared by [`apply_fader`]'s two call sites.
struct FaderCtx<'a> {
    gain: f32,
    ramp: &'a [AbsParamEvent],
    pan: PanGains,
    on: bool,
}

/// Shared fader: `gain * ramp * pan`, mute, mix into `out`, fold meters.
/// One implementation for clips, live, and the insert chain.
#[allow(clippy::too_many_arguments)]
fn apply_fader(
    buf: &[f32],
    run: usize,
    f: usize,
    pos: u64,
    ctx: &FaderCtx<'_>,
    pan_last: usize,
    ramp_cursor: &mut RampCursor,
    out: &mut [f32],
    out_ch: usize,
    acc: &mut TrackAccum,
) {
    for i in 0..run {
        let g = ctx.gain * ramp_cursor.value(ctx.ramp, pos + i as u64).unwrap_or(1.0);
        let (gl, gr) = lerp_pan(ctx.pan.gl0, ctx.pan.gr0, ctx.pan.gl1, ctx.pan.gr1, f + i, pan_last);
        let mut l = buf[i * 2] * g * gl;
        let mut r = buf[i * 2 + 1] * g * gr;
        if !ctx.on {
            l = 0.0;
            r = 0.0;
        }
        acc.fold(l, r);
        mix_out(out, f + i, out_ch, l, r);
    }
}

/// Queue live-in events (and an optional discontinuity release) once per
/// callback block. The `EV_ALL_OFF` arm is NOT what the engine's RT path
/// sends: `OutputCb::render` expands a node-wide release into per-key
/// note-offs for monitoring's own keys before the events get here, because
/// this node also plays the track's clips and a node-wide release would cut
/// a sounding clip note. Only the tests and the non-RT render path still
/// reach this arm — do not "simplify" the engine expansion away on the
/// strength of it.
fn prime_live(tr: &RtTrack, discontinuity: bool, live_in_events: &[LiveMidiEvent]) {
    let Some(live) = &tr.live else { return };
    // SAFETY: RCU discipline — exactly one graph snapshot is rendered at a
    // time, on this (the only RT) thread; see `LiveNodeCell`.
    let node = unsafe { live.node.rt_mut() };
    if discontinuity {
        node.all_notes_off();
    }
    // Hardware MIDI-in for this block, delivered at offset 0 (ruling 9: no
    // sub-block placement in this slice). Velocity 0 IS the note-off
    // convention on this path — see `AbsNoteEvent`'s use in
    // `playback::track_events`.
    for ev in live_in_events {
        match ev.kind {
            EV_ALL_OFF => node.all_notes_off(),
            EV_NOTE_ON => {
                node.queue_event(BlockNoteEvent { offset: 0, key: ev.key, velocity: ev.velocity });
            }
            _ => {
                node.queue_event(BlockNoteEvent { offset: 0, key: ev.key, velocity: 0 });
            }
        }
    }
}

/// ADD one run of the track's live instrument into `buf` (already holding
/// the clip sum). No gain/pan — the shared fader runs after inserts.
fn render_live_into(
    tr: &RtTrack,
    buf: &mut [f32],
    pos: u64,
    run: usize,
    sample_rate: u32,
    discontinuity: bool,
    steady_base: Option<u64>,
) {
    let Some(live) = &tr.live else { return };
    // SAFETY: RCU discipline — exactly one graph snapshot is rendered at a
    // time, on this (the only RT) thread; see `LiveNodeCell`.
    let node = unsafe { live.node.rt_mut() };
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
    node.set_block_context(pos, discontinuity);
    // Round-2 §3.5: the same engine-global steady_time base for every
    // run in this block (a loop wrap can split one callback block into
    // several runs) — non-decreasing is the CLAP contract, not
    // strictly-increasing every call.
    let mut io = ProcessBlock { samples: buf, channels: 2, sample_rate, steady: steady_base };
    node.process(&mut io);
}

/// Walk inserts REPLACE in document order, then apply this track's PDC
/// (Task 6): a shorter path is padded to match the slowest sibling's, so
/// every track lands at the fader in step. True bypass skips `process()`
/// on the plugin but NOT the compensating delay — `compile_pdc` (Task 7)
/// still counts a bypassed insert's latency, so the track must still wait.
fn process_inserts(
    tr: &mut RtTrack,
    buf: &mut [f32],
    sample_rate: u32,
    steady_base: Option<u64>,
) {
    for insert in &tr.inserts {
        if insert.bypassed {
            continue;
        }
        // SAFETY: same RCU contract as `LiveNodeCell` — see `InsertNodeCell`.
        let proc = unsafe { insert.proc.rt_mut() };
        let mut io = ProcessBlock { samples: buf, channels: 2, sample_rate, steady: steady_base };
        proc.process(&mut io);
    }
    if let Some(d) = tr.pdc.as_mut() {
        d.process(buf);
    }
}

fn live_all_notes_off(tr: &RtTrack) {
    let Some(live) = &tr.live else { return };
    // SAFETY: RCU discipline — exactly one graph snapshot is rendered at a
    // time, on this (the only RT) thread; see `LiveNodeCell`.
    unsafe { live.node.rt_mut() }.all_notes_off();
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
    render_impl(
        graph,
        base_pos,
        lp,
        out,
        out_ch,
        sample_rate,
        discontinuity,
        None,
        None,
        None,
        meter_tx,
    )
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
        None,
        None,
        meter_tx,
    )
}

/// `render_rt` plus hardware MIDI-in. `render_rt`'s own signature is
/// UNCHANGED (it forwards `None`), so every existing caller keeps compiling.
#[allow(clippy::too_many_arguments)]
pub fn render_rt_with_input(
    graph: &mut RtGraph,
    base_pos: u64,
    lp: &LoopSpec,
    out: &mut [f32],
    out_ch: usize,
    sample_rate: u32,
    discontinuity: bool,
    steady_base: u64,
    live_in: Option<LiveInBlock<'_>>,
    meter_tx: Option<&mut rtrb::Producer<RawMeterBlock>>,
) -> u32 {
    render_rt_launch(graph, base_pos, lp, out, out_ch, sample_rate, discontinuity, steady_base, live_in, None, meter_tx)
}

#[allow(clippy::too_many_arguments)]
pub fn render_rt_launch(
    graph: &mut RtGraph,
    base_pos: u64,
    lp: &LoopSpec,
    out: &mut [f32],
    out_ch: usize,
    sample_rate: u32,
    discontinuity: bool,
    steady_base: u64,
    live_in: Option<LiveInBlock<'_>>,
    launch: Option<LaunchPlayhead>,
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
        live_in,
        launch,
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
    live_in: Option<LiveInBlock<'_>>,
    launch: Option<LaunchPlayhead>,
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
    let RtGraph { tracks, track_buf, meter_scratch, track_ramps, .. } = graph;
    let track_ramps: &[super::rt::TrackRamps] = track_ramps;

    // Reset this callback's chunk templates in place (Task 7: preallocated
    // at BUILD time on the control thread; `render` never grows this Vec).
    for (i, chunk) in meter_scratch.iter_mut().enumerate() {
        *chunk = RawMeterBlock::new(generation, base_pos, frames as u32);
        chunk.base_slot = (i * METER_CHUNK_SLOTS) as u32;
    }

    for tr in tracks.iter_mut() {
        if tr.slot >= n_slots {
            continue;
        }
        let gain = f32::from_bits(params.gain[tr.slot].load(Relaxed));
        let pan = f32::from_bits(params.pan[tr.slot].load(Relaxed));
        let flags = params.flags[tr.slot].load(Relaxed);
        let flagged = flags & FLAG_LAUNCH != 0;
        let exclusive = launch.as_ref().is_some_and(|ov| ov.exclusive && !ov.ended);
        let overlaying = flagged && launch.is_some_and(|ov| !ov.ended);
        let ending = flagged && launch.is_some_and(|ov| ov.ended);
        let on = if exclusive && !flagged {
            false
        } else {
            audible_with_launch(
                flags & FLAG_MUTE != 0,
                flags & FLAG_SOLO != 0,
                any_solo,
                flagged,
            )
        };
        let (track_base, track_lp, track_disc) = if overlaying {
            let ov = launch.unwrap();
            (ov.pos, &LoopSpec::OFF, ov.discontinuity)
        } else if ending {
            (base_pos, lp, true)
        } else {
            (base_pos, lp, discontinuity)
        };
        let (gl_atomic, gr_atomic) = pan_gains(pan);
        let mut acc = TrackAccum::default();

        // Track D: this snapshot's compiled gain automation for the slot.
        // RT-safe: a slice read + an index walk, no allocation, no locks.
        let ramps = track_ramps.get(tr.slot);
        let ramp: &[AbsParamEvent] = ramps
            .and_then(|t| t.gain.as_ref())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        let mut clip_ramp = RampCursor::new();

        let pan_gains_quad = pan_gain_quad(
            ramps.and_then(|t| t.pan.as_ref()).map(|a| a.as_slice()),
            pan,
            (gl_atomic, gr_atomic),
            frame_pos(track_base, 0, track_lp),
            frame_pos(track_base, frames.saturating_sub(1) as u64, track_lp),
        );
        let pan_last = frames.saturating_sub(1);
        let fader = FaderCtx { gain, ramp, pan: pan_gains_quad, on };

        let live_in_events = live_in.filter(|b| b.slot == tr.slot).map(|b| b.events).unwrap_or(&[]);
        prime_live(tr, track_disc, live_in_events);

        // Unified strip, in runs of MAX_LIVE_BLOCK so track_buf stays
        // preallocated. A loop wrap is a run boundary (`frame_pos` / LoopSpec).
        let mut run_discontinuity = track_disc;
        let mut f = 0usize;
        while f < frames {
            let pos = frame_pos(track_base, f as u64, track_lp);
            let mut run = (frames - f).min(MAX_LIVE_BLOCK);
            let mut wraps = false;
            if track_lp.active() && pos < track_lp.end {
                let to_end = (track_lp.end - pos) as usize;
                if to_end <= run {
                    run = to_end;
                    wraps = true;
                }
            }
            if run == 0 || track_buf.len() < run * 2 {
                break;
            }
            let buf = &mut track_buf[..run * 2];
            buf.fill(0.0);
            if !tr.clips.is_empty() {
                for i in 0..run {
                    let p = frame_pos(track_base, (f + i) as u64, track_lp);
                    let mut l = 0.0f32;
                    let mut r = 0.0f32;
                    for clip in &tr.clips {
                        let s = clip_sample(clip, p);
                        l += s[0];
                        r += s[1];
                    }
                    buf[i * 2] = l;
                    buf[i * 2 + 1] = r;
                }
            }
            render_live_into(tr, buf, pos, run, sample_rate, run_discontinuity, steady_base);
            process_inserts(tr, buf, sample_rate, steady_base);
            apply_fader(buf, run, f, pos, &fader, pan_last, &mut clip_ramp, out, out_ch, &mut acc);
            if wraps {
                live_all_notes_off(tr);
                run_discontinuity = true;
            } else {
                run_discontinuity = false;
            }
            f += run;
        }

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

/// Render ONLY the live-in target track's instrument — no clips, no
/// scheduled note events, no transport advance. This is what a STOPPED
/// transport renders so monitoring is audible without playback: the
/// scheduled-event slice would otherwise replay the clip's own notes.
///
/// Same strip as `render_impl` minus clips (monitoring through FX). It
/// never reads the track's own `live.events` (the pre-scheduled clip notes)
/// and never advances a position — `base_pos` is handed to every run
/// unchanged, because a stopped transport has nowhere to advance to.
#[allow(clippy::too_many_arguments)]
pub fn render_live_input_only(
    graph: &mut RtGraph,
    base_pos: u64,
    out: &mut [f32],
    out_ch: usize,
    sample_rate: u32,
    steady_base: u64,
    live_in: LiveInBlock<'_>,
    meter_tx: Option<&mut rtrb::Producer<RawMeterBlock>>,
) -> u32 {
    let out_ch = out_ch.max(1);
    let frames = out.len() / out_ch;
    out.fill(0.0);
    let params = graph.params.clone();
    let any_solo = params.any_solo.load(Relaxed);
    let n_slots = params.len();
    let generation = graph.generation;
    let RtGraph { tracks, track_buf, meter_scratch, track_ramps, .. } = graph;
    let track_ramps: &[super::rt::TrackRamps] = track_ramps;

    for (i, chunk) in meter_scratch.iter_mut().enumerate() {
        *chunk = RawMeterBlock::new(generation, base_pos, frames as u32);
        chunk.base_slot = (i * METER_CHUNK_SLOTS) as u32;
    }

    for tr in tracks.iter_mut() {
        if tr.slot != live_in.slot || tr.slot >= n_slots {
            continue;
        }
        if tr.live.is_none() {
            continue;
        }
        let gain = f32::from_bits(params.gain[tr.slot].load(Relaxed));
        let pan = f32::from_bits(params.pan[tr.slot].load(Relaxed));
        let flags = params.flags[tr.slot].load(Relaxed);
        let on = audible_with_launch(
            flags & FLAG_MUTE != 0,
            flags & FLAG_SOLO != 0,
            any_solo,
            flags & FLAG_LAUNCH != 0,
        );
        let (gl_atomic, gr_atomic) = pan_gains(pan);
        let mut acc = TrackAccum::default();

        let ramps = track_ramps.get(tr.slot);
        let ramp: &[AbsParamEvent] = ramps
            .and_then(|t| t.gain.as_ref())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        let mut clip_ramp = RampCursor::new();
        let pan_gains_quad = pan_gain_quad(
            ramps.and_then(|t| t.pan.as_ref()).map(|a| a.as_slice()),
            pan,
            (gl_atomic, gr_atomic),
            base_pos,
            base_pos.saturating_add(frames.saturating_sub(1) as u64),
        );
        let pan_last = frames.saturating_sub(1);
        let fader = FaderCtx { gain, ramp, pan: pan_gains_quad, on };

        prime_live(tr, false, live_in.events);

        let mut f = 0usize;
        while f < frames {
            let run = (frames - f).min(MAX_LIVE_BLOCK);
            if run == 0 || track_buf.len() < run * 2 {
                break;
            }
            let buf = &mut track_buf[..run * 2];
            buf.fill(0.0);
            // Monitoring: ADD live (no scheduled clip events) then inserts.
            if let Some(live) = &tr.live {
                // SAFETY: RCU discipline — see `LiveNodeCell`.
                let node = unsafe { live.node.rt_mut() };
                node.set_block_context(base_pos, false);
                let mut io = ProcessBlock {
                    samples: buf,
                    channels: 2,
                    sample_rate,
                    steady: Some(steady_base),
                };
                node.process(&mut io);
            }
            process_inserts(tr, buf, sample_rate, Some(steady_base));
            apply_fader(buf, run, f, base_pos + f as u64, &fader, pan_last, &mut clip_ramp, out, out_ch, &mut acc);
            f += run;
        }

        let chunk_idx = tr.slot / METER_CHUNK_SLOTS;
        let lane = tr.slot % METER_CHUNK_SLOTS;
        if let Some(chunk) = meter_scratch.get_mut(chunk_idx) {
            chunk.set_slot_local(lane, acc.pk_l, acc.pk_r, acc.ss_l, acc.ss_r);
        }
    }

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
    use super::super::insert::{GainHalfEffect, InvertEffect, LatencyDummy};
    use super::super::pdc::DelayLine;
    use super::super::rt::ParamTable;
    use super::super::rt::RtClipData;
    use super::super::rt::RtTrack;
    use std::sync::Arc;

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
        assert!(
            audible_with_launch(false, false, true, true),
            "launch target plays while another track is soloed"
        );
        assert!(
            audible_with_launch(true, false, false, true),
            "launch target bypasses its own mute"
        );
    }

    #[test]
    fn launch_overlay_plays_the_scene_not_the_arrangement_playhead() {
        let mut g = one_track_graph(0, clip(100, 0, 4, vec![1.0; 4], 1));
        g.params.set_flag(0, FLAG_LAUNCH, true);
        g.params.set_pan(0, -1.0);
        let mut silent = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut silent, 2);
        assert!(
            silent.iter().all(|s| *s == 0.0),
            "without overlay the clip is off the arrangement playhead"
        );
        let mut out = vec![0.0f32; 8];
        render_rt_launch(
            &mut g,
            0,
            &LoopSpec::OFF,
            &mut out,
            2,
            48_000,
            false,
            0,
            None,
            Some(LaunchPlayhead { pos: 100, discontinuity: true, exclusive: false, ended: false }),
            None,
        );
        assert!(
            (out[0] - 1.0).abs() < 1e-5,
            "overlay hears the scene at its own position"
        );
    }

    #[test]
    fn exclusive_overlay_silences_tracks_without_the_launch_flag() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);
        let mut out = vec![0.0f32; 8];
        render_rt_launch(
            &mut g,
            0,
            &LoopSpec::OFF,
            &mut out,
            2,
            48_000,
            false,
            0,
            None,
            Some(LaunchPlayhead { pos: 0, discontinuity: false, exclusive: true, ended: false }),
            None,
        );
        assert!(
            out.iter().all(|s| *s == 0.0),
            "parked arrangement must stay silent during stopped preview"
        );
        g.params.set_flag(0, FLAG_LAUNCH, true);
        render_rt_launch(
            &mut g,
            0,
            &LoopSpec::OFF,
            &mut out,
            2,
            48_000,
            false,
            0,
            None,
            Some(LaunchPlayhead { pos: 0, discontinuity: false, exclusive: true, ended: false }),
            None,
        );
        assert!((out[0] - 1.0).abs() < 1e-5, "launched track still plays");
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

    fn insert_on(g: &mut RtGraph, slot: usize, node: Box<dyn crate::audio::dsp::AudioProcessor>, bypassed: bool) {
        use crate::audio::insert::{InsertNode, InsertNodeCell};
        let latency = node.latency_samples();
        let Some(tr) = g.tracks.iter_mut().find(|t| t.slot == slot) else {
            return;
        };
        let n = tr.inserts.len();
        tr.inserts.push(InsertNode {
            slot_id: format!("slot-{n}"),
            instance_id: format!("inst-{n}"),
            bypassed,
            latency,
            proc: InsertNodeCell::new(node),
        });
    }

    #[test]
    fn empty_inserts_are_byte_identical_to_today() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);
        let mut a = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut a, 2);
        // attach an empty inserts vec explicitly and render again
        g.tracks[0].inserts = vec![];
        let mut b = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut b, 2);
        assert_eq!(a, b, "G-11 pin: no inserts must not change a single sample");
    }

    #[test]
    fn insert_sees_the_sum_and_runs_before_the_fader() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_gain_linear(0, 0.5);
        g.params.set_pan(0, -1.0);
        insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        // clip 1.0 → insert 0.5 → fader 0.5 → 0.25 on the left
        assert!((out[0] - 0.25).abs() < 1e-6, "got {}", out[0]);
        assert!(out[1].abs() < 1e-6, "hard left");
    }

    #[test]
    fn two_inserts_run_in_document_order() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);
        insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
        insert_on(&mut g, 0, Box::new(InvertEffect), false);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!((out[0] + 0.5).abs() < 1e-6, "1.0 → half → invert = -0.5, got {}", out[0]);
    }

    #[test]
    fn bypassed_insert_is_true_bypass() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);
        insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), true);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!((out[0] - 1.0).abs() < 1e-6, "bypassed GainHalf must not run, got {}", out[0]);
    }

    // ---- PDC (Plan G1 Task 6): DelayLine attached to RtTrack::pdc lines
    // tracks up at the master after inserts add latency. ----

    fn impulse_clip() -> RtClip {
        let mut data = vec![0.0f32; 512];
        data[0] = 1.0;
        clip(0, 0, 512, data, 1)
    }

    #[test]
    fn two_tracks_line_up_when_one_has_256_samples_of_latency() {
        let mut dummy = LatencyDummy::new(256);
        dummy.prepare(48_000, crate::audio::rt::MAX_LIVE_BLOCK);
        let mut g = RtGraph::new(
            vec![
                RtTrack::clips(0, vec![impulse_clip()]),
                RtTrack::clips(1, vec![impulse_clip()]),
            ],
            1,
            Arc::new(ParamTable::default()),
        );
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        insert_on(&mut g, 0, Box::new(dummy), false);
        g.tracks[0].pdc = None; // wet path: plugin supplies the 256
        g.tracks[1].pdc = Some(DelayLine::new(256, crate::audio::rt::MAX_LIVE_BLOCK, 2));
        let mut out = vec![0.0f32; 512 * 2];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let peak = left
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        assert_eq!(peak.0, 256, "both impulses must land at 256, got {}", peak.0);
        assert!(
            (left[256] - 2.0).abs() < 1e-4,
            "A + B impulses coincide (2.0), got {}",
            left[256]
        );
    }

    #[test]
    fn bypassed_insert_still_contributes_latency() {
        let mut dummy = LatencyDummy::new(256);
        dummy.prepare(48_000, crate::audio::rt::MAX_LIVE_BLOCK);
        let mut g = RtGraph::new(
            vec![
                RtTrack::clips(0, vec![impulse_clip()]),
                RtTrack::clips(1, vec![impulse_clip()]),
            ],
            1,
            Arc::new(ParamTable::default()),
        );
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        insert_on(&mut g, 0, Box::new(dummy), true); // true bypass
        // G-5: compile_pdc still sees 256 on A, so BOTH tracks get 256 of PDC
        // (A's plugin does not delay; A's DelayLine does).
        g.tracks[0].pdc = Some(DelayLine::new(256, crate::audio::rt::MAX_LIVE_BLOCK, 2));
        g.tracks[1].pdc = Some(DelayLine::new(256, crate::audio::rt::MAX_LIVE_BLOCK, 2));
        let mut out = vec![0.0f32; 512 * 2];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert!(
            (left[256] - 2.0).abs() < 1e-4,
            "bypass must not make A early; both at 256. left[0]={} left[256]={}",
            left[0],
            left[256]
        );
        assert!(left[0].abs() < 1e-6, "nothing at t=0");
    }

    #[test]
    fn clips_and_live_are_summed_before_inserts() {
        use super::super::dsp::AudioProcessor;
        use super::super::rt::{LiveNodeCell, LiveSource};
        struct ConstNode(f32);
        impl AudioProcessor for ConstNode {
            fn prepare(&mut self, _: u32, _: usize) {}
            fn process(&mut self, io: &mut ProcessBlock<'_>) {
                for s in io.samples.iter_mut() {
                    *s += self.0;
                }
            }
            fn reset(&mut self) {}
        }
        impl crate::audio::dsp::LiveInstrument for ConstNode {
            fn queue_event(&mut self, _: crate::midi::synth::BlockNoteEvent) -> bool { true }
            fn all_notes_off(&mut self) {}
        }
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![0.25; 4], 1));
        g.tracks[0].live = Some(LiveSource {
            node: LiveNodeCell::new(Box::new(ConstNode(0.25))),
            events: Arc::new(vec![]),
        });
        g.params.set_pan(0, -1.0);
        insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 0.25).abs() < 1e-6,
            "sum 0.50 then half = 0.25 (not per-source half). got {}",
            out[0]
        );
    }

    #[test]
    fn launch_overlay_still_plays_the_scene_through_inserts() {
        let mut g = one_track_graph(0, clip(100, 0, 4, vec![1.0; 4], 1));
        g.params.set_flag(0, FLAG_LAUNCH, true);
        g.params.set_pan(0, -1.0);
        insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
        let mut out = vec![0.0f32; 8];
        render_rt_launch(
            &mut g,
            0,
            &LoopSpec::OFF,
            &mut out,
            2,
            48_000,
            false,
            0,
            None,
            Some(LaunchPlayhead { pos: 100, discontinuity: true, exclusive: false, ended: false }),
            None,
        );
        assert!(
            (out[0] - 0.5).abs() < 1e-5,
            "overlay must hear the scene through the insert, got {}",
            out[0]
        );
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
            inserts: Vec::new(),
            pdc: None,
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
        assert!(!g.track_buf.is_empty(), "graph allocates the strip buffer at build");
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
            inserts: Vec::new(),
            pdc: None,
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

    // ---- live MIDI-in routing (slice 2 audibility core) ------------------

    use crate::audio::midi_in::LiveMidiEvent;

    #[test]
    fn live_in_note_on_is_audible_through_render_rt_with_input() {
        const RATE: u32 = 48_000;
        let mut g = RtGraph::new(vec![live_track(0, Vec::new(), RATE)], 1, Arc::new(ParamTable::default()));
        let mut out = vec![0.0f32; 4_096 * 2];
        // No events queued and no live-in: silence.
        render_rt_with_input(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, 0, None, None);
        assert_eq!(peak(&out), 0.0);
        // A hardware note-on: audible from this block on.
        let evs = [LiveMidiEvent::note_on(69, 110)];
        let n = render_rt_with_input(
            &mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, 0,
            Some(LiveInBlock { slot: 0, events: &evs }), None,
        );
        assert_eq!(n, 0, "no meter chunks dropped without a ring");
        assert!(peak(&out) > 0.02, "routed note is audible");
    }

    /// Pins the mixer's `EV_ALL_OFF` arm, which the engine's RT path no
    /// longer reaches — `OutputCb::render` expands a node-wide release into
    /// per-key note-offs first, so monitoring never cuts a sounding clip
    /// note. This is a primitive-level test, not a live contract.
    #[test]
    fn live_in_all_off_releases_the_routed_voice() {
        const RATE: u32 = 48_000;
        let mut g = RtGraph::new(vec![live_track(0, Vec::new(), RATE)], 1, Arc::new(ParamTable::default()));
        let mut out = vec![0.0f32; 4_096 * 2];
        let on = [LiveMidiEvent::note_on(69, 110)];
        render_rt_with_input(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, 0, Some(LiveInBlock { slot: 0, events: &on }), None);
        let off = [LiveMidiEvent::all_off()];
        render_rt_with_input(&mut g, 4_096, &LoopSpec::OFF, &mut out, 2, RATE, false, 4_096, Some(LiveInBlock { slot: 0, events: &off }), None);
        // Render past the release tail (PolySynth releases in ~80 ms).
        for i in 2..8u64 {
            render_rt_with_input(&mut g, i * 4_096, &LoopSpec::OFF, &mut out, 2, RATE, false, i * 4_096, None, None);
        }
        assert_eq!(peak(&out), 0.0, "all-off released the voice");
    }

    #[test]
    fn live_in_events_go_only_to_the_target_slot() {
        const RATE: u32 = 48_000;
        let mut g = RtGraph::new(
            vec![live_track(0, Vec::new(), RATE), live_track(1, Vec::new(), RATE)],
            1, Arc::new(ParamTable::with_slots(2)),
        );
        let mut out = vec![0.0f32; 4_096 * 2];
        let evs = [LiveMidiEvent::note_on(69, 110)];
        render_rt_with_input(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, 0, Some(LiveInBlock { slot: 1, events: &evs }), None);
        // Mute slot 1 and re-render: if the note had gone to slot 0 it would
        // still sound.
        g.params.set_flag(1, crate::audio::rt::FLAG_MUTE, true);
        let mut out2 = vec![0.0f32; 4_096 * 2];
        render_rt_with_input(&mut g, 4_096, &LoopSpec::OFF, &mut out2, 2, RATE, false, 4_096, None, None);
        assert_eq!(peak(&out2), 0.0, "the routed voice lives on slot 1 only");
    }

    #[test]
    fn render_live_input_only_ignores_scheduled_clip_events() {
        const RATE: u32 = 48_000;
        let scheduled = vec![
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 100 },
            crate::midi::schedule::AbsNoteEvent { sample: 40_000, key: 60, velocity: 0 },
        ];
        let mut g = RtGraph::new(vec![live_track(0, scheduled, RATE)], 1, Arc::new(ParamTable::default()));
        let mut out = vec![0.0f32; 4_096 * 2];
        // Stopped-transport monitoring at position 0: the clip's own note at
        // sample 0 must NOT sound.
        render_live_input_only(&mut g, 0, &mut out, 2, RATE, 0, LiveInBlock { slot: 0, events: &[] }, None);
        assert_eq!(peak(&out), 0.0, "scheduled events never play on the monitor path");
        // A hardware note does sound.
        let evs = [LiveMidiEvent::note_on(69, 110)];
        render_live_input_only(&mut g, 0, &mut out, 2, RATE, 0, LiveInBlock { slot: 0, events: &evs }, None);
        assert!(peak(&out) > 0.02);
    }

    #[test]
    fn render_rt_without_live_in_is_unchanged() {
        // Regression guard for every existing caller: the old entry point still
        // renders scheduled events exactly as before.
        const RATE: u32 = 48_000;
        let events = vec![
            crate::midi::schedule::AbsNoteEvent { sample: 1_000, key: 69, velocity: 110 },
            crate::midi::schedule::AbsNoteEvent { sample: 9_000, key: 69, velocity: 0 },
        ];
        let mut g = RtGraph::new(vec![live_track(0, events, RATE)], 1, Arc::new(ParamTable::default()));
        let mut out = vec![0.0f32; 4_096 * 2];
        render_rt(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, 0, None);
        assert!(peak(&out) > 0.02);
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
    /// so one ramp covers both source kinds. A note is actually triggered
    /// (an empty event list would render silence regardless of gain, making
    /// the assertion pass no matter what the ramp code does) and the ramped
    /// render is checked frame-by-frame against an un-ramped control render
    /// of the identical note, scaled by the lane's own `value_at`.
    #[test]
    fn track_gain_ramp_scales_live_output_too() {
        use crate::plugins::automation::AbsParamEvent;
        const RATE: u32 = 48_000;
        let events = vec![AbsNoteEvent { sample: 0, key: 69, velocity: 110 }];

        let mut control = RtGraph::new(
            vec![live_track(0, events.clone(), RATE)],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        let mut control_out = vec![0.0f32; 512 * 2];
        render(&mut control, 0, &LoopSpec::OFF, &mut control_out, 2, RATE, false, None);
        assert!(peak(&control_out) > 0.01, "control note is audible");

        let mut g = RtGraph::new(
            vec![live_track(0, events, RATE)],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 1.0 },
            AbsParamEvent { sample: 512, value: 0.0 },
        ]);
        g.set_gain_ramps(vec![Some(ev.clone())]);
        let mut out = vec![0.0f32; 512 * 2];
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, None);

        for (i, (frame, cframe)) in out.chunks_exact(2).zip(control_out.chunks_exact(2)).enumerate() {
            let want = crate::plugins::automation::value_at(&ev, i as u64).unwrap();
            for ch in 0..2 {
                let expected = cframe[ch] * want;
                assert!(
                    (frame[ch] - expected).abs() < 1e-4,
                    "frame {i} ch {ch}: got {} want {expected} (control {}, want-gain {want})",
                    frame[ch],
                    cframe[ch]
                );
            }
        }
        assert!(
            peak(&out) < peak(&control_out),
            "the closing ramp must leave the live path quieter than the unramped control"
        );
    }

    // ---- Task 6: per-track pan ramps (block-boundary lerp of (gl, gr)) ----

    #[test]
    fn a_pan_ramp_moves_energy_from_left_to_right_across_the_block() {
        use crate::plugins::automation::AbsParamEvent;
        use super::super::rt::TrackRamps;
        // Native pan 0.0 (center) -> 1.0 (hard right). First frames keep
        // more energy in L than the last; last frames are louder in R.
        // Block-boundary lerp of (gl, gr) stays within 1 dB of constant
        // power on this sweep (a hard-L→hard-R lerp would not — the chord
        // dips ~3 dB; that is why the table is not per-sample pan_gains).
        const N: usize = 1024;
        let data = Arc::new(RtClipData { channels: 1, data: vec![1.0; N] });
        let clip = RtClip {
            start: 0, offset: 0, len: N as u64, gain: 1.0,
            fade_in: 0, fade_out: 0, samples: data,
        };
        let mut g = RtGraph::new(
            vec![RtTrack::clips(0, vec![clip])],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 0.0 },
            AbsParamEvent { sample: (N as u64) - 1, value: 1.0 },
        ]);
        g.set_track_ramps(vec![TrackRamps { gain: None, pan: Some(ev) }]);

        let mut out = vec![0.0f32; N * 2];
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);

        let first_l: f32 = out[..16].iter().step_by(2).map(|s| s.abs()).sum();
        let first_r: f32 = out[1..16].iter().step_by(2).map(|s| s.abs()).sum();
        let last = &out[(N - 8) * 2..];
        let last_l: f32 = last.iter().step_by(2).map(|s| s.abs()).sum();
        let last_r: f32 = last.iter().skip(1).step_by(2).map(|s| s.abs()).sum();
        assert!(first_l > last_l, "first frames louder in L than the last: {first_l} vs {last_l}");
        assert!(last_r > last_l, "last frames louder in R: L={last_l} R={last_r}");
        assert!(last_r > first_r, "energy moved right: first R={first_r} last R={last_r}");
        assert!(first_l >= first_r * 0.9, "start is left-of-or-at center: L={first_l} R={first_r}");

        for (i, frame) in out.chunks_exact(2).enumerate() {
            let e = frame[0] * frame[0] + frame[1] * frame[1];
            let db = 10.0 * e.max(1e-12).log10();
            assert!(
                db.abs() < 1.0,
                "frame {i}: energy {e} is {db} dB off unity (constant-power)"
            );
        }
    }

    #[test]
    fn pan_ramp_absent_leaves_the_atomic_pan_authoritative() {
        // no ramp => byte-identical output to today's path
        const N: usize = 256;
        let data = Arc::new(RtClipData { channels: 1, data: vec![1.0; N] });
        let clip = RtClip {
            start: 0, offset: 0, len: N as u64, gain: 1.0,
            fade_in: 0, fade_out: 0, samples: data,
        };
        let mut g = RtGraph::new(
            vec![RtTrack::clips(0, vec![clip])],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        g.params.set_pan(0, 0.4);
        let mut out = vec![0.0f32; N * 2];
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        let (gl, gr) = pan_gains(0.4);
        for (i, frame) in out.chunks_exact(2).enumerate() {
            assert_eq!(
                frame[0], gl,
                "frame {i} L must be the atomic pan, not a defaulted ramp"
            );
            assert_eq!(
                frame[1], gr,
                "frame {i} R must be the atomic pan, not a defaulted ramp"
            );
        }
    }
}
