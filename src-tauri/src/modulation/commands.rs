//! Tauri commands for the modulation document (Track F Task 7).
//!
//! Same batch shape as `automation_get`/`automation_set` (D-03: one invoke
//! carries a whole gesture's result). Each mutator returns the full
//! [`ModulationSnapshot`] so the frontend store can adopt backend
//! normalization in one round-trip.

use serde::{Deserialize, Serialize};
use tauri::State;

use super::model::{AutomationClip, Binding, Curve};
use super::ModulationDoc;

/// Wire snapshot of `session.modulation`. Field names match the v4
/// `modulation{}` object (`automationClips` camelCase).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModulationSnapshot {
    pub curves: Vec<Curve>,
    pub bindings: Vec<Binding>,
    pub automation_clips: Vec<AutomationClip>,
}

impl From<ModulationDoc> for ModulationSnapshot {
    fn from(doc: ModulationDoc) -> Self {
        Self {
            curves: doc.curves,
            bindings: doc.bindings,
            automation_clips: doc.automation_clips,
        }
    }
}

fn snapshot(control: &crate::control::ControlPlane) -> ModulationSnapshot {
    control.modulation_doc().into()
}

/// The full modulation document. PURE session-lock read — no disk.
#[tauri::command]
pub fn modulation_get(
    control: State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<ModulationSnapshot, String> {
    Ok(snapshot(&control))
}

/// Upsert one curve (batch-shaped, D-03). Returns the updated snapshot.
#[tauri::command]
pub fn modulation_set_curve(
    mut curve: Curve,
    control: State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<ModulationSnapshot, String> {
    if curve.id.is_empty() {
        curve.id = uuid::Uuid::new_v4().to_string();
    }
    let key = curve.id.clone();
    control.set_curve(key, Some(curve), crate::control::op::TxMeta::user("edit curve"))?;
    Ok(snapshot(&control))
}

/// Upsert one binding. Returns the updated snapshot.
#[tauri::command]
pub fn modulation_set_binding(
    mut binding: Binding,
    control: State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<ModulationSnapshot, String> {
    if binding.id.is_empty() {
        binding.id = uuid::Uuid::new_v4().to_string();
    }
    let key = binding.id.clone();
    control.set_binding(key, Some(binding), crate::control::op::TxMeta::user("edit binding"))?;
    Ok(snapshot(&control))
}

/// Upsert one automation clip. Returns the updated snapshot.
#[tauri::command]
pub fn automation_clip_set(
    mut clip: AutomationClip,
    control: State<'_, std::sync::Arc<crate::control::ControlPlane>>,
) -> Result<ModulationSnapshot, String> {
    if clip.id.is_empty() {
        clip.id = uuid::Uuid::new_v4().to_string();
    }
    let key = clip.id.clone();
    control.set_automation_clip(
        key,
        Some(clip),
        crate::control::op::TxMeta::user("edit automation clip"),
    )?;
    Ok(snapshot(&control))
}
