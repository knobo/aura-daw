//! The modulation graph (Track F): curves are shapes, bindings say what a
//! shape drives and how, targets name the thing driven.
//!
//! Replaces Track D's `AutomationLane`, whose `{targetNode: String,
//! paramId: u32}` pair could not address a fader — the identity gap ADR 0004
//! recorded and assigned to this work ("automation-lane identity … remains a
//! live gap, explicitly assigned to the node-graph round"). The tagged
//! [`model::TargetRef`] is the step toward that round's port model, with the
//! `port` arm already reserved.
//!
//! Two properties of the model are worth knowing before reading further,
//! because they are why the change is not a rewrite of what Track D landed:
//!
//! - Track-gain automation is `mode: Multiply` on a unity base — Track D
//!   ruling 6 ("linear gain multipliers applied on top of the fader").
//! - Plugin-param automation is `mode: Absolute` — Track D ruling 2
//!   ("automation OVERRIDES the stored knob value during playback").
//!
//! So the migration is a translation, not a change of sound.
//!
//! Design: `docs/superpowers/specs/2026-08-15-modulation-system-design.md`.

pub mod compile;
pub mod model;
pub mod persist;
pub mod resolve;

pub use compile::{
    compile, CompileCtx, CompiledModulation, ParamLaneSpec, Placement, TrackRampSpec,
    MAX_EVENTS_PER_BINDING,
};

pub use resolve::{plan_targets, resolve_target, ResolveCtx, ResolvedTarget, TargetPlan};

pub use model::{
    normalize_curve, validate_binding, AutomationClip, AutomationPoint, Binding, BindingMode,
    Curve, Domain, Range, RangeSnapshot, Source, TargetRef, TrackParam,
};

pub use persist::{load_from_project, migrate_v3_lanes, save_into_project};

/// The document half of the modulation graph. Lives in `Session`, mirrors
/// `project.json`'s `modulation{}` object.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ModulationDoc {
    pub curves: Vec<Curve>,
    pub bindings: Vec<Binding>,
    pub automation_clips: Vec<AutomationClip>,
}

impl ModulationDoc {
    pub fn curve(&self, id: &str) -> Option<&Curve> {
        self.curves.iter().find(|c| c.id == id)
    }

    pub fn binding(&self, id: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.curves.is_empty() && self.bindings.is_empty() && self.automation_clips.is_empty()
    }

    /// Automation clips on one track, in timeline order.
    pub fn clips_on(&self, track_id: &str) -> Vec<&AutomationClip> {
        let mut v: Vec<&AutomationClip> =
            self.automation_clips.iter().filter(|c| c.track_id == track_id).collect();
        v.sort_by_key(|c| c.timeline_start_ticks);
        v
    }
}
