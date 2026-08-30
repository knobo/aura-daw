//! Mixer math and the RT render function.
//!
//! `render` is called from the cpal output callback: it must not allocate,
//! lock, or syscall. All inputs are preallocated (`RtGraph`, incl. the live
//! scratch buffer) or atomics (`ParamTable`); meter results are returned by
//! value as a POD block.
//!
//! Track strip (Plan G1): per run of `MAX_LIVE_BLOCK`, a track zeros
//! `track_buf`, mixes clips (no fader), ADDs its LIVE instrument when
//! present, walks inserts REPLACE in document order, applies this track's
//! PDC (`RtTrack::pdc`, Task 6 — pads a shorter path to match the slowest
//! sibling's, see `audio::pdc`), then applies the shared gain/pan/mute
//! fader into `out`. Live nodes are fed pre-scheduled absolute-sample note
//! events (sliced per run, converted to block offsets — zero ticks, zero
//! allocation on this thread).

use std::sync::atomic::Ordering::Relaxed;

use super::dsp::ProcessBlock;
use super::meters::{RawMeterBlock, METER_CHUNK_SLOTS};
use super::midi_in::{LiveMidiEvent, EV_ALL_OFF, EV_NOTE_ON};
use super::clock::{ClockTable, TRANSPORT_CLOCK};
use super::rt::{RtClip, RtGraph, RtSend, RtTrack, FLAG_MUTE, FLAG_SOLO, MAX_LIVE_BLOCK};
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

/// BALANCE law, used by bus/return strips (Plan G2): center = UNITY on both
/// channels, moving off center attenuates the far side to zero.
///
/// Why a return does not use [`pan_gains`]: a bus's input is an already-
/// panned stereo sum, not a mono source being placed in the field.
/// Constant-power would take another 3 dB off it at center, so a send at
/// unity would come back 3 dB down and "0 dB send into a 0 dB return"
/// would not mean what every mixing desk means by it. This is the same
/// distinction consoles draw between a channel's PAN and a stereo
/// channel's BALANCE.
#[inline]
pub fn balance_gains(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
}

/// Solo/mute resolution: when any track is soloed only soloed tracks sound;
/// mute always silences its own track (mute wins over its own solo, and
/// over a node on its own clock — an explicitly muted track stays silent
/// even while it drives a launched scene). `own_clock` only bypasses
/// another track's solo, so a launched scene stays audible while
/// auditioning across a solo elsewhere.
#[inline]
pub fn audible(muted: bool, soloed: bool, any_solo: bool) -> bool {
    audible_with_launch(muted, soloed, any_solo, false)
}

/// `own_clock` = "this node reads a clock of its own, not the transport"
/// (Plan V — V2). Same predicate the deleted `FLAG_LAUNCH` bit carried,
/// stated in the vocabulary that now owns it: a pad that goes silent
/// because someone soloed a vocal is the deck cutting out mid-performance.
#[inline]
pub fn audible_with_launch(muted: bool, soloed: bool, any_solo: bool, own_clock: bool) -> bool {
    !muted && (own_clock || !any_solo || soloed)
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
///
/// `law` is the pan law to evaluate a ramp point under — [`pan_gains`] for a
/// source strip, [`balance_gains`] for a bus (Plan G2).
fn pan_gain_quad(
    pan_ramp: Option<&[AbsParamEvent]>,
    pan: f32,
    atomic: (f32, f32),
    first_pos: u64,
    last_pos: u64,
    law: fn(f32) -> (f32, f32),
) -> PanGains {
    match pan_ramp {
        Some(events) if !events.is_empty() => {
            let p0 = value_at(events, first_pos).unwrap_or(pan);
            let p1 = value_at(events, last_pos).unwrap_or(pan);
            let (a, b) = (law(p0), law(p1));
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
    /// This track's PDC delay in samples (0 when no compensating delay). The
    /// strip's `DelayLine` shifts the audio by this many samples AFTER the
    /// insert chain, so the fader/pan automation ramps applied here must read
    /// the SOURCE position (`pos + i - pdc_delay`) — otherwise an automated
    /// move on a latency-compensated track would be heard `pdc_delay` samples
    /// early (see `audio::pdc`).
    pdc_delay: u64,
}

/// Shared fader: `gain * ramp * pan`, mute, REPLACE into `post`, fold
/// meters. One implementation for clips, live, and the insert chain.
///
/// Plan G2 moved the destination from `out` to a stereo `post` scratch. Two
/// things need the faded signal before it reaches the master: a POST-fader
/// send has to tap it, and `RtTrack::out_pdc` has to delay it (after
/// that tap) so the dry path lands in step with the returns. Neither is
/// expressible while the fader is adding straight into a mixed-down,
/// `out_ch`-wide master buffer. [`mix_post_into`] does the adding.
#[allow(clippy::too_many_arguments)]
fn apply_fader_into(
    buf: &[f32],
    run: usize,
    f: usize,
    pos: u64,
    ctx: &FaderCtx<'_>,
    pan_last: usize,
    ramp_cursor: &mut RampCursor,
    post: &mut [f32],
    acc: &mut TrackAccum,
) {
    for i in 0..run {
        let g = ctx.gain
            * ramp_cursor
                .value(ctx.ramp, pos.saturating_add(i as u64).saturating_sub(ctx.pdc_delay))
                .unwrap_or(1.0);
        let (gl, gr) = lerp_pan(ctx.pan.gl0, ctx.pan.gr0, ctx.pan.gl1, ctx.pan.gr1, f + i, pan_last);
        let mut l = buf[i * 2] * g * gl;
        let mut r = buf[i * 2 + 1] * g * gr;
        if !ctx.on {
            l = 0.0;
            r = 0.0;
        }
        acc.fold(l, r);
        post[i * 2] = l;
        post[i * 2 + 1] = r;
    }
}

/// Add a stereo `post` run into the `out_ch`-wide master at frame `f`.
#[inline]
fn mix_post_into(out: &mut [f32], f: usize, out_ch: usize, post: &[f32], run: usize) {
    for i in 0..run {
        mix_out(out, f + i, out_ch, post[i * 2], post[i * 2 + 1]);
    }
}

/// Peel one send's copy of `src` into its bus accumulator (Plan G2).
///
/// `at` is the offset in FRAMES into the current window. The edge's own
/// compensating delay runs on the copy in `scratch`, never on the strip
/// buffer — the dry signal must not inherit a wait that belongs to one of
/// its copies.
///
/// The delay line is fed even at zero amount. A silent block still has to
/// travel through it, or the moment the knob comes back up the line replays
/// whatever it was holding from before the silence.
#[inline]
fn tap_into_bus(
    send: &mut RtSend,
    bus_buf: &mut [f32],
    scratch: &mut [f32],
    at: usize,
    src: &[f32],
    run: usize,
    amount: f32,
) {
    let n = run * 2;
    if scratch.len() < n {
        return;
    }
    let base = send.bus * MAX_LIVE_BLOCK * 2 + at * 2;
    if bus_buf.len() < base + n {
        return;
    }
    let copy = &mut scratch[..n];
    copy.copy_from_slice(&src[..n]);
    if let Some(d) = send.delay.as_mut() {
        d.process(copy);
    }
    if amount == 0.0 {
        return;
    }
    for i in 0..n {
        bus_buf[base + i] += copy[i] * amount;
    }
}

/// Send a strip's finished output on to where it is ROUTED: the master
/// (`None`) or a bus accumulator. The bus case is the difference between an
/// output and a send — nothing of this signal also reaches the master.
#[inline]
fn route_out(
    output: Option<usize>,
    out: &mut [f32],
    out_ch: usize,
    bus_buf: &mut [f32],
    f: usize,
    at: usize,
    post: &[f32],
    run: usize,
) {
    match output {
        None => mix_post_into(out, f, out_ch, post, run),
        Some(bus) => {
            let base = bus * MAX_LIVE_BLOCK * 2 + at * 2;
            let n = run * 2;
            if bus_buf.len() < base + n {
                return;
            }
            for i in 0..n {
                bus_buf[base + i] += post[i];
            }
        }
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

/// What one run of the strip is, for the live node that renders into it. A
/// callback block is split into runs by the loop wrap and by
/// `MAX_LIVE_BLOCK`, so `pos`, `run` and `discontinuity` all vary WITHIN a
/// block; `flushing` is a property of the row and does not.
#[derive(Clone, Copy)]
struct LiveRun {
    /// Absolute engine position of the run's first frame.
    pos: u64,
    /// Frames in the run.
    run: usize,
    /// The playhead jumped into `pos` (seek, loop wrap, launch).
    discontinuity: bool,
    /// The row is draining its tail after its clock stopped, so it has
    /// stopped being FED — see below.
    flushing: bool,
}

/// ADD one run of the track's live instrument into `buf` (already holding
/// the clip sum). No gain/pan — the shared fader runs after inserts.
///
/// `flushing` splits the two halves of this function, and they answer to
/// different rules. A flushing row has stopped being FED but has not stopped
/// SOUNDING (V-17 (b)), so:
///
/// * The event queue is FEEDING, exactly as the clip read is, and is skipped.
///   A flushing row's `pos` is frozen, so an event inside `[pos, pos + run)`
///   would be re-queued once per flush block — a MIDI pad whose clock stops
///   with a note-on at the frozen position would re-trigger that note
///   `ceil(tail_frames / block)` times. That is the same shape as fix round
///   1's re-read clip fragment, through the other source.
/// * `node.process` is NOT skipped: the node's own pipeline is part of what
///   the flush window exists to drain. Skipping it wholesale would truncate a
///   synth's release and replay it at the next press's onset — the very
///   defect the window was added to fix.
///
/// `set_block_context` still runs, and it gets the FROZEN `pos`, because that
/// is where the row's playhead honestly is: it is the automation seam's
/// "absolute base position of this run", and an off clock does not advance.
/// Feeding it a fabricated advancing position would walk the node's ramp
/// cursors across material nobody is playing.
fn render_live_into(
    tr: &RtTrack,
    buf: &mut [f32],
    r: LiveRun,
    sample_rate: u32,
    steady_base: Option<u64>,
) {
    let Some(live) = &tr.live else { return };
    // SAFETY: RCU discipline — exactly one graph snapshot is rendered at a
    // time, on this (the only RT) thread; see `LiveNodeCell`.
    let node = unsafe { live.node.rt_mut() };
    let LiveRun { pos, run, discontinuity, flushing } = r;
    if !flushing {
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

/// Where this node's playhead is for THIS block, whether that position is a
/// discontinuity, and whether the node renders at all.
///
/// Returns `(pos, loop_spec, discontinuity, audible, own_clock,
/// exclusive_idle)`. `exclusive_idle` is "nothing is FEEDING this node this
/// block" — true only in the exclusive-and-off case below, which is a player
/// whose pad is not being pressed. It is not yet "produces nothing": a row
/// stops being fed the moment its clock does, while whatever is still inside
/// its chain and its delay lines has to come out. So the strip stops the
/// CLIP READ on it immediately and skips the row outright only once its tail
/// is out; see the early-out in `render_impl`, and fix round 1 finding 1 for
/// the three things that cost. `own_clock` is "this node reads a clock of its
/// own" — the predicate that
/// bypasses another track's solo, and exactly `clock_of(slot) !=
/// TRANSPORT_CLOCK` (a slot can never hold an out-of-range clock index:
/// `bind_slot` refuses one). Returned rather than re-derived so the strip
/// does not pay a second indexed load for something this call already knows.
///
/// Factored out (Plan
/// G2) because the windowed render needs the answer twice: once in the
/// prologue, to decide whether the live node owes an `all_notes_off` before
/// the block's first run, and again per window, to place the runs. Calling
/// it twice is safe because `ClockTable::begin_block` latched this block's
/// discontinuity once, up front — `playhead` only ever reads that latch.
///
/// Costs one indexed atomic load more than the `FLAG_LAUNCH` test it
/// replaces: the flag test became a clock lookup.
///
/// THREE cases, and the third is the one that keeps this swap
/// behaviour-neutral:
///
/// * The **transport clock** — the arrangement's `LoopSpec` applies, and the
///   clock's `on` flag IS the transport's play state (V-13). "Only launched
///   nodes render while stopped" (`LaunchPlayhead::exclusive`) is now a
///   consequence of that flag, not a special case.
/// * A **running clock of its own** — its loop is the start/end pair the
///   fire recorded and `ClockTable::advance` wraps it, so the arrangement's
///   `LoopSpec` does not apply at all. That is exactly the `&LoopSpec::OFF`
///   the overlay handed a launched track.
/// * A **stopped clock of its own** — the node REJOINS THE ARRANGEMENT until
///   the control plane releases its slot. This is the old `ended` frame
///   (`track_playhead` returned `(base_pos, lp, true)`), and it is load
///   bearing at both ends of a launch: `ControlPlane::fire_scene` binds the
///   slots and then fires, and the release
///   (`GraphTables::release_finished_scenes`, on the drive thread's poll)
///   runs after the clip ends AND after the flush block, so without this
///   case a launched track would drop out for a block at the press and go
///   silent for at least a poll at the end instead of returning to the
///   song. The discontinuity that comes with it is `ClockTable::stop`'s /
///   `advance`'s parting flush — the `all_notes_off` `launch_ended` bought —
///   OR'd with the TRANSPORT's, because this node is reading the transport's
///   position now: a seek or a Play edge in the same block is a jump for it
///   just as it is for every other arrangement node, and dropping it would
///   hang a held note until something else happened to jump.
#[inline]
fn node_playhead(
    clocks: &ClockTable,
    slot: usize,
    base_pos: u64,
    lp: &LoopSpec,
    discontinuity: bool,
) -> (u64, LoopSpec, bool, bool, bool, bool, f32) {
    let ph = clocks.playhead(slot, base_pos, discontinuity);
    if ph.is_transport {
        return (ph.pos, *lp, ph.discontinuity, ph.on, false, false, 1.0);
    }
    if ph.on {
        return (ph.pos, LoopSpec::OFF, ph.discontinuity, true, true, false, ph.gain);
    }
    if ph.exclusive {
        // Plan V — V-2: a PLAYER's slot has no arrangement to rejoin. Its row
        // carries one clip at position 0 (the ephemeral placement), so the
        // fallback below would sound the pad's sample at bar 1 of the song
        // for as long as the transport rolled past it. `on = false` renders
        // it silent instead — but the discontinuity is still delivered, so a
        // player's live node (V3's MIDI sources) gets the `all_notes_off` its
        // clip's end owes it.
        //
        // The last element says the strip may skip this row ENTIRELY, and
        // `on = false` alone was not enough (fix round 1, finding 1): `on` is
        // consumed inside `apply_fader_into`, which zeroes only AFTER the
        // clip read, the inserts and the pre-fader taps have already run. An
        // off clock never advances, so an unpressed pad sat at `pos = 0` and
        // re-read the first `frames` of its own clip every block, forever —
        // feeding its inserts and leaking into its pre-fader buses.
        //
        // The transport's `discontinuity` is OR'd in, which strictly speaking
        // is a flush an idle pad does not owe: it reads its own position, so
        // a seek is not a jump for it. Harmless while player rows carry no
        // live node, and deliberately left that way — TASK 10 makes it live
        // work, and the choice then is between one spurious `all_notes_off`
        // on a silent pad (this) and inventing a second discontinuity
        // channel. A sounding pad is unaffected either way: it takes the
        // `ph.on` branch above, which does NOT OR the transport's in, so a
        // seek can never cut a pad mid-press.
        return (ph.pos, LoopSpec::OFF, ph.discontinuity || discontinuity, false, true, true, ph.gain);
    }
    (
        base_pos,
        *lp,
        ph.discontinuity || discontinuity,
        clocks.transport_on(),
        true,
        false,
        1.0,
    )
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
    // Plan V — V2: this graph's playheads, taken the same way and for the
    // same borrow-check reason as `params`.
    let clocks = graph.clocks.clone();
    // The rate the graph was BUILT at — not this call's `sample_rate`. The
    // `debug_assert` below re-derives every row's tail, and the allowance in
    // it is rate-dependent; checking a row built at one rate against another
    // would fire on the RT thread for a graph that is perfectly correct. One
    // source of truth, and it travels with the graph.
    let graph_rate = graph.rate;
    // ONCE per block, before any `playhead()` call: latch every clock's
    // pending discontinuity. A scene clock is bound to MANY slots, and a
    // per-reader consume would hand the jump to whichever track read first
    // and hang a note on all the others — see `ClockTable::begin_block`.
    //
    // `arm_pending` runs FIRST, so a quantized fire (V-21) that starts in
    // this block has its own discontinuity latched by this block's latch
    // rather than the next one's — one block, one jump, exactly what an
    // immediate fire from the control thread gets.
    clocks.arm_pending(base_pos, frames as u64);
    clocks.begin_block();
    let any_solo = params.any_solo.load(Relaxed);
    let n_slots = params.len();
    let generation = graph.generation;
    let RtGraph {
        tracks,
        buses,
        track_buf,
        post_buf,
        bus_buf,
        tap_buf,
        meter_scratch,
        track_ramps,
        ..
    } = graph;
    let track_ramps: &[super::rt::TrackRamps] = track_ramps;

    // Reset this callback's chunk templates in place (Task 7: preallocated
    // at BUILD time on the control thread; `render` never grows this Vec).
    for (i, chunk) in meter_scratch.iter_mut().enumerate() {
        *chunk = RawMeterBlock::new(generation, base_pos, frames as u32);
        chunk.base_slot = (i * METER_CHUNK_SLOTS) as u32;
    }

    // PROLOGUE (Plan G2). Two things used to be block-lifetime locals inside
    // the per-track body and now have to survive the window loop below:
    //
    //  * the meter fold, because a track is visited once per window;
    //  * the "this run follows a discontinuity" flag, because a loop wrap
    //    landing exactly on a window boundary carries into the next window.
    //
    // `prime_live` moves here for the same reason and a sharper one: it
    // queues THIS BLOCK's hardware MIDI-in events into the node and may fire
    // `all_notes_off`. Left inside the window loop it would deliver every
    // live-in event once per window.
    //
    // It is also the ONE deliberate exception to "an idle pad is fed
    // nothing", and the reason the row loop's early-out sits inside that loop
    // rather than up here: both things `prime_live` delivers must reach a row
    // the strip loop skips — hardware MIDI-in, which is how a pad's
    // instrument is PLAYED from a keyboard with no clock running at all, and
    // the `all_notes_off` a cut pad's discontinuity owes its node, which is
    // precisely what a row that has just stopped no longer runs to collect.
    for tr in tracks.iter_mut() {
        tr.win = super::rt::TrackWindow::default();
        if tr.slot >= n_slots {
            continue;
        }
        let (_, _, track_disc, _, _, _, _) =
            node_playhead(&clocks, tr.slot, base_pos, lp, discontinuity);
        tr.win.disc = track_disc;
        let live_in_events = live_in.filter(|b| b.slot == tr.slot).map(|b| b.events).unwrap_or(&[]);
        prime_live(tr, track_disc, live_in_events);
    }
    for bus in buses.iter_mut() {
        bus.win = super::rt::TrackWindow::default();
    }

    // WINDOWED RENDER (Plan G2). A bus cannot run until every track that
    // sends into it has contributed, so the loops invert: windows outside,
    // tracks inside, then the bus pass. The window is `MAX_LIVE_BLOCK`,
    // which is what bounds `bus_buf` — the RT thread never sizes a buffer.
    //
    // A callback block of `MAX_LIVE_BLOCK` frames or fewer (every real one:
    // cpal quanta here are 128–2048) makes this loop run exactly ONCE, so
    // the strip below is the pre-G2 strip with the send taps added. Only a
    // block larger than the window splits, and then the pan lerp still
    // interpolates over the WHOLE block, because `f`, `pan_last` and the
    // pan quad are all block-global.
    let pan_last = frames.saturating_sub(1);
    let mut w0 = 0usize;
    while w0 < frames {
        let w_len = (frames - w0).min(MAX_LIVE_BLOCK);
        let w_end = w0 + w_len;

        // Every bus starts the window empty; the taps below add into it.
        for b in 0..buses.len() {
            let base = b * MAX_LIVE_BLOCK * 2;
            if let Some(slice) = bus_buf.get_mut(base..base + w_len * 2) {
                slice.fill(0.0);
            }
        }

        for tr in tracks.iter_mut() {
            if tr.slot >= n_slots {
                continue;
            }
            // `RtGraph::with_buses`'s funnel holds only if nothing adds a
            // line to a row AFTER the graph is built. Production never
            // does; test rigs do (assigning `pdc` or pushing an insert onto
            // an already-built graph), and such a row would silently keep
            // `tail_frames = 0` — a flush window of nothing, which is the
            // very defect fix round 3 closed. Debug-only, and an internal
            // invariant rather than data arriving from outside.
            debug_assert_eq!(
                tr.tail_frames,
                tr.computed_tail_frames(graph_rate),
                "row {} carries lines its tail_frames does not know about — \
                 call RtTrack::recompute_tail_frames after changing them",
                tr.slot
            );
            let (track_base, track_lp, _, clock_on, own_clock, exclusive_idle, voice_gain) =
                node_playhead(&clocks, tr.slot, base_pos, lp, discontinuity);
            // Plan V — V-2, the cost half (fix round 1, finding 1). An
            // exclusive-and-off slot is a pad nobody is pressing, and it can
            // produce nothing BY CONSTRUCTION — not "produces something the
            // fader will zero". Everything below it is therefore waste, and
            // three kinds of waste at that: the per-sample clip read (an off
            // clock never advances, so the row re-read the first `frames` of
            // its own sample every block, forever), the insert chain (fed
            // that repeating fragment continuously, so a pad with a reverb
            // fired from polluted state), and the PRE-fader taps, which are
            // deliberately NOT gated by `on` — that gate is for a muted
            // track, whose reverb send should keep ringing, and it made an
            // idle pad leak its own first 10 ms into its bus on every block.
            // That last one is "every pad sounds at bar 1" reached through
            // the send path instead of the master path.
            //
            // Skipping is safe HERE and only here. `prime_live` above already
            // ran, so a cut pad's `all_notes_off` is delivered whatever this
            // branch does (that is why the early-out is not hoisted into the
            // prologue), and `acc` would stay zero, so `tr.win.pk_*.max(0)`
            // and `+= 0` leave the meters exactly where rendering-then-zeroing
            // left them.
            //
            // The rule, and it is ONE rule rather than a list of hazards:
            // NOTHING on this row is skipped until the row's whole tail has
            // left it. A pad that has stopped being TRIGGERED is still
            // SOUNDING for `tail_frames` — its insert chain's pipeline, its
            // source-alignment line, its `out_pdc`, its send edges' delays.
            // None of those has a reset and none of them advances by itself,
            // so a row skipped while it still holds material both TRUNCATES
            // that material and replays it at the onset of the next press.
            // Enumerating the lines by name is how that defect got in twice:
            // the guard named `out_pdc` and the send edges, and missed a
            // plugin's own latency, which is the case that swallows a whole
            // press (a 2048-sample linear-phase EQ over an 800-sample
            // one-shot: the clock is off from block 2 with every real sample
            // still inside the plugin).
            //
            // So while `flush_left` is non-zero the ORDINARY strip runs, with
            // two differences below, and the first of them is one rule rather
            // than one line: the row stops being FED. An off clock does not
            // advance, so every SOURCE reading the frozen position would
            // repeat itself once per flush block — the clip read re-reading
            // the same fragment (fix round 1's defect), and
            // `render_live_into` re-queueing every scheduled event inside the
            // frozen window (see there). What the row already HOLDS is not a
            // source and keeps running: the insert chain, the delay lines,
            // and the live node's own `process`. The second difference is
            // that the fader's gate is the tail rather than the clock. Mute
            // and solo still apply; a muted pad's tail stays muted.
            //
            // Only when the tail is out does the row take the BARE skip —
            // which is what every raw pad takes on every block (V-6 leaves it
            // no inserts, no sends and no `out_pdc`, so `tail_frames` is 0),
            // and what the measured 1.98 µs idle case is.
            //
            // Consequence, stated rather than fixed: an insert with an
            // UNBOUNDED tail — a reverb on a pad — is hard-cut when
            // `flush_left` reaches 0. That is exactly what a transport stop
            // already does to a track's inserts, and "when does a pad stop
            // ringing" is a separate question this does not answer.
            if clock_on {
                tr.flush_left = tr.tail_frames;
            }
            if exclusive_idle && tr.flush_left == 0 {
                continue;
            }
            let flushing = exclusive_idle;
            // V-18: the press's velocity, folded into the fader. This is the
            // one place an audio pad and a MIDI pad converge, and the only
            // one that reaches a RAW pad at all — `raw` compiles to an empty
            // node, so `params.gain` for it is a fixed unity that no
            // per-press value could live in.
            //
            // The LIMIT that comes with the choice: a non-raw pad's own
            // inserts and its PRE-fader sends see the unscaled signal, so a
            // soft press does not hit its own compressor any softer. Making
            // velocity a property of the SOURCE instead would need two
            // mechanisms (a per-sample multiply in `clip_sample` for audio, a
            // note-velocity scale for MIDI) where this is one; per-voice
            // modulation is modulation §8.8's business, and V4's.
            let gain = f32::from_bits(params.gain[tr.slot].load(Relaxed)) * voice_gain;
            let pan = f32::from_bits(params.pan[tr.slot].load(Relaxed));
            let flags = params.flags[tr.slot].load(Relaxed);
            // Two gates, and they used to be three. `clock_on` false is a
            // stopped transport (the old `exclusive`) or a clock that is not
            // running; `own_clock` is what bypasses another track's solo
            // (the old `FLAG_LAUNCH`). Both are now read off the same
            // binding, so they cannot disagree.
            //
            // The second disjunct is the flush window, and it is qualified by
            // `flushing` for a reason: an ordinary track on a STOPPED
            // transport also has `clock_on` false, but nothing stops feeding
            // it — its clip read still runs, at a frozen `base_pos`. Opening
            // its fader for `tail_frames` after a stop would hold whatever
            // sits under the playhead. A stop is a stop; only a pad whose
            // trigger has ended is flushing.
            let on = (clock_on || (flushing && tr.flush_left > 0))
                && audible_with_launch(
                    flags & FLAG_MUTE != 0,
                    flags & FLAG_SOLO != 0,
                    any_solo,
                    own_clock,
                );
            let (gl_atomic, gr_atomic) = pan_gains(pan);
            let mut acc = TrackAccum::default();

            // Track D: this snapshot's compiled gain automation for the slot.
            // RT-safe: a slice read + an index walk, no allocation, no locks.
            let ramps = track_ramps.get(tr.slot);
            let ramp: &[AbsParamEvent] = if params.gain_automation_owner(tr.slot).is_some() {
                &[]
            } else {
                ramps
                    .and_then(|t| t.gain.as_ref())
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
            };
            let mut clip_ramp = RampCursor::new();

            let pdc_delay = tr.pdc.as_ref().map_or(0u64, |d| d.delay() as u64);
            let pan_gains_quad = pan_gain_quad(
                ramps.and_then(|t| t.pan.as_ref()).map(|a| a.as_slice()),
                pan,
                (gl_atomic, gr_atomic),
                frame_pos(track_base, 0, &track_lp).saturating_sub(pdc_delay),
                frame_pos(track_base, frames.saturating_sub(1) as u64, &track_lp)
                    .saturating_sub(pdc_delay),
                pan_gains,
            );
            let fader = FaderCtx { gain, ramp, pan: pan_gains_quad, on, pdc_delay };

            // Unified strip, in runs of MAX_LIVE_BLOCK so track_buf stays
            // preallocated. A loop wrap is a run boundary (`frame_pos` /
            // LoopSpec); so is the window edge.
            let mut f = w0;
            while f < w_end {
                let pos = frame_pos(track_base, f as u64, &track_lp);
                let mut run = (w_end - f).min(MAX_LIVE_BLOCK);
                let mut wraps = false;
                if track_lp.active() && pos < track_lp.end {
                    let to_end = (track_lp.end - pos) as usize;
                    if to_end <= run {
                        run = to_end;
                        wraps = true;
                    }
                }
                if run == 0 || track_buf.len() < run * 2 || post_buf.len() < run * 2 {
                    break;
                }
                let run_disc = tr.win.disc;
                let buf = &mut track_buf[..run * 2];
                buf.fill(0.0);
                if !tr.clips.is_empty() && !flushing {
                    for i in 0..run {
                        let p = frame_pos(track_base, (f + i) as u64, &track_lp);
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
                let live_run = LiveRun { pos, run, discontinuity: run_disc, flushing };
                render_live_into(tr, buf, live_run, sample_rate, steady_base);
                process_inserts(tr, buf, sample_rate, steady_base);
                // PRE-fader taps leave here: post-insert, source-aligned by
                // `RtTrack::pdc`, before the fader and before `out_pdc`.
                // Every source therefore leaves at the same latency, which
                // is what makes most edge delays zero.
                for snd in tr.sends.iter_mut().filter(|s| s.pre_fader) {
                    let amount = params.send_amount_linear(snd.amount);
                    tap_into_bus(snd, bus_buf, tap_buf, f - w0, buf, run, amount);
                }
                let post = &mut post_buf[..run * 2];
                apply_fader_into(buf, run, f, pos, &fader, pan_last, &mut clip_ramp, post, &mut acc);
                // POST-fader taps follow the fader and the mute, which is
                // what a reverb send normally wants — and they leave BEFORE
                // `out_pdc`, so they are aligned with the pre-fader taps.
                for snd in tr.sends.iter_mut().filter(|s| !s.pre_fader) {
                    let amount = params.send_amount_linear(snd.amount);
                    tap_into_bus(snd, bus_buf, tap_buf, f - w0, post, run, amount);
                }
                if let Some(d) = tr.out_pdc.as_mut() {
                    d.process(post);
                }
                // Master, or the bus this track is ROUTED to — the routed
                // signal does not also reach the master. That is the whole
                // difference between an output and a send.
                route_out(tr.output, out, out_ch, bus_buf, f, f - w0, post, run);
                if wraps {
                    live_all_notes_off(tr);
                }
                tr.win.disc = wraps;
                f += run;
            }
            if flushing {
                tr.flush_left = tr.flush_left.saturating_sub(w_len);
            }

            tr.win.pk_l = tr.win.pk_l.max(acc.pk_l);
            tr.win.pk_r = tr.win.pk_r.max(acc.pk_r);
            tr.win.ss_l += acc.ss_l;
            tr.win.ss_r += acc.ss_r;
        }

        // BUS PASS (Plan G2). Every source has contributed, so each bus is
        // now an ordinary strip whose input happens to be an accumulator:
        // inserts (the shared reverb), taps of its own, then fader, meters,
        // and on to wherever IT is routed.
        //
        // `buses` is in TOPOLOGICAL order, so one forward pass is enough: a
        // bus only ever writes into an accumulator later in the vec, which
        // this loop has not read yet.
        for bi in 0..buses.len() {
            let bus = &mut buses[bi];
            if bus.slot >= n_slots {
                continue;
            }
            let base = bi * MAX_LIVE_BLOCK * 2;
            let gain = f32::from_bits(params.gain[bus.slot].load(Relaxed));
            let pan = f32::from_bits(params.pan[bus.slot].load(Relaxed));
            let flags = params.flags[bus.slot].load(Relaxed);
            // A return is silenced by its OWN mute only. Soloing a vocal
            // must not take its reverb with it — that is Live's default and
            // the only one that makes solo usable on a project with returns.
            let on = flags & FLAG_MUTE == 0;
            let (gl_atomic, gr_atomic) = balance_gains(pan);
            let ramps = track_ramps.get(bus.slot);
            let ramp: &[AbsParamEvent] = if params.gain_automation_owner(bus.slot).is_some() {
                &[]
            } else {
                ramps
                    .and_then(|t| t.gain.as_ref())
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
            };
            let mut bus_ramp = RampCursor::new();
            let pdc_delay = bus.out_pdc.as_ref().map_or(0u64, |d| d.delay() as u64);
            let pan_gains_quad = pan_gain_quad(
                ramps.and_then(|t| t.pan.as_ref()).map(|a| a.as_slice()),
                pan,
                (gl_atomic, gr_atomic),
                frame_pos(base_pos, 0, lp).saturating_sub(pdc_delay),
                frame_pos(base_pos, frames.saturating_sub(1) as u64, lp)
                    .saturating_sub(pdc_delay),
                balance_gains,
            );
            let fader = FaderCtx { gain, ramp, pan: pan_gains_quad, on, pdc_delay };
            let mut acc = TrackAccum::default();

            let mut f = w0;
            while f < w_end {
                let run = (w_end - f).min(MAX_LIVE_BLOCK);
                if run == 0 || post_buf.len() < run * 2 || track_buf.len() < run * 2 {
                    break;
                }
                let at = (f - w0) * 2;
                // COPY the accumulated input out into the shared strip
                // scratch before processing. A bus both reads its own
                // accumulator and writes into other buses' accumulators —
                // same Vec — and lifting the read out first is what keeps
                // those two from aliasing, without any index arithmetic to
                // get wrong.
                let strip = &mut track_buf[..run * 2];
                match bus_buf.get(base + at..base + at + run * 2) {
                    Some(src) => strip.copy_from_slice(src),
                    None => break,
                }
                for insert in &bus.inserts {
                    if insert.bypassed {
                        continue;
                    }
                    // SAFETY: same RCU contract as the track chain — see
                    // `InsertNodeCell`.
                    let proc = unsafe { insert.proc.rt_mut() };
                    let mut io = ProcessBlock {
                        samples: strip,
                        channels: 2,
                        sample_rate,
                        steady: steady_base,
                    };
                    proc.process(&mut io);
                }
                for snd in bus.sends.iter_mut().filter(|s| s.pre_fader) {
                    let amount = params.send_amount_linear(snd.amount);
                    tap_into_bus(snd, bus_buf, tap_buf, f - w0, strip, run, amount);
                }
                let pos = frame_pos(base_pos, f as u64, lp);
                let post = &mut post_buf[..run * 2];
                apply_fader_into(strip, run, f, pos, &fader, pan_last, &mut bus_ramp, post, &mut acc);
                for snd in bus.sends.iter_mut().filter(|s| !s.pre_fader) {
                    let amount = params.send_amount_linear(snd.amount);
                    tap_into_bus(snd, bus_buf, tap_buf, f - w0, post, run, amount);
                }
                if let Some(d) = bus.out_pdc.as_mut() {
                    d.process(post);
                }
                route_out(bus.output, out, out_ch, bus_buf, f, f - w0, post, run);
                f += run;
            }

            bus.win.pk_l = bus.win.pk_l.max(acc.pk_l);
            bus.win.pk_r = bus.win.pk_r.max(acc.pk_r);
            bus.win.ss_l += acc.ss_l;
            bus.win.ss_r += acc.ss_r;
        }

        w0 = w_end;
    }

    // Meter lanes, once per block — folded across every window.
    for tr in tracks.iter() {
        if tr.slot >= n_slots {
            continue;
        }
        let chunk_idx = tr.slot / METER_CHUNK_SLOTS;
        let lane = tr.slot % METER_CHUNK_SLOTS;
        if let Some(chunk) = meter_scratch.get_mut(chunk_idx) {
            chunk.set_slot_local(lane, tr.win.pk_l, tr.win.pk_r, tr.win.ss_l, tr.win.ss_r);
        }
    }
    for bus in buses.iter() {
        if bus.slot >= n_slots {
            continue;
        }
        let chunk_idx = bus.slot / METER_CHUNK_SLOTS;
        let lane = bus.slot % METER_CHUNK_SLOTS;
        if let Some(chunk) = meter_scratch.get_mut(chunk_idx) {
            chunk.set_slot_local(lane, bus.win.pk_l, bus.win.pk_r, bus.win.ss_l, bus.win.ss_r);
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
    let clocks = graph.clocks.clone();
    let any_solo = params.any_solo.load(Relaxed);
    let n_slots = params.len();
    let generation = graph.generation;
    let RtGraph { tracks, track_buf, post_buf, meter_scratch, track_ramps, .. } = graph;
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
            clocks.clock_of(tr.slot) != TRANSPORT_CLOCK,
        );
        let (gl_atomic, gr_atomic) = pan_gains(pan);
        let mut acc = TrackAccum::default();

        let ramps = track_ramps.get(tr.slot);
        let ramp: &[AbsParamEvent] = if params.gain_automation_owner(tr.slot).is_some() {
            &[]
        } else {
            ramps
                .and_then(|t| t.gain.as_ref())
                .map(|a| a.as_slice())
                .unwrap_or(&[])
        };
        let mut clip_ramp = RampCursor::new();
        let pdc_delay = tr.pdc.as_ref().map_or(0u64, |d| d.delay() as u64);
        let pan_gains_quad = pan_gain_quad(
            ramps.and_then(|t| t.pan.as_ref()).map(|a| a.as_slice()),
            pan,
            (gl_atomic, gr_atomic),
            base_pos.saturating_sub(pdc_delay),
            base_pos
                .saturating_add(frames.saturating_sub(1) as u64)
                .saturating_sub(pdc_delay),
            pan_gains,
        );
        let pan_last = frames.saturating_sub(1);
        let fader = FaderCtx { gain, ramp, pan: pan_gains_quad, on, pdc_delay };

        prime_live(tr, false, live_in.events);

        let mut f = 0usize;
        while f < frames {
            let run = (frames - f).min(MAX_LIVE_BLOCK);
            if run == 0 || track_buf.len() < run * 2 || post_buf.len() < run * 2 {
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
            // Monitoring has no bus pass and no `out_pdc`: a stopped
            // transport is not mixing returns, so the faded run goes
            // straight from the scratch into the master.
            let post = &mut post_buf[..run * 2];
            apply_fader_into(
                buf,
                run,
                f,
                base_pos + f as u64,
                &fader,
                pan_last,
                &mut clip_ramp,
                post,
                &mut acc,
            );
            mix_post_into(out, f, out_ch, post, run);
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
    use super::super::rt::{RtBus, RtSend, RtTrack};
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
            !audible_with_launch(true, false, false, true),
            "mute silences a launch target too"
        );
    }

    /// The clock table this graph would get for a launch: `n` slots, the
    /// transport plus one scene clock, transport play state as given.
    fn scene_clocks(g: &RtGraph, playing: bool) -> ClockTable {
        let c = ClockTable::with_slots_and_clocks(g.params.len(), 2);
        c.set_transport_playing(playing);
        c
    }

    /// The claim the overlay test made, now made of clocks: a node bound to
    /// a running non-transport clock renders at THAT clock's position, not
    /// the arrangement's.
    #[test]
    fn a_node_on_a_scene_clock_plays_the_scene_not_the_arrangement_playhead() {
        let mut g = one_track_graph(0, clip(100, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);
        let mut silent = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut silent, 2);
        assert!(
            silent.iter().all(|s| *s == 0.0),
            "on the transport clock the clip is off the arrangement playhead"
        );

        let clocks = scene_clocks(&g, true);
        clocks.fire(1, 100, 10_000, false, 1.0);
        clocks.bind_slot(0, 1);
        g.clocks = Arc::new(clocks);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 1.0).abs() < 1e-5,
            "the scene sounds at its own clock's position, got {}",
            out[0]
        );
    }

    /// V-13: with the transport stopped, a transport-clock node renders
    /// nothing while a fired clock still sounds. This is what
    /// `LaunchPlayhead::exclusive` used to say, and it is now a consequence
    /// of clock 0's `on` flag rather than a separate concept.
    #[test]
    fn a_stopped_transport_silences_arrangement_nodes_but_not_a_fired_one() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);

        g.clocks = Arc::new(scene_clocks(&g, false));
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            out.iter().all(|s| *s == 0.0),
            "parked arrangement must stay silent while the transport is stopped"
        );

        let clocks = scene_clocks(&g, false);
        clocks.fire(1, 0, 10_000, false, 1.0);
        clocks.bind_slot(0, 1);
        g.clocks = Arc::new(clocks);
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 1.0).abs() < 1e-5,
            "only the fired node contributes, got {}",
            out[0]
        );
    }

    /// The other half of `LaunchPlayhead::ended`: a scene that stops does
    /// not silence its tracks — they rejoin the ARRANGEMENT until the
    /// control plane releases their slots a poll later, and they get one
    /// discontinuity on the way so a held note is released.
    #[test]
    fn a_stopped_scene_clock_returns_its_nodes_to_the_arrangement() {
        let mut g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);
        let clocks = scene_clocks(&g, true);
        clocks.fire(1, 500, 10_000, false, 1.0); // far from the clip
        clocks.bind_slot(0, 1);
        g.clocks = Arc::new(clocks);

        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(out.iter().all(|s| *s == 0.0), "the scene is past the clip");

        g.clocks.stop(1); // Escape, or the clip reaching its end

        // The flush frame `launch_ended` used to carry: `begin_block`
        // latches the stop's discontinuity and every node still bound to the
        // clock reads it, which is what makes the live node all-notes-off in
        // `render_impl`'s prologue instead of hanging the cut voice.
        g.clocks.begin_block();
        let (pos, _, disc, on, own, _, _) = node_playhead(&g.clocks, 0, 0, &LoopSpec::OFF, false);
        assert!(own, "still bound to the scene clock, so still past a solo");
        assert_eq!(pos, 0, "back on the arrangement playhead");
        assert!(on, "and audible: the transport is still running");
        assert!(disc, "the return is a discontinuity");

        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 1.0).abs() < 1e-5,
            "and it is the arrangement that sounds, got {}",
            out[0]
        );
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
        // This helper adds a line to a row that is already built, which
        // production never does — `RtGraph::with_buses` is the funnel. The
        // row's flush window has to follow, or `render_impl`'s
        // `debug_assert` catches the rig rather than the code.
        tr.recompute_tail_frames(48_000);
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
        for tr in g.tracks.iter_mut() {
            tr.recompute_tail_frames(48_000);
        }
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
        for tr in g.tracks.iter_mut() {
            tr.recompute_tail_frames(48_000);
        }
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

    // ---- sends and bus returns (Plan G2) ----

    /// A graph with returns and ONE send lane. The lane count matters:
    /// `ParamTable::default()` has zero of them, and an unknown send index
    /// reads back as unity — which would make every amount assertion below
    /// pass for the wrong reason.
    fn graph_with_bus(tracks: Vec<RtTrack>, buses: Vec<RtBus>) -> RtGraph {
        RtGraph::with_buses(tracks, buses, 1, Arc::new(ParamTable::with_slots_and_sends(64, 1)), 48_000)
    }

    fn empty_bus(slot: usize) -> RtBus {
        RtBus {
            slot,
            inserts: Vec::new(),
            sends: Vec::new(),
            output: None,
            out_pdc: None,
            win: Default::default(),
        }
    }

    /// Unity send into an empty (pass-through) bus: the master hears the dry
    /// signal AND the return, i.e. exactly twice the dry signal. This is the
    /// end-to-end shape of "one reverb, many sources" with the reverb
    /// removed, so nothing but the routing is under test.
    #[test]
    fn a_unity_send_into_an_empty_bus_doubles_the_signal() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: false, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_send_amount_linear(0, 1.0);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!((out[0] - 2.0).abs() < 1e-6, "dry + unity return = 2.0, got {}", out[0]);
    }

    /// A CENTRED return comes back at the amount you dialled — no hidden
    /// 3 dB. The bus uses the balance law, not constant-power: its input is
    /// an already-panned stereo sum, and taking another 3 dB off it would
    /// make "unity send into a unity return" quieter than the dry signal.
    #[test]
    fn a_centred_return_is_not_attenuated_by_the_pan_law() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: false, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        // Both strips centred (the default). The track pays the
        // constant-power -3 dB once, at its own fader; the return carries
        // that already-panned signal and must not pay it a second time.
        g.params.set_send_amount_linear(0, 1.0);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let dry = std::f32::consts::FRAC_1_SQRT_2; // centred track
        assert!(
            (out[0] - 2.0 * dry).abs() < 1e-6,
            "dry {dry} + an equally loud return, got {}",
            out[0]
        );
    }

    /// A PRE-fader tap is pre-pan as well as pre-gain — it leaves the strip
    /// before the fader does anything at all. Pinned because "pre-fader"
    /// reads like it might only mean pre-GAIN.
    #[test]
    fn a_pre_fader_tap_is_pre_pan_too() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: true, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_send_amount_linear(0, 1.0);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let dry = std::f32::consts::FRAC_1_SQRT_2;
        assert!(
            (out[0] - (dry + 1.0)).abs() < 1e-6,
            "centred dry {dry} + an UNPANNED 1.0 tap, got {}",
            out[0]
        );
    }

    #[test]
    fn balance_law_is_unity_at_centre_and_one_sided_at_the_extremes() {
        let (l, r) = balance_gains(0.0);
        assert!((l - 1.0).abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
        let (l, r) = balance_gains(-1.0);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
        let (l, r) = balance_gains(1.0);
        assert!(l.abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
    }

    /// The amount is a live parameter, not a rebuild: writing the atomic
    /// between two renders changes the next block and nothing else.
    #[test]
    fn the_send_amount_scales_the_return_without_a_rebuild() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: false, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_send_amount_linear(0, 0.25);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!((out[0] - 1.25).abs() < 1e-6, "dry + 0.25 return, got {}", out[0]);
        g.params.set_send_amount_linear(0, 0.0);
        let mut out2 = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out2, 2);
        assert!((out2[0] - 1.0).abs() < 1e-6, "return off, dry only, got {}", out2[0]);
    }

    /// An insert on the BUS processes the return and leaves the dry path
    /// alone — the whole point of a shared effect.
    #[test]
    fn a_bus_insert_processes_only_the_return() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: false, delay: None });
        let mut bus = empty_bus(1);
        bus.inserts.push(crate::audio::insert::InsertNode {
            slot_id: "s".into(),
            instance_id: "i".into(),
            bypassed: false,
            latency: 0,
            proc: crate::audio::insert::InsertNodeCell::new(Box::new(GainHalfEffect {
                bypassed: false,
            })),
        });
        let mut g = graph_with_bus(vec![track], vec![bus]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_send_amount_linear(0, 1.0);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!((out[0] - 1.5).abs() < 1e-6, "dry 1.0 + halved return 0.5, got {}", out[0]);
    }

    /// Post-fader (the default) means the mute takes the send with it;
    /// pre-fader means the return survives pulling the dry signal out. Both
    /// tap BEFORE `out_pdc`, so they stay aligned with each other.
    #[test]
    fn a_muted_track_still_feeds_a_pre_fader_send_but_not_a_post_fader_one() {
        for (pre_fader, expected) in [(false, 0.0f32), (true, 1.0)] {
            let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
            track.sends.push(RtSend { bus: 0, amount: 0, pre_fader, delay: None });
            let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
            g.params.set_pan(0, -1.0);
            g.params.set_pan(1, -1.0);
            g.params.set_send_amount_linear(0, 1.0);
            g.params.set_flag(0, FLAG_MUTE, true);
            let mut out = vec![0.0f32; 8];
            render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
            assert!(
                (out[0] - expected).abs() < 1e-6,
                "pre_fader={pre_fader}: expected {expected}, got {}",
                out[0]
            );
        }
    }

    /// Soloing a track must NOT take the returns with it — solo the vocal
    /// and you still hear its reverb. A bus answers to its own mute only.
    #[test]
    fn a_return_stays_audible_under_someone_else_s_solo() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: false, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_send_amount_linear(0, 1.0);
        g.params.set_flag(0, FLAG_SOLO, true);
        g.params.any_solo.store(true, Relaxed);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!((out[0] - 2.0).abs() < 1e-6, "dry + return under solo, got {}", out[0]);

        g.params.set_flag(1, FLAG_MUTE, true);
        let mut muted = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut muted, 2);
        assert!((muted[0] - 1.0).abs() < 1e-6, "muting the bus DOES silence it, got {}", muted[0]);
    }

    /// A latency-reporting insert on the bus makes the return late. The dry
    /// path waits for it via `out_pdc`, so the two land on the same
    /// sample instead of smearing into a flam.
    #[test]
    fn a_latency_carrying_return_and_the_dry_path_land_together() {
        let mut dummy = LatencyDummy::new(256);
        dummy.prepare(48_000, crate::audio::rt::MAX_LIVE_BLOCK);
        let mut track = RtTrack::clips(0, vec![impulse_clip()]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: true, delay: None });
        // `compile_routing` would derive this: the slowest return declares
        // 256, so the dry path waits 256 and the bus itself waits nothing.
        track.out_pdc = Some(DelayLine::new(256, crate::audio::rt::MAX_LIVE_BLOCK, 2));
        let mut bus = empty_bus(1);
        bus.inserts.push(crate::audio::insert::InsertNode {
            slot_id: "s".into(),
            instance_id: "i".into(),
            bypassed: false,
            latency: 256,
            proc: crate::audio::insert::InsertNodeCell::new(Box::new(dummy)),
        });
        let mut g = graph_with_bus(vec![track], vec![bus]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_send_amount_linear(0, 1.0);
        let mut out = vec![0.0f32; 1024 * 2];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert!(left[..256].iter().all(|v| v.abs() < 1e-6), "nothing before the compensation");
        assert!(
            (left[256] - 2.0).abs() < 1e-4,
            "dry and return coincide at 256 (2.0), got {}",
            left[256]
        );
    }

    // ---- output routing: a MOVE, not a copy (Plan G2) ----

    /// A track routed into a bus stops reaching the master. This is the
    /// whole difference between an output and a send, and the reason there
    /// is no "double output" flag: routing IS the single-path case.
    #[test]
    fn a_routed_track_reaches_the_master_only_through_its_bus() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.output = Some(0);
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 1.0).abs() < 1e-6,
            "exactly one copy arrives, through the bus — got {}",
            out[0]
        );

        // Proof it really is going THROUGH the bus and not around it.
        g.params.set_flag(1, FLAG_MUTE, true);
        let mut muted = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut muted, 2);
        assert!(
            muted[0].abs() < 1e-6,
            "muting the bus silences the routed track entirely — got {}",
            muted[0]
        );
    }

    /// The bus fader is now in the routed track's signal path, so it is a
    /// submix fader: pulling it down takes the group with it.
    #[test]
    fn the_bus_fader_scales_everything_routed_into_it() {
        let mut a = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        let mut b = RtTrack::clips(1, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        a.output = Some(0);
        b.output = Some(0);
        let mut g = graph_with_bus(vec![a, b], vec![empty_bus(2)]);
        for slot in 0..3 {
            g.params.set_pan(slot, -1.0);
        }
        g.params.set_gain_linear(2, 0.5);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!((out[0] - 1.0).abs() < 1e-6, "(1 + 1) * 0.5 = 1.0, got {}", out[0]);
    }

    /// A drum bus into a mix bus. `buses` arrives in topological order, so
    /// one forward pass carries the signal all the way down — and a bus's
    /// own insert runs on everything routed into it.
    #[test]
    fn a_bus_routed_into_another_bus_is_carried_by_one_forward_pass() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.output = Some(0); // -> drum bus
        let mut drums = empty_bus(1);
        drums.output = Some(1); // -> mix bus, LATER in the vec
        let mut mix = empty_bus(2);
        mix.inserts.push(crate::audio::insert::InsertNode {
            slot_id: "s".into(),
            instance_id: "i".into(),
            bypassed: false,
            latency: 0,
            proc: crate::audio::insert::InsertNodeCell::new(Box::new(GainHalfEffect {
                bypassed: false,
            })),
        });
        let mut g = graph_with_bus(vec![track], vec![drums, mix]);
        for slot in 0..3 {
            g.params.set_pan(slot, -1.0);
        }
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 0.5).abs() < 1e-6,
            "1.0 through the drum bus, halved by the mix bus, got {}",
            out[0]
        );
    }

    /// A send is still a COPY when the source is routed elsewhere: the
    /// track goes through its bus AND a duplicate reaches the reverb.
    #[test]
    fn a_routed_track_can_still_send_a_copy_somewhere_else() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.output = Some(0);
        track.sends.push(RtSend { bus: 1, amount: 0, pre_fader: false, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1), empty_bus(2)]);
        for slot in 0..3 {
            g.params.set_pan(slot, -1.0);
        }
        g.params.set_send_amount_linear(0, 0.5);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 1.5).abs() < 1e-6,
            "1.0 via the group + 0.5 via the send, got {}",
            out[0]
        );
    }

    /// An edge delay is CLOCKED even while its amount is zero, so the edge
    /// stays in time across a knob move. Skip the line on a silent block
    /// and whatever it was holding comes out late — the return would drift
    /// by however long the send sat at zero.
    #[test]
    fn a_send_edge_delay_keeps_running_at_zero_amount() {
        let mut track = RtTrack::clips(0, vec![impulse_clip()]);
        track.sends.push(RtSend {
            bus: 0,
            amount: 0,
            pre_fader: true,
            delay: Some(DelayLine::new(64, crate::audio::rt::MAX_LIVE_BLOCK, 2)),
        });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_gain_linear(0, 0.0); // dry silent, so `out` is the return alone
        g.params.set_send_amount_linear(0, 0.0);

        // The impulse (at sample 0) enters the delay line during these 32
        // frames, while the amount is still zero.
        let mut swallowed = vec![0.0f32; 32 * 2];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut swallowed, 2);
        // Amount comes up. The impulse is still inside the 64-sample line
        // and is due out at ABSOLUTE frame 64 — index 32 of this buffer.
        g.params.set_send_amount_linear(0, 1.0);
        let mut later = vec![0.0f32; 256 * 2];
        render_simple(&mut g, 32, &LoopSpec::OFF, &mut later, 2);
        let left: Vec<f32> = later.iter().step_by(2).copied().collect();
        let peak = left
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        assert_eq!(
            peak.0, 32,
            "the line kept clocking while muted, so the impulse is due at absolute 64"
        );
        assert!((left[32] - 1.0).abs() < 1e-4, "and arrives whole, got {}", left[32]);
    }

    /// The window loop only exists so `bus_buf` can be a fixed size. A block
    /// LARGER than one window must still render every frame exactly once,
    /// with the returns intact — this is the only path that runs the loop
    /// more than once, so nothing else would catch an off-by-one in it.
    #[test]
    fn a_block_longer_than_one_window_still_routes_every_frame() {
        let frames = MAX_LIVE_BLOCK + 777;
        let mut track = RtTrack::clips(0, vec![clip(0, 0, frames as u64, vec![1.0; frames], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: false, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_send_amount_linear(0, 1.0);
        let mut out = vec![0.0f32; frames * 2];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        for (i, v) in out.iter().step_by(2).enumerate() {
            assert!((v - 2.0).abs() < 1e-6, "frame {i}: dry + return = 2.0, got {v}");
        }
    }

    /// The bus writes its OWN meter lane, and the track that fed it keeps
    /// writing its own — a return is a visible strip, not an invisible sum.
    #[test]
    fn a_bus_meters_its_return_on_its_own_slot() {
        let mut track = RtTrack::clips(0, vec![clip(0, 0, 4, vec![1.0; 4], 1)]);
        track.sends.push(RtSend { bus: 0, amount: 0, pre_fader: false, delay: None });
        let mut g = graph_with_bus(vec![track], vec![empty_bus(1)]);
        g.params.set_pan(0, -1.0);
        g.params.set_pan(1, -1.0);
        g.params.set_send_amount_linear(0, 0.5);
        let mut out = vec![0.0f32; 8];
        let blocks = render_with_meters(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let blk = blocks[0];
        assert!((blk.peak[0][0] - 1.0).abs() < 1e-6, "track lane, got {}", blk.peak[0][0]);
        assert!((blk.peak[1][0] - 0.5).abs() < 1e-6, "bus lane, got {}", blk.peak[1][0]);
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

    /// Finding 3: a node that has rejoined the arrangement is reading the
    /// TRANSPORT's position, so the transport's own jumps are its jumps. A
    /// seek in the same block as the scene's death used to be swallowed —
    /// `node_playhead` returned the clock's discontinuity alone — and the
    /// note held across the seek hung until something else happened to jump.
    #[test]
    fn a_rejoined_node_still_hears_the_transports_own_discontinuity() {
        let g = one_track_graph(0, clip(0, 0, 4, vec![1.0; 4], 1));
        let clocks = scene_clocks(&g, true);
        clocks.fire(1, 500, 10_000, false, 1.0);
        clocks.bind_slot(0, 1);
        clocks.stop(1);
        clocks.begin_block(); // consumes the stop's own flush
        assert!(node_playhead(&clocks, 0, 0, &LoopSpec::OFF, false).2, "the flush");

        clocks.begin_block(); // a later block: nothing pending on the clock
        let (_, _, disc, _, _, _, _) = node_playhead(&clocks, 0, 0, &LoopSpec::OFF, false);
        assert!(!disc, "nothing jumped");
        let (_, _, disc, _, _, _, _) = node_playhead(&clocks, 0, 7_000, &LoopSpec::OFF, true);
        assert!(disc, "but the transport's seek is this node's seek too");
    }

    /// Task 10, fix round 2. A cut pad's stranded voices must not land on
    /// the NEXT press.
    ///
    /// TWO PRESSES, because one cannot see it: whatever is stranded in the
    /// node stays stranded for the rest of that press and only surfaces at
    /// the next onset — the same lesson fix round 4 paid for.
    ///
    /// What was measured before the allowance existed: `all_notes_off` only
    /// MARKS a voice released (`midi/synth.rs`, and the same for the sampler
    /// and both plugin hosts — the trait contract at `dsp.rs` says "release,
    /// not hard kill"); the ramp to zero runs inside `process`. A bare pad's
    /// `tail_frames` was 0, so the row took the bare skip and its node was
    /// never processed again — leaving a voice stranded at FULL SUSTAIN
    /// amplitude, which then resumed on top of the next press's onset. Press
    /// 2 began at 0.1096 instead of 0.0 (a step discontinuity at sample 0 —
    /// a click) and peaked at 0.208 against press 1's 0.106.
    #[test]
    fn a_cut_pads_frozen_release_does_not_land_on_the_next_press() {
        use super::super::dsp::AudioProcessor;
        use super::super::rt::{LiveNodeCell, LiveSource, MAX_LIVE_BLOCK};
        use crate::midi::schedule::AbsNoteEvent;
        use crate::midi::synth::PolySynth;

        const FRAMES: usize = 128;
        let params = Arc::new(ParamTable::with_slots_and_sends(1, 0));
        params.set_gain_pair_linear(0, 1.0);
        params.set_pan(0, 0.0);
        let mut synth = PolySynth::new();
        synth.prepare(48_000, MAX_LIVE_BLOCK);
        let mut pad = RtTrack::clips(0, Vec::new());
        pad.live = Some(LiveSource {
            node: LiveNodeCell::new(Box::new(synth)),
            // Held past the pad's end, so the cut lands mid-note.
            events: Arc::new(vec![AbsNoteEvent { sample: 0, key: 69, velocity: 100, channel: 0 }]),
        });
        let mut g = RtGraph::new(vec![pad], 1, params);
        let clocks = crate::audio::clock::ClockTable::with_slots_clocks_and_players(1, 2, 1);
        clocks.set_transport_playing(true);
        clocks.bind_slot(0, 1);
        g.clocks = Arc::new(clocks);

        let peak = |b: &[f32]| b.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let mut out = vec![0.0f32; FRAMES * 2];

        // PRESS 1 — one block long, so the clock ends under a held note.
        g.clocks.fire(1, 0, FRAMES as u64, false, 1.0);
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        let first_press = out.clone();
        assert!(peak(&first_press) > 0.0, "the press has to sound at all");
        assert_eq!(first_press[0], 0.0, "a clean onset attacks from silence");
        g.clocks.advance(FRAMES as u64);

        // The pad is cut. `prime_live` all-notes-offs the node; the flush
        // window is what lets the node RENDER that release instead of
        // freezing mid-note. 40 blocks is 5120 frames — past both the
        // allowance and PolySynth's own 80 ms ramp.
        for _ in 0..40 {
            out.fill(0.0);
            render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
            g.clocks.advance(FRAMES as u64);
        }

        // PRESS 2 — identical clock, identical events, from zero.
        g.clocks.fire(1, 0, FRAMES as u64, false, 1.0);
        out.fill(0.0);
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        assert_eq!(out[0], 0.0, "press 2 attacks from silence too, with no click");
        let worst = first_press
            .iter()
            .zip(out.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 1e-6,
            "the second press is not the same sound as the first: worst frame differs by {worst}"
        );
    }

    /// Task 10, fix round 3 — the case round 2's `max` got wrong.
    ///
    /// The release happens INSIDE the node; its output then has to traverse
    /// the insert chain, the `pdc` and the output branch. Those are in
    /// SERIES, which is exactly why they add rather than dominate. With a
    /// 2048-frame chain the last of a 3840-frame release enters the strip at
    /// 3840 and does not leave until 5888 — still in the pipeline when a
    /// `max(2048, 4096)` window closes at 4096. Stranded in the insert chain,
    /// truncated tail, contaminated next onset: the same pair, a fourth time,
    /// through the one path round 2 left open.
    ///
    /// Two presses again, and the whole press is compared rather than one
    /// block, because a delaying chain makes the first block of a press
    /// silent — a one-block assertion would pass for the wrong reason.
    #[test]
    fn a_cut_pads_insert_chain_is_drained_before_the_next_press() {
        use super::super::dsp::AudioProcessor;
        use super::super::insert::{InsertNode, InsertNodeCell, LatencyDummy};
        use super::super::rt::{LiveNodeCell, LiveSource, MAX_LIVE_BLOCK};
        use crate::midi::schedule::AbsNoteEvent;
        use crate::midi::synth::PolySynth;

        const FRAMES: usize = 128;
        const CHAIN: usize = 2048;
        const BLOCKS: usize = 80; // 10240 frames: past the release AND the chain

        let build = || {
            let params = Arc::new(ParamTable::with_slots_and_sends(1, 0));
            params.set_gain_pair_linear(0, 1.0);
            params.set_pan(0, 0.0);
            let mut synth = PolySynth::new();
            synth.prepare(48_000, MAX_LIVE_BLOCK);
            let mut dummy = LatencyDummy::new(CHAIN);
            dummy.prepare(48_000, MAX_LIVE_BLOCK);
            let mut pad = RtTrack::clips(0, Vec::new());
            pad.live = Some(LiveSource {
                node: LiveNodeCell::new(Box::new(synth)),
                events: Arc::new(vec![AbsNoteEvent {
                    sample: 0,
                    key: 69,
                    velocity: 100,
                    channel: 0,
                }]),
            });
            pad.inserts = vec![InsertNode {
                slot_id: "s".into(),
                instance_id: "i".into(),
                bypassed: false,
                latency: CHAIN,
                proc: InsertNodeCell::new(Box::new(dummy)),
            }];
            let mut g = RtGraph::new(vec![pad], 1, params);
            let clocks = crate::audio::clock::ClockTable::with_slots_clocks_and_players(1, 2, 1);
            clocks.set_transport_playing(true);
            clocks.bind_slot(0, 1);
            g.clocks = Arc::new(clocks);
            g
        };

        // One press, rendered to exhaustion: fire for a single block, then
        // keep rendering while the row drains.
        let press = |g: &mut RtGraph| -> Vec<f32> {
            let mut all = Vec::with_capacity(BLOCKS * FRAMES * 2);
            let mut out = vec![0.0f32; FRAMES * 2];
            g.clocks.fire(1, 0, FRAMES as u64, false, 1.0);
            for _ in 0..BLOCKS {
                out.fill(0.0);
                render(g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
                all.extend_from_slice(&out);
                g.clocks.advance(FRAMES as u64);
            }
            all
        };

        let mut g = build();
        let first = press(&mut g);
        assert!(
            first.iter().fold(0.0f32, |m, s| m.max(s.abs())) > 0.0,
            "the press has to sound at all"
        );

        // A pristine graph pressed once is the reference: whatever the second
        // press on the USED graph produces must be the same sound.
        let mut fresh = build();
        let reference = press(&mut fresh);
        let second = press(&mut g);
        let worst = reference
            .iter()
            .zip(second.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            worst < 1e-6,
            "the second press differs from a first press by {worst}: material was \
             stranded in the row and replayed at the next onset"
        );
    }

    /// Fix round 3, item 2's trap. The tail `debug_assert` re-derives a row's
    /// window, and the allowance in it is now RATE-DEPENDENT — so the rate it
    /// re-derives with has to be the one the row was BUILT at, never this
    /// call's `sample_rate`.
    ///
    /// The two disagree for real: a device rate change moves what
    /// `render_impl` is handed immediately, and the graph is only rebuilt
    /// afterwards. In that window a perfectly correct graph would fail its
    /// own invariant check and panic on the RT thread. One source of truth,
    /// and it travels with the graph.
    #[test]
    fn a_graphs_tail_check_uses_the_rate_it_was_built_at_not_the_callers() {
        use super::super::dsp::AudioProcessor;
        use super::super::rt::{live_tail_frames, LiveNodeCell, LiveSource, MAX_LIVE_BLOCK};
        use crate::midi::synth::PolySynth;

        let mut synth = PolySynth::new();
        synth.prepare(96_000, MAX_LIVE_BLOCK);
        let mut pad = RtTrack::clips(0, Vec::new());
        pad.live = Some(LiveSource {
            node: LiveNodeCell::new(Box::new(synth)),
            events: Arc::new(Vec::new()),
        });
        let mut g = RtGraph::with_buses(
            vec![pad],
            Vec::new(),
            1,
            Arc::new(ParamTable::with_slots_and_sends(1, 0)),
            96_000,
        );
        assert_eq!(g.tracks[0].tail_frames, live_tail_frames(96_000));

        // The device dropped to 48 kHz; the rebuild has not landed yet.
        let mut out = vec![0.0f32; 64 * 2];
        render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
    }

    /// The integrated half of the flush: `render_impl`'s prologue must turn
    /// a stopped scene clock's discontinuity into a real `all_notes_off` on
    /// the live node still bound to it. Nothing asserted this before —
    /// `prime_live` is where the hanging note is actually prevented.
    #[test]
    fn a_stopped_scene_clock_all_notes_offs_the_live_node_bound_to_it() {
        use super::super::dsp::{AudioProcessor, LiveInstrument};
        use super::super::rt::{LiveNodeCell, LiveSource};
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        /// Counts `all_notes_off` calls and nothing else.
        struct OffCounter(Arc<AtomicUsize>);
        impl AudioProcessor for OffCounter {
            fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}
            fn process(&mut self, _io: &mut ProcessBlock<'_>) {}
            fn reset(&mut self) {}
        }
        impl LiveInstrument for OffCounter {
            fn queue_event(&mut self, _ev: BlockNoteEvent) -> bool {
                true
            }
            fn all_notes_off(&mut self) {
                self.0.fetch_add(1, AtomicOrdering::Relaxed);
            }
        }

        let offs = Arc::new(AtomicUsize::new(0));
        let tr = RtTrack {
            sends: Vec::new(),
            out_pdc: None,
            output: None,
            tail_frames: 0,
            flush_left: 0,
            win: Default::default(),
            slot: 0,
            clips: Vec::new(),
            live: Some(LiveSource {
                node: LiveNodeCell::new(Box::new(OffCounter(offs.clone()))),
                events: Arc::new(Vec::new()),
            }),
            inserts: Vec::new(),
            pdc: None,
        };
        let mut g = RtGraph::new(vec![tr], 1, Arc::new(ParamTable::default()));
        let clocks = scene_clocks(&g, true);
        clocks.fire(1, 0, 10_000, false, 1.0);
        clocks.bind_slot(0, 1);
        g.clocks = Arc::new(clocks);

        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        let after_fire = offs.load(AtomicOrdering::Relaxed);
        assert_eq!(after_fire, 1, "the fire itself is a jump");

        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert_eq!(offs.load(AtomicOrdering::Relaxed), 1, "steady block");

        g.clocks.stop(1);
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert_eq!(
            offs.load(AtomicOrdering::Relaxed),
            2,
            "the cut releases the held voice instead of freezing it"
        );
    }

    #[test]
    fn a_node_on_a_scene_clock_still_plays_the_scene_through_inserts() {
        let mut g = one_track_graph(0, clip(100, 0, 4, vec![1.0; 4], 1));
        g.params.set_pan(0, -1.0);
        insert_on(&mut g, 0, Box::new(GainHalfEffect { bypassed: false }), false);
        let clocks = scene_clocks(&g, true);
        clocks.fire(1, 100, 10_000, false, 1.0);
        clocks.bind_slot(0, 1);
        g.clocks = Arc::new(clocks);
        let mut out = vec![0.0f32; 8];
        render_simple(&mut g, 0, &LoopSpec::OFF, &mut out, 2);
        assert!(
            (out[0] - 0.5).abs() < 1e-5,
            "the scene must be heard through the insert, got {}",
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
            sends: Vec::new(),
            out_pdc: None,
            output: None,
            tail_frames: 0,
            flush_left: 0,
            win: Default::default(),
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

    /// Task 10 fix round 4, item 2: the raised tail is inert PER ROW KIND.
    ///
    /// Since round 3 a live row's `tail_frames` is `strip + allowance`, and
    /// `with_buses` recomputes it for every row — a live MIDI TRACK included,
    /// which now writes a much larger `flush_left` than it ever did before.
    /// Nothing may read it: `flushing` is `exclusive_idle`, and only a
    /// player's row is that. An ordinary track on a stopped transport still
    /// has something feeding it, so a window that opened its fader would hold
    /// whatever sits under the frozen playhead — here, a note the synth is
    /// still sustaining.
    ///
    /// `a_raw_pad_owns_no_tail_and_takes_the_bare_skip` pins the row kind that
    /// gets no window; this pins the row kind that gets one and must not use
    /// it. The branch has already shipped a gate that opened every track's
    /// fader for `tail_frames` after a stop, with the whole suite green — a
    /// per-kind assertion is the only thing that saw it.
    #[test]
    fn a_live_midi_track_row_never_flushes_after_a_stop_however_long_its_tail() {
        const RATE: u32 = 48_000;
        let held = vec![AbsNoteEvent { sample: 0, key: 69, velocity: 100, channel: 0 }];
        let params = Arc::new(ParamTable::with_slots_and_sends(1, 0));
        params.set_gain_pair_linear(0, 1.0);
        params.set_pan(0, 0.0);
        let mut g =
            RtGraph::with_buses(vec![live_track(0, held, RATE)], Vec::new(), 1, params, RATE);
        let clocks = crate::audio::clock::ClockTable::with_slots_and_clocks(1, 1);
        clocks.set_transport_playing(true);
        g.clocks = Arc::new(clocks);
        assert!(
            g.tracks[0].tail_frames >= crate::audio::rt::live_tail_frames(RATE),
            "premise: a live row does carry a window now — without one this \
             test would pass for want of a tail rather than for the gate"
        );

        let mut out = vec![0.0f32; 128 * 2];
        for _ in 0..4 {
            out.fill(0.0);
            render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, None);
        }
        assert!(peak(&out) > 0.0, "the sustained note has to sound while the transport rolls");

        // Every one of these blocks is INSIDE the window — 8 x 128 frames
        // against a window of at least 4096 — which is where a wrongly
        // opened fader would sound.
        g.clocks.set_transport_playing(false);
        for i in 0..8 {
            out.fill(0.0);
            render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, RATE, false, None);
            assert_eq!(
                peak(&out),
                0.0,
                "block {i} after the stop: a track is not a pad, and its \
                 flush window must stay unread"
            );
        }
    }

    /// The headless seam proof: a live PolySynth node inside the RCU graph
    /// renders audible audio through `render`, sample-positioned, and the
    /// note's release leaves silence.
    #[test]
    fn live_node_renders_audibly_through_graph() {
        const RATE: u32 = 48_000;
        let events = vec![
            AbsNoteEvent { sample: 1_000, key: 69, velocity: 110, channel: 0 },
            AbsNoteEvent { sample: 9_000, key: 69, velocity: 0, channel: 0 },
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
            AbsNoteEvent { sample: 0, key: 60, velocity: 100, channel: 0 },
            AbsNoteEvent { sample: 50_000, key: 60, velocity: 0, channel: 0 },
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
            AbsNoteEvent { sample: 0, key: 72, velocity: 100, channel: 0 },
            AbsNoteEvent { sample: 40_000, key: 72, velocity: 0, channel: 0 },
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
            sends: Vec::new(),
            out_pdc: None,
            output: None,
            tail_frames: 0,
            flush_left: 0,
            win: Default::default(),
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
            crate::midi::schedule::AbsNoteEvent { sample: 0, key: 60, velocity: 100, channel: 0 },
            crate::midi::schedule::AbsNoteEvent { sample: 40_000, key: 60, velocity: 0, channel: 0 },
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
            crate::midi::schedule::AbsNoteEvent { sample: 1_000, key: 69, velocity: 110, channel: 0 },
            crate::midi::schedule::AbsNoteEvent { sample: 9_000, key: 69, velocity: 0, channel: 0 },
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

    #[test]
    fn live_automation_ownership_bypasses_the_compiled_gain_ramp() {
        use crate::plugins::automation::AbsParamEvent;
        let data = Arc::new(RtClipData { channels: 1, data: vec![1.0; 64] });
        let clip = RtClip {
            start: 0, offset: 0, len: 64, gain: 1.0,
            fade_in: 0, fade_out: 0, samples: data,
        };
        let params = Arc::new(ParamTable::with_slots(1));
        params.set_gain_linear(0, 0.5);
        params.set_pan(0, -1.0);
        params.set_gain_automation_owner(0, Some(7));
        let mut graph = RtGraph::new(vec![RtTrack::clips(0, vec![clip])], 1, params);
        graph.set_gain_ramps(vec![Some(Arc::new(vec![
            AbsParamEvent { sample: 0, value: 0.25 },
            AbsParamEvent { sample: 64, value: 0.25 },
        ]))]);

        let mut out = vec![0.0f32; 64 * 2];
        render(&mut graph, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        for frame in out.chunks_exact(2) {
            assert!((frame[0] - 0.5).abs() < 1e-6, "live fader must be audible exactly once");
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
        let events = vec![AbsNoteEvent { sample: 0, key: 69, velocity: 110, channel: 0 }];

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
