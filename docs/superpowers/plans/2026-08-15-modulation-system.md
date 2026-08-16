# Modulation System Implementation Plan (Track F)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single-target `AutomationLane` with a modulation graph — curves bound to targets through explicit bindings — so a track can carry several automation lanes, an automation track can drive many targets at once, and a clip can carry automation that loops with it.

**Architecture:** Document objects are `Curve` (normalized 0..1 point data), `Binding` (source → target, with mode/depth/range) and `AutomationClip` (a placement of a curve on an automation track). All looping and time-base resolution happens on the CONTROL thread at rebuild, expanding bindings into the same `AbsParamEvent` lists the RT thread already reads — so the audio callback learns nothing new. Track-gain and plugin-param automation keep their exact current behaviour, re-expressed as `mode:"multiply"` and `mode:"absolute"` bindings.

**Tech Stack:** Rust (Tauri backend, `parking_lot`, `serde`, `byteorder`), Svelte 5 runes + TypeScript frontend, `cargo test` + `vitest`.

**Spec:** `docs/superpowers/specs/2026-08-15-modulation-system-design.md` — read it before Task 1. The plan argues from it; every ruling reference below (R1–R5, §4.2, §5) points there.

## Global Constraints

- **Baseline at branch point: 673 backend tests + 276 frontend tests, all green.** Never merge a task that lowers either count without replacing what it removed.
- **No behaviour change in P0 (Tasks 1–7).** A v3 project must load, play, bounce and re-save with identical audio. The compatibility facade in Task 7 exists solely to hold this line.
- **RT discipline ([C1], ARCHITECTURE §13/§15.1):** no allocation, no locks, no tick→sample math, no blocking host calls on the audio callback. Everything tick-shaped is resolved on the control thread.
- **Values in curves are normalized `[0,1]`** (R3). Native units appear only in `rangeSnapshot` and in `domain:"native"` migration bindings.
- **Ticks are `u32` in points, `u64` on the timeline** — matching `AutomationPoint::tick` and `MidiClip::timeline_start_ticks` respectively. Do not unify them in this round.
- **Every edit goes through the transaction channel** as an op, gesture-bracketed, preview-locally/commit-once (Track D rulings 4 and 7). No new side channels.
- **New op enum arms are additive** and do NOT bump `OP_FORMAT_VERSION` (see its doc comment in `src-tauri/src/control/op.rs:32`).
- **Rust edition/style:** follow the surrounding file. Doc comments explain *why*, matching the density already in `plugins/automation.rs`.
- Run `cargo test` (in `src-tauri/`), `npm test` and `npx svelte-check` before every commit that touches the corresponding half.

---

## File Structure

**Created:**

| File | Responsibility |
|---|---|
| `src-tauri/src/modulation/mod.rs` | module root: re-exports, `ModulationDoc` |
| `src-tauri/src/modulation/model.rs` | `Curve`, `Source`, `TargetRef`, `Binding`, `AutomationClip`, `BindingMode`, validation/normalization |
| `src-tauri/src/modulation/resolve.rs` | `ResolvedTarget`, `resolve_target`, arbitration (`plan_targets`) |
| `src-tauri/src/modulation/compile.rs` | source expansion (timeline / automation track / clip envelope), `compile_track_ramps`, `compile_param_lanes`, `MAX_EVENTS_PER_BINDING` |
| `src-tauri/src/modulation/persist.rs` | v4 read/write of `modulation{}`, v3 `automation[]` migration, chunk GC |
| `src-tauri/src/modulation/compat.rs` | lane ⇄ (curve, binding) translation behind `automation_get`/`automation_set` |
| `src-tauri/src/modulation/commands.rs` | `modulation_get`, `modulation_set_curve`, `modulation_set_binding`, `automation_clip_set` |
| `src/lib/state/modulation.svelte.ts` | frontend mirror of curves/bindings/automation clips |
| `src/lib/components/ModulationLaneView.svelte` | one binding's editable curve overlay (generalizes `AutomationLaneView`) |
| `src/lib/components/LanePickerMenu.svelte` | the track header's target menu |
| `src/lib/components/AutomationTrackRow.svelte` | automation-track timeline row + its clips |
| `src/lib/components/ClipEnvelopeLane.svelte` | piano-roll envelope lane |
| `docs/adr/0008-modulation-graph.md` | the ADR R2 requires |

**Modified:** `src-tauri/src/lib.rs` (module + command registration), `src-tauri/src/control/session.rs` (`Session.modulation`, op arms), `src-tauri/src/control/op.rs` (new arms), `src-tauri/src/audio/rt.rs` (`RtGraph::track_ramps`), `src-tauri/src/audio/mixer.rs` (pan ramp), `src-tauri/src/audio/engine.rs` (`compile_automation`), `src-tauri/src/audio/offline.rs` (same table), `src-tauri/src/plugins/automation.rs` (keeps `AbsParamEvent`, `RampCursor`, `compile_lane`, chunk codec; loses the lane-as-document role), `src/lib/types/ipc.ts`, `src/lib/tauri.ts`, `src/lib/components/TrackHeader.svelte`, `src/lib/components/Timeline.svelte`, `src/lib/components/PianoRoll.svelte`, `next-prompt.md`, `docs/PHASE4-PLAN.md`, `README.md` + `CONTRIBUTING.md` (test counts).

**Deliberately untouched:** `plugins::automation::GainAutomatedNode` (Track D non-goal: it stays, tested, as the per-node ramp applier), the gesture/undo machinery, `OP_FORMAT_VERSION`.

---

# Phase P0 — Foundation (no behaviour change)

## Task 1: Model types

**Files:**
- Create: `src-tauri/src/modulation/mod.rs`, `src-tauri/src/modulation/model.rs`
- Modify: `src-tauri/src/lib.rs` (add `mod modulation;` next to `mod plugins;`)
- Test: inline `#[cfg(test)] mod tests` in `model.rs`

**Interfaces:**
- Consumes: `crate::plugins::automation::AutomationPoint` (unchanged struct, reused as the curve's point type).
- Produces: `Curve`, `Source`, `TargetRef`, `TrackParam`, `BindingMode`, `Binding`, `AutomationClip`, `ModulationDoc`, `normalize_curve`, `validate_binding`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/modulation/model.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_and_target_wire_shapes_are_tagged_camel_case() {
        let b = Binding {
            id: "bnd-1".into(),
            source: Source::Curve { curve_id: "cur-1".into() },
            target: TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Pan },
            mode: BindingMode::Absolute,
            depth: 1.0,
            range: Range { min: 0.0, max: 1.0 },
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        };
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains(r#""source":{"kind":"curve","curveId":"cur-1"}"#), "got: {s}");
        assert!(s.contains(r#""target":{"kind":"trackParam","trackId":"t1","param":"pan"}"#), "got: {s}");
        assert!(s.contains(r#""mode":"absolute""#), "got: {s}");
        let back: Binding = serde_json::from_str(&s).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn normalize_curve_sorts_collapses_duplicates_and_clamps() {
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
    fn validate_binding_rejects_zero_length_ids_and_out_of_range_depth() {
        let mut b = Binding {
            id: "bnd-1".into(),
            source: Source::Curve { curve_id: String::new() },
            target: TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain },
            mode: BindingMode::Multiply,
            depth: 1.0,
            range: Range { min: 0.0, max: 1.0 },
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        };
        assert!(validate_binding(&b).is_err(), "empty curveId must be rejected");
        b.source = Source::Curve { curve_id: "cur-1".into() };
        b.depth = 4.0;
        assert!(validate_binding(&b).is_err(), "depth outside [-1,1] must be rejected");
        b.depth = -1.0;
        assert!(validate_binding(&b).is_ok());
    }

    #[test]
    fn self_targets_are_only_valid_under_a_clip_envelope_source() {
        let b = Binding {
            id: "bnd-1".into(),
            source: Source::Curve { curve_id: "cur-1".into() },
            target: TargetRef::SelfTrackParam { param: TrackParam::Gain },
            mode: BindingMode::Multiply,
            depth: 1.0,
            range: Range { min: 0.0, max: 1.0 },
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        };
        assert!(validate_binding(&b).is_err(), "a self target needs a clipEnvelope source");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd src-tauri && cargo test modulation::model`
Expected: FAIL — `unresolved module` / the types do not exist.

- [ ] **Step 3: Write the implementation**

`src-tauri/src/modulation/mod.rs`:

```rust
//! The modulation graph (Track F): curves are shapes, bindings say what a
//! shape drives and how, targets name the thing driven. Replaces Track D's
//! `AutomationLane`, whose `{targetNode: String, paramId: u32}` pair could
//! not address a fader — the identity gap ADR 0004 recorded and assigned to
//! this work.
//!
//! Design: `docs/superpowers/specs/2026-08-15-modulation-system-design.md`.

pub mod model;

pub use model::{
    AutomationClip, Binding, BindingMode, Curve, Domain, Range, RangeSnapshot, Source, TargetRef,
    TrackParam,
};

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
    /// Automation clips on one track, in timeline order.
    pub fn clips_on(&self, track_id: &str) -> Vec<&AutomationClip> {
        let mut v: Vec<&AutomationClip> =
            self.automation_clips.iter().filter(|c| c.track_id == track_id).collect();
        v.sort_by_key(|c| c.timeline_start_ticks);
        v
    }
}
```

`src-tauri/src/modulation/model.rs`:

```rust
//! Document objects. Wire format is camelCase tagged JSON; every union is
//! `#[serde(tag = "kind")]` so a future arm (the node-graph round's `port`,
//! §8.1 of the design) is additive for readers.

use serde::{Deserialize, Serialize};

pub use crate::plugins::automation::AutomationPoint;

/// A reusable shape. Values are normalized `[0,1]`; ticks are relative to
/// the curve's own origin (R3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Curve {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// Natural/loop length. `None` = a timeline curve with no intrinsic length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length_ticks: Option<u64>,
    #[serde(default)]
    pub points: Vec<AutomationPoint>,
}

/// Where a value comes from. The time base is IMPLIED by the kind — there is
/// deliberately no separate `timeBase` field (design §3.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Source {
    /// Absolute timeline curve — Track D's lane, migrated.
    Curve { curve_id: String },
    /// The evaluated output of an automation track's clips.
    AutomationTrack { track_id: String },
    /// A curve owned by a clip's CONTENT, looping with every placement of it
    /// (keyed by content, not placement — ADR 0004).
    ClipEnvelope { content_id: String, curve_id: String },
    // Reserved (design §8.2-3), parsed but never produced this round:
    Lfo { modulator_id: String },
    Macro { macro_id: String },
    MidiCc { modulator_id: String },
    EnvFollower { modulator_id: String },
}

/// Built-in track parameters. `Mute` and `Send(n)` are format-and-resolver
/// only this round: `resolve_target` returns `None` for them, following the
/// existing "an unknown target is ignored, never guessed at" policy.
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
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TargetRef {
    TrackParam { track_id: String, param: TrackParam },
    PluginParam { instance_id: String, param_id: u32 },
    /// Clip-envelope only: resolves against the track the PLACEMENT plays on,
    /// so shared content stays sane.
    SelfTrackParam { param: TrackParam },
    /// Clip-envelope only: the instrument on the placement's track, through
    /// `TrackState::instrument_id == "plugin:<id>"`.
    SelfInstrumentParam { param_id: u32 },
    // Reserved (design §8.1, §8.3):
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

/// The target's native range at bind time — diagnostics and migration only
/// (design §7); never used to evaluate a normalized binding.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeSnapshot {
    pub min: f32,
    pub max: f32,
}

/// Curve values are normalized unless a v3 migration could not learn the
/// param's range (plugin missing at load) — see design §7.
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
    /// `[-1, 1]`. Every mode is inert at 0 (design §4.2), which is what makes
    /// a depth slider safe to drag through zero.
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

/// A placement of a curve on an automation track. Same field names and the
/// same loop rule as `MidiClip` on purpose: `content_length_ticks` absent
/// means "no repeat", present means "repeat every N ticks".
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
    /// The repeat period: the content length when set, else the placement
    /// length (mirrors `MidiClip::effective_content_length_ticks`).
    pub fn effective_content_length_ticks(&self) -> u64 {
        self.content_length_ticks.filter(|n| *n > 0).unwrap_or(self.length_ticks)
    }
}

/// Validate + normalize a curve in place: finite values clamped to `[0,1]`,
/// points sorted by tick, duplicate ticks collapsed last-wins (the same
/// batch semantics `normalize_lane` established for lanes).
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
    curve.points.dedup_by_key(|p| p.tick); // keeps the FIRST of a run…
    // …so re-apply last-wins explicitly: walk the original order again.
    for p in curve.points.iter_mut() {
        p.value = p.value.clamp(0.0, 1.0);
    }
    Ok(())
}

pub fn validate_binding(b: &Binding) -> Result<(), String> {
    if b.id.is_empty() {
        return Err("binding id must not be empty".into());
    }
    if !b.depth.is_finite() || b.depth < -1.0 || b.depth > 1.0 {
        return Err(format!("depth must be finite and within [-1,1], got {}", b.depth));
    }
    if !b.range.min.is_finite() || !b.range.max.is_finite() || b.range.min > b.range.max {
        return Err("range must be finite with min <= max".into());
    }
    match &b.source {
        Source::Curve { curve_id } | Source::ClipEnvelope { curve_id, .. } if curve_id.is_empty() => {
            return Err("curveId must not be empty".into())
        }
        Source::AutomationTrack { track_id } if track_id.is_empty() => {
            return Err("trackId must not be empty".into())
        }
        _ => {}
    }
    let self_target =
        matches!(b.target, TargetRef::SelfTrackParam { .. } | TargetRef::SelfInstrumentParam { .. });
    if self_target && !matches!(b.source, Source::ClipEnvelope { .. }) {
        return Err("a self* target is only meaningful under a clipEnvelope source".into());
    }
    Ok(())
}
```

**Note on `normalize_curve`'s dedup:** `dedup_by_key` keeps the first of each
run, but the contract is last-wins. Implement it as a reverse-stable pass
instead:

```rust
    curve.points.sort_by_key(|p| p.tick);
    // last-wins on duplicate ticks: walk from the end, keep the first seen.
    let mut out: Vec<AutomationPoint> = Vec::with_capacity(curve.points.len());
    for p in curve.points.iter().rev() {
        if out.last().map(|q: &AutomationPoint| q.tick) != Some(p.tick) {
            out.push(*p);
        }
    }
    out.reverse();
    curve.points = out;
```

Use this version; the `dedup_by_key` line above is shown only to explain why
it is wrong.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test modulation::model`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/modulation src-tauri/src/lib.rs
git commit -m "feat(modulation): document objects for the modulation graph"
```

---

## Task 2: Target resolution and arbitration

**Files:**
- Create: `src-tauri/src/modulation/resolve.rs`
- Modify: `src-tauri/src/modulation/mod.rs` (`pub mod resolve;`)
- Test: inline in `resolve.rs`

**Interfaces:**
- Consumes: Task 1's `Binding`, `TargetRef`, `TrackParam`, `BindingMode`.
- Produces:
  - `enum ResolvedTarget { TrackGain(String), TrackPan(String), PluginParam { instance: String, index: u32 } }`
  - `fn resolve_target(t: &TargetRef, ctx: &ResolveCtx) -> Option<ResolvedTarget>`
  - `struct ResolveCtx<'a> { pub self_track: Option<&'a str>, pub instrument_of: &'a dyn Fn(&str) -> Option<String> }`
  - `fn plan_targets(bindings: &[Binding], ctx: &ResolveCtx) -> TargetPlan`
  - `struct TargetPlan { pub groups: Vec<TargetGroup>, pub conflicts: Vec<String> }`
  - `struct TargetGroup { pub target: ResolvedTarget, pub absolute: Option<usize>, pub multiply: Vec<usize>, pub add: Vec<usize> }` (indices into `bindings`)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modulation::model::*;

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

    fn ctx() -> ResolveCtx<'static> {
        ResolveCtx { self_track: None, instrument_of: &|_| None }
    }

    #[test]
    fn unknown_and_unimplemented_targets_resolve_to_none() {
        let c = ctx();
        assert_eq!(
            resolve_target(&TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain }, &c),
            Some(ResolvedTarget::TrackGain("t1".into()))
        );
        assert_eq!(
            resolve_target(&TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Mute }, &c),
            None,
            "mute has no engine path this round and must be ignored, never guessed at"
        );
        assert_eq!(
            resolve_target(&TargetRef::Port { node_id: "n".into(), port_id: "p".into() }, &c),
            None,
            "the reserved port arm resolves to nothing until the node-graph round"
        );
    }

    #[test]
    fn self_targets_resolve_through_the_placements_track() {
        let c = ResolveCtx { self_track: Some("t9"), instrument_of: &|t| {
            (t == "t9").then(|| "inst-1".to_string())
        }};
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
    fn one_absolute_binding_wins_and_the_second_is_reported() {
        let t = || TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Pan };
        let bindings = vec![
            bind("first", BindingMode::Absolute, t()),
            bind("second", BindingMode::Absolute, t()),
            bind("m", BindingMode::Multiply, t()),
        ];
        let plan = plan_targets(&bindings, &ctx());
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].absolute, Some(0), "first in document order wins");
        assert_eq!(plan.groups[0].multiply, vec![2]);
        assert_eq!(plan.conflicts, vec!["second".to_string()]);
    }

    #[test]
    fn disabled_bindings_are_left_out_entirely() {
        let mut b = bind("x", BindingMode::Absolute,
            TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain });
        b.enabled = false;
        let plan = plan_targets(&[b], &ctx());
        assert!(plan.groups.is_empty());
        assert!(plan.conflicts.is_empty());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd src-tauri && cargo test modulation::resolve`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write the implementation**

```rust
//! Target resolution and per-target arbitration (design §4.3).
//!
//! Track D's `compile_gain_ramps` documented "if two lanes name the same
//! track's gain, the LAST one wins, silently" — safe then, because the UI
//! could not produce a collision. An automation track bound to many targets
//! can, so last-wins-silently is replaced here by first-wins-and-report.

use super::model::{Binding, BindingMode, TargetRef, TrackParam};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedTarget {
    TrackGain(String),
    TrackPan(String),
    PluginParam { instance: String, index: u32 },
}

/// What a `self*` target needs to resolve: the track a placement plays on,
/// and a way to find that track's instrument instance.
pub struct ResolveCtx<'a> {
    pub self_track: Option<&'a str>,
    pub instrument_of: &'a dyn Fn(&str) -> Option<String>,
}

pub fn resolve_target(t: &TargetRef, ctx: &ResolveCtx) -> Option<ResolvedTarget> {
    match t {
        TargetRef::TrackParam { track_id, param } if !track_id.is_empty() => match param {
            TrackParam::Gain => Some(ResolvedTarget::TrackGain(track_id.clone())),
            TrackParam::Pan => Some(ResolvedTarget::TrackPan(track_id.clone())),
            // No engine path this round (design §3.3).
            TrackParam::Mute | TrackParam::Send0 | TrackParam::Send1 => None,
        },
        TargetRef::PluginParam { instance_id, param_id } if !instance_id.is_empty() => {
            Some(ResolvedTarget::PluginParam { instance: instance_id.clone(), index: *param_id })
        }
        TargetRef::SelfTrackParam { param } => {
            let t = ctx.self_track?;
            resolve_target(&TargetRef::TrackParam { track_id: t.to_string(), param: *param }, ctx)
        }
        TargetRef::SelfInstrumentParam { param_id } => {
            let t = ctx.self_track?;
            let instance = (ctx.instrument_of)(t)?;
            Some(ResolvedTarget::PluginParam { instance, index: *param_id })
        }
        // Reserved arms, and malformed refs.
        _ => None,
    }
}

#[derive(Debug, Default)]
pub struct TargetGroup {
    pub target: ResolvedTarget,
    pub absolute: Option<usize>,
    pub multiply: Vec<usize>,
    pub add: Vec<usize>,
}

#[derive(Debug, Default)]
pub struct TargetPlan {
    pub groups: Vec<TargetGroup>,
    /// Binding ids dropped as a second `absolute` owner of a target.
    pub conflicts: Vec<String>,
}

/// Group enabled, resolvable bindings by target and pick the one `absolute`
/// owner per target: FIRST in document order (design §4.3).
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
```

`TargetGroup`'s `#[derive(Default)]` needs `ResolvedTarget: Default`; instead
of adding a meaningless default, drop `Default` from `TargetGroup` and
construct it explicitly as shown above.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test modulation::resolve`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/modulation
git commit -m "feat(modulation): target resolution and per-target arbitration"
```

---

## Task 3: Source expansion and compilation

**Files:**
- Create: `src-tauri/src/modulation/compile.rs`
- Modify: `src-tauri/src/modulation/mod.rs`
- Test: inline in `compile.rs`

**Interfaces:**
- Consumes: Tasks 1–2; `crate::plugins::automation::{AbsParamEvent, compile_lane, AutomationLane}`; `crate::midi::TempoMap`.
- Produces:
  - `pub const MAX_EVENTS_PER_BINDING: usize = 200_000;`
  - `fn expand_source(doc: &ModulationDoc, b: &Binding, placements: &[Placement], map: &TempoMap) -> Vec<AbsParamEvent>`
  - `struct Placement { pub start_ticks: u64, pub length_ticks: u64, pub content_length_ticks: u64 }`
  - `fn combine(base: f32, groups_values: &GroupValues) -> f32` — the §4.2 formula
  - `fn compile_track_ramps(...) -> TrackRampTable` where `pub struct TrackRamps { pub gain: Option<Arc<Vec<AbsParamEvent>>>, pub pan: Option<Arc<Vec<AbsParamEvent>>> }` and `pub type TrackRampTable = Vec<TrackRamps>`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi::types::{TempoEvent, DEFAULT_PPQ};

    fn map() -> TempoMap {
        TempoMap::new(DEFAULT_PPQ, vec![TempoEvent { tick: 0, bpm: 120.0 }], 48_000).unwrap()
    }

    /// One quarter note at 120 bpm = 0.5 s = 24000 samples at 48 kHz.
    #[test]
    fn a_clip_placement_repeats_the_curve_every_content_length() {
        let curve = Curve {
            id: "cur".into(),
            name: "env".into(),
            length_ticks: Some(DEFAULT_PPQ as u64),
            points: vec![
                AutomationPoint { tick: 0, value: 0.0 },
                AutomationPoint { tick: DEFAULT_PPQ - 1, value: 1.0 },
            ],
        };
        let placements = vec![Placement {
            start_ticks: 0,
            length_ticks: (DEFAULT_PPQ as u64) * 3,
            content_length_ticks: DEFAULT_PPQ as u64,
        }];
        let events = expand_repeated(&curve, &placements, &map());
        // three repeats, two points each
        assert_eq!(events.len(), 6, "got {events:?}");
        assert_eq!(events[0].sample, 0);
        assert_eq!(events[2].sample, 24_000, "second repeat starts one quarter note in");
        assert_eq!(events[4].sample, 48_000);
        assert!((events[2].value - 0.0).abs() < 1e-6, "each repeat restarts the shape");
    }

    #[test]
    fn expansion_is_cropped_to_the_placement() {
        let curve = Curve {
            id: "cur".into(),
            name: "env".into(),
            length_ticks: Some(DEFAULT_PPQ as u64),
            points: vec![
                AutomationPoint { tick: 0, value: 0.0 },
                AutomationPoint { tick: DEFAULT_PPQ / 2, value: 1.0 },
            ],
        };
        // one and a half repeats fit
        let placements = vec![Placement {
            start_ticks: 0,
            length_ticks: (DEFAULT_PPQ as u64) * 3 / 2,
            content_length_ticks: DEFAULT_PPQ as u64,
        }];
        let events = expand_repeated(&curve, &placements, &map());
        assert!(
            events.iter().all(|e| e.sample < 36_000),
            "no event may land past the placement end: {events:?}"
        );
    }

    #[test]
    fn expansion_stops_at_the_cap_instead_of_allocating_without_limit() {
        let curve = Curve {
            id: "cur".into(),
            name: "env".into(),
            length_ticks: Some(2),
            points: (0..64).map(|i| AutomationPoint { tick: i % 2, value: 0.5 }).collect(),
        };
        let placements = vec![Placement {
            start_ticks: 0,
            length_ticks: 10_000_000,
            content_length_ticks: 2,
        }];
        let events = expand_repeated(&curve, &placements, &map());
        assert!(events.len() <= MAX_EVENTS_PER_BINDING, "cap must hold, got {}", events.len());
    }

    #[test]
    fn combine_reproduces_track_ds_two_behaviours() {
        // Track gain: multiply at full depth on a unity base == today's ramp.
        let gain = combine(1.0, &GroupValues { absolute: None, multiply: vec![0.25], add: vec![] });
        assert!((gain - 0.25).abs() < 1e-6);
        // Plugin param: absolute at full depth replaces the stored value.
        let p = combine(0.9, &GroupValues { absolute: Some(0.2), multiply: vec![], add: vec![] });
        assert!((p - 0.2).abs() < 1e-6);
        // add stacks, output clamps
        let a = combine(0.5, &GroupValues { absolute: None, multiply: vec![], add: vec![0.8] });
        assert!((a - 1.0).abs() < 1e-6, "clamped to 1.0, got {a}");
    }

    #[test]
    fn depth_zero_is_inert_in_every_mode() {
        for mode in [BindingMode::Absolute, BindingMode::Multiply, BindingMode::Add] {
            let v = contribution(mode, /*base*/ 0.4, /*mapped*/ 1.0, /*depth*/ 0.0);
            assert!((v.applied(0.4) - 0.4).abs() < 1e-6, "{mode:?} must be inert at depth 0");
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd src-tauri && cargo test modulation::compile`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Write the implementation**

Key pieces (write the module around them):

```rust
/// Hard bound on one binding's expansion (design §5). A one-bar envelope
/// under a 20-minute placement is ~1150 repeats and perfectly fine; the
/// product is nonetheless unbounded in principle, and the audio thread must
/// never wait on an unbounded control-thread allocation. Lazy per-block
/// evaluation is the eventual answer (design §8.7).
pub const MAX_EVENTS_PER_BINDING: usize = 200_000;

pub struct Placement {
    pub start_ticks: u64,
    pub length_ticks: u64,
    pub content_length_ticks: u64,
}

/// Repeat `curve` across every placement, cropped to each placement's end,
/// compiled to absolute samples through the tempo map. Ticks never cross to
/// the RT thread — this function is the boundary.
pub fn expand_repeated(
    curve: &Curve,
    placements: &[Placement],
    map: &TempoMap,
) -> Vec<AbsParamEvent> {
    let mut out: Vec<AbsParamEvent> = Vec::new();
    for p in placements {
        let period = p.content_length_ticks.max(1);
        let mut origin = p.start_ticks;
        let end = p.start_ticks + p.length_ticks;
        while origin < end {
            for pt in &curve.points {
                let tick = origin + pt.tick as u64;
                if tick >= end {
                    continue;
                }
                out.push(AbsParamEvent { sample: map.sample_at_tick(tick), value: pt.value });
                if out.len() >= MAX_EVENTS_PER_BINDING {
                    out.sort_by_key(|e| e.sample);
                    return out;
                }
            }
            origin += period;
        }
    }
    out.sort_by_key(|e| e.sample);
    out
}
```

Check `TempoMap`'s actual tick→sample entry point before writing
`map.sample_at_tick`: Track D's `compile_lane` in
`src-tauri/src/plugins/automation.rs` already does this conversion — reuse
the exact call it makes rather than inventing a name.

The combination formula (design §4.2), evaluated per event after merging all
contributing event lists onto one sorted timeline:

```rust
pub struct GroupValues {
    pub absolute: Option<f32>,
    pub multiply: Vec<f32>,
    pub add: Vec<f32>,
}

/// `out = clamp(v * f + a, 0, 1)` where `v` is the absolute contribution or
/// the base, `f` the product of multiply contributions, `a` the sum of adds.
pub fn combine(base: f32, g: &GroupValues) -> f32 {
    let v = g.absolute.unwrap_or(base);
    let f: f32 = g.multiply.iter().product();
    let a: f32 = g.add.iter().sum();
    (v * f + a).clamp(0.0, 1.0)
}

/// One binding's already-depth-scaled contribution, per mode:
///   absolute: base + depth * (m - base)
///   multiply: (1 - depth) + depth * m
///   add:      depth * m
pub fn contribution(mode: BindingMode, base: f32, mapped: f32, depth: f32) -> Contribution { … }
```

`mapped` is `range.min + (range.max - range.min) * source_value`, except for
`domain: Native` bindings, where the stored value passes through unchanged
(design §7).

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd src-tauri && cargo test modulation::compile`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/modulation
git commit -m "feat(modulation): source expansion, looping, and the combination law"
```

---

## Task 4: Persistence — v4 write, v3 migration

**Files:**
- Create: `src-tauri/src/modulation/persist.rs`
- Modify: `src-tauri/src/modulation/mod.rs`
- Test: inline in `persist.rs` (use `tempfile`, as `plugins/automation.rs`'s persistence tests do — copy their harness shape)

**Interfaces:**
- Consumes: Task 1 types; `crate::plugins::automation::{encode_points, decode_points, AUTOMATION_DIR}`; `crate::midi::persist::update_project_v2`.
- Produces: `fn save_into_project(dir: &Path, doc: &ModulationDoc) -> Result<(), String>`, `fn load_from_project(dir: &Path) -> Result<ModulationDoc, String>`, `fn migrate_v3_lanes(lanes: &[AutomationLane], params: &dyn Fn(&str, u32) -> Option<(f32, f32)>) -> ModulationDoc`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn v3_track_gain_lane_migrates_to_a_multiply_binding_with_the_points_untouched() {
    let lanes = vec![AutomationLane {
        id: "lane-1".into(),
        target_node: "track:t1".into(),
        param_id: 0,
        points: vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 960, value: 0.25 },
        ],
    }];
    let doc = migrate_v3_lanes(&lanes, &|_, _| None);
    assert_eq!(doc.curves.len(), 1);
    assert_eq!(doc.curves[0].points[1].value, 0.25, "gain points are ALREADY 0..1");
    assert_eq!(doc.bindings.len(), 1);
    let b = &doc.bindings[0];
    assert_eq!(b.mode, BindingMode::Multiply, "Track D ruling 6: a multiplier on the fader");
    assert_eq!(b.depth, 1.0);
    assert_eq!(b.target, TargetRef::TrackParam { track_id: "t1".into(), param: TrackParam::Gain });
    assert_eq!(b.id, "lane-1", "the lane id carries over so journals and UI keep addressing it");
}

#[test]
fn v3_plugin_lane_normalizes_when_the_param_range_is_known() {
    let lanes = vec![AutomationLane {
        id: "lane-2".into(),
        target_node: "inst-1".into(),
        param_id: 7,
        points: vec![AutomationPoint { tick: 0, value: 5_000.0 }],
    }];
    let doc = migrate_v3_lanes(&lanes, &|inst, idx| {
        (inst == "inst-1" && idx == 7).then_some((0.0, 10_000.0))
    });
    assert_eq!(doc.curves[0].points[0].value, 0.5);
    assert_eq!(doc.bindings[0].mode, BindingMode::Absolute, "Track D ruling 2: overrides the knob");
    assert_eq!(doc.bindings[0].domain, Domain::Normalized);
    assert_eq!(doc.bindings[0].range_snapshot, Some(RangeSnapshot { min: 0.0, max: 10_000.0 }));
}

#[test]
fn v3_plugin_lane_with_an_unknown_range_stays_native_so_the_project_still_sounds_right() {
    let lanes = vec![AutomationLane {
        id: "lane-3".into(),
        target_node: "inst-gone".into(),
        param_id: 2,
        points: vec![AutomationPoint { tick: 0, value: 440.0 }],
    }];
    let doc = migrate_v3_lanes(&lanes, &|_, _| None);
    assert_eq!(doc.curves[0].points[0].value, 440.0, "left alone — normalizing it would be a guess");
    assert_eq!(doc.bindings[0].domain, Domain::Native);
    assert_eq!(doc.bindings[0].range_snapshot, None);
}

#[test]
fn save_then_load_round_trips_and_writes_schema_version_4() { … }

#[test]
fn loading_a_v3_project_directory_yields_the_migrated_doc_and_leaves_the_file_alone_until_save() { … }

#[test]
fn stale_point_chunks_are_gcd_after_a_successful_save() { … }
```

Fill the last three bodies from the existing tests in
`src-tauri/src/plugins/automation.rs` (`save_into_project` / `load_*` /
GC tests) — same tempdir harness, same assertions, new object shape.

- [ ] **Step 2: Run to verify it fails**

Run: `cd src-tauri && cargo test modulation::persist`
Expected: FAIL.

- [ ] **Step 3: Implement**

- Write `modulation{}` into `project.json` through `update_project_v2`, set `schemaVersion` to 4, and REMOVE any `automation[]` key in the same edit (one-way upgrade, design §7).
- Point chunks keep `encode_points`/`decode_points` and the `automation/` directory unchanged; one chunk per curve, referenced as `pointsRef`.
- GC: collect live chunk names, delete every other `*.bin` after a successful write — copy `plugins::automation::save_into_project`'s existing loop verbatim.
- `load_from_project`: read `modulation{}` when present; otherwise read `automation[]` and run `migrate_v3_lanes`. A corrupt chunk degrades to an empty curve rather than failing the open (existing rule).

- [ ] **Step 4: Run to verify they pass**

Run: `cd src-tauri && cargo test modulation::persist`

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/modulation
git commit -m "feat(modulation): project v4 persistence with a one-way v3 migration"
```

---

## Task 5: Session, ops, and the rebuild pin

**Files:**
- Modify: `src-tauri/src/control/session.rs` (add `pub modulation: ModulationDoc` to `Session`; new apply arms), `src-tauri/src/control/op.rs` (new arms), `src-tauri/src/control/mod.rs` (`ControlPlane` accessors + persist effect)
- Test: inline in `session.rs`'s existing test module + `op.rs`'s wire-form tests

**Interfaces:**
- Produces on `Op`:
  ```rust
  ModulationSetCurve { key: String, curve: Option<crate::modulation::Curve> },
  ModulationSetBinding { key: String, binding: Option<crate::modulation::Binding> },
  AutomationClipSet { key: String, clip: Option<crate::modulation::AutomationClip> },
  ```
- Produces on `ControlPlane`: `modulation_doc()`, `set_curve(...)`, `set_binding(...)`, `set_automation_clip(...)`, each taking `TxMeta` like `set_automation_lane` does.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn set_curve_upserts_and_its_inverse_restores_store_truth() {
    // mirrors the existing AutomationSetLane apply/inverse test
}

#[test]
fn set_binding_rejects_an_invalid_binding_without_mutating_the_session() {
    // atomicity — round-2 §4: a failed apply leaves the session untouched
}

#[test]
fn every_modulation_op_sets_rebuild_on_its_effect() {
    // the Track D REBUILD PIN lesson: a document change nobody rebuilds on is inaudible
}

#[test]
fn deleting_a_curve_that_bindings_still_reference_leaves_those_bindings_inert_not_dangling() {
    // a binding whose curve is gone contributes nothing and is reported, never panics
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test control::session::tests::`

- [ ] **Step 3: Implement**

Model each arm on `Op::AutomationSetLane`'s existing arm
(`src-tauri/src/control/session.rs:776-808`): validate-and-normalize BEFORE
any mutation, upsert/remove by `key`, return the previous value as the
inverse payload (store truth, never guessed), and set `effect.rebuild = true`.

Keep `Op::AutomationSetLane` applying — route it through
`modulation::compat` (Task 7) so old journals replay into the new document.

- [ ] **Step 4: Run to verify they pass**

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/control src-tauri/src/modulation
git commit -m "feat(modulation): session document, ops, and rebuild wiring"
```

---

## Task 6: Engine — generalized ramp table and pan

**Files:**
- Modify: `src-tauri/src/audio/rt.rs` (`RtGraph::track_ramps`, keep `set_gain_ramps` as a shim), `src-tauri/src/audio/mixer.rs:378-430` (pan ramp), `src-tauri/src/audio/engine.rs:1245-1276` (`compile_automation`), `src-tauri/src/audio/offline.rs:144-168`
- Test: inline in `mixer.rs` (rendered-output tests) and `offline.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_pan_ramp_moves_energy_from_left_to_right_across_the_block() {
    // build a graph with one track, a full-scale mono clip, and a pan ramp
    // 0.0 -> 1.0 across N samples; assert the first frames are louder in L,
    // the last frames louder in R, and total energy stays within 1 dB
    // (constant-power law).
}

#[test]
fn pan_ramp_absent_leaves_the_atomic_pan_authoritative() {
    // no ramp => byte-identical output to today's path
}

#[test]
fn pan_automation_survives_a_bounce() {
    // offline::build_graph compiles the same table (design §6.3)
}
```

**Sabotage check (Track D's review lesson):** after these pass, temporarily
hard-code the pan ramp lookup to `None` and confirm the first test goes RED.
A test that passes with the feature disabled is vacuous — that exact defect
was found in Track D's Task 8.

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test audio::mixer`

- [ ] **Step 3: Implement**

```rust
// src-tauri/src/audio/rt.rs
pub struct TrackRamps {
    pub gain: Option<Arc<Vec<AbsParamEvent>>>,
    pub pan: Option<Arc<Vec<AbsParamEvent>>>,
}
// RtGraph gains: pub track_ramps: Vec<TrackRamps>
```

In `mixer::render_impl`, keep gain per-sample through `RampCursor`. For pan,
evaluate the ramp at the block's first and last frame, convert both through
`pan_gains`, and lerp `(gl, gr)` across the block — `pan_gains` is two trig
calls and per-sample evaluation would cost ~3M sin/cos per second at 32
tracks for a difference nobody can hear (design §6.1). Document that
divergence in a comment at the call site so a later reader does not "fix" it.

Attach the table BEFORE publishing the graph (RCU discipline, Track D
ruling 1). Keep `set_gain_ramps` as a shim that fills only the `gain` field,
so Track D's mixer tests keep compiling.

- [ ] **Step 4: Run the full backend suite**

Run: `cd src-tauri && cargo test`
Expected: baseline 673 + the new tests, zero failures.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/audio
git commit -m "feat(engine): per-track ramp table with pan automation"
```

---

## Task 7: IPC, compatibility facade, and the frontend mirror

**Files:**
- Create: `src-tauri/src/modulation/commands.rs`, `src-tauri/src/modulation/compat.rs`, `src/lib/state/modulation.svelte.ts`
- Modify: `src-tauri/src/lib.rs:251` (register the new commands beside the old), `src/lib/types/ipc.ts:520-540`, `src/lib/tauri.ts:148-155` and `:493-498`
- Test: `src/lib/state/modulation.svelte.test.ts`, plus inline Rust tests in `compat.rs`

**Interfaces:**
- Produces (Tauri commands): `modulation_get() -> ModulationSnapshot`, `modulation_set_curve(curve)`, `modulation_set_binding(binding)`, `automation_clip_set(clip)` — each returning the full `ModulationSnapshot { curves, bindings, automationClips }`, the same batch shape `automation_set` uses (D-03: one invoke carries a whole gesture's result).
- Produces (TS): `modulation` store with `curves`, `bindings`, `automationClips`, `bindingsFor(target)`, `preview(curveId, points)`, `commit(curve)`, `setBinding(b)`.

- [ ] **Step 1: Write the failing tests**

```rust
// compat.rs
#[test]
fn automation_get_still_reports_a_gain_binding_as_the_lane_it_used_to_be() {
    // curve + multiply binding on track gain  =>  AutomationLane { targetNode: "track:t1", paramId: 0 }
}

#[test]
fn automation_set_upserts_the_curve_and_the_binding_as_one_pair() {
    // lane.id becomes the binding id; the curve id is derived and stable across calls
}

#[test]
fn a_plugin_lane_round_trips_through_native_units() {
    // normalized 0.5 with rangeSnapshot 0..10000 reads back as 5000.0
}
```

```ts
// src/lib/state/modulation.svelte.test.ts
it("mirrors curves and bindings from the backend", async () => { … });
it("preview patches points locally without invoking", async () => { … });
it("commit sends the whole curve once", async () => { … });
```

- [ ] **Step 2: Run to verify they fail**

Run: `cd src-tauri && cargo test modulation::compat` and `npm test -- modulation`

- [ ] **Step 3: Implement**

The facade is the whole point of this task: `automation_get`/`automation_set`
keep working unchanged, translating to and from curves+bindings, so the
existing `automation.svelte.ts` store, `AutomationLaneView.svelte` and all
276 frontend tests stay green while the document underneath is replaced.
Do not touch any Svelte component in this task.

- [ ] **Step 4: Run BOTH suites and the type checker**

Run: `cd src-tauri && cargo test` then `npm test` then `npx svelte-check`
Expected: 673+ backend, 276+ frontend, svelte-check clean.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src src/lib
git commit -m "feat(modulation): IPC surface, lane compatibility facade, frontend mirror"
```

**P0 gate — verify before starting P1:** open an existing v3 project in the
running app (`PATH="$PWD/.venv-sidecars/bin:$PATH" npm run tauri dev`), play a
track that has gain automation, confirm it still ducks, save, and confirm
`project.json` now has `schemaVersion: 4` and a `modulation` object.

---

# Phase P1 — Several curves per track, and pan

## Task 8: Lane picker, multiple overlays, pan target

**Files:**
- Create: `src/lib/components/ModulationLaneView.svelte`, `src/lib/components/LanePickerMenu.svelte`
- Modify: `src/lib/components/TrackHeader.svelte:120-135`, `src/lib/components/Timeline.svelte:497-525`, `src/lib/components/plugins/PluginParamPanel.svelte:158-170`
- Delete: `src/lib/components/AutomationLaneView.svelte` (superseded; move its gesture logic verbatim into `ModulationLaneView`)
- Test: `src/lib/components/*.test.ts` equivalents of `automation-gesture.test.ts`

- [ ] **Step 1: Write the failing tests**

```ts
it("picking a target that has no curve mints a curve and a binding", async () => { … });
it("a track shows one overlay per visible binding, each editing its own curve", async () => { … });
it("pan lane values map 0..1 to hard left..hard right", () => {
  expect(panNativeOf(0)).toBeCloseTo(-1);
  expect(panNativeOf(0.5)).toBeCloseTo(0);
  expect(panNativeOf(1)).toBeCloseTo(1);
});
it("a drag previews locally and commits exactly once on pointerup", async () => { … });
```

The last one must be a port of `src/lib/state/automation-gesture.test.ts` —
including its regression assertion that a click-insert's commit is awaited
before the closing commit reads the store (the `commitLatest` barrier).

- [ ] **Step 2: Run to verify they fail** — `npm test`

- [ ] **Step 3: Implement**

- `ModulationLaneView` takes `{ binding, curve }` as props instead of deriving `gainLaneFor(track.id)`.
- The store's `visible` set becomes `visible: Map<trackId, Set<bindingId>>`.
- Replace `commitLatest(trackId)`'s track-keyed barrier with a `Map<bindingId, Promise>` — the KNOWN EDGE recorded in `automation.svelte.ts:99-109` becomes reachable the moment a track has two lanes, so it must be fixed here, not deferred again.
- The header's `A` opens `LanePickerMenu`: gain, pan, then each plugin instance's params grouped by instance, with a checkmark where a binding exists.

- [ ] **Step 4: Run `npm test`, `npx svelte-check`, and drive the app**

- [ ] **Step 5: Commit**

```bash
git add src/lib
git commit -m "feat(ui): several automation lanes per track, with a target picker and pan"
```

---

# Phase P2 — Automation tracks

## Task 9: Automation track kind, clips, and routing

**Files:**
- Create: `src/lib/components/AutomationTrackRow.svelte`
- Modify: `src-tauri/src/audio/types.rs` (accept `kind: "automation"`; excluded from `derive_slots`), `src-tauri/src/audio/engine.rs` (skip in the mixer graph), `src/lib/components/Timeline.svelte`, `src/lib/components/TrackHeader.svelte`
- Test: inline Rust tests + `src/lib/state/modulation.svelte.test.ts`

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn an_automation_track_takes_no_mixer_slot_and_renders_no_audio() { … }

#[test]
fn one_automation_track_bound_to_two_targets_moves_both() {
    // offline render: two tracks, one curve, two bindings, both amplitudes follow
}

#[test]
fn outside_every_automation_clip_the_target_falls_back_to_its_document_value() {
    // design §5: no contribution, NOT a held value
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement** — `derive_slots` must skip automation tracks, or every existing slot-indexed table shifts under an added automation track; that is the one place this task can silently break audio, so test it directly.

- [ ] **Step 4: Run `cargo test`, `npm test`, drive the app**

- [ ] **Step 5: Commit**

```bash
git commit -m "feat: automation tracks routed to several targets"
```

---

# Phase P3 — Clip envelopes

## Task 10: Content-keyed envelopes that loop with the clip

**Files:**
- Create: `src/lib/components/ClipEnvelopeLane.svelte`
- Modify: `src-tauri/src/modulation/compile.rs` (placements from `MidiClip`s sharing a `content_id`), `src/lib/components/PianoRoll.svelte`
- Test: inline Rust + frontend

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_clip_envelope_produces_the_same_shape_in_every_repeat() {
    // a 1-bar envelope under a 4-bar looping placement: sample the compiled
    // events at bar 1 and bar 3, assert equal values
}

#[test]
fn every_placement_of_shared_content_gets_the_envelope() {
    // two MidiClips with the same content_id, one envelope binding
}

#[test]
fn a_self_track_target_resolves_per_placement() {
    // the same content on two different tracks drives each one's own gain
}
```

- [ ] **Step 2–5:** as above; commit as
  `feat: clip envelopes that loop with the clip`.

---

# Phase P4 — Documentation

## Task 11: ADR, handoff, and the path forward

**Files:**
- Create: `docs/adr/0008-modulation-graph.md`
- Modify: `docs/adr/README.md`, `next-prompt.md`, `docs/PHASE4-PLAN.md` (a "Track F handoff" section in the shape Track D's uses), `README.md` + `CONTRIBUTING.md` (test counts, re-measured), `docs/SIDE-CHANNEL-INVENTORY.md` (only if a new side channel appeared — it should not have)

- [ ] **Step 1:** Write ADR 0008: the tagged `TargetRef` decision, why it is a step toward ports rather than a detour (R2), and what the node-graph round must do to finish it. Reference design §8.
- [ ] **Step 2:** Add the Track F handoff to `docs/PHASE4-PLAN.md`: what landed, per-task commits, the scope rulings taken under R5, the non-goals held (design §9), and any divergence found on the way.
- [ ] **Step 3:** Point `next-prompt.md` at design §8 as the ordered path to the finished system — R2 requires this to be findable locally, so link it, do not restate it.
- [ ] **Step 4:** Re-measure both suites and update the counts in `README.md` and `CONTRIBUTING.md`.
- [ ] **Step 5:** Commit as `docs: ADR 0008 and the Track F handoff`.

---

## Self-Review

**Spec coverage:** §3.1 Curve → T1; §3.2 Source → T1, T3; §3.3 TargetRef → T1, T2; §3.4 Binding → T1; §3.5 AutomationClip → T1, T9; §3.6 automation track → T9; §4.1 domains → T3, T6, T8; §4.2 combination → T3; §4.3 arbitration → T2; §5 time/looping/cap → T3, T9, T10; §6.1 engine table + pan → T6; §6.2 param driver → T5, T6; §6.3 export → T6; §7 persistence/migration → T4; §8 path forward → T11; §9 non-goals → held, nothing implements them; §10.1 → T8; §10.2 → T9; §10.3 → T10; §10.4 → T7 (facade) + T8; §11 verification → distributed across every task's tests; §12 phases → the plan's phase headings.

**Known thin spots, called out rather than hidden:** Tasks 5, 9 and 10 give
test names and intent but not full test bodies, because their assertions
depend on harness shapes (the session test module, the offline-render
helpers) that the implementer will have open in front of them and that this
plan would only paraphrase inaccurately. Every one of them names the exact
existing test to copy the harness from. Task 3 leaves one call
(`map.sample_at_tick`) to be read off `compile_lane` rather than guessed.
