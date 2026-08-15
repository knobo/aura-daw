//! Document objects for the modulation graph (Track F).
//!
//! Wire format is camelCase tagged JSON: every union is `#[serde(tag =
//! "kind")]` so the arms the design reserves — the node-graph round's
//! `port` target (design §8.1), the LFO/macro sources (§8.2-3) — are
//! additive for readers that predate them.
//!
//! Design: `docs/superpowers/specs/2026-08-15-modulation-system-design.md`.

use serde::{Deserialize, Serialize};

pub use crate::plugins::automation::AutomationPoint;

/// A reusable shape. Values are normalized `[0,1]` (owner ruling R3); ticks
/// are relative to the curve's OWN origin, never the timeline — where it
/// sounds is the binding's business, not the curve's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Curve {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Natural/loop length. `None` = a timeline curve with no intrinsic
    /// length (Track D's lane, migrated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_ticks: Option<u64>,
    #[serde(default)]
    pub points: Vec<AutomationPoint>,
}

/// Where a value comes from. The TIME BASE IS IMPLIED BY THE KIND — there is
/// deliberately no separate `timeBase` field, because every combination such
/// a field could express that is not already a kind here is meaningless
/// (design §3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Source {
    /// Absolute timeline curve: ticks are project ticks, the value holds
    /// before the first and after the last point.
    Curve { curve_id: String },
    /// The evaluated output of an automation track's clips. Outside every
    /// clip it contributes NOTHING (design §5) — that is what makes an
    /// automation track composable with a fader instead of fighting it.
    AutomationTrack { track_id: String },
    /// A curve owned by a clip's CONTENT, repeating with every placement of
    /// that content. Keyed by content, not placement (ADR 0004), so
    /// duplicating a clip duplicates its automation for free.
    ClipEnvelope { content_id: String, curve_id: String },
    // ---- Reserved (design §8.2-3): parsed, never produced this round. ----
    Lfo { modulator_id: String },
    Macro { macro_id: String },
    MidiCc { modulator_id: String },
    EnvFollower { modulator_id: String },
}

/// Built-in track parameters.
///
/// `Mute` and `Send0`/`Send1` are FORMAT AND RESOLVER ARMS ONLY this round:
/// `resolve_target` returns `None` for them, following the policy Track D
/// established for unknown built-in ids — an unimplemented target is
/// ignored, never guessed at. Sends have no engine infrastructure at all
/// yet; mute needs a de-click ramp that belongs with the engine round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrackParam {
    Gain,
    Pan,
    Mute,
    Send0,
    Send1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TargetRef {
    TrackParam { track_id: String, param: TrackParam },
    PluginParam { instance_id: String, param_id: u32 },
    /// Clip-envelope only: resolves against the track the PLACEMENT plays
    /// on, so shared content stays sane — an envelope that dips the volume
    /// dips whichever track it was dropped on.
    SelfTrackParam { param: TrackParam },
    /// Clip-envelope only: the instrument on the placement's track, reached
    /// through `TrackState::instrument_id == "plugin:<id>"`.
    SelfInstrumentParam { param_id: u32 },
    // ---- Reserved (design §8.1, §8.3). ----
    Macro { macro_id: String },
    Port { node_id: String, port_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BindingMode {
    Absolute,
    Add,
    Multiply,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    pub min: f32,
    pub max: f32,
}

impl Default for Range {
    fn default() -> Self {
        Self { min: 0.0, max: 1.0 }
    }
}

/// The target's native range at bind time. Diagnostics and migration only
/// (design §7) — a normalized binding never consults it to evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeSnapshot {
    pub min: f32,
    pub max: f32,
}

/// Curve values are normalized unless a v3 migration could not learn the
/// param's range (the plugin was missing at load). Then the points stay in
/// the param's own units and the evaluator passes them through — normalizing
/// them would have been a guess, and a guess here changes what the project
/// sounds like (design §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Domain {
    #[default]
    Normalized,
    Native,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub id: String,
    pub source: Source,
    pub target: TargetRef,
    pub mode: BindingMode,
    /// `[-1, 1]`. Every mode is inert at 0 (design §4.2), which is what
    /// makes a depth slider safe to drag through zero.
    pub depth: f32,
    #[serde(default)]
    pub range: Range,
    #[serde(default)]
    pub domain: Domain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_snapshot: Option<RangeSnapshot>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// A placement of a curve on an automation track. Deliberately the same
/// field names and the same loop rule as `MidiClip`, so the timeline's
/// existing clip geometry reads the same way for both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationClip {
    pub id: String,
    pub track_id: String,
    pub curve_id: String,
    pub timeline_start_ticks: u64,
    pub length_ticks: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_length_ticks: Option<u64>,
}

impl AutomationClip {
    /// The repeat period: the content length when set and non-zero, else the
    /// placement length (mirrors `MidiClip::effective_content_length_ticks`,
    /// so "absent means no repeat" holds for both clip kinds).
    pub fn effective_content_length_ticks(&self) -> u64 {
        self.content_length_ticks.filter(|n| *n > 0).unwrap_or(self.length_ticks)
    }
}

/// Validate + normalize a curve in place: values finite and clamped to
/// `[0,1]`, points sorted by tick, duplicate ticks collapsed LAST-WINS —
/// the batch semantics `plugins::automation::normalize_lane` established,
/// kept identical so a migrated lane normalizes the way it always did.
pub fn normalize_curve(curve: &mut Curve) -> Result<(), String> {
    if curve.id.is_empty() {
        return Err("curve id must not be empty".into());
    }
    for p in &curve.points {
        if !p.value.is_finite() {
            return Err(format!("non-finite automation value at tick {}", p.tick));
        }
    }
    curve.points.sort_by_key(|p| p.tick);
    // Last-wins on duplicate ticks. `dedup_by_key` keeps the FIRST of a run,
    // which is the opposite contract, so walk from the end instead.
    let mut kept: Vec<AutomationPoint> = Vec::with_capacity(curve.points.len());
    for p in curve.points.iter().rev() {
        if kept.last().map(|q: &AutomationPoint| q.tick) != Some(p.tick) {
            kept.push(*p);
        }
    }
    kept.reverse();
    curve.points = kept;
    for p in curve.points.iter_mut() {
        p.value = p.value.clamp(0.0, 1.0);
    }
    Ok(())
}

/// Structural validation of a binding. Semantic resolution (does this track
/// exist, is this param implemented) is `resolve::resolve_target`'s job —
/// a binding to a target this build cannot drive is VALID and inert, which
/// is what lets a project survive a missing plugin.
pub fn validate_binding(b: &Binding) -> Result<(), String> {
    if b.id.is_empty() {
        return Err("binding id must not be empty".into());
    }
    if !b.depth.is_finite() || !(-1.0..=1.0).contains(&b.depth) {
        return Err(format!("depth must be finite and within [-1,1], got {}", b.depth));
    }
    if !b.range.min.is_finite() || !b.range.max.is_finite() || b.range.min > b.range.max {
        return Err("range must be finite with min <= max".into());
    }
    match &b.source {
        Source::Curve { curve_id } | Source::ClipEnvelope { curve_id, .. } => {
            if curve_id.is_empty() {
                return Err("curveId must not be empty".into());
            }
            if let Source::ClipEnvelope { content_id, .. } = &b.source {
                if content_id.is_empty() {
                    return Err("contentId must not be empty".into());
                }
            }
        }
        Source::AutomationTrack { track_id } => {
            if track_id.is_empty() {
                return Err("trackId must not be empty".into());
            }
        }
        Source::Lfo { modulator_id }
        | Source::MidiCc { modulator_id }
        | Source::EnvFollower { modulator_id } => {
            if modulator_id.is_empty() {
                return Err("modulatorId must not be empty".into());
            }
        }
        Source::Macro { macro_id } => {
            if macro_id.is_empty() {
                return Err("macroId must not be empty".into());
            }
        }
    }
    let self_target =
        matches!(b.target, TargetRef::SelfTrackParam { .. } | TargetRef::SelfInstrumentParam { .. });
    if self_target && !matches!(b.source, Source::ClipEnvelope { .. }) {
        return Err("a self* target is only meaningful under a clipEnvelope source".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(source: Source, target: TargetRef, mode: BindingMode) -> Binding {
        Binding {
            id: "bnd-1".into(),
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

    #[test]
    fn source_and_target_wire_shapes_are_tagged_camel_case() {
        let b = binding(
            Source::Curve { curve_id: "cur-1".into() },
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Pan },
            BindingMode::Absolute,
        );
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains(r#""source":{"kind":"curve","curveId":"cur-1"}"#), "got: {s}");
        assert!(
            s.contains(r#""target":{"kind":"trackParam","trackId":"t1","param":"pan"}"#),
            "got: {s}"
        );
        assert!(s.contains(r#""mode":"absolute""#), "got: {s}");
        let back: Binding = serde_json::from_str(&s).unwrap();
        assert_eq!(back, b);
    }

    /// The reserved arms must survive a round trip today, or the node-graph
    /// round's `port` target is a format break instead of an addition.
    #[test]
    fn reserved_arms_round_trip() {
        let b = binding(
            Source::Lfo { modulator_id: "mod-1".into() },
            TargetRef::Port { node_id: "n1".into(), port_id: "cutoff".into() },
            BindingMode::Add,
        );
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains(r#""kind":"port""#), "got: {s}");
        assert_eq!(serde_json::from_str::<Binding>(&s).unwrap(), b);
    }

    #[test]
    fn binding_defaults_fill_in_for_a_minimal_row() {
        let b: Binding = serde_json::from_str(
            r#"{"id":"b","source":{"kind":"curve","curveId":"c"},
                "target":{"kind":"pluginParam","instanceId":"i","paramId":3},
                "mode":"add","depth":0.5}"#,
        )
        .unwrap();
        assert_eq!(b.range, Range { min: 0.0, max: 1.0 });
        assert_eq!(b.domain, Domain::Normalized);
        assert!(b.enabled, "a row without `enabled` is enabled");
    }

    #[test]
    fn normalize_curve_sorts_collapses_duplicates_last_wins_and_clamps() {
        let mut c = Curve {
            id: "cur-1".into(),
            name: "x".into(),
            length_ticks: None,
            points: vec![
                AutomationPoint { tick: 480, value: 2.0 },
                AutomationPoint { tick: 0, value: -1.0 },
                AutomationPoint { tick: 480, value: 0.25 },
            ],
        };
        normalize_curve(&mut c).unwrap();
        assert_eq!(
            c.points,
            vec![
                AutomationPoint { tick: 0, value: 0.0 },
                AutomationPoint { tick: 480, value: 0.25 },
            ],
            "sorted by tick, duplicate ticks last-wins, values clamped to [0,1]"
        );
    }

    #[test]
    fn normalize_curve_rejects_non_finite_values() {
        let mut c = Curve {
            id: "cur-1".into(),
            name: "x".into(),
            length_ticks: None,
            points: vec![AutomationPoint { tick: 0, value: f32::NAN }],
        };
        assert!(normalize_curve(&mut c).is_err());
    }

    #[test]
    fn validate_binding_rejects_empty_ids_and_out_of_range_depth() {
        let mut b = binding(
            Source::Curve { curve_id: String::new() },
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
            BindingMode::Multiply,
        );
        assert!(validate_binding(&b).is_err(), "empty curveId must be rejected");
        b.source = Source::Curve { curve_id: "cur-1".into() };
        b.depth = 4.0;
        assert!(validate_binding(&b).is_err(), "depth outside [-1,1] must be rejected");
        b.depth = -1.0;
        assert!(validate_binding(&b).is_ok());
    }

    #[test]
    fn self_targets_are_only_valid_under_a_clip_envelope_source() {
        let b = binding(
            Source::Curve { curve_id: "cur-1".into() },
            TargetRef::SelfTrackParam { param: TrackParam::Gain },
            BindingMode::Multiply,
        );
        assert!(validate_binding(&b).is_err(), "a self target needs a clipEnvelope source");

        let ok = binding(
            Source::ClipEnvelope { content_id: "con-1".into(), curve_id: "cur-1".into() },
            TargetRef::SelfTrackParam { param: TrackParam::Gain },
            BindingMode::Multiply,
        );
        assert!(validate_binding(&ok).is_ok());
    }

    #[test]
    fn an_automation_clip_without_a_content_length_does_not_repeat() {
        let c = AutomationClip {
            id: "acl".into(),
            track_id: "t".into(),
            curve_id: "cur".into(),
            timeline_start_ticks: 0,
            length_ticks: 3840,
            content_length_ticks: None,
        };
        assert_eq!(c.effective_content_length_ticks(), 3840);
        let looped = AutomationClip { content_length_ticks: Some(960), ..c.clone() };
        assert_eq!(looped.effective_content_length_ticks(), 960);
        let zero = AutomationClip { content_length_ticks: Some(0), ..c };
        assert_eq!(zero.effective_content_length_ticks(), 3840, "0 is not a period");
    }
}
