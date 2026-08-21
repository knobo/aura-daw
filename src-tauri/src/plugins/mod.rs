//! AURA plugin hosting (phase 3) — CLAP + LV2. ARCHITECTURE §15.
//!
//! * [`descriptor`] — wire types (`plugin-descriptor` / `plugin-state`
//!   schemas).
//! * [`catalog`]    — the machine-global, persistent plugin catalog
//!   (`plugin-catalog.json`): scan cache + favorites/recents/tags/pinned
//!   params, and the incremental-rescan planner `scan.rs` drives.
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
//! * [`stderr_guard`] — dedupes/re-homes fd 2 so a chatty native plugin
//!   (Zyn's headless "Events Output" spam) can't block the RT audio thread
//!   on a full pipe/terminal buffer.
//!
//! This file is Tauri glue: `PluginState`, the phase-3 commands (names
//! frozen once registered in lib.rs), `init`, and [`live_node_for`] — the
//! engine's hook for resolving a track's `instrumentId = "plugin:<id>"`
//! binding into a live node.

pub mod automation;
pub mod catalog;
pub mod clap_host;
pub mod descriptor;
pub mod host;
#[cfg(feature = "lv2")]
pub mod lv2_host;
#[cfg(feature = "lv2")]
mod lv2_ui;
/// Without the `lv2` feature (Windows: no system lilv to link) the stub takes
/// the module's place, so every LV2 call site below compiles unchanged.
#[cfg(not(feature = "lv2"))]
#[path = "lv2_host_stub.rs"]
pub mod lv2_host;
pub mod patches;
pub mod scan;
pub mod wm_stack;
pub mod scan_worker;
pub mod state;
pub mod stderr_guard;

use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

use crate::audio::dsp::{AudioProcessor, LiveInstrument};
pub use descriptor::{ParamInfo, PluginDescriptor, PluginInstanceInfo};

/// How a hosted plugin node mixes its main output into the process block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IoMode {
    /// Instrument contract: silence inputs, `+=` main out (default).
    #[default]
    Add,
    /// Effect contract: copy `io.samples` into inputs, assign main out.
    Replace,
}

/// Port-negotiation role for CLAP/LV2 host instantiation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostRole {
    Instrument,
    Effect,
}

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

/// Plan G1 Task 7: resolve an insert-FX instance into a real RT **effect**
/// node ([`AudioProcessor`] in [`IoMode::Replace`]). Returns `None` for
/// unknown instances and for instances that are not `"active"` — the caller
/// (`audio::insert::compile_inserts`) skips the slot, leaving the strip dry.
/// Mirrors [`live_node_for`], but for the insert chain (effects only) instead
/// of the instrument slot.
pub fn insert_node_for(
    doc: &crate::control::session::PluginDoc,
    instance_id: &str,
    rate: u32,
) -> Option<Box<dyn AudioProcessor>> {
    let info = doc.instances.iter().find(|r| r.id == instance_id)?.clone();
    if info.status != "active" {
        log::info!(
            "plugins: insert instance {instance_id} is not active ({}); skipping",
            info.status
        );
        return None;
    }
    match info.format.as_str() {
        "clap" => match clap_host::activate_effect_node(instance_id, rate) {
            Ok(node) => Some(node),
            Err(e) => {
                log::warn!("plugins: CLAP effect node for {instance_id} unavailable ({e})");
                None
            }
        },
        "lv2" => match lv2_host::global().make_effect_node(instance_id, rate) {
            Ok(node) => Some(node),
            Err(e) => {
                log::warn!("plugins: LV2 effect node for {instance_id} unavailable ({e})");
                None
            }
        },
        _ => {
            log::warn!("plugins: unsupported format '{}' for insert {instance_id}", info.format);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri state / commands
// ---------------------------------------------------------------------------

pub struct PluginState {
    registry: Arc<Mutex<PluginRegistry>>,
    /// The persistent catalog (scan cache + favorites/recents/tags/pinned
    /// params) — loaded once in [`init`], mutated in memory by
    /// `plugin_catalog_update` / `plugin_scan`, and written back to disk on
    /// every mutation. A SEPARATE lock from `registry` (not a field on
    /// [`PluginRegistry`]) on purpose: `PluginRegistry` is constructed as a
    /// bare struct literal by test scaffolding elsewhere in the crate
    /// (`control::mod`'s engine-hook tests), and growing that struct's
    /// public shape would break every such call site outside this module's
    /// ownership. Machine-global, NOT part of `Session` — same "filesystem
    /// probe, not document state" reasoning as `registry.scanned`.
    catalog: Arc<Mutex<catalog::PluginCatalog>>,
}

impl Default for PluginState {
    fn default() -> Self {
        Self {
            registry: Arc::new(Mutex::new(PluginRegistry::default())),
            catalog: Arc::new(Mutex::new(catalog::PluginCatalog::default())),
        }
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
    // Load the persistent catalog and seed the registry's scan cache from
    // it (the whole point of the catalog — PR spec 4.1.B): after this,
    // `plugin_list` returns a populated list and `scanned: true` on the
    // FIRST frame after launch, instead of an empty list until the user
    // rescans by hand.
    let disk_catalog = catalog::load();
    let seeded = disk_catalog.scan.entries.len();
    if !disk_catalog.scan.entries.is_empty() {
        state.registry.lock().scanned = Some(
            disk_catalog
                .scan
                .entries
                .iter()
                .map(|e| e.descriptor.clone())
                .collect(),
        );
    }
    *state.catalog.lock() = disk_catalog;
    register_registry(state.registry.clone());
    // The production host-state bridge (ARCHITECTURE §15.3 "State"): project
    // save/open serializes live CLAP/LV2 instance state through the shared
    // plugin main thread. Registered here so `state::save_into_project` /
    // `reactivate_restored` reach the real hosts.
    state::register_state_bridge(Arc::new(state::FormatStateBridge));
    log::info!("plugins::init — registry + state bridge published; catalog seeded {seeded} cached descriptors");
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
    /// Instance id → native floating GUI available. Additive (D-06); not
    /// document state — probed from the live host.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub gui: std::collections::HashMap<String, bool>,
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
    let prev_entries = state.catalog.lock().scan.entries.clone();
    let result = tauri::async_runtime::spawn_blocking(move || scan::scan_incremental(prev_entries))
        .await
        .map_err(|e| format!("plugin scan task failed: {e}"))?;
    state.registry.lock().scanned = Some(result.descriptors.clone());
    let merged_catalog = {
        let mut cat = state.catalog.lock();
        cat.scan = catalog::ScanCache {
            scanned_at: Some(catalog::now_secs()),
            entries: result.entries,
        };
        cat.clone()
    };
    if let Err(e) = catalog::save(&merged_catalog) {
        log::warn!("plugin catalog: failed to persist after scan: {e}");
    }
    log::info!(
        "plugin scan: returning {} descriptors to the webview",
        result.descriptors.len()
    );
    Ok(result.descriptors)
}

/// Additive command D (spec 4.1): read the full persistent catalog.
#[tauri::command]
pub fn plugin_catalog_get(state: State<'_, PluginState>) -> Result<catalog::PluginCatalog, String> {
    Ok(state.catalog.lock().clone())
}

/// Additive command D: apply one patch and persist. Returns the FULL merged
/// catalog so the frontend never guesses at the result (spec 4.1).
#[tauri::command]
pub fn plugin_catalog_update(
    patch: catalog::PluginCatalogPatch,
    state: State<'_, PluginState>,
) -> Result<catalog::PluginCatalog, String> {
    let now = catalog::now_secs();
    let merged = {
        let mut cat = state.catalog.lock();
        *cat = catalog::apply_patch(cat.clone(), &patch, now);
        cat.clone()
    };
    catalog::save(&merged)?;
    Ok(merged)
}

/// Additive command D: "cache from N ago, M bundles changed" without
/// actually rescanning — stats the same CLAP bundles `plugin_scan` would,
/// classifies them against the cached catalog, and reports counts only.
#[tauri::command]
pub fn plugin_scan_status(
    state: State<'_, PluginState>,
) -> Result<catalog::PluginScanStatus, String> {
    let (scanned_at, cached_clap) = {
        let cat = state.catalog.lock();
        let cached_clap: Vec<catalog::CatalogEntry> = cat
            .scan
            .entries
            .iter()
            .filter(|e| e.descriptor.format == "clap")
            .cloned()
            .collect();
        (cat.scan.scanned_at, cached_clap)
    };
    let roots = scan::clap_search_paths();
    let bundles = scan::find_clap_bundles(&roots);
    let discovered: Vec<catalog::BundleStat> =
        bundles.iter().filter_map(|b| scan::stat_bundle(b)).collect();
    let plan = catalog::plan_rescan(&cached_clap, &discovered);
    Ok(catalog::PluginScanStatus {
        scanned_at,
        cached: plan.reused.len(),
        stale: plan.to_scan.len(),
        missing: plan.missing,
    })
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
        gui: gui_flags(),
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

/// Plan G1: insert-FX path — same prepare-outside host half as
/// [`instantiate_and_activate`], with the instrument/effect gate inverted.
/// Instruments stay on the instrument slot; effects go through here via
/// [`clap_host::instantiate_effect`] / [`lv2_host::Lv2Host::register_instance_effect`].
pub fn instantiate_and_activate_effect(
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
    if desc.is_instrument {
        return Err(format!(
            "{} is an instrument; insert slots host effects only",
            desc.name
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let hosted = match desc.format.as_str() {
        "lv2" => lv2_host::global().register_instance_effect(&id, &desc.uid),
        "clap" => clap_host::instantiate_effect(&id, &desc.uid),
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

/// Report hosted-instance latency in samples (0 when unknown / no ext).
/// Dispatches by format on the document row's format when known; otherwise
/// probes CLAP then LV2 host maps.
pub fn latency_of(instance_id: &str) -> usize {
    if clap_host::has_instance(instance_id).unwrap_or(false) {
        return clap_host::latency_samples(instance_id);
    }
    if let Some(host) = lv2_host::try_global() {
        if host.has_instance(instance_id).unwrap_or(false) {
            return host.latency_samples(instance_id);
        }
    }
    0
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

/// Which midi track hosts this instance as its instrument
/// (`instrumentId = "plugin:<id>"`). Insert-only placements are ignored —
/// they have no note input to audition through.
pub fn midi_track_bound_to_plugin(
    tracks: &[crate::audio::types::TrackState],
    instance_id: &str,
) -> Option<String> {
    let want = format!("plugin:{instance_id}");
    tracks
        .iter()
        .find(|t| t.kind == "midi" && t.instrument_id.as_deref() == Some(want.as_str()))
        .map(|t| t.id.as_str().to_string())
}

/// Play a one-shot note through a live plugin instance's bound midi track.
/// No op, no dirty flag: the note rides the existing live-in hub (the same
/// path hardware monitoring uses), so a stopped transport still sounds it.
/// The instance must already be on a midi track — this does not instantiate
/// a preview slot of its own.
#[tauri::command]
pub fn plugin_preview_note(
    instance_id: String,
    key: u8,
    velocity: u8,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    if !control.plugin_exists(&instance_id) {
        return Err(format!("unknown plugin instance: {instance_id}"));
    }
    let track_id = {
        let session = control.session().lock();
        midi_track_bound_to_plugin(&session.store.tracks, &instance_id).ok_or_else(|| {
            format!("plugin instance {instance_id} is not bound to a midi track")
        })?
    };
    crate::audio::midi_in::fire_preview_note(
        crate::audio::midi_in::hub(),
        &track_id,
        key,
        velocity,
    )
}

/// Held plugin audition: note-on until `plugin_preview_note_off`.
#[tauri::command]
pub fn plugin_preview_note_on(
    instance_id: String,
    key: u8,
    velocity: u8,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    if !control.plugin_exists(&instance_id) {
        return Err(format!("unknown plugin instance: {instance_id}"));
    }
    let track_id = {
        let session = control.session().lock();
        midi_track_bound_to_plugin(&session.store.tracks, &instance_id).ok_or_else(|| {
            format!("plugin instance {instance_id} is not bound to a midi track")
        })?
    };
    crate::audio::midi_in::begin_preview_hold(
        crate::audio::midi_in::hub(),
        &track_id,
        key,
        velocity,
    )
}

#[tauri::command]
pub fn plugin_preview_note_off() -> Result<(), String> {
    crate::audio::midi_in::end_preview_hold(crate::audio::midi_in::hub());
    Ok(())
}

fn gui_flags() -> std::collections::HashMap<String, bool> {
    let mut gui = clap_host::gui_flags();
    if let Some(host) = lv2_host::try_global() {
        gui.extend(host.gui_flags());
    }
    gui
}

/// Open the native plugin-owned floating editor for a live hosted instance
/// (CLAP `clap.gui` floating / LV2 `ui:showInterface`). Additive command.
#[tauri::command]
pub fn plugin_show_gui(instance_id: String, control: State<'_, Arc<crate::control::ControlPlane>>) -> Result<(), String> {
    let row = control
        .plugin_row(&instance_id)
        .ok_or_else(|| format!("unknown plugin instance: {instance_id}"))?;
    match row.format.as_str() {
        "clap" => clap_host::show_gui(&instance_id),
        "lv2" => lv2_host::global().show_gui(&instance_id),
        other => Err(format!("{other} instances have no native gui host")),
    }
}

/// Additive: INTERFACE pref `pluginGuiOnTop`. Live — already-open editors follow.
#[tauri::command]
pub fn plugin_set_gui_on_top(enabled: bool) {
    wm_stack::set_on_top(enabled);
    // CLAP `set_transient` only runs on the plugin-main thread. When the
    // pref turns on, re-apply it to already-created floating editors.
    if enabled {
        let _ = clap_host::reapply_gui_transient();
    }
}

/// Plan G1: add an effect as an insert slot on a track. Prepare-outside
/// (same shape as `plugin_instantiate`): host instantiate first, then one
/// commit with `PluginAdd` + `InsertAdd`. Orphan host cleanup on commit failure.
#[tauri::command]
pub async fn insert_add(
    track_id: String,
    uid: String,
    index: Option<usize>,
    state: State<'_, PluginState>,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<crate::audio::types::InsertSlot, String> {
    let registry = state.registry.clone();
    let control = control.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        {
            let session = control.session().lock();
            let t = session
                .store
                .tracks
                .iter()
                .find(|t| t.id.as_str() == track_id)
                .ok_or_else(|| format!("unknown track: {track_id}"))?;
            if t.kind != "audio" && t.kind != "midi" {
                return Err(format!(
                    "inserts are only legal on audio|midi tracks, not {}",
                    t.kind
                ));
            }
        }
        let (info, _params) = instantiate_and_activate_effect(&registry, &uid)?;
        let slot = crate::audio::types::InsertSlot {
            id: uuid::Uuid::new_v4().to_string(),
            instance_id: info.id.clone(),
            bypassed: false,
        };
        let mut row = info.clone();
        row.track_id = Some(track_id.clone());
        let idx = index.unwrap_or(usize::MAX);
        match control.insert_add(
            &track_id,
            row,
            slot,
            idx,
            crate::control::op::TxMeta::user("insert add"),
        ) {
            Ok(slot_out) => Ok(slot_out),
            Err(e) => {
                destroy_orphan(&info.format, &info.id);
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| format!("insert add task failed: {e}"))?
}

/// Plan G1: remove one insert slot and its plugin instance in one transaction.
#[tauri::command]
pub fn insert_remove(
    track_id: String,
    slot_id: String,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.insert_remove(&track_id, &slot_id, crate::control::op::TxMeta::user("insert remove"))
}

/// Plan G1: reorder one insert slot on a track.
#[tauri::command]
pub fn insert_reorder(
    track_id: String,
    slot_id: String,
    to_index: usize,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.insert_reorder(
        &track_id,
        &slot_id,
        to_index,
        crate::control::op::TxMeta::user("insert reorder"),
    )
}

/// Plan G1: set bypass on one insert slot.
#[tauri::command]
pub fn insert_set_bypass(
    track_id: String,
    slot_id: String,
    bypassed: bool,
    control: State<'_, Arc<crate::control::ControlPlane>>,
) -> Result<(), String> {
    control.insert_set_bypass(
        &track_id,
        &slot_id,
        bypassed,
        crate::control::op::TxMeta::user("insert set bypass"),
    )
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

    #[test]
    fn instantiate_effect_rejects_instruments_and_unknown_uids() {
        let reg = Arc::new(Mutex::new(scanned_registry()));
        assert!(
            instantiate_and_activate_effect(&reg, "lv2:urn:test:synth").is_err(),
            "instruments stay on the instrument slot"
        );
        assert!(instantiate_and_activate_effect(&reg, "lv2:urn:nope").is_err());
        // The *gate* accepts an effect uid. The host call is Task 4; this test
        // only asserts we no longer bounce !is_instrument before looking up.
        // With the fake scanned "TestFx", the function must get past the
        // is_instrument check and fail at the host (or succeed once Task 4 lands).
        let err = instantiate_and_activate_effect(&reg, "clap:/x.clap#fx").err();
        if let Some(e) = err {
            assert!(
                !e.contains("v1 hosts instruments"),
                "effect rejection must not use the instrument-only message, got: {e}"
            );
        }
    }

    #[test]
    fn instantiate_still_rejects_effects_on_the_instrument_path() {
        let reg = Arc::new(Mutex::new(scanned_registry()));
        let err = instantiate_and_activate(&reg, "clap:/x.clap#fx").unwrap_err();
        assert!(
            err.contains("effect") || err.contains("instruments"),
            "plugin_instantiate stays instrument-only, got: {err}"
        );
    }

    fn midi_track(id: &str, instrument_id: Option<&str>) -> crate::audio::types::TrackState {
        let mut t = crate::audio::types::testutil::test_track(id);
        t.kind = "midi".into();
        t.instrument_id = instrument_id.map(str::to_string);
        t
    }

    #[test]
    fn midi_track_bound_to_plugin_finds_the_instrument_host() {
        let tracks = [
            midi_track("t-other", Some("plugin:other")),
            midi_track("t-bass", Some("plugin:zyn-1")),
        ];
        assert_eq!(midi_track_bound_to_plugin(&tracks, "zyn-1").as_deref(), Some("t-bass"));
    }

    #[test]
    fn midi_track_bound_to_plugin_ignores_audio_and_unbound() {
        let mut audio = crate::audio::types::testutil::test_track("t-audio");
        audio.instrument_id = Some("plugin:zyn-1".into());
        let tracks = [audio, midi_track("t-midi", None)];
        assert_eq!(midi_track_bound_to_plugin(&tracks, "zyn-1"), None);
    }
}
