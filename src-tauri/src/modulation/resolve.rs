//! Target resolution and per-target arbitration (design §4.3).
//!
//! Track D's `compile_gain_ramps` documented its collision rule as "if two
//! lanes name the same track's gain, the LAST one wins and the earlier is
//! silently dropped … Nothing produces that today". Something does now: an
//! automation track exists precisely to drive several targets, and two of
//! them can name one param. So last-wins-silently is replaced here by
//! first-wins-and-report — the dropped binding is named, never blended away.

use super::model::{Binding, BindingMode, TargetRef, TrackParam};

/// A target this build can actually drive. Everything else — `mute`, sends,
/// macros, the reserved `port` arm — resolves to `None` and is inert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    TrackGain(String),
    TrackPan(String),
    PluginParam { instance: String, index: u32 },
}

/// What a `self*` target needs in order to resolve: the track a placement
/// plays on, and a way to find that track's instrument instance.
pub struct ResolveCtx<'a> {
    pub self_track: Option<&'a str>,
    pub instrument_of: &'a dyn Fn(&str) -> Option<String>,
}

impl ResolveCtx<'_> {
    /// A context with no placement — the right one for timeline curves and
    /// automation tracks, where `self*` targets are meaningless anyway.
    pub fn none() -> ResolveCtx<'static> {
        ResolveCtx { self_track: None, instrument_of: &|_| None }
    }
}

pub fn resolve_target(t: &TargetRef, ctx: &ResolveCtx) -> Option<ResolvedTarget> {
    match t {
        TargetRef::TrackParam { track_id, param } if !track_id.is_empty() => match param {
            TrackParam::Gain => Some(ResolvedTarget::TrackGain(track_id.clone())),
            TrackParam::Pan => Some(ResolvedTarget::TrackPan(track_id.clone())),
            // No engine path this round (design §3.3). Ignored, never
            // guessed at — the policy Track D's `resolve_target` set for
            // built-in ids this build does not know.
            TrackParam::Mute | TrackParam::Send0 | TrackParam::Send1 => None,
        },
        TargetRef::PluginParam { instance_id, param_id } if !instance_id.is_empty() => {
            Some(ResolvedTarget::PluginParam { instance: instance_id.clone(), index: *param_id })
        }
        TargetRef::SelfTrackParam { param } => {
            let track = ctx.self_track?;
            resolve_target(
                &TargetRef::TrackParam { track_id: track.to_string(), param: *param },
                ctx,
            )
        }
        TargetRef::SelfInstrumentParam { param_id } => {
            let track = ctx.self_track?;
            let instance = (ctx.instrument_of)(track)?;
            Some(ResolvedTarget::PluginParam { instance, index: *param_id })
        }
        // Reserved arms, and refs with empty ids.
        _ => None,
    }
}

/// Every enabled binding that drives one target, split by mode. Indices
/// point into the `bindings` slice `plan_targets` was given.
#[derive(Debug)]
pub struct TargetGroup {
    pub target: ResolvedTarget,
    pub absolute: Option<usize>,
    pub multiply: Vec<usize>,
    pub add: Vec<usize>,
}

#[derive(Debug, Default)]
pub struct TargetPlan {
    pub groups: Vec<TargetGroup>,
    /// Ids of bindings dropped as a SECOND `absolute` owner of a target.
    /// Surfaced, not silently blended (design §4.3).
    pub conflicts: Vec<String>,
}

/// Group enabled, resolvable bindings by target and pick the one `absolute`
/// owner per target: FIRST in document order.
pub fn plan_targets(bindings: &[Binding], ctx: &ResolveCtx) -> TargetPlan {
    let mut plan = TargetPlan::default();
    for (i, b) in bindings.iter().enumerate() {
        if !b.enabled {
            continue;
        }
        let Some(target) = resolve_target(&b.target, ctx) else { continue };
        let gi = match plan.groups.iter().position(|g| g.target == target) {
            Some(gi) => gi,
            None => {
                plan.groups.push(TargetGroup {
                    target,
                    absolute: None,
                    multiply: Vec::new(),
                    add: Vec::new(),
                });
                plan.groups.len() - 1
            }
        };
        match b.mode {
            BindingMode::Absolute => {
                if plan.groups[gi].absolute.is_some() {
                    plan.conflicts.push(b.id.clone());
                } else {
                    plan.groups[gi].absolute = Some(i);
                }
            }
            BindingMode::Multiply => plan.groups[gi].multiply.push(i),
            BindingMode::Add => plan.groups[gi].add.push(i),
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modulation::model::{Domain, Range, Source};

    fn bind(id: &str, mode: BindingMode, target: TargetRef) -> Binding {
        Binding {
            id: id.into(),
            source: Source::Curve { curve_id: "cur".into() },
            target,
            mode,
            depth: 1.0,
            range: Range::default(),
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        }
    }

    #[test]
    fn unknown_and_unimplemented_targets_resolve_to_none() {
        let c = ResolveCtx::none();
        assert_eq!(
            resolve_target(
                &TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
                &c
            ),
            Some(ResolvedTarget::TrackGain("t1".into()))
        );
        assert_eq!(
            resolve_target(
                &TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Mute },
                &c
            ),
            None,
            "mute has no engine path this round and must be ignored, never guessed at"
        );
        assert_eq!(
            resolve_target(&TargetRef::Port { node_id: "n".into(), port_id: "p".into() }, &c),
            None,
            "the reserved port arm resolves to nothing until the node-graph round"
        );
        assert_eq!(
            resolve_target(
                &TargetRef::TrackParam { track_id: String::new(), param: TrackParam::Gain },
                &c
            ),
            None,
            "an empty track id is not a target"
        );
    }

    #[test]
    fn self_targets_resolve_through_the_placements_track() {
        let instrument_of = |t: &str| (t == "t9").then(|| "inst-1".to_string());
        let c = ResolveCtx { self_track: Some("t9"), instrument_of: &instrument_of };
        assert_eq!(
            resolve_target(&TargetRef::SelfTrackParam { param: TrackParam::Pan }, &c),
            Some(ResolvedTarget::TrackPan("t9".into()))
        );
        assert_eq!(
            resolve_target(&TargetRef::SelfInstrumentParam { param_id: 7 }, &c),
            Some(ResolvedTarget::PluginParam { instance: "inst-1".into(), index: 7 })
        );
    }

    #[test]
    fn a_self_target_without_a_placement_is_inert_rather_than_wrong() {
        let c = ResolveCtx::none();
        assert_eq!(resolve_target(&TargetRef::SelfTrackParam { param: TrackParam::Gain }, &c), None);
        assert_eq!(resolve_target(&TargetRef::SelfInstrumentParam { param_id: 0 }, &c), None);
    }

    #[test]
    fn one_absolute_binding_wins_and_the_second_is_reported() {
        let t = || TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Pan };
        let bindings = vec![
            bind("first", BindingMode::Absolute, t()),
            bind("second", BindingMode::Absolute, t()),
            bind("m", BindingMode::Multiply, t()),
        ];
        let plan = plan_targets(&bindings, &ResolveCtx::none());
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].absolute, Some(0), "first in document order wins");
        assert_eq!(plan.groups[0].multiply, vec![2]);
        assert_eq!(plan.conflicts, vec!["second".to_string()]);
    }

    #[test]
    fn distinct_targets_get_distinct_groups() {
        let bindings = vec![
            bind(
                "g",
                BindingMode::Multiply,
                TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
            ),
            bind(
                "p",
                BindingMode::Absolute,
                TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Pan },
            ),
            bind(
                "x",
                BindingMode::Absolute,
                TargetRef::PluginParam { instance_id: "i".into(), param_id: 2 },
            ),
        ];
        let plan = plan_targets(&bindings, &ResolveCtx::none());
        assert_eq!(plan.groups.len(), 3, "gain, pan and a plugin param are three targets");
        assert!(plan.conflicts.is_empty());
    }

    #[test]
    fn disabled_bindings_are_left_out_entirely() {
        let mut b = bind(
            "x",
            BindingMode::Absolute,
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
        );
        b.enabled = false;
        let plan = plan_targets(std::slice::from_ref(&b), &ResolveCtx::none());
        assert!(plan.groups.is_empty());
        assert!(plan.conflicts.is_empty(), "a disabled binding cannot conflict with anything");
    }
}
