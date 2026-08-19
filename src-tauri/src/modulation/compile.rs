//! Source expansion and the combination law (design §4.2, §5).
//!
//! EVERYTHING about looping is resolved here, on the CONTROL THREAD, by
//! expanding bindings into the same `AbsParamEvent` lists Track D's RT path
//! already reads. The audio callback does not learn what a loop is, what a
//! placement is, or what a binding is — it walks a sorted breakpoint list
//! exactly as it did before this round. That is the whole reason clip-local
//! automation costs the callback nothing.
//!
//! Ticks cross into samples in this module and nowhere downstream
//! (ARCHITECTURE §13/§15.1).

use super::model::{BindingMode, Curve, Domain};
use super::resolve::{resolve_target, ResolveCtx, ResolvedTarget};
use super::ModulationDoc;
use crate::midi::TempoMap;
use crate::plugins::automation::{value_at, AbsParamEvent};

/// Hard bound on one binding's expansion (design §5). A one-bar envelope
/// under a 20-minute placement is ~1150 repeats and perfectly ordinary; the
/// product of points × repeats is nonetheless unbounded in principle, and a
/// rebuild must not turn a document edit into an unbounded allocation while
/// the transport is rolling. Lazy per-block evaluation is the eventual
/// answer (design §8.7); until then this stops at the last whole repeat and
/// the binding is REPORTED, never silently cropped.
pub const MAX_EVENTS_PER_BINDING: usize = 200_000;

/// Where a repeating source sounds, in ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub start_ticks: u64,
    pub length_ticks: u64,
    /// Repeat period. Equal to `length_ticks` when the placement does not
    /// loop (mirrors `MidiClip::effective_content_length_ticks`).
    pub content_length_ticks: u64,
}

impl Placement {
    pub fn from_midi_clip(clip: &crate::midi::MidiClip) -> Self {
        Self {
            start_ticks: clip.timeline_start_ticks,
            length_ticks: clip.length_ticks,
            content_length_ticks: clip.effective_content_length_ticks(),
        }
    }
}

/// Every placement of `content_id` among `clips`, with the track it plays
/// on. Shared content on two tracks yields two rows; a looping placement
/// is one row whose `length_ticks` outruns `content_length_ticks`.
pub fn placements_from_midi_clips(
    clips: &[crate::midi::MidiClip],
    content_id: &str,
) -> Vec<(Placement, String)> {
    clips
        .iter()
        .filter(|c| c.content_id.as_str() == content_id)
        .map(|c| (Placement::from_midi_clip(c), c.track_id.as_str().to_string()))
        .collect()
}

/// One binding's compiled source: breakpoints plus the spans during which it
/// is ACTIVE. An empty `spans` means "always active" — a timeline curve.
/// Outside its spans a binding contributes nothing, which is different from
/// contributing its last value (design §5).
#[derive(Debug, Default, Clone)]
pub struct CompiledSource {
    pub events: Vec<AbsParamEvent>,
    pub spans: Vec<(u64, u64)>,
    /// Hit `MAX_EVENTS_PER_BINDING`.
    pub truncated: bool,
}

/// A timeline curve: points are absolute project ticks, always active.
pub fn expand_timeline(curve: &Curve, map: &TempoMap) -> CompiledSource {
    let mut events: Vec<AbsParamEvent> = Vec::with_capacity(curve.points.len());
    for p in &curve.points {
        let sample = map.tick_to_samples(p.tick as u64);
        match events.last_mut() {
            Some(last) if last.sample == sample => last.value = p.value,
            _ => events.push(AbsParamEvent { sample, value: p.value }),
        }
    }
    CompiledSource { events, spans: Vec::new(), truncated: false }
}

/// Repeat `curve` across every placement, cropped to each placement's end.
/// The curve restarts at every period boundary — that is what "the
/// automation loops with the clip" means, and it is the same
/// `contentLengthTicks` rule MIDI notes already follow.
pub fn expand_repeated(
    curve: &Curve,
    placements: &[Placement],
    map: &TempoMap,
) -> CompiledSource {
    let mut out = CompiledSource::default();
    if curve.points.is_empty() {
        return out;
    }
    for p in placements {
        let period = p.content_length_ticks.max(1);
        let end_tick = p.start_ticks.saturating_add(p.length_ticks);
        out.spans.push((map.tick_to_samples(p.start_ticks), map.tick_to_samples(end_tick)));
        let mut origin = p.start_ticks;
        while origin < end_tick {
            for pt in &curve.points {
                let tick = origin.saturating_add(pt.tick as u64);
                if tick >= end_tick {
                    continue;
                }
                out.events.push(AbsParamEvent { sample: map.tick_to_samples(tick), value: pt.value });
                if out.events.len() >= MAX_EVENTS_PER_BINDING {
                    out.truncated = true;
                    finish(&mut out.events);
                    return out;
                }
            }
            origin = origin.saturating_add(period);
        }
    }
    finish(&mut out.events);
    out
}

/// Sort by sample and collapse breakpoints that landed on the same sample
/// (last wins) — `compile_lane`'s contract, applied to a merged list.
fn finish(events: &mut Vec<AbsParamEvent>) {
    events.sort_by_key(|e| e.sample);
    let mut kept: Vec<AbsParamEvent> = Vec::with_capacity(events.len());
    for e in events.iter() {
        match kept.last_mut() {
            Some(last) if last.sample == e.sample => last.value = e.value,
            _ => kept.push(*e),
        }
    }
    *events = kept;
}

// ---------------------------------------------------------------------------
// The combination law (design §4.2)
// ---------------------------------------------------------------------------

/// One binding's contribution at one instant, already depth-scaled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Contribution {
    /// Replaces the base.
    Absolute(f32),
    /// Multiplies the running value.
    Multiply(f32),
    /// Adds to the running value.
    Add(f32),
}

/// `mapped` is the source value already mapped into the target's normalized
/// domain (`range.min + (range.max - range.min) * raw`). Each mode is INERT
/// at `depth == 0`, which is what makes a depth slider safe to drag through
/// zero.
pub fn contribution(mode: BindingMode, base: f32, mapped: f32, depth: f32) -> Contribution {
    match mode {
        BindingMode::Absolute => Contribution::Absolute(base + depth * (mapped - base)),
        BindingMode::Multiply => Contribution::Multiply((1.0 - depth) + depth * mapped),
        BindingMode::Add => Contribution::Add(depth * mapped),
    }
}

/// `out = clamp(v * f + a, 0, 1)`, where `v` is the absolute contribution
/// (or the base when there is none), `f` the product of the multiply
/// contributions and `a` the sum of the adds.
fn combine_raw(base: f32, contributions: &[Contribution]) -> f32 {
    let mut v = base;
    let mut f = 1.0f32;
    let mut a = 0.0f32;
    for c in contributions {
        match c {
            Contribution::Absolute(x) => v = *x,
            Contribution::Multiply(x) => f *= *x,
            Contribution::Add(x) => a += *x,
        }
    }
    v * f + a
}

pub fn combine(base: f32, contributions: &[Contribution]) -> f32 {
    combine_raw(base, contributions).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Whole-document compilation
// ---------------------------------------------------------------------------

/// Everything compilation needs to know about the rest of the document.
/// Passed as closures so this module depends on no store type — the same
/// shape `compile_gain_ramps` used for its slot lookup.
pub struct CompileCtx<'a> {
    pub n_slots: usize,
    pub slot_of: &'a dyn Fn(&str) -> Option<usize>,
    /// Stored pan of a track, `-1.0..=1.0`.
    pub track_pan: &'a dyn Fn(&str) -> Option<f32>,
    /// Declared range of a plugin param.
    pub param_range: &'a dyn Fn(&str, u32) -> Option<(f32, f32)>,
    /// Stored (document) value of a plugin param, in native units.
    pub param_value: &'a dyn Fn(&str, u32) -> Option<f32>,
    /// The plugin instance a track's instrument resolves to.
    pub instrument_of: &'a dyn Fn(&str) -> Option<String>,
    /// Every placement of one content id, with the track it plays on.
    pub content_placements: &'a dyn Fn(&str) -> Vec<(Placement, String)>,
}

#[derive(Debug, Default, Clone)]
pub struct TrackRampSpec {
    pub gain: Option<Vec<AbsParamEvent>>,
    pub pan: Option<Vec<AbsParamEvent>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamLaneSpec {
    pub instance: String,
    pub index: u32,
    pub events: Vec<AbsParamEvent>,
}

#[derive(Debug, Default)]
pub struct CompiledModulation {
    /// Slot-indexed, `n_slots` long.
    pub tracks: Vec<TrackRampSpec>,
    pub params: Vec<ParamLaneSpec>,
    /// Binding ids dropped as a second `absolute` owner of a target.
    pub conflicts: Vec<String>,
    /// Binding ids whose expansion hit `MAX_EVENTS_PER_BINDING`.
    pub truncated: Vec<String>,
}

/// One binding's contribution to ONE target. A clip-envelope binding with a
/// `self*` target produces one of these PER PLACEMENT TRACK, which is why
/// grouping happens over contributors rather than over bindings.
struct Contributor {
    binding: usize,
    target: ResolvedTarget,
    source: CompiledSource,
}

pub fn compile(doc: &ModulationDoc, map: &TempoMap, ctx: &CompileCtx) -> CompiledModulation {
    let mut out = CompiledModulation {
        tracks: vec![TrackRampSpec::default(); ctx.n_slots],
        ..Default::default()
    };
    let mut contributors: Vec<Contributor> = Vec::new();

    for (i, b) in doc.bindings.iter().enumerate() {
        if !b.enabled {
            continue;
        }
        match &b.source {
            super::Source::Curve { curve_id } => {
                let Some(curve) = doc.curve(curve_id) else { continue };
                let Some(target) = resolve_target(&b.target, &ResolveCtx::none()) else { continue };
                contributors.push(Contributor {
                    binding: i,
                    target,
                    source: expand_timeline(curve, map),
                });
            }
            super::Source::AutomationTrack { track_id } => {
                let Some(target) = resolve_target(&b.target, &ResolveCtx::none()) else { continue };
                let mut merged = CompiledSource::default();
                for clip in doc.clips_on(track_id) {
                    let Some(curve) = doc.curve(&clip.curve_id) else { continue };
                    let placement = Placement {
                        start_ticks: clip.timeline_start_ticks,
                        length_ticks: clip.length_ticks,
                        content_length_ticks: clip.effective_content_length_ticks(),
                    };
                    let part = expand_repeated(curve, std::slice::from_ref(&placement), map);
                    merged.truncated |= part.truncated;
                    merged.events.extend(part.events);
                    merged.spans.extend(part.spans);
                }
                if merged.events.is_empty() {
                    continue;
                }
                finish(&mut merged.events);
                contributors.push(Contributor { binding: i, target, source: merged });
            }
            super::Source::ClipEnvelope { content_id, curve_id } => {
                let Some(curve) = doc.curve(curve_id) else { continue };
                // One contributor per distinct target: placements of shared
                // content on two tracks drive each track's own param.
                let mut by_target: Vec<(ResolvedTarget, Vec<Placement>)> = Vec::new();
                for (placement, track) in (ctx.content_placements)(content_id) {
                    let rctx = ResolveCtx {
                        self_track: Some(&track),
                        instrument_of: ctx.instrument_of,
                    };
                    let Some(target) = resolve_target(&b.target, &rctx) else { continue };
                    match by_target.iter_mut().find(|(t, _)| *t == target) {
                        Some((_, ps)) => ps.push(placement),
                        None => by_target.push((target, vec![placement])),
                    }
                }
                for (target, placements) in by_target {
                    let source = expand_repeated(curve, &placements, map);
                    if source.events.is_empty() {
                        continue;
                    }
                    contributors.push(Contributor { binding: i, target, source });
                }
            }
            // Reserved source kinds: no evaluator this round (design §8.2).
            _ => {}
        }
    }

    // Group by target, keeping document order within a group.
    let mut groups: Vec<(ResolvedTarget, Vec<usize>)> = Vec::new();
    for (ci, c) in contributors.iter().enumerate() {
        match groups.iter_mut().find(|(t, _)| *t == c.target) {
            Some((_, v)) => v.push(ci),
            None => groups.push((c.target.clone(), vec![ci])),
        }
    }

    for (target, members) in groups {
        let compiled = compile_group(&target, &members, &contributors, doc, ctx, &mut out);
        let Some(events) = compiled else { continue };
        match &target {
            ResolvedTarget::TrackGain(track) => {
                if let Some(slot) = (ctx.slot_of)(track) {
                    if slot < out.tracks.len() {
                        out.tracks[slot].gain = Some(events);
                    }
                }
            }
            ResolvedTarget::TrackPan(track) => {
                if let Some(slot) = (ctx.slot_of)(track) {
                    if slot < out.tracks.len() {
                        out.tracks[slot].pan = Some(events);
                    }
                }
            }
            ResolvedTarget::PluginParam { instance, index } => {
                out.params.push(ParamLaneSpec {
                    instance: instance.clone(),
                    index: *index,
                    events,
                });
            }
        }
    }
    out
}

/// Evaluate one target's contributors onto a single breakpoint list.
///
/// The list is sampled at the UNION of every contributor's breakpoints and
/// span edges. Between two breakpoints the product of two ramps is not
/// itself linear, so a stacked group is a piecewise-linear APPROXIMATION of
/// the true product — inaudible at control rate, and exactly equal whenever
/// a single binding drives the target, which is every project migrated from
/// Track D.
fn compile_group(
    target: &ResolvedTarget,
    members: &[usize],
    contributors: &[Contributor],
    doc: &ModulationDoc,
    ctx: &CompileCtx,
    out: &mut CompiledModulation,
) -> Option<Vec<AbsParamEvent>> {
    // Base: the document value, normalized into the target's domain.
    let (base, denorm): (f32, Box<dyn Fn(f32) -> f32>) = match target {
        // Gain's base is the UNITY MULTIPLIER, not the fader: the mixer
        // applies the fader separately at the same stage, which is what
        // keeps the fader independently movable under a running curve
        // (Track D ruling 6, design §4.1).
        ResolvedTarget::TrackGain(_) => (1.0, Box::new(|v| v)),
        ResolvedTarget::TrackPan(track) => {
            let pan = (ctx.track_pan)(track).unwrap_or(0.0);
            ((pan + 1.0) / 2.0, Box::new(|v| v * 2.0 - 1.0))
        }
        ResolvedTarget::PluginParam { instance, index } => {
            let (min, max) = (ctx.param_range)(instance, *index)?;
            let span = max - min;
            if !span.is_finite() || span <= 0.0 {
                return None;
            }
            let stored = (ctx.param_value)(instance, *index).unwrap_or(min);
            (((stored - min) / span).clamp(0.0, 1.0), Box::new(move |v| min + v * span))
        }
    };

    // Arbitrate: first `absolute` in document order owns the target.
    let mut absolute: Option<usize> = None;
    let mut others: Vec<usize> = Vec::new();
    for &ci in members {
        let b = &doc.bindings[contributors[ci].binding];
        if contributors[ci].source.truncated {
            out.truncated.push(b.id.clone());
        }
        match b.mode {
            BindingMode::Absolute if absolute.is_none() => absolute = Some(ci),
            BindingMode::Absolute => out.conflicts.push(b.id.clone()),
            _ => others.push(ci),
        }
    }

    // A `native`-domain binding (a v3 plugin lane whose range could not be
    // learned at load) bypasses the whole normalized pipeline: its points
    // ARE the host values. It cannot be stacked, so anything else on the
    // target is reported rather than silently mixed in (design §7).
    if let Some(ci) = absolute {
        let b = &doc.bindings[contributors[ci].binding];
        if b.domain == Domain::Native {
            for &other in &others {
                out.conflicts.push(doc.bindings[contributors[other].binding].id.clone());
            }
            return Some(contributors[ci].source.events.clone());
        }
    }

    let active: Vec<usize> = absolute.into_iter().chain(others.iter().copied()).collect();
    if active.is_empty() {
        return None;
    }

    // Sample points: every breakpoint, plus each span edge and the sample
    // before it, so a span boundary reads as a step rather than a slow ramp
    // to neutral.
    let mut samples: Vec<u64> = Vec::new();
    for &ci in &active {
        let src = &contributors[ci].source;
        samples.extend(src.events.iter().map(|e| e.sample));
        for (start, end) in &src.spans {
            samples.push(*start);
            samples.push(start.saturating_sub(1));
            samples.push(*end);
            samples.push(end.saturating_sub(1));
        }
    }
    samples.sort_unstable();
    samples.dedup();

    let mut events: Vec<AbsParamEvent> = Vec::with_capacity(samples.len());
    let mut contribs: Vec<Contribution> = Vec::with_capacity(active.len());
    for sample in samples {
        contribs.clear();
        for &ci in &active {
            let c = &contributors[ci];
            let b = &doc.bindings[c.binding];
            let inside = c.source.spans.is_empty()
                || c.source.spans.iter().any(|(s, e)| sample >= *s && sample < *e);
            if !inside {
                continue; // contributes nothing — NOT its last value
            }
            let Some(raw) = value_at(&c.source.events, sample) else { continue };
            let mapped = b.range.min + (b.range.max - b.range.min) * raw;
            contribs.push(contribution(b.mode, base, mapped, b.depth));
        }
        // Gain lanes are positive linear multipliers. Unlike normalized pan
        // and plugin domains, boosts above unity must survive compilation.
        let v = if matches!(target, ResolvedTarget::TrackGain(_)) {
            combine_raw(base, &contribs).max(0.0)
        } else {
            combine(base, &contribs)
        };
        let native = denorm(v);
        match events.last_mut() {
            Some(last) if last.sample == sample => last.value = native,
            _ => events.push(AbsParamEvent { sample, value: native }),
        }
    }

    // Drop interior points that sit exactly on the line between their
    // neighbours: a static curve becomes two events instead of hundreds.
    dedup_collinear(&mut events);
    (!events.is_empty()).then_some(events)
}

fn dedup_collinear(events: &mut Vec<AbsParamEvent>) {
    if events.len() < 3 {
        return;
    }
    let mut kept: Vec<AbsParamEvent> = Vec::with_capacity(events.len());
    kept.push(events[0]);
    for i in 1..events.len() - 1 {
        let (a, b, c) = (kept[kept.len() - 1], events[i], events[i + 1]);
        let span = (c.sample - a.sample) as f32;
        let interpolated = if span == 0.0 {
            b.value
        } else {
            a.value + (c.value - a.value) * ((b.sample - a.sample) as f32 / span)
        };
        if (interpolated - b.value).abs() > 1e-6 {
            kept.push(b);
        }
    }
    kept.push(events[events.len() - 1]);
    *events = kept;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::types::{TempoEvent, DEFAULT_PPQ};
    use crate::modulation::model::{
        AutomationClip, AutomationPoint, Binding, Domain, Range, Source, TargetRef, TrackParam,
    };

    fn map() -> TempoMap {
        TempoMap::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: 120.0 }], 48_000).unwrap()
    }

    /// One quarter note at 120 bpm = 0.5 s = 24000 samples at 48 kHz.
    const QUARTER: u64 = 24_000;

    fn curve(id: &str, length: Option<u64>, points: Vec<(u32, f32)>) -> Curve {
        Curve {
            id: id.into(),
            name: id.into(),
            length_ticks: length,
            points: points
                .into_iter()
                .map(|(tick, value)| AutomationPoint { tick, value })
                .collect(),
        }
    }

    fn binding(id: &str, source: Source, target: TargetRef, mode: BindingMode) -> Binding {
        Binding {
            id: id.into(),
            source,
            target,
            mode,
            depth: 1.0,
            range: Range::default(),
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        }
    }

    fn ctx<'a>(n_slots: usize, slot_of: &'a dyn Fn(&str) -> Option<usize>) -> CompileCtx<'a> {
        CompileCtx {
            n_slots,
            slot_of,
            track_pan: &|_| Some(0.0),
            param_range: &|_, _| Some((0.0, 1.0)),
            param_value: &|_, _| Some(0.0),
            instrument_of: &|_| None,
            content_placements: &|_| Vec::new(),
        }
    }

    #[test]
    fn a_placement_repeats_the_curve_every_content_length() {
        let c = curve("cur", Some(DEFAULT_PPQ as u64), vec![(0, 0.0), (DEFAULT_PPQ - 1, 1.0)]);
        let placements = vec![Placement {
            start_ticks: 0,
            length_ticks: (DEFAULT_PPQ as u64) * 3,
            content_length_ticks: DEFAULT_PPQ as u64,
        }];
        let src = expand_repeated(&c, &placements, &map());
        assert_eq!(src.events.len(), 6, "three repeats, two points each: {:?}", src.events);
        assert_eq!(src.events[0].sample, 0);
        assert_eq!(src.events[2].sample, QUARTER, "the second repeat starts one quarter in");
        assert_eq!(src.events[4].sample, QUARTER * 2);
        assert!(src.events[2].value.abs() < 1e-6, "each repeat restarts the shape");
        assert_eq!(src.spans, vec![(0, QUARTER * 3)]);
    }

    #[test]
    fn expansion_is_cropped_to_the_placement() {
        let c = curve("cur", Some(DEFAULT_PPQ as u64), vec![(0, 0.0), (DEFAULT_PPQ / 2, 1.0)]);
        let placements = vec![Placement {
            start_ticks: 0,
            length_ticks: (DEFAULT_PPQ as u64) * 3 / 2,
            content_length_ticks: DEFAULT_PPQ as u64,
        }];
        let src = expand_repeated(&c, &placements, &map());
        let end = QUARTER * 3 / 2;
        assert!(
            src.events.iter().all(|e| e.sample < end),
            "no event may land past the placement end: {:?}",
            src.events
        );
    }

    #[test]
    fn expansion_stops_at_the_cap_instead_of_allocating_without_limit() {
        let c = curve(
            "cur",
            Some(2),
            (0..64u32).map(|i| (i % 2, 0.5)).collect(),
        );
        let placements =
            vec![Placement { start_ticks: 0, length_ticks: 10_000_000, content_length_ticks: 2 }];
        let src = expand_repeated(&c, &placements, &map());
        assert!(src.truncated, "the cap must be reported, not silent");
        assert!(src.events.len() <= MAX_EVENTS_PER_BINDING);
    }

    #[test]
    fn combine_reproduces_track_ds_two_behaviours() {
        // Track gain: multiply at full depth on a unity base == today's ramp.
        let gain = combine(1.0, &[contribution(BindingMode::Multiply, 1.0, 0.25, 1.0)]);
        assert!((gain - 0.25).abs() < 1e-6, "got {gain}");
        // Plugin param: absolute at full depth replaces the stored value.
        let p = combine(0.9, &[contribution(BindingMode::Absolute, 0.9, 0.2, 1.0)]);
        assert!((p - 0.2).abs() < 1e-6, "got {p}");
        // add stacks, and the output clamps.
        let a = combine(0.5, &[contribution(BindingMode::Add, 0.5, 0.8, 1.0)]);
        assert!((a - 1.0).abs() < 1e-6, "clamped to 1.0, got {a}");
    }

    #[test]
    fn depth_zero_is_inert_in_every_mode() {
        for mode in [BindingMode::Absolute, BindingMode::Multiply, BindingMode::Add] {
            let v = combine(0.4, &[contribution(mode, 0.4, 1.0, 0.0)]);
            assert!((v - 0.4).abs() < 1e-6, "{mode:?} must be inert at depth 0, got {v}");
        }
    }

    #[test]
    fn a_timeline_gain_binding_compiles_into_the_slot_ramp() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", None, vec![(0, 1.0), (DEFAULT_PPQ, 0.25)]));
        doc.bindings.push(binding(
            "b",
            Source::Curve { curve_id: "cur".into() },
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let out = compile(&doc, &map(), &ctx(1, &slot_of));
        let gain = out.tracks[0].gain.as_ref().expect("a gain ramp");
        assert_eq!(gain.first().map(|e| e.value), Some(1.0));
        assert!((gain.last().unwrap().value - 0.25).abs() < 1e-6);
        assert!(out.tracks[0].pan.is_none());
    }

    #[test]
    fn native_track_gain_boost_is_not_clamped_at_unity() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", None, vec![(0, 2.0), (DEFAULT_PPQ, 2.0)]));
        let mut gain_binding = binding(
            "b",
            Source::Curve { curve_id: "cur".into() },
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
            BindingMode::Multiply,
        );
        gain_binding.domain = Domain::Native;
        doc.bindings.push(gain_binding);
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let out = compile(&doc, &map(), &ctx(1, &slot_of));
        let gain = out.tracks[0].gain.as_ref().expect("a gain ramp");
        assert!((gain[0].value - 2.0).abs() < 1e-6);
    }

    #[test]
    fn a_pan_binding_denormalizes_to_the_minus_one_to_one_range() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", None, vec![(0, 0.0), (DEFAULT_PPQ, 1.0)]));
        doc.bindings.push(binding(
            "b",
            Source::Curve { curve_id: "cur".into() },
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Pan },
            BindingMode::Absolute,
        ));
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let out = compile(&doc, &map(), &ctx(1, &slot_of));
        let pan = out.tracks[0].pan.as_ref().expect("a pan ramp");
        assert!((pan.first().unwrap().value + 1.0).abs() < 1e-6, "0.0 is hard left");
        assert!((pan.last().unwrap().value - 1.0).abs() < 1e-6, "1.0 is hard right");
    }

    #[test]
    fn one_automation_track_bound_to_two_targets_produces_two_ramps() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", Some(DEFAULT_PPQ as u64), vec![(0, 1.0), (DEFAULT_PPQ - 1, 0.0)]));
        doc.automation_clips.push(AutomationClip {
            id: "acl".into(),
            track_id: "auto".into(),
            curve_id: "cur".into(),
            timeline_start_ticks: 0,
            length_ticks: DEFAULT_PPQ as u64,
            content_length_ticks: None,
        });
        for (i, track) in ["t1", "t2"].iter().enumerate() {
            doc.bindings.push(binding(
                &format!("b{i}"),
                Source::AutomationTrack { track_id: "auto".into() },
                TargetRef::TrackParam { track_id: (*track).into(), param: TrackParam::Gain },
                BindingMode::Multiply,
            ));
        }
        let slot_of = |t: &str| match t {
            "t1" => Some(0usize),
            "t2" => Some(1usize),
            _ => None,
        };
        let out = compile(&doc, &map(), &ctx(2, &slot_of));
        assert!(out.tracks[0].gain.is_some(), "the first target follows the curve");
        assert!(out.tracks[1].gain.is_some(), "and so does the second");
    }

    #[test]
    fn outside_an_automation_clip_the_target_returns_to_its_document_value() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", None, vec![(0, 0.0), (DEFAULT_PPQ - 1, 0.0)]));
        doc.automation_clips.push(AutomationClip {
            id: "acl".into(),
            track_id: "auto".into(),
            curve_id: "cur".into(),
            timeline_start_ticks: 0,
            length_ticks: DEFAULT_PPQ as u64,
            content_length_ticks: None,
        });
        doc.bindings.push(binding(
            "b",
            Source::AutomationTrack { track_id: "auto".into() },
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let out = compile(&doc, &map(), &ctx(1, &slot_of));
        let gain = out.tracks[0].gain.as_ref().unwrap();
        let after = value_at(gain, QUARTER + 1000).unwrap();
        assert!(
            (after - 1.0).abs() < 1e-6,
            "past the clip the gain multiplier is unity, not the curve's last value: {after}"
        );
        let inside = value_at(gain, QUARTER / 2).unwrap();
        assert!(inside.abs() < 1e-6, "inside the clip the curve silences the track: {inside}");
    }

    #[test]
    fn a_clip_envelope_drives_each_placements_own_track() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", Some(DEFAULT_PPQ as u64), vec![(0, 0.5)]));
        doc.bindings.push(binding(
            "b",
            Source::ClipEnvelope { content_id: "con".into(), curve_id: "cur".into() },
            TargetRef::SelfTrackParam { param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let slot_of = |t: &str| match t {
            "t1" => Some(0usize),
            "t2" => Some(1usize),
            _ => None,
        };
        let placements = |content: &str| {
            if content != "con" {
                return Vec::new();
            }
            vec![
                (
                    Placement {
                        start_ticks: 0,
                        length_ticks: DEFAULT_PPQ as u64,
                        content_length_ticks: DEFAULT_PPQ as u64,
                    },
                    "t1".to_string(),
                ),
                (
                    Placement {
                        start_ticks: 0,
                        length_ticks: DEFAULT_PPQ as u64,
                        content_length_ticks: DEFAULT_PPQ as u64,
                    },
                    "t2".to_string(),
                ),
            ]
        };
        let mut c = ctx(2, &slot_of);
        c.content_placements = &placements;
        let out = compile(&doc, &map(), &c);
        assert!(out.tracks[0].gain.is_some(), "the placement on t1 drives t1");
        assert!(out.tracks[1].gain.is_some(), "the placement on t2 drives t2");
    }

    #[test]
    fn a_clip_envelope_repeats_the_same_shape_in_every_loop() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve(
            "cur",
            Some(DEFAULT_PPQ as u64),
            vec![(0, 0.0), (DEFAULT_PPQ / 2, 1.0)],
        ));
        doc.bindings.push(binding(
            "b",
            Source::ClipEnvelope { content_id: "con".into(), curve_id: "cur".into() },
            TargetRef::SelfTrackParam { param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let placements = |_: &str| {
            vec![(
                Placement {
                    start_ticks: 0,
                    length_ticks: (DEFAULT_PPQ as u64) * 4,
                    content_length_ticks: DEFAULT_PPQ as u64,
                },
                "t1".to_string(),
            )]
        };
        let mut c = ctx(1, &slot_of);
        c.content_placements = &placements;
        let out = compile(&doc, &map(), &c);
        let gain = out.tracks[0].gain.as_ref().unwrap();
        let bar1 = value_at(gain, QUARTER / 4).unwrap();
        let bar3 = value_at(gain, QUARTER * 2 + QUARTER / 4).unwrap();
        assert!(
            (bar1 - bar3).abs() < 1e-4,
            "the third repeat must read the same as the first: {bar1} vs {bar3}"
        );
    }

    #[test]
    fn a_second_absolute_binding_on_one_target_is_reported_not_blended() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", None, vec![(0, 0.2)]));
        for id in ["first", "second"] {
            doc.bindings.push(binding(
                id,
                Source::Curve { curve_id: "cur".into() },
                TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Pan },
                BindingMode::Absolute,
            ));
        }
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let out = compile(&doc, &map(), &ctx(1, &slot_of));
        assert_eq!(out.conflicts, vec!["second".to_string()]);
    }

    #[test]
    fn a_native_domain_binding_passes_its_points_through_untouched() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", None, vec![(0, 440.0)]));
        let mut b = binding(
            "b",
            Source::Curve { curve_id: "cur".into() },
            TargetRef::PluginParam { instance_id: "inst".into(), param_id: 3 },
            BindingMode::Absolute,
        );
        b.domain = Domain::Native;
        doc.bindings.push(b);
        let slot_of = |_: &str| None;
        let mut c = ctx(0, &slot_of);
        c.param_range = &|_, _| Some((0.0, 20_000.0));
        let out = compile(&doc, &map(), &c);
        assert_eq!(out.params.len(), 1);
        assert_eq!(
            out.params[0].events[0].value, 440.0,
            "a native curve is already in host units — normalizing it would be a guess"
        );
    }

    #[test]
    fn a_plugin_param_binding_denormalizes_through_the_declared_range() {
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", None, vec![(0, 0.5)]));
        doc.bindings.push(binding(
            "b",
            Source::Curve { curve_id: "cur".into() },
            TargetRef::PluginParam { instance_id: "inst".into(), param_id: 3 },
            BindingMode::Absolute,
        ));
        let slot_of = |_: &str| None;
        let mut c = ctx(0, &slot_of);
        c.param_range = &|_, _| Some((0.0, 10_000.0));
        c.param_value = &|_, _| Some(0.0);
        let out = compile(&doc, &map(), &c);
        assert_eq!(out.params[0].events[0].value, 5_000.0);
    }

    #[test]
    fn a_binding_whose_curve_is_gone_is_inert_rather_than_a_panic() {
        let mut doc = ModulationDoc::default();
        doc.bindings.push(binding(
            "b",
            Source::Curve { curve_id: "missing".into() },
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let out = compile(&doc, &map(), &ctx(1, &slot_of));
        assert!(out.tracks[0].gain.is_none());
    }

    fn midi_clip(
        id: &str,
        track: &str,
        content: &str,
        start: u64,
        length: u64,
        content_len: Option<u64>,
    ) -> crate::midi::MidiClip {
        crate::midi::MidiClip {
            id: id.into(),
            track_id: track.into(),
            name: id.into(),
            timeline_start_ticks: start,
            length_ticks: length,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: content.into(),
            lane_id: crate::ids::LaneId::default_for_track(track),
            content_length_ticks: content_len,
            transpose_semitones: 0,
            velocity_offset: 0,
        }
    }

    #[test]
    fn a_clip_envelope_produces_the_same_shape_in_every_repeat() {
        // a 1-bar envelope under a 4-bar looping placement: sample the compiled
        // events at bar 1 and bar 3, assert equal values
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve(
            "cur",
            Some(DEFAULT_PPQ as u64),
            vec![(0, 0.0), (DEFAULT_PPQ / 2, 1.0)],
        ));
        doc.bindings.push(binding(
            "b",
            Source::ClipEnvelope { content_id: "con".into(), curve_id: "cur".into() },
            TargetRef::SelfTrackParam { param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let clips = [midi_clip(
            "c1",
            "t1",
            "con",
            0,
            (DEFAULT_PPQ as u64) * 4,
            Some(DEFAULT_PPQ as u64),
        )];
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let placements = |id: &str| placements_from_midi_clips(&clips, id);
        let mut c = ctx(1, &slot_of);
        c.content_placements = &placements;
        let out = compile(&doc, &map(), &c);
        let gain = out.tracks[0].gain.as_ref().expect("the looping placement compiled");
        let bar1 = value_at(gain, QUARTER / 4).unwrap();
        let bar3 = value_at(gain, QUARTER * 2 + QUARTER / 4).unwrap();
        assert!(
            (bar1 - bar3).abs() < 1e-4,
            "a 1-bar envelope under a 4-bar loop must read the same in bar 1 and bar 3: {bar1} vs {bar3}"
        );
    }

    #[test]
    fn every_placement_of_shared_content_gets_the_envelope() {
        // two MidiClips with the same content_id, one envelope binding
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", Some(DEFAULT_PPQ as u64), vec![(0, 0.25)]));
        doc.bindings.push(binding(
            "b",
            Source::ClipEnvelope { content_id: "con".into(), curve_id: "cur".into() },
            TargetRef::SelfTrackParam { param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let clips = [
            midi_clip("c1", "t1", "con", 0, DEFAULT_PPQ as u64, None),
            midi_clip("c2", "t1", "con", (DEFAULT_PPQ as u64) * 4, DEFAULT_PPQ as u64, None),
            midi_clip("other", "t1", "other", (DEFAULT_PPQ as u64) * 8, DEFAULT_PPQ as u64, None),
        ];
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let placements = |id: &str| placements_from_midi_clips(&clips, id);
        let mut c = ctx(1, &slot_of);
        c.content_placements = &placements;
        let out = compile(&doc, &map(), &c);
        let gain = out.tracks[0].gain.as_ref().expect("shared content compiled");
        let first = value_at(gain, QUARTER / 2).unwrap();
        let second = value_at(gain, QUARTER * 4 + QUARTER / 2).unwrap();
        let stranger = value_at(gain, QUARTER * 8 + QUARTER / 2).unwrap();
        assert!((first - 0.25).abs() < 1e-4, "first placement of shared content: {first}");
        assert!((second - 0.25).abs() < 1e-4, "second placement of shared content: {second}");
        assert!(
            (stranger - 1.0).abs() < 1e-4,
            "a different content_id must not receive the envelope: {stranger}"
        );
    }

    #[test]
    fn a_self_track_target_resolves_per_placement() {
        // the same content on two different tracks drives each one's own gain
        let mut doc = ModulationDoc::default();
        doc.curves.push(curve("cur", Some(DEFAULT_PPQ as u64), vec![(0, 0.5)]));
        doc.bindings.push(binding(
            "b",
            Source::ClipEnvelope { content_id: "con".into(), curve_id: "cur".into() },
            TargetRef::SelfTrackParam { param: TrackParam::Gain },
            BindingMode::Multiply,
        ));
        let clips = [
            midi_clip("c1", "t1", "con", 0, DEFAULT_PPQ as u64, None),
            midi_clip("c2", "t2", "con", 0, DEFAULT_PPQ as u64, None),
        ];
        let slot_of = |t: &str| match t {
            "t1" => Some(0usize),
            "t2" => Some(1usize),
            _ => None,
        };
        let placements = |id: &str| placements_from_midi_clips(&clips, id);
        let mut c = ctx(2, &slot_of);
        c.content_placements = &placements;
        let out = compile(&doc, &map(), &c);
        assert!(out.tracks[0].gain.is_some(), "the placement on t1 drives t1");
        assert!(out.tracks[1].gain.is_some(), "the placement on t2 drives t2");
    }
}
