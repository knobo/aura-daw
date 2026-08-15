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

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::audio::dsp::LiveInstrument;
pub use descriptor::{ParamInfo, PluginDescriptor, PluginInstanceInfo};

// ---------------------------------------------------------------------------
// Registry (LIVE-only control-plane state; engine reads via the registered
// global). Plan E Task 9 (round-2 inventory rows 12-15): the DOCUMENT half
// (instance rows, param mirror, pending state blobs) moved into
// `Session.plugins` (`control::session::PluginDoc`) — undoable, persisted
// through the transaction channel like every other document field. This
// struct keeps ONLY what genuinely isn't document state: the scan cache
// (`plugin_scan` stays outside the document — a scan result isn't something
// undo/redo or project persistence should touch, it's a machine-local
// filesystem probe, cached for convenience).
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct PluginRegistry {
    /// Result of the last scan (None = never scanned).
    pub scanned: Option<Vec<PluginDescriptor>>,
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
/// `doc` is `session.plugins`, read by the CALLER under whatever lock it
/// already holds and passed in BY VALUE lookup here — this function itself
/// never (re-)locks the session, only reads the slice it's handed, so it
/// never ADDS a new lock-then-host-call span beyond what the caller already
/// has. HONEST NOTE (Task 9 review round 1, Important-3, correcting an
/// earlier inaccurate claim here): `engine::rebuild` — the only production
/// caller, via `midi::playback::node_for_track` — calls this WITH the
/// session lock held for the whole rebuild (pre-existing, out of Task 9's
/// scope; a Plan A handoff deferral awaiting Plan F's snapshot-based
/// rebuild), so the host calls below (both may block briefly — contract 3)
/// DO currently run while the session lock is held. New callers must not
/// make this worse (no ADDITIONAL lock spans layered on top of this one);
/// they must not assume "no session lock" as a hard guarantee either.
///
/// Active LV2 instances resolve to a real [`lv2_host::Lv2Node`] through the
/// LV2 host thread; active CLAP instances resolve to a real
/// [`clap_host::ClapNode`] through the plugin main thread.
pub fn live_node_for(
    doc: &crate::control::session::PluginDoc,
    instance_id: &str,
    rate: u32,
) -> Option<Box<dyn LiveInstrument>> {
    let info = doc.instances.iter().find(|r| r.id == instance_id)?.clone();
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

// ---------------------------------------------------------------------------
// Tauri state / commands
// ---------------------------------------------------------------------------

pub struct PluginState {
    registry: Arc<Mutex<PluginRegistry>>,
}

impl Default for PluginState {
    fn default() -> Self {
        Self { registry: Arc::new(Mutex::new(PluginRegistry::default())) }
    }
}

/// Module init hook (lib.rs setup): publish the registry for the engine and
/// the project-open restore seam (zone P4). Automation lanes moved into
/// `Session` (Plan E Task 10) — their project-adoption seam is registered
/// separately, from `MidiState::shared` (`midi/mod.rs`), once the shared
/// `Session` exists (this hook runs BEFORE it does — see lib.rs's setup
/// order — so it can't be done here).
pub fn init(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let state = app.state::<PluginState>();
    register_registry(state.registry.clone());
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
/// The command boundary is logged on BOTH sides on purpose. A scan that the
/// UI shows as still running while the process sits idle has two very
/// different causes — the blocking task never finished, or it finished and
/// the response never reached the webview — and the pair of lines below is
/// what tells them apart after the fact.
#[tauri::command]
pub async fn plugin_scan(state: State<'_, PluginState>) -> Result<Vec<PluginDescriptor>, String> {
    log::info!("plugin scan: requested");
    let found = tauri::async_runtime::spawn_blocking(scan::scan_all)
        .await
        .map_err(|e| format!("plugin scan task failed: {e}"))?;
    state.registry.lock().scanned = Some(found.clone());
    log::info!("plugin scan: returning {} descriptors to the webview", found.len());
    Ok(found)
}

#[tauri::command]
pub fn plugin_list(
    state: State<'_, PluginState>,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<PluginListResult, String> {
    let reg = state.registry.lock();
    Ok(PluginListResult {
        plugins: reg.scanned.clone().unwrap_or_default(),
        instances: control.plugin_rows(),
        scanned: reg.scanned.is_some(),
    })
}

/// Look up a scanned instrument uid and instantiate it through its format
/// host. LV2 (zone P2): registers the instance on the LV2 host thread and
/// returns its real control-port param list. CLAP (zone P1): instantiates
/// on the plugin main thread (entry/factory/ports/params negotiation) and
/// returns its real params-ext list. Prepare-outside (Task 9): this is the
/// HOST half only — no document/session write happens here (a plugin the
/// host rejects never touches the document); the caller commits
/// `Op::PluginAdd { row, .. }` with the returned row afterward. Registry
/// lock only guards the `scanned` lookup — never held across the (briefly
/// blocking) host call.
pub fn instantiate_and_activate(
    registry: &Arc<Mutex<PluginRegistry>>,
    uid: &str,
) -> Result<(PluginInstanceInfo, Vec<ParamInfo>), String> {
    let desc = {
        let reg = registry.lock();
        let known = reg.scanned.as_ref().ok_or("no plugin scan yet — call plugin_scan first")?;
        known
            .iter()
            .find(|d| d.uid == uid)
            .cloned()
            .ok_or_else(|| format!("unknown plugin uid: {uid}"))?
    };
    if !desc.is_instrument {
        return Err(format!(
            "{} is an effect; v1 hosts instruments on midi tracks only",
            desc.name
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let hosted = match desc.format.as_str() {
        "lv2" => lv2_host::global().register_instance(&id, &desc.uid),
        "clap" => clap_host::instantiate(&id, &desc.uid),
        _ => Ok(Vec::new()),
    };
    let params = hosted?;
    let status = if desc.format == "lv2" || desc.format == "clap" { "active" } else { "stub" };
    let info = PluginInstanceInfo {
        id,
        uid: desc.uid.clone(),
        name: desc.name.clone(),
        format: desc.format.clone(),
        status: status.into(),
        track_id: None,
    };
    Ok((info, params))
}

/// Destroy a just-instantiated (not-yet-committed) host instance — the
/// orphan-cleanup half of `plugin_instantiate`'s prepare-outside pattern.
fn destroy_orphan(format: &str, id: &str) {
    match format {
        "lv2" => {
            if let Some(host) = lv2_host::try_global() {
                host.unregister_instance(id);
            }
        }
        "clap" => {
            if let Err(e) = clap_host::remove(id) {
                log::warn!("plugins: orphan clap cleanup for {id}: {e}");
            }
        }
        _ => {}
    }
}

/// Async: the LV2 path blocks on the host thread (first call pays the lilv
/// world load), so it runs on a blocking worker like `plugin_scan`.
/// Prepare-outside (Task 9): the host instance is created FIRST (above); on
/// commit failure the orphan host instance is destroyed and the document
/// stays untouched (§7-test-3). On success, `Op::PluginAdd`'s
/// `effect.persist.plugins` makes this durable through `commit` — no manual
/// persist call needed here anymore (the retired `persist_after_mutation`
/// hook is gone).
#[tauri::command]
pub async fn plugin_instantiate(
    uid: String,
    state: State<'_, PluginState>,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<PluginInstanceInfo, String> {
    let registry = state.registry.clone();
    let control = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (info, _params) = instantiate_and_activate(&registry, &uid)?;
        let meta = crate::control::op::TxMeta::user("plugin instantiate");
        let row = info.clone();
        match control.commit(meta, |tx| {
            tx.apply(crate::control::op::Op::PluginAdd { row: row.clone(), index: usize::MAX })
        }) {
            Ok(_) => Ok(info),
            Err(e) => {
                destroy_orphan(&info.format, &info.id);
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| format!("plugin instantiate task failed: {e}"))?
}

#[tauri::command]
pub fn plugin_remove(
    instance_id: String,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<PluginInstanceInfo, String> {
    let row = control
        .plugin_row(&instance_id)
        .ok_or_else(|| format!("unknown plugin instance: {instance_id}"))?;
    // Capture the host state blob OUTSIDE the session lock, before teardown
    // (§4.4 restore-from-blob) — self-describing APST bytes, the same
    // encoding written to `<project>/plugins/<id>.state`. `None` when the
    // instance has nothing to save (stub / no state interface).
    let blob = state::registered_state_bridge()
        .and_then(|b| b.save_state(&instance_id).ok().flatten())
        .map(|blob| state::encode_state(&row.uid, &blob));
    // Current param mirror, for the recorded op's own completeness (Task 9
    // review round 1, Important-1) — `apply_raw` parks the LIVE mirror
    // itself regardless, so this doesn't change undo behavior, only makes
    // the forward-recorded `Op::PluginRemove` self-describing.
    let params = control.plugin_params(&instance_id).unwrap_or_default();
    let meta = crate::control::op::TxMeta::user("plugin remove");
    control.commit(meta, |tx| {
        tx.apply(crate::control::op::Op::PluginRemove {
            row: row.clone(),
            index: 0,
            state: blob.clone(),
            params: params.clone(),
        })
    })?;
    // Host teardown happens as `commit`'s `HostForward::Destroy` effect
    // (post-lock) — nothing more to do here.
    Ok(row)
}

#[tauri::command]
pub fn plugin_get_params(
    instance_id: String,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<Vec<ParamInfo>, String> {
    control
        .plugin_params(&instance_id)
        .ok_or_else(|| format!("unknown plugin instance: {instance_id}"))
}

/// Batched (D-03): one invoke carries N parameter changes, each committed as
/// its own `Op::Set { Plugin, Param }` within ONE transaction — `commit`'s
/// fold coalesces same-(instance, param) writes, and the whole batch is one
/// `EngineEffect` (rebuild-free; one `host_forward` per changed param,
/// walked post-lock). Task 9: this replaces the retired
/// `PluginRegistry::set_params` + manual host-forward two-step — clamping
/// and host forwarding now both live in `apply_raw` / `commit`.
#[tauri::command]
pub fn plugin_set_param(
    instance_id: String,
    changes: Vec<ParamChange>,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<Vec<ParamInfo>, String> {
    control.set_plugin_params(
        &instance_id,
        &changes,
        crate::control::op::TxMeta::user("plugin set param"),
    )?;
    control
        .plugin_params(&instance_id)
        .ok_or_else(|| format!("unknown plugin instance: {instance_id}"))
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

    /// Task 9: `instantiate_and_activate`'s validation (scan/uid/instrument
    /// gates) runs BEFORE any host call, so these error paths are still
    /// unit-testable without a real plugin binary. The SUCCESS path (real
    /// host registration) is exercised where a real/fake host is available
    /// — `clap_host`/`lv2_host`'s own tests — and the DOCUMENT half
    /// (row/param/state bookkeeping `apply_raw` now owns) is covered by
    /// `control::session`'s plugin op tests, working from a
    /// manually-constructed row (no host needed at all).
    #[test]
    fn instantiate_requires_scan_and_known_instrument_uid() {
        let empty = Arc::new(Mutex::new(PluginRegistry::default()));
        assert!(
            instantiate_and_activate(&empty, "lv2:urn:test:synth").is_err(),
            "no scan yet"
        );

        let reg = Arc::new(Mutex::new(scanned_registry()));
        assert!(instantiate_and_activate(&reg, "lv2:urn:nope").is_err(), "unknown uid");
        assert!(
            instantiate_and_activate(&reg, "clap:/x.clap#fx").is_err(),
            "effects rejected in v1"
        );
    }
}
