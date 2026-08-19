//! Lane ⇄ (curve, binding) translation behind `automation_get`/`automation_set`.
//!
//! The existing lane IPC and `automation.svelte.ts` store stay unchanged.
//! This module is the only place that knows a Track D gain lane is a
//! multiply binding on track gain, and that a plugin lane's native units
//! come back out through `rangeSnapshot`.

use crate::plugins::automation::{
    AutomationLane, AutomationPoint, TRACK_PARAM_GAIN, TRACK_TARGET_PREFIX,
};

use super::model::{
    Binding, BindingMode, Curve, Domain, RangeSnapshot, Source, TargetRef, TrackParam,
};
use super::ModulationDoc;

/// Binding ids carry over from the lane so journals and the UI keep
/// addressing the same row. Curve ids are minted as `cur_<laneId>` and
/// stay stable across later upserts of that lane.
pub fn curve_id_for_lane(lane_id: &str) -> String {
    format!("cur_{lane_id}")
}

/// Project a modulation document back into the Track D lane list
/// `automation_get` still ships. Only timeline-curve bindings that the
/// old lane model could name — track gain, or a plugin param — appear.
/// Pan / clip-envelope / automation-track bindings are invisible here
/// (the new `modulation_get` surface is what shows them).
pub fn lanes_from_doc(doc: &ModulationDoc) -> Vec<AutomationLane> {
    let mut lanes = Vec::new();
    for b in &doc.bindings {
        let Some(lane) = lane_from_binding(doc, b) else { continue };
        lanes.push(lane);
    }
    lanes
}

fn lane_from_binding(doc: &ModulationDoc, b: &Binding) -> Option<AutomationLane> {
    let Source::Curve { curve_id } = &b.source else { return None };
    let (target_node, param_id) = match &b.target {
        TargetRef::TrackParam { track_id, param: TrackParam::Gain } => {
            (format!("{TRACK_TARGET_PREFIX}{track_id}"), TRACK_PARAM_GAIN)
        }
        TargetRef::PluginParam { instance_id, param_id } => (instance_id.clone(), *param_id),
        _ => return None,
    };
    let points = doc.curve(curve_id).map(|c| denormalize_points(&c.points, b)).unwrap_or_default();
    Some(AutomationLane { id: b.id.clone(), target_node, param_id, points })
}

/// Native units for the old lane IPC: a normalized plugin binding with a
/// `rangeSnapshot` denormalizes on the way out so `automation_get` still
/// reports the param's own units (5000, not 0.5). Gain is already 0..1.
fn denormalize_points(points: &[AutomationPoint], b: &Binding) -> Vec<AutomationPoint> {
    match (b.domain, b.range_snapshot) {
        (Domain::Normalized, Some(RangeSnapshot { min, max })) if max > min => points
            .iter()
            .map(|p| AutomationPoint { tick: p.tick, value: min + p.value * (max - min) })
            .collect(),
        _ => points.to_vec(),
    }
}

/// Upsert or delete one lane as a curve+binding pair. `lane.id` (forced
/// to `key` by the apply arm) becomes the binding id; the curve id is
/// derived and reused on later calls. `None` deletes the pair.
pub fn apply_lane(doc: &mut ModulationDoc, key: &str, lane: Option<&AutomationLane>) {
    match lane {
        None => delete_pair(doc, key),
        Some(lane) => {
            let existing = doc.binding(key).cloned();
            let (mut curve, binding) = lane_to_pair(lane, existing.as_ref());
            if let Some(old) = doc.curve(&curve.id) {
                curve.name = old.name.clone();
                curve.length_ticks = old.length_ticks;
            }
            upsert_curve(doc, curve);
            upsert_binding(doc, binding);
        }
    }
}

fn delete_pair(doc: &mut ModulationDoc, key: &str) {
    let curve_id = doc
        .binding(key)
        .and_then(|b| match &b.source {
            Source::Curve { curve_id } | Source::ClipEnvelope { curve_id, .. } => {
                Some(curve_id.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| curve_id_for_lane(key));
    doc.bindings.retain(|b| b.id != key);
    if curve_still_referenced(doc, &curve_id) {
        return;
    }
    doc.curves.retain(|c| c.id != curve_id);
}

fn curve_still_referenced(doc: &ModulationDoc, curve_id: &str) -> bool {
    doc.bindings.iter().any(|b| match &b.source {
        Source::Curve { curve_id: id } | Source::ClipEnvelope { curve_id: id, .. } => id == curve_id,
        _ => false,
    }) || doc.automation_clips.iter().any(|c| c.curve_id == curve_id)
}

fn upsert_curve(doc: &mut ModulationDoc, curve: Curve) {
    if let Some(existing) = doc.curves.iter_mut().find(|c| c.id == curve.id) {
        *existing = curve;
    } else {
        doc.curves.push(curve);
    }
}

fn upsert_binding(doc: &mut ModulationDoc, binding: Binding) {
    if let Some(existing) = doc.bindings.iter_mut().find(|b| b.id == binding.id) {
        *existing = binding;
    } else {
        doc.bindings.push(binding);
    }
}

/// Translate one incoming lane into a curve+binding pair. `existing` is
/// the binding already stored under the lane's id, if any — used to keep
/// the curve id stable and to (re)normalize plugin points through a
/// known `rangeSnapshot`.
pub fn lane_to_pair(lane: &AutomationLane, existing: Option<&Binding>) -> (Curve, Binding) {
    let curve_id = existing
        .and_then(|b| match &b.source {
            Source::Curve { curve_id } => Some(curve_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| curve_id_for_lane(&lane.id));

    let (target, mode, domain, range_snapshot) = match lane.target_node.strip_prefix(TRACK_TARGET_PREFIX)
    {
        Some(track_id) => (
            TargetRef::TrackParam { track_id: track_id.to_string(), param: TrackParam::Gain },
            BindingMode::Multiply,
            Domain::Native,
            None,
        ),
        None => {
            let snap = existing.and_then(|b| b.range_snapshot);
            let domain = existing.map(|b| b.domain).unwrap_or(Domain::Native);
            (
                TargetRef::PluginParam {
                    instance_id: lane.target_node.clone(),
                    param_id: lane.param_id,
                },
                BindingMode::Absolute,
                domain,
                snap,
            )
        }
    };

    let points = normalize_incoming_points(&lane.points, domain, range_snapshot);
    let curve = Curve {
        id: curve_id.clone(),
        name: String::new(),
        length_ticks: None,
        points,
    };
    let binding = Binding {
        id: lane.id.clone(),
        source: Source::Curve { curve_id },
        target,
        mode: existing.map(|b| b.mode).unwrap_or(mode),
        depth: existing.map(|b| b.depth).unwrap_or(1.0),
        range: existing.map(|b| b.range).unwrap_or_default(),
        domain,
        range_snapshot,
        enabled: existing.map(|b| b.enabled).unwrap_or(true),
    };
    (curve, binding)
}

fn normalize_incoming_points(
    points: &[AutomationPoint],
    domain: Domain,
    snap: Option<RangeSnapshot>,
) -> Vec<AutomationPoint> {
    match (domain, snap) {
        (Domain::Normalized, Some(RangeSnapshot { min, max })) if max > min => {
            let span = max - min;
            points
                .iter()
                .map(|p| AutomationPoint { tick: p.tick, value: (p.value - min) / span })
                .collect()
        }
        _ => points.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modulation::model::Range;

    fn gain_binding(id: &str, curve_id: &str, track_id: &str) -> Binding {
        Binding {
            id: id.into(),
            source: Source::Curve { curve_id: curve_id.into() },
            target: TargetRef::TrackParam {
                track_id: track_id.into(),
                param: TrackParam::Gain,
            },
            mode: BindingMode::Multiply,
            depth: 1.0,
            range: Range::default(),
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        }
    }

    #[test]
    fn automation_get_still_reports_a_gain_binding_as_the_lane_it_used_to_be() {
        // curve + multiply binding on track gain  =>  AutomationLane { targetNode: "track:t1", paramId: 0 }
        let doc = ModulationDoc {
            curves: vec![Curve {
                id: "cur_lane-1".into(),
                name: String::new(),
                length_ticks: None,
                points: vec![
                    AutomationPoint { tick: 0, value: 1.0 },
                    AutomationPoint { tick: 960, value: 0.25 },
                ],
            }],
            bindings: vec![gain_binding("lane-1", "cur_lane-1", "t1")],
            automation_clips: vec![],
        };
        let lanes = lanes_from_doc(&doc);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].id, "lane-1");
        assert_eq!(lanes[0].target_node, "track:t1");
        assert_eq!(lanes[0].param_id, 0);
        assert_eq!(
            lanes[0].points,
            vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 960, value: 0.25 },
            ]
        );
    }

    #[test]
    fn automation_set_upserts_the_curve_and_the_binding_as_one_pair() {
        // lane.id becomes the binding id; the curve id is derived and stable across calls
        let mut doc = ModulationDoc::default();
        let lane = AutomationLane {
            id: "lane-1".into(),
            target_node: "track:t1".into(),
            param_id: 0,
            points: vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 960, value: 0.5 },
            ],
        };
        apply_lane(&mut doc, "lane-1", Some(&lane));
        assert_eq!(doc.bindings.len(), 1);
        assert_eq!(doc.bindings[0].id, "lane-1", "lane.id becomes the binding id");
        assert_eq!(doc.bindings[0].mode, BindingMode::Multiply);
        assert_eq!(
            doc.bindings[0].target,
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain }
        );
        let curve_id = match &doc.bindings[0].source {
            Source::Curve { curve_id } => curve_id.clone(),
            other => panic!("expected curve source, got {other:?}"),
        };
        assert_eq!(curve_id, curve_id_for_lane("lane-1"));
        assert_eq!(doc.curves.len(), 1);
        assert_eq!(doc.curves[0].id, curve_id);
        assert_eq!(doc.curves[0].points, lane.points);

        let mut again = lane.clone();
        again.points[1].value = 0.1;
        apply_lane(&mut doc, "lane-1", Some(&again));
        assert_eq!(doc.curves.len(), 1, "second upsert does not mint another curve");
        assert_eq!(doc.bindings.len(), 1);
        assert_eq!(doc.curves[0].id, curve_id, "the curve id is derived and stable across calls");
        assert_eq!(doc.curves[0].points[1].value, 0.1);
    }

    #[test]
    fn a_plugin_lane_round_trips_through_native_units() {
        // normalized 0.5 with rangeSnapshot 0..10000 reads back as 5000.0
        let doc = ModulationDoc {
            curves: vec![Curve {
                id: "cur_p".into(),
                name: String::new(),
                length_ticks: None,
                points: vec![AutomationPoint { tick: 0, value: 0.5 }],
            }],
            bindings: vec![Binding {
                id: "lane-p".into(),
                source: Source::Curve { curve_id: "cur_p".into() },
                target: TargetRef::PluginParam {
                    instance_id: "inst-1".into(),
                    param_id: 7,
                },
                mode: BindingMode::Absolute,
                depth: 1.0,
                range: Range::default(),
                domain: Domain::Normalized,
                range_snapshot: Some(RangeSnapshot { min: 0.0, max: 10_000.0 }),
                enabled: true,
            }],
            automation_clips: vec![],
        };
        let lanes = lanes_from_doc(&doc);
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].id, "lane-p");
        assert_eq!(lanes[0].target_node, "inst-1");
        assert_eq!(lanes[0].param_id, 7);
        assert_eq!(
            lanes[0].points[0].value, 5000.0,
            "normalized 0.5 with rangeSnapshot 0..10000 reads back as 5000.0"
        );
    }
}
