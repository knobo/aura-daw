//! AURA plugin hosting (phase 3) — CLAP + LV2. ARCHITECTURE §15.
//!
//! * [`descriptor`] — wire types (`plugin-descriptor` / `plugin-state`
//!   schemas).
//! * [`scan`]       — discovery: LV2 world (metadata-only, safe) + CLAP
//!   search paths (via the sacrificial [`scan_worker`] subprocess).
//! * [`host`]       — the ONE shared plugin main thread (`aura-plugin-host`)
//!   both format hosts live on, + the silent [`StubPluginNode`] occupying
//!   the seam for un-hosted instances.
//! * [`clap_host`]  — real CLAP hosting (zone P1: clack instantiation,
//!   RT processing, params, state) in the thread's `"clap"` slot.
//! * [`lv2_host`]   — real LV2 hosting (zone P2: livi instantiation, RT
//!   processing, params, state:interface) in the `"lv2"` slot.
//! * [`state`]      — project persistence for instances (APST blobs,
//!   [`state::FormatStateBridge`]) — zone P4.
//! * [`automation`] — automation lanes: schema, persistence, ramp
//!   compilation and the automated-node wrapper — zone P4.
//!
//! This file is Tauri glue: `PluginState`, the phase-3 commands (names
//! frozen once registered in lib.rs), `init`, and [`live_node_for`] — the
//! engine's hook for resolving a track's `instrumentId = "plugin:<id>"`
//! binding into a live node.

pub mod automation;
pub mod clap_host;
pub mod descriptor;
pub mod host;
pub mod lv2_host;
pub mod patches;
pub mod scan;
pub mod scan_worker;
pub mod state;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::audio::dsp::LiveInstrument;
pub use descriptor::{ParamInfo, PluginDescriptor, PluginInstanceInfo};

// ---------------------------------------------------------------------------
// Registry (control-plane state; engine reads via the registered global)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct PluginRegistry {
    /// Result of the last scan (None = never scanned).
    pub scanned: Option<Vec<PluginDescriptor>>,
    /// Live instances by instance id.
    pub instances: HashMap<String, PluginInstanceInfo>,
    /// Generic-UI parameter mirror per instance (stub: set via
    /// `plugin_set_param`, served by `plugin_get_params`).
    pub params: HashMap<String, Vec<ParamInfo>>,
    /// State blobs restored from disk for instances whose host half has not
    /// (yet) accepted them — missing plugin, host not booted, or pre-P1/P2
    /// stub (zone P4). Preserved verbatim across saves so state is NEVER
    /// lost on a machine without the plugin.
    pub pending_state: HashMap<String, state::StateBlob>,
}

impl PluginRegistry {
    /// Instantiate a plugin by descriptor uid. v1 scaffold: registers the
    /// instance with status `"stub"` (renders silence through
    /// `StubPluginNode`); zones P1/P2 turn this into real instantiation on
    /// the plugin main thread and flip the status to `"active"`.
    pub fn instantiate(&mut self, uid: &str) -> Result<PluginInstanceInfo, String> {
        let known = self
            .scanned
            .as_ref()
            .ok_or("no plugin scan yet — call plugin_scan first")?;
        let desc = known
            .iter()
            .find(|d| d.uid == uid)
            .ok_or_else(|| format!("unknown plugin uid: {uid}"))?;
        if !desc.is_instrument {
            return Err(format!(
                "{} is an effect; v1 hosts instruments on midi tracks only",
                desc.name
            ));
        }
        let info = PluginInstanceInfo {
            id: uuid::Uuid::new_v4().to_string(),
            uid: desc.uid.clone(),
            name: desc.name.clone(),
            format: desc.format.clone(),
            status: "stub".into(),
            track_id: None,
        };
        self.instances.insert(info.id.clone(), info.clone());
        self.params.insert(info.id.clone(), Vec::new());
        Ok(info)
    }

    /// Flip an instance from `"stub"` to `"active"` and install its REAL
    /// parameter list (zone P2: LV2 control ports; zone P1: CLAP params ext).
    /// Contract 4 lifecycle: `"stub"` -> `"active"`.
    pub fn activate(
        &mut self,
        instance_id: &str,
        params: Vec<ParamInfo>,
    ) -> Result<PluginInstanceInfo, String> {
        let info = self
            .instances
            .get_mut(instance_id)
            .ok_or_else(|| format!("unknown plugin instance: {instance_id}"))?;
        info.status = "active".into();
        self.params.insert(instance_id.to_string(), params);
        Ok(info.clone())
    }

    pub fn remove(&mut self, instance_id: &str) -> Result<PluginInstanceInfo, String> {
        self.params.remove(instance_id);
        self.pending_state.remove(instance_id);
        self.instances
            .remove(instance_id)
            .ok_or_else(|| format!("unknown plugin instance: {instance_id}"))
    }

    pub fn set_params(
        &mut self,
        instance_id: &str,
        changes: &[ParamChange],
    ) -> Result<Vec<ParamInfo>, String> {
        if !self.instances.contains_key(instance_id) {
            return Err(format!("unknown plugin instance: {instance_id}"));
        }
        let params = self.params.entry(instance_id.to_string()).or_default();
        for ch in changes {
            match params.iter_mut().find(|p| p.id == ch.id) {
                Some(p) => p.value = ch.value.clamp(p.min, p.max),
                None => params.push(ParamInfo {
                    id: ch.id,
                    name: format!("param {}", ch.id),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    value: ch.value,
                    steps: 0,
                }),
            }
        }
        Ok(params.clone())
    }
}

static REGISTRY: OnceLock<Arc<Mutex<PluginRegistry>>> = OnceLock::new();

/// Register the app-wide plugin registry (first registration wins — same
/// pattern as `sampler::register_bank`). Tests use a local registry instead.
pub fn register_registry(reg: Arc<Mutex<PluginRegistry>>) {
    let _ = REGISTRY.set(reg);
}

pub fn registered_registry() -> Option<&'static Arc<Mutex<PluginRegistry>>> {
    REGISTRY.get()
}

/// ENGINE HOOK (control thread, during graph rebuild): resolve a track's
/// `plugin:<instanceId>` binding into a live node for the seam
/// (`midi::playback::node_for_track`). Returns None for unknown instances
/// AND for active instances whose host half cannot build a node — the
/// caller falls back to PolySynth and logs (MIDI never goes silent).
///
/// Active LV2 instances resolve to a real [`lv2_host::Lv2Node`] through the
/// LV2 host thread; active CLAP instances resolve to a real
/// [`clap_host::ClapNode`] through the plugin main thread (both may block
/// briefly — contract 3; never called on RT). The registry lock is NOT held
/// across the host call.
pub fn live_node_for(instance_id: &str, rate: u32) -> Option<Box<dyn LiveInstrument>> {
    let reg = REGISTRY.get()?;
    let info = reg.lock().instances.get(instance_id).cloned()?;
    if info.format == "lv2" && info.status == "active" {
        return match lv2_host::global().make_node(instance_id, rate) {
            Ok(node) => Some(node),
            Err(e) => {
                log::warn!("plugins: LV2 node for instance {instance_id} unavailable ({e})");
                None
            }
        };
    }
    if info.format == "clap" && info.status == "active" {
        return match clap_host::activate_node(instance_id, rate) {
            Ok(node) => Some(node),
            Err(e) => {
                log::warn!("plugins: CLAP node for instance {instance_id} unavailable ({e})");
                None
            }
        };
    }
    log::info!(
        "plugins: track bound to {} instance {instance_id} (silent stub)",
        info.status
    );
    Some(Box::new(host::StubPluginNode::new()) as Box<dyn LiveInstrument>)
}

/// True when `instance_id` names a registered instance (used by
/// `set_track_instrument` validation for `plugin:` refs).
pub fn instance_exists(instance_id: &str) -> bool {
    REGISTRY
        .get()
        .is_some_and(|r| r.lock().instances.contains_key(instance_id))
}

// ---------------------------------------------------------------------------
// Tauri state / commands
// ---------------------------------------------------------------------------

pub struct PluginState {
    registry: Arc<Mutex<PluginRegistry>>,
    /// Automation lanes (zone P4 groundwork) — lives here because lib.rs's
    /// managed-state roster is frozen; `automation.rs` owns the semantics.
    pub(crate) automation: Arc<Mutex<automation::AutomationStore>>,
}

impl Default for PluginState {
    fn default() -> Self {
        Self {
            registry: Arc::new(Mutex::new(PluginRegistry::default())),
            automation: Arc::new(Mutex::new(automation::AutomationStore::default())),
        }
    }
}

/// Module init hook (lib.rs setup): publish the registry + automation store
/// for the engine and the project-open restore seam (zone P4).
pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let state = app.state::<PluginState>();
    register_registry(state.registry.clone());
    automation::register_store(state.automation.clone());
    // The production host-state bridge (ARCHITECTURE §15.3 "State"): project
    // save/open serializes live CLAP/LV2 instance state through the shared
    // plugin main thread. Registered here so `state::save_into_project` /
    // `reactivate_restored` reach the real hosts.
    state::register_state_bridge(Arc::new(state::FormatStateBridge));
    log::info!("plugins::init — registry + state bridge published (scan is on-demand)");
    Ok(())
}

/// One batched parameter change (D-03 discipline: born batch-shaped).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamChange {
    pub id: u32,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginListResult {
    /// Last scan result (empty until `plugin_scan` ran).
    pub plugins: Vec<PluginDescriptor>,
    pub instances: Vec<PluginInstanceInfo>,
    pub scanned: bool,
}

/// Scan LV2 + CLAP search paths. Runs on a blocking worker (the CLAP half
/// dlopens bundles — see scan.rs crash-safety note); results are cached in
/// the registry and returned.
#[tauri::command]
pub async fn plugin_scan(state: State<'_, PluginState>) -> Result<Vec<PluginDescriptor>, String> {
    let found = tauri::async_runtime::spawn_blocking(scan::scan_all)
        .await
        .map_err(|e| format!("plugin scan task failed: {e}"))?;
    state.registry.lock().scanned = Some(found.clone());
    Ok(found)
}

#[tauri::command]
pub fn plugin_list(state: State<'_, PluginState>) -> Result<PluginListResult, String> {
    let reg = state.registry.lock();
    Ok(PluginListResult {
        plugins: reg.scanned.clone().unwrap_or_default(),
        instances: reg.instances.values().cloned().collect(),
        scanned: reg.scanned.is_some(),
    })
}

/// Instantiate + activate through the format host. LV2 (zone P2): registers
/// the instance on the LV2 host thread and flips `"stub"` -> `"active"` with
/// the real control-port param list. CLAP (zone P1): instantiates on the
/// plugin main thread (entry/factory/ports/params negotiation) and flips to
/// `"active"` with the real params-ext list. On host failure the instance is
/// removed again and the error surfaces (no zombie registrations). The
/// registry lock is never held across the (briefly blocking) host calls.
pub fn instantiate_and_activate(
    registry: &Arc<Mutex<PluginRegistry>>,
    uid: &str,
) -> Result<PluginInstanceInfo, String> {
    let info = registry.lock().instantiate(uid)?;
    let hosted = match info.format.as_str() {
        "lv2" => lv2_host::global().register_instance(&info.id, &info.uid),
        "clap" => clap_host::instantiate(&info.id, &info.uid),
        _ => return Ok(info),
    };
    match hosted {
        Ok(params) => registry.lock().activate(&info.id, params),
        Err(e) => {
            let _ = registry.lock().remove(&info.id);
            Err(e)
        }
    }
}

/// Async: the LV2 path blocks on the host thread (first call pays the lilv
/// world load), so it runs on a blocking worker like `plugin_scan`.
/// Mutations auto-persist into the open project (zone P4 — the frozen
/// `save_project` command cannot carry plugin state).
#[tauri::command]
pub async fn plugin_instantiate(
    uid: String,
    state: State<'_, PluginState>,
    audio: State<'_, crate::audio::AudioState>,
) -> Result<PluginInstanceInfo, String> {
    let registry = state.registry.clone();
    let (session, _, _) = audio.control_parts();
    tauri::async_runtime::spawn_blocking(move || {
        let info = instantiate_and_activate(&registry, &uid)?;
        state::persist_after_mutation(&session, &registry, true);
        Ok(info)
    })
    .await
    .map_err(|e| format!("plugin instantiate task failed: {e}"))?
}

#[tauri::command]
pub fn plugin_remove(
    instance_id: String,
    state: State<'_, PluginState>,
    audio: State<'_, crate::audio::AudioState>,
) -> Result<PluginInstanceInfo, String> {
    let info = state.registry.lock().remove(&instance_id)?;
    if info.format == "lv2" {
        // Fire-and-forget; never boots the host just to forget an id.
        if let Some(host) = lv2_host::try_global() {
            host.unregister_instance(&instance_id);
        }
    } else if info.format == "clap" && info.status == "active" {
        // Destroys on the plugin main thread (deferred while a live node
        // still holds the audio processor — clap_host graveyard).
        if let Err(e) = clap_host::remove(&instance_id) {
            log::warn!("plugins: clap remove for {instance_id}: {e}");
        }
    }
    // Auto-persist (zone P4): drops the row and GCs the instance's blob.
    let (session, _, _) = audio.control_parts();
    state::persist_after_mutation(&session, &state.registry, true);
    Ok(info)
}

#[tauri::command]
pub fn plugin_get_params(
    instance_id: String,
    state: State<'_, PluginState>,
) -> Result<Vec<ParamInfo>, String> {
    let reg = state.registry.lock();
    if !reg.instances.contains_key(&instance_id) {
        return Err(format!("unknown plugin instance: {instance_id}"));
    }
    Ok(reg.params.get(&instance_id).cloned().unwrap_or_default())
}

/// Batched (D-03): one invoke carries N parameter changes. The registry
/// mirror updates first (clamped against the real ranges once active); for
/// active LV2/CLAP instances the CLAMPED values are then forwarded to the
/// host, which pushes them through the wait-free ring to the RT node
/// (contract 5; inactive CLAP instances get a main-thread params flush).
pub fn set_params_and_forward(
    registry: &Arc<Mutex<PluginRegistry>>,
    instance_id: &str,
    changes: &[ParamChange],
) -> Result<Vec<ParamInfo>, String> {
    let (updated, format) = {
        let mut reg = registry.lock();
        let updated = reg.set_params(instance_id, changes)?;
        let format = reg
            .instances
            .get(instance_id)
            .filter(|i| i.status == "active")
            .map(|i| i.format.clone());
        (updated, format)
    };
    let clamped = |changes: &[ParamChange]| -> Vec<ParamChange> {
        changes
            .iter()
            .filter_map(|c| {
                updated
                    .iter()
                    .find(|p| p.id == c.id)
                    .map(|p| ParamChange { id: p.id, value: p.value })
            })
            .collect()
    };
    match format.as_deref() {
        Some("lv2") => {
            if let Some(host) = lv2_host::try_global() {
                let vals: Vec<(u32, f32)> =
                    clamped(changes).iter().map(|c| (c.id, c.value as f32)).collect();
                host.set_params(instance_id, vals);
            }
        }
        Some("clap") => {
            if let Err(e) = clap_host::set_params(instance_id, clamped(changes)) {
                log::warn!("plugins: clap set_params for {instance_id}: {e}");
            }
        }
        _ => {}
    }
    Ok(updated)
}

#[tauri::command]
pub fn plugin_set_param(
    instance_id: String,
    changes: Vec<ParamChange>,
    state: State<'_, PluginState>,
    audio: State<'_, crate::audio::AudioState>,
) -> Result<Vec<ParamInfo>, String> {
    let updated = set_params_and_forward(&state.registry, &instance_id, &changes)?;
    // Auto-persist the param snapshot (zone P4). `with_host_state: false` —
    // a knob drag must not round-trip the plugin main thread per rAF batch.
    let (session, _, _) = audio.control_parts();
    state::persist_after_mutation(&session, &state.registry, false);
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned_registry() -> PluginRegistry {
        let mut reg = PluginRegistry::default();
        reg.scanned = Some(vec![
            PluginDescriptor {
                uid: "lv2:urn:test:synth".into(),
                format: "lv2".into(),
                name: "TestSynth".into(),
                vendor: None,
                version: None,
                path: None,
                is_instrument: true,
                audio_inputs: 0,
                audio_outputs: 2,
                has_note_input: true,
                categories: vec![],
            },
            PluginDescriptor {
                uid: "clap:/x.clap#fx".into(),
                format: "clap".into(),
                name: "TestFx".into(),
                vendor: None,
                version: None,
                path: Some("/x.clap".into()),
                is_instrument: false,
                audio_inputs: 2,
                audio_outputs: 2,
                has_note_input: false,
                categories: vec![],
            },
        ]);
        reg
    }

    #[test]
    fn instantiate_requires_scan_and_known_instrument_uid() {
        let mut empty = PluginRegistry::default();
        assert!(empty.instantiate("lv2:urn:test:synth").is_err(), "no scan yet");

        let mut reg = scanned_registry();
        assert!(reg.instantiate("lv2:urn:nope").is_err(), "unknown uid");
        assert!(
            reg.instantiate("clap:/x.clap#fx").is_err(),
            "effects rejected in v1"
        );
        let inst = reg.instantiate("lv2:urn:test:synth").unwrap();
        assert_eq!(inst.status, "stub");
        assert_eq!(inst.format, "lv2");
        assert!(reg.instances.contains_key(&inst.id));
    }

    #[test]
    fn remove_and_param_flows() {
        let mut reg = scanned_registry();
        let inst = reg.instantiate("lv2:urn:test:synth").unwrap();
        assert!(reg.set_params("nope", &[]).is_err());
        let params = reg
            .set_params(&inst.id, &[ParamChange { id: 7, value: 0.5 }])
            .unwrap();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].value, 0.5);
        // Clamped into the (stub) range on update.
        let params = reg
            .set_params(&inst.id, &[ParamChange { id: 7, value: 9.0 }])
            .unwrap();
        assert_eq!(params[0].value, 1.0);

        reg.remove(&inst.id).unwrap();
        assert!(reg.remove(&inst.id).is_err(), "double remove errors");
        assert!(reg.params.get(&inst.id).is_none(), "params cleaned up");
    }
}
