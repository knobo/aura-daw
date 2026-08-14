//! Plugin state persistence (phase 3, zone P4) — ARCHITECTURE §15.3 "State",
//! PHASE3-PLAN contract 6, `plugin-state.schema.json`.
//!
//! What lives here:
//!
//! * The **APST chunked state blob** written to
//!   `<project>/plugins/<instanceId>.state` — opaque binary, never inline
//!   JSON (the AMEV rule of SCALABILITY §3 applied to plugin state).
//! * **Project v2 `plugins[]` persistence** (`persistedInstance` rows) —
//!   the document (`control::session::PluginDoc`, Task 9) lives in
//!   `Session.plugins`; [`save_snapshot_into_project`] persists a SNAPSHOT
//!   of it on every commit that touches plugins (`PersistEffect::plugins`,
//!   `ControlPlane::execute_persist`), same auto-persist philosophy as midi
//!   edits. [`restore_into_session`]/[`adopt_open_project`] +
//!   [`reactivate_restored`] run on project open through
//!   `midi::sync_midi_store`'s adoption seam (called AFTER its session
//!   guard drops — see `adopt_open_project`'s doc for the lock contract).
//! * The **host state bridge** ([`HostStateBridge`]) — the contract through
//!   which zones P1 (CLAP state extension) / P2 (LV2) serialize live
//!   instance state on the plugin main thread. Until a bridge registers,
//!   the param snapshot + preserved blobs carry the data.
//! * **Raw-lilv LV2 `state:interface`** glue on top of livi handles
//!   ([`save_lv2_state`] / [`restore_lv2_state`] + the property-list
//!   encoding) — livi 0.7 exposes no state support; this is the machinery
//!   the LV2 half of the bridge calls on the host thread.
//!
//! Data-safety rules (the review's hardening standard):
//!
//! * A plugin missing on this machine keeps its instance row, its param
//!   snapshot AND its state blob across open/save cycles: status stays
//!   `"stub"` (renders silence), nothing is ever dropped.
//! * Corrupt blobs are detected (magic/version/bounds), logged, and LEFT ON
//!   DISK (still referenced, so GC keeps them); the instance restores
//!   without state.
//! * `stateRef` is path-traversal-safe: only `plugins/<name>.state` directly
//!   inside the project directory resolves.
//!
//! **Known gap (Task 9 review round 1, Important-4)**: live host state is
//! captured on `save` only via an explicit op that already produced fresh
//! `pending_state` bytes (`PluginRemove`'s captured blob, `PluginSetState`)
//! or via `with_host_state: true` (seed_demo's own instantiate path
//! currently the only production caller — ordinary auto-persist always
//! passes `false`, since a bare param write must not round-trip the plugin
//! main thread per rAF batch). A user tweaking a plugin through ITS OWN
//! native GUI (no AURA op involved at all) is NEVER captured by this path —
//! that state is lost on save/reopen until a later round adds a periodic
//! host-state poll.

use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::descriptor::{ParamInfo, PluginInstanceInfo};
use super::ParamChange;
use crate::control::session::PluginDoc;
use crate::control::Session;

/// Project subdirectory holding the state blobs.
pub const STATE_DIR: &str = "plugins";

// ---------------------------------------------------------------------------
// APST blob format (design language of AMEV/AWTF: magic, version, sizes)
// ---------------------------------------------------------------------------

/// "APST" — Aura Plugin STate.
pub const APST_MAGIC: u32 = 0x4150_5354;
pub const APST_VERSION: u16 = 1;
/// Opaque host stream (CLAP `clap_plugin_state` save/load — zone P1).
pub const KIND_OPAQUE: u16 = 1;
/// LV2 `state:interface` property list ([`encode_lv2_props`]).
pub const KIND_LV2_PROPS: u16 = 2;

const MAX_UID_LEN: usize = 64 * 1024;

/// One chunk of opaque plugin state plus its format discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateBlob {
    /// [`KIND_OPAQUE`] | [`KIND_LV2_PROPS`] (readers must tolerate unknown
    /// kinds — they round-trip as opaque bytes).
    pub kind: u16,
    pub data: Vec<u8>,
}

/// Encode a blob (with the owning plugin `uid` for restore-time sanity
/// checks) into APST bytes.
pub fn encode_state(uid: &str, blob: &StateBlob) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + uid.len() + blob.data.len());
    out.write_u32::<LittleEndian>(APST_MAGIC).unwrap();
    out.write_u16::<LittleEndian>(APST_VERSION).unwrap();
    out.write_u16::<LittleEndian>(blob.kind).unwrap();
    out.write_u32::<LittleEndian>(uid.len() as u32).unwrap();
    out.extend_from_slice(uid.as_bytes());
    out.write_u32::<LittleEndian>(blob.data.len() as u32).unwrap();
    out.extend_from_slice(&blob.data);
    out
}

/// Decode APST bytes -> `(uid, blob)`. Every length is bounds-checked; a
/// newer version than supported is rejected (same rule as AMEV).
pub fn decode_state(bytes: &[u8]) -> Result<(String, StateBlob), String> {
    let mut r = std::io::Cursor::new(bytes);
    let magic = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    if magic != APST_MAGIC {
        return Err(format!("not an APST blob (magic {magic:#010x})"));
    }
    let version = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    if version > APST_VERSION {
        return Err(format!(
            "APST version {version} is newer than supported {APST_VERSION}"
        ));
    }
    let kind = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    let uid_len = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())? as usize;
    let pos = r.position() as usize;
    if uid_len > MAX_UID_LEN || pos + uid_len > bytes.len() {
        return Err(format!("APST uid length {uid_len} out of bounds"));
    }
    let uid = String::from_utf8(bytes[pos..pos + uid_len].to_vec())
        .map_err(|e| format!("APST uid not UTF-8: {e}"))?;
    r.set_position((pos + uid_len) as u64);
    let data_len = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())? as usize;
    let pos = r.position() as usize;
    if pos + data_len > bytes.len() {
        return Err(format!("APST payload length {data_len} out of bounds"));
    }
    let data = bytes[pos..pos + data_len].to_vec();
    Ok((uid, StateBlob { kind, data }))
}

/// Atomic write of already-APST-ENCODED bytes (tmp + fsync + rename — the
/// project.json discipline). Used both for a freshly encoded blob and for
/// bytes that were ALREADY encoded elsewhere (`PluginDoc::pending_state` —
/// Task 9 keeps that map self-describing so it never needs re-encoding).
fn write_state_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("state.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

fn write_state_file(path: &Path, uid: &str, blob: &StateBlob) -> Result<(), String> {
    write_state_bytes(path, &encode_state(uid, blob))
}

fn read_state_file(path: &Path) -> Result<(String, StateBlob), String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    decode_state(&bytes)
}

/// A single path component that cannot escape the project dir.
fn safe_component(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
}

/// Resolve a `stateRef` ("plugins/<name>.state") to an absolute path inside
/// the project — anything else is rejected (path-traversal hardening, same
/// rule as midi `eventsRef`).
fn blob_path(dir: &Path, state_ref: &str) -> Option<PathBuf> {
    let name = state_ref
        .strip_prefix("plugins/")
        .filter(|n| safe_component(n) && n.ends_with(".state"))?;
    Some(dir.join(STATE_DIR).join(name))
}

fn blob_rel(instance_id: &str) -> String {
    format!("{STATE_DIR}/{instance_id}.state")
}

/// Absolute state dir for a project (`<dir>/plugins`).
pub fn state_dir(dir: &Path) -> PathBuf {
    dir.join(STATE_DIR)
}

// ---------------------------------------------------------------------------
// Host state bridge (contract 6 — implemented by zones P1/P2)
// ---------------------------------------------------------------------------

/// Serialize/deserialize LIVE instance state through the plugin main thread.
///
/// Contract for implementations (P1: CLAP state extension via clack; P2:
/// LV2 `state:interface` via [`save_lv2_state`] / [`restore_lv2_state`] on
/// the host thread):
///
/// * `save_state` returns `Ok(None)` when the instance is not hosted or the
///   plugin implements no state interface — callers fall back to the param
///   snapshot (and preserve any prior blob).
/// * Implementations MUST NOT take the plugin registry lock: the bridge is
///   called while the registry is locked by the persistence path.
pub trait HostStateBridge: Send + Sync {
    fn save_state(&self, instance_id: &str) -> Result<Option<StateBlob>, String>;
    fn load_state(&self, instance_id: &str, blob: &StateBlob) -> Result<(), String>;
}

static BRIDGE: OnceLock<Arc<dyn HostStateBridge>> = OnceLock::new();

/// Register the app-wide host state bridge (first registration wins — the
/// registry-global pattern).
pub fn register_state_bridge(bridge: Arc<dyn HostStateBridge>) {
    let _ = BRIDGE.set(bridge);
}

pub fn registered_state_bridge() -> Option<&'static Arc<dyn HostStateBridge>> {
    BRIDGE.get()
}

/// THE production [`HostStateBridge`] (architect merge): routes save/load to
/// the format host that owns the instance, all on the shared plugin main
/// thread. Registered once from `plugins::init`.
///
/// * **CLAP** (zone P1): `clap_host::save_state`/`load_state` through the
///   CLAP state extension; blobs are [`KIND_OPAQUE`] host streams.
/// * **LV2** (zone P2 + P4): `lv2_host` runs the raw-lilv `state:interface`
///   glue ([`lv2_state_blob`]/[`apply_lv2_state_blob`]) against a
///   main-thread shadow instance; blobs are [`KIND_LV2_PROPS`] property
///   lists. Loaded blobs are re-applied to every future RT node.
///
/// Never takes the registry lock (bridge contract): instance format is
/// probed by asking each host whether it owns the id.
pub struct FormatStateBridge;

impl HostStateBridge for FormatStateBridge {
    fn save_state(&self, instance_id: &str) -> Result<Option<StateBlob>, String> {
        if super::clap_host::has_instance(instance_id)? {
            return match super::clap_host::save_state(instance_id) {
                Ok(data) => Ok(Some(StateBlob { kind: KIND_OPAQUE, data })),
                // No state extension: param snapshot is the fallback.
                Err(e) if e.contains("no state extension") => Ok(None),
                Err(e) => Err(e),
            };
        }
        if let Some(host) = super::lv2_host::try_global() {
            if host.has_instance(instance_id)? {
                return host.save_state(instance_id);
            }
        }
        // Not hosted (stub / plugin missing here): callers keep the param
        // snapshot and any preserved blob.
        Ok(None)
    }

    fn load_state(&self, instance_id: &str, blob: &StateBlob) -> Result<(), String> {
        match blob.kind {
            KIND_OPAQUE => super::clap_host::load_state(instance_id, blob.data.clone()),
            KIND_LV2_PROPS => super::lv2_host::global().load_state(instance_id, blob.clone()),
            k => Err(format!("unknown state blob kind {k} for {instance_id}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Project v2 `plugins[]` save / restore
// ---------------------------------------------------------------------------

/// `plugin-state.schema.json#/$defs/persistedInstance` (additive `name` /
/// `format` fields help the UI label instances whose plugin is missing).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedPluginInstance {
    pub id: String,
    pub uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Relative path to the chunked state blob (`plugins/<id>.state`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_ref: Option<String>,
    /// Param snapshot — fallback for plugins without a state interface and
    /// the restore source for the generic param UI before activation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamChange>,
}

/// Persist a `PluginDoc` snapshot into `<dir>/project.json` `plugins[]` plus
/// `<dir>/plugins/<id>.state` blobs, then GC orphaned blobs. Task 9: takes a
/// PLAIN SNAPSHOT (no lock, no `Session`/`PluginRegistry` reference) — the
/// caller (`ControlPlane::execute_persist`) takes `Session::plugin_snapshot`
/// under a short lock and calls this AFTER releasing it (round-2 §4: no
/// disk I/O under the session lock).
///
/// State source priority per instance (never lose data):
/// 1. live host state via the registered [`HostStateBridge`] (only when
///    `with_host_state` — param-mirror updates skip the host round-trip),
/// 2. `doc.pending_state`, when `doc.dirty_state` marks it newer than disk
///    (`PluginRemove`'s captured blob, `PluginSetState`'s new blob — Task 9
///    review round 1, Critical-2: a SECOND `PluginSetState` for the same
///    instance must not be silently dropped just because the FIRST one
///    already produced a `.state` file that "exists"),
/// 3. the existing on-disk blob (kept verbatim) — the pre-Critical-2
///    fallback, still correct for a NON-dirty id (e.g. one whose blob was
///    only ever restored from that exact file, never touched this session),
/// 4. the pending blob when nothing is on disk yet (host not yet available
///    for a restored instance). These bytes are ALREADY APST-encoded
///    (self-describing — see `PluginDoc`'s doc), so they're written
///    verbatim in cases 2/4, never re-encoded.
///
/// The param snapshot from the document mirror always persists in the row.
///
/// Returns the ids whose `dirty_state` flag the CALLER should now clear
/// (every dirty id actually written this call — cases 2 above); `execute_
/// persist` does that clearing under a short session lock (this function
/// itself never touches `Session`, only the plain `doc` snapshot it was
/// handed).
pub fn save_snapshot_into_project(
    dir: &Path,
    doc: &PluginDoc,
    with_host_state: bool,
) -> Result<Vec<String>, String> {
    save_snapshot_into_project_with(
        dir,
        doc,
        with_host_state,
        registered_state_bridge().map(|b| b.as_ref()),
    )
}

/// [`save_snapshot_into_project`] with an explicit bridge (tests inject
/// [`FormatStateBridge`] or a fake without touching the process global).
pub fn save_snapshot_into_project_with(
    dir: &Path,
    doc: &PluginDoc,
    with_host_state: bool,
    bridge: Option<&dyn HostStateBridge>,
) -> Result<Vec<String>, String> {
    let sdir = state_dir(dir);
    // Deterministic on-disk ordering (matches the retired
    // `save_into_project`'s `HashMap`-derived sort) — `doc.instances` itself
    // stays insertion-ordered (Task 9: `Vec`, addressed by index like
    // `TrackAdd`/`ClipAdd`), this sort is output-only.
    let mut instances: Vec<&PluginInstanceInfo> = doc.instances.iter().collect();
    instances.sort_by(|a, b| a.id.cmp(&b.id));

    let mut rows = Vec::with_capacity(instances.len());
    let mut live_files: Vec<String> = Vec::new();
    let mut cleared_dirty: Vec<String> = Vec::new();
    for info in instances {
        if !safe_component(&info.id) {
            log::warn!("plugins: refusing to persist unsafe instance id {:?}", info.id);
            continue;
        }
        let fresh = if with_host_state {
            bridge.and_then(|b| match b.save_state(&info.id) {
                Ok(blob) => blob,
                Err(e) => {
                    log::warn!("plugins: state save for {} failed: {e}", info.id);
                    None
                }
            })
        } else {
            None
        };

        let file = sdir.join(format!("{}.state", info.id));
        let is_dirty = doc.dirty_state.contains(&info.id);
        let mut state_ref = None;
        if let Some(blob) = fresh {
            fs::create_dir_all(&sdir).map_err(|e| e.to_string())?;
            write_state_file(&file, &info.uid, &blob)?;
            state_ref = Some(blob_rel(&info.id));
            // Fresh host state is even more authoritative than a dirty
            // pending blob — satisfies the same "now on disk" invariant.
            if is_dirty {
                cleared_dirty.push(info.id.clone());
            }
        } else if is_dirty {
            if let Some(pending_bytes) = doc.pending_state.get(&info.id) {
                // Dirty beats "file exists" (Critical-2 fix): these bytes
                // are newer than the file, if any.
                fs::create_dir_all(&sdir).map_err(|e| e.to_string())?;
                write_state_bytes(&file, pending_bytes)?;
                state_ref = Some(blob_rel(&info.id));
                cleared_dirty.push(info.id.clone());
            } else if file.exists() {
                // Dirty but nothing pending (shouldn't happen — defensive):
                // fall back to whatever's on disk rather than losing the
                // reference.
                state_ref = Some(blob_rel(&info.id));
            }
        } else if file.exists() {
            // Keep whatever is on disk (possibly written by an older or
            // newer session) — referenced files survive GC.
            state_ref = Some(blob_rel(&info.id));
        } else if let Some(pending_bytes) = doc.pending_state.get(&info.id) {
            fs::create_dir_all(&sdir).map_err(|e| e.to_string())?;
            write_state_bytes(&file, pending_bytes)?;
            state_ref = Some(blob_rel(&info.id));
        }
        if let Some(r) = &state_ref {
            live_files.push(r.trim_start_matches("plugins/").to_string());
        }

        let params = doc
            .params
            .get(&info.id)
            .map(|ps| {
                ps.iter()
                    .map(|p| ParamChange { id: p.id, value: p.value })
                    .collect()
            })
            .unwrap_or_default();
        rows.push(PersistedPluginInstance {
            id: info.id.clone(),
            uid: info.uid.clone(),
            name: Some(info.name.clone()),
            format: Some(info.format.clone()),
            state_ref,
            params,
        });
    }

    let value = serde_json::to_value(&rows).map_err(|e| e.to_string())?;
    crate::midi::persist::update_project_v2(dir, |obj| {
        obj.insert("plugins".into(), value);
        Ok(())
    })?;

    // GC blobs no longer referenced (best-effort, after a successful write).
    if let Ok(entries) = fs::read_dir(&sdir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".state") && !live_files.iter().any(|f| f == &name) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    Ok(cleared_dirty)
}

/// Read the `plugins[]` rows of `<dir>/project.json`. `Ok(None)` when the
/// project carries no plugins field at all (pre-persistence project — the
/// caller must NOT clear the document then).
pub fn load_rows(dir: &Path) -> Result<Option<Vec<PersistedPluginInstance>>, String> {
    let file = dir.join("project.json");
    let bytes = fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", file.display()))?;
    let Some(arr) = root.get("plugins") else { return Ok(None) };
    let Some(arr) = arr.as_array() else {
        return Err("plugins field is not an array".into());
    };
    // Per-row decode: one malformed row never discards the others.
    let mut rows = Vec::with_capacity(arr.len());
    for v in arr {
        match serde_json::from_value::<PersistedPluginInstance>(v.clone()) {
            Ok(row) => rows.push(row),
            Err(e) => log::warn!("plugins: skipping malformed persisted row ({e}): {v}"),
        }
    }
    Ok(Some(rows))
}

/// Adopt a project's persisted plugins into `session.plugins`, REPLACING
/// the current instance set (mirrors how `open_project` replaces tracks).
/// Restored instances come back as `"stub"` (silent) with their param
/// snapshot mirrored and their state blob parked in
/// `session.plugins.pending_state` until a host accepts it —
/// [`reactivate_restored`] then flips available plugins to `"active"`;
/// unavailable ones stay stub with everything preserved.
///
/// A restored row, fully assembled from disk — plain data, no `Session`
/// reference. Task 9 review round 1 (Critical-1/Important-2): splitting
/// "read from disk" from "install into the document" is what lets the
/// production path (`adopt_open_project`) do the disk I/O with NO session
/// lock held at all, then take one SHORT lock just to swap the document's
/// contents in.
struct RestoredRow {
    info: PluginInstanceInfo,
    params: Vec<ParamInfo>,
    /// APST-encoded bytes (self-describing), or `None` when the row had no
    /// `stateRef` / its blob was unreadable.
    pending_state: Option<Vec<u8>>,
}

/// Read (from `<dir>/project.json` + `<dir>/plugins/*.state`) everything
/// [`install_restored_rows`] needs — PURE disk I/O, no `Session`, no lock
/// of any kind. `Ok(None)` when the project carries no `plugins` field at
/// all (pre-persistence project — the caller must NOT clear the document
/// then).
fn read_restored_rows(dir: &Path) -> Result<Option<Vec<RestoredRow>>, String> {
    let Some(rows) = load_rows(dir)? else { return Ok(None) };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if !safe_component(&row.id) {
            log::warn!("plugins: skipping persisted instance with unsafe id {:?}", row.id);
            continue;
        }
        let format = row.format.clone().unwrap_or_else(|| {
            if row.uid.starts_with("clap:") { "clap".into() } else { "lv2".into() }
        });
        let info = PluginInstanceInfo {
            id: row.id.clone(),
            uid: row.uid.clone(),
            name: row.name.clone().unwrap_or_else(|| row.uid.clone()),
            format,
            status: "stub".into(),
            track_id: None,
        };
        // Param snapshot mirror. Ranges are unknown until activation, so the
        // stub ParamInfo spans the persisted value (NO clamping data loss);
        // activation replaces this with the real list.
        let params: Vec<ParamInfo> = row
            .params
            .iter()
            .map(|c| ParamInfo {
                id: c.id,
                name: format!("param {}", c.id),
                min: c.value.min(0.0),
                max: c.value.max(1.0),
                default: c.value,
                value: c.value,
                steps: 0,
            })
            .collect();
        let pending_state = if let Some(r) = &row.state_ref {
            match blob_path(dir, r)
                .ok_or_else(|| format!("invalid stateRef {r:?}"))
                .and_then(|p| read_state_file(&p))
            {
                Ok((uid, blob)) => {
                    if uid != row.uid {
                        log::warn!(
                            "plugin instance {}: blob uid {uid:?} != row uid {:?}; keeping blob",
                            row.id,
                            row.uid
                        );
                    }
                    // Re-encoded (self-describing) so `pending_state` stays
                    // byte-identical to what a save would write — Task 9's
                    // "same bytes on disk and in the document" invariant.
                    Some(encode_state(&row.uid, &blob))
                }
                Err(e) => {
                    log::warn!(
                        "plugin instance {}: state blob unavailable ({e}); restoring without state",
                        row.id
                    );
                    None
                }
            }
        } else {
            None
        };
        out.push(RestoredRow { info, params, pending_state });
    }
    Ok(Some(out))
}

/// Install already-read rows into `session.plugins`, REPLACING the current
/// instance set. PURE in-memory (no disk I/O, no host calls) — the only
/// piece of this whole restore that needs the session lock, and only for
/// as long as this call takes. Returns `(restored_count, prior_hosted)` —
/// `(format, id)` pairs of the PREVIOUS session's active instances; the
/// caller unregisters them from their format hosts WITHOUT holding the
/// session lock (this function never touches a host, so it's the caller's
/// job either way). `dirty_state` is untouched: a blob just read from that
/// exact file is, by definition, not ahead of it.
fn install_restored_rows(
    session: &mut Session,
    rows: Vec<RestoredRow>,
) -> (usize, Vec<(String, String)>) {
    let prior_hosted: Vec<(String, String)> = session
        .plugins
        .instances
        .iter()
        .filter(|i| i.status == "active" && (i.format == "lv2" || i.format == "clap"))
        .map(|i| (i.format.clone(), i.id.clone()))
        .collect();
    session.plugins.instances.clear();
    session.plugins.params.clear();
    session.plugins.pending_state.clear();

    let restored = rows.len();
    for r in rows {
        if let Some(bytes) = r.pending_state {
            session.plugins.pending_state.insert(r.info.id.clone(), bytes);
        }
        session.plugins.params.insert(r.info.id.clone(), r.params);
        session.plugins.instances.push(r.info);
    }
    (restored, prior_hosted)
}

/// Combined disk-read + install, for callers that already own an UNLOCKED
/// `Session` outright (this module's own tests: a plain `Session`, never
/// behind a shared `Arc<Mutex<_>>`, so there is no reentrancy risk reading
/// disk while "holding" it). The production, LOCKED path
/// (`adopt_open_project`) does NOT call this — it calls
/// [`read_restored_rows`]/[`install_restored_rows`] separately so disk I/O
/// never runs with the session lock held (Task 9 review round 1,
/// Critical-1/Important-2).
///
/// Returns `(restored_count, prior_hosted)` — see [`install_restored_rows`].
/// Projects WITHOUT a `plugins` field leave the document untouched
/// (`(0, vec![])`).
pub fn restore_into_session(
    dir: &Path,
    session: &mut Session,
) -> Result<(usize, Vec<(String, String)>), String> {
    let Some(rows) = read_restored_rows(dir)? else { return Ok((0, Vec::new())) };
    Ok(install_restored_rows(session, rows))
}

/// Re-activate restored `"stub"` instances through the format hosts (LV2 on
/// the shared plugin main thread; CLAP likewise). Call WITHOUT holding the
/// session lock. Failures are graceful: the instance stays `"stub"`
/// (silent), its snapshot and pending blob remain — nothing lost.
///
/// After activation the persisted param snapshot is re-applied (clamped
/// against the REAL ranges) and forwarded to the host; a pending state blob
/// is offered to the registered [`HostStateBridge`], which restores it on
/// the plugin main thread.
pub fn reactivate_restored(session: &Arc<Mutex<Session>>) {
    reactivate_restored_with(session, registered_state_bridge().map(|b| b.as_ref()))
}

/// [`reactivate_restored`] with an explicit bridge (test injection point).
pub fn reactivate_restored_with(
    session: &Arc<Mutex<Session>>,
    bridge: Option<&dyn HostStateBridge>,
) {
    let stubs: Vec<(String, String, String, Vec<ParamChange>)> = {
        let s = session.lock();
        s.plugins
            .instances
            .iter()
            .filter(|i| i.status == "stub" && (i.format == "lv2" || i.format == "clap"))
            .map(|i| {
                let snapshot = s
                    .plugins
                    .params
                    .get(&i.id)
                    .map(|ps| {
                        ps.iter()
                            .map(|p| ParamChange { id: p.id, value: p.value })
                            .collect()
                    })
                    .unwrap_or_default();
                (i.id.clone(), i.uid.clone(), i.format.clone(), snapshot)
            })
            .collect()
    };
    for (id, uid, format, snapshot) in stubs {
        let hosted = match format.as_str() {
            "clap" => super::clap_host::instantiate(&id, &uid),
            _ => super::lv2_host::global().register_instance(&id, &uid),
        };
        match hosted {
            Ok(real_params) => {
                let clamped: Vec<ParamChange> = {
                    let mut s = session.lock();
                    let Some(r) = s.plugins.instances.iter_mut().find(|r| r.id == id) else {
                        continue; // instance vanished meanwhile
                    };
                    r.status = "active".into();
                    s.plugins.params.insert(id.clone(), real_params);
                    if snapshot.is_empty() {
                        Vec::new()
                    } else {
                        let params = s.plugins.params.get_mut(&id).expect("just inserted");
                        let mut applied = Vec::with_capacity(snapshot.len());
                        for c in &snapshot {
                            if let Some(p) = params.iter_mut().find(|p| p.id == c.id) {
                                p.value = c.value.clamp(p.min, p.max);
                                applied.push(ParamChange { id: p.id, value: p.value });
                            }
                        }
                        applied
                    }
                };
                match format.as_str() {
                    "clap" => {
                        if !clamped.is_empty() {
                            if let Err(e) = super::clap_host::set_params(&id, clamped) {
                                log::warn!("plugins: clap snapshot restore for {id}: {e}");
                            }
                        }
                    }
                    _ => {
                        // lv2_host speaks (control-port index, plugin-unit value).
                        let vals: Vec<(u32, f32)> =
                            clamped.iter().map(|c| (c.id, c.value as f32)).collect();
                        super::lv2_host::global().set_params(&id, vals);
                    }
                }
                let pending = session.lock().plugins.pending_state.get(&id).cloned();
                if let (Some(bridge), Some(bytes)) = (bridge, pending) {
                    match decode_state(&bytes) {
                        Ok((_uid, blob)) => match bridge.load_state(&id, &blob) {
                            // Blob delivered; keep it pending too (save prefers
                            // fresh host state once a bridge exists).
                            Ok(()) => log::info!("plugins: state restored into instance {id}"),
                            Err(e) => log::warn!(
                                "plugins: state restore for {id} failed ({e}); blob preserved"
                            ),
                        },
                        Err(e) => log::warn!(
                            "plugins: pending state blob for {id} unreadable ({e}); blob preserved"
                        ),
                    }
                }
                log::info!("plugins: restored instance {id} re-activated ({uid})");
            }
            Err(e) => log::warn!(
                "plugins: instance {id} ({uid}) unavailable on this machine ({e}); \
                 kept as stub — state and params preserved"
            ),
        }
    }
}

/// PROJECT-OPEN SEAM (Task 6: called explicitly from `ControlPlane`'s
/// sanctioned epoch functions, e.g. `open_project_epoch`/`create_project_at`
/// — AFTER the session lock drops, never from a read path): adopt the
/// opened project's plugins into the app-global session. Inert until the
/// app registers the session (`midi::playback::register_store`; unit tests
/// use local sessions directly via `restore_into_session`).
///
/// CALLER CONTRACT (Task 9 review round 1, Critical-1): this function
/// takes the session lock itself, so it must NEVER be called by code that
/// already holds it — every current call site (`ControlPlane`'s epoch
/// functions in `control/mod.rs`) is careful to call this only AFTER
/// dropping its own guard; see their comments. Internally: disk I/O runs
/// FIRST with no lock at all ([`read_restored_rows`]), then one SHORT lock
/// installs the rows ([`install_restored_rows`]) and is dropped, THEN host
/// reactivation runs (no lock — `reactivate_restored` itself only takes
/// brief, separate relocks to write back param mirrors, never spanning a
/// host call).
pub fn adopt_open_project(dir: &Path) {
    let Some(session) = crate::midi::playback::registered_store() else { return };
    let rows = match read_restored_rows(dir) {
        Ok(Some(r)) => r,
        Ok(None) => return, // no `plugins` field — document untouched
        Err(e) => {
            log::warn!("plugins: cannot restore from {}: {e}", dir.display());
            return;
        }
    };
    let (restored, prior_hosted) = {
        let mut s = session.lock();
        install_restored_rows(&mut s, rows)
    };
    if restored == 0 && prior_hosted.is_empty() {
        return;
    }
    // Host bookkeeping outside the session lock: forget the previous
    // project's hosted registrations, then re-activate the restored set.
    for (format, id) in &prior_hosted {
        match format.as_str() {
            "clap" => {
                if let Err(e) = super::clap_host::remove(id) {
                    log::warn!("plugins: dropping prior clap instance {id}: {e}");
                }
            }
            _ => {
                if let Some(host) = super::lv2_host::try_global() {
                    host.unregister_instance(id);
                }
            }
        }
    }
    reactivate_restored(session);
    log::info!(
        "plugins: adopted {} persisted instance(s) from {}",
        restored,
        dir.display()
    );
}

// ---------------------------------------------------------------------------
// LV2 state:interface — raw lilv on top of livi (ARCHITECTURE §15.2 note)
// ---------------------------------------------------------------------------

pub const LV2_STATE_INTERFACE_URI: &str = "http://lv2plug.in/ns/ext/state#interface";
const LV2_STATE_SUCCESS: u32 = 0;
const LV2_STATE_ERR_UNKNOWN: u32 = 1;
const LV2_STATE_IS_POD: u32 = 1;
const LV2_STATE_IS_PORTABLE: u32 = 2;

type StateHandle = *mut c_void;
type StoreFn = unsafe extern "C" fn(
    handle: StateHandle,
    key: u32,
    value: *const c_void,
    size: usize,
    ty: u32,
    flags: u32,
) -> u32;
type RetrieveFn = unsafe extern "C" fn(
    handle: StateHandle,
    key: u32,
    size: *mut usize,
    ty: *mut u32,
    flags: *mut u32,
) -> *const c_void;

/// `LV2_State_Interface` from state.h (livi/lilv expose no typed wrapper;
/// this is the raw extension-data layout, fetched per instance).
#[repr(C)]
pub struct Lv2StateInterface {
    save: unsafe extern "C" fn(
        instance: *mut c_void,
        store: StoreFn,
        handle: StateHandle,
        flags: u32,
        features: *const *const c_void,
    ) -> u32,
    restore: unsafe extern "C" fn(
        instance: *mut c_void,
        retrieve: RetrieveFn,
        handle: StateHandle,
        flags: u32,
        features: *const *const c_void,
    ) -> u32,
}

/// One stored state property, with urids unmapped to URIs so blobs survive
/// across sessions (urids are session-local).
#[derive(Debug, Clone, PartialEq)]
pub struct Lv2Property {
    pub key: String,
    pub type_uri: String,
    pub flags: u32,
    pub value: Vec<u8>,
}

struct SaveCtx<'a> {
    features: &'a livi::Features,
    props: Vec<Lv2Property>,
    error: Option<String>,
}

unsafe extern "C" fn store_cb(
    handle: StateHandle,
    key: u32,
    value: *const c_void,
    size: usize,
    ty: u32,
    flags: u32,
) -> u32 {
    let ctx = &mut *(handle as *mut SaveCtx);
    let Some(key_uri) = ctx.features.uri(key).map(str::to_owned) else {
        ctx.error = Some(format!("plugin stored unmapped key urid {key}"));
        return LV2_STATE_ERR_UNKNOWN;
    };
    let Some(type_uri) = ctx.features.uri(ty).map(str::to_owned) else {
        ctx.error = Some(format!("plugin stored unmapped type urid {ty}"));
        return LV2_STATE_ERR_UNKNOWN;
    };
    if value.is_null() && size > 0 {
        ctx.error = Some(format!("plugin stored NULL value of size {size}"));
        return LV2_STATE_ERR_UNKNOWN;
    }
    let bytes = if size == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(value as *const u8, size).to_vec()
    };
    ctx.props.push(Lv2Property { key: key_uri, type_uri, flags, value: bytes });
    LV2_STATE_SUCCESS
}

struct RestoreCtx {
    /// key urid -> (type urid, flags, value bytes). Values stay alive (and
    /// unmoved) for the whole restore call.
    by_key: HashMap<u32, (u32, u32, Vec<u8>)>,
}

unsafe extern "C" fn retrieve_cb(
    handle: StateHandle,
    key: u32,
    size: *mut usize,
    ty: *mut u32,
    flags: *mut u32,
) -> *const c_void {
    let ctx = &*(handle as *const RestoreCtx);
    match ctx.by_key.get(&key) {
        Some((t, f, v)) => {
            if !size.is_null() {
                *size = v.len();
            }
            if !ty.is_null() {
                *ty = *t;
            }
            if !flags.is_null() {
                *flags = *f;
            }
            v.as_ptr() as *const c_void
        }
        None => std::ptr::null(),
    }
}

fn state_interface(
    instance: &livi::Instance,
) -> Option<std::ptr::NonNull<Lv2StateInterface>> {
    // SAFETY: extension_data runs plugin code; the caller guarantees the
    // instance-owning thread (plugin main thread / single-threaded test).
    unsafe {
        instance
            .raw()
            .instance()
            .extension_data::<Lv2StateInterface>(LV2_STATE_INTERFACE_URI)
    }
}

/// Serialize an LV2 instance's state via its `state:interface`.
///
/// `Ok(None)` when the plugin implements no state interface (callers fall
/// back to the param snapshot). MUST run on the thread owning the instance
/// (the plugin main thread — lilv thread affinity, contract 2).
pub fn save_lv2_state(
    instance: &livi::Instance,
    features: &livi::Features,
) -> Result<Option<Vec<Lv2Property>>, String> {
    let Some(iface) = state_interface(instance) else { return Ok(None) };
    let mut ctx = SaveCtx { features, props: Vec::new(), error: None };
    let features_arr: [*const c_void; 1] = [std::ptr::null()];
    // SAFETY: iface points at the plugin's static extension struct; the
    // store callback only dereferences the ctx we pass and copies bytes.
    let status = unsafe {
        (iface.as_ref().save)(
            instance.raw().instance().handle(),
            store_cb,
            &mut ctx as *mut SaveCtx as StateHandle,
            LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE,
            features_arr.as_ptr(),
        )
    };
    if let Some(e) = ctx.error {
        return Err(e);
    }
    if status != LV2_STATE_SUCCESS {
        return Err(format!("plugin state save failed (LV2_State_Status {status})"));
    }
    Ok(Some(ctx.props))
}

/// Restore previously saved properties into an LV2 instance. Same threading
/// contract as [`save_lv2_state`].
pub fn restore_lv2_state(
    instance: &mut livi::Instance,
    features: &livi::Features,
    props: &[Lv2Property],
) -> Result<(), String> {
    let Some(iface) = state_interface(instance) else {
        return Err("plugin implements no state:interface".into());
    };
    let mut by_key = HashMap::with_capacity(props.len());
    for p in props {
        let key = CString::new(p.key.as_str())
            .map_err(|_| format!("state key contains NUL: {:?}", p.key))?;
        let ty = CString::new(p.type_uri.as_str())
            .map_err(|_| format!("state type contains NUL: {:?}", p.type_uri))?;
        by_key.insert(
            features.urid(&key),
            (features.urid(&ty), p.flags, p.value.clone()),
        );
    }
    let ctx = RestoreCtx { by_key };
    let features_arr: [*const c_void; 1] = [std::ptr::null()];
    // SAFETY: as in save; retrieve only reads from ctx, which outlives the
    // call and never moves during it.
    let status = unsafe {
        (iface.as_ref().restore)(
            instance.raw().instance().handle(),
            retrieve_cb,
            &ctx as *const RestoreCtx as StateHandle,
            LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE,
            features_arr.as_ptr(),
        )
    };
    if status != LV2_STATE_SUCCESS {
        return Err(format!("plugin state restore failed (LV2_State_Status {status})"));
    }
    Ok(())
}

// Property-list payload encoding (KIND_LV2_PROPS):
// [count u32] then per property:
//   [key_len u32][key][type_len u32][type][flags u32][val_len u32][value]

const MAX_PROP_STR: usize = 64 * 1024;

pub fn encode_lv2_props(props: &[Lv2Property]) -> Vec<u8> {
    let mut out = Vec::new();
    out.write_u32::<LittleEndian>(props.len() as u32).unwrap();
    for p in props {
        out.write_u32::<LittleEndian>(p.key.len() as u32).unwrap();
        out.extend_from_slice(p.key.as_bytes());
        out.write_u32::<LittleEndian>(p.type_uri.len() as u32).unwrap();
        out.extend_from_slice(p.type_uri.as_bytes());
        out.write_u32::<LittleEndian>(p.flags).unwrap();
        out.write_u32::<LittleEndian>(p.value.len() as u32).unwrap();
        out.extend_from_slice(&p.value);
    }
    out
}

pub fn decode_lv2_props(bytes: &[u8]) -> Result<Vec<Lv2Property>, String> {
    fn take<'a>(bytes: &'a [u8], pos: &mut usize, n: usize) -> Result<&'a [u8], String> {
        if n > bytes.len().saturating_sub(*pos) {
            return Err(format!("lv2 props: length {n} out of bounds at {pos}"));
        }
        let s = &bytes[*pos..*pos + n];
        *pos += n;
        Ok(s)
    }
    fn take_u32(bytes: &[u8], pos: &mut usize) -> Result<u32, String> {
        let s = take(bytes, pos, 4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn take_str(bytes: &[u8], pos: &mut usize) -> Result<String, String> {
        let n = take_u32(bytes, pos)? as usize;
        if n > MAX_PROP_STR {
            return Err(format!("lv2 props: string length {n} exceeds limit"));
        }
        String::from_utf8(take(bytes, pos, n)?.to_vec())
            .map_err(|e| format!("lv2 props: string not UTF-8: {e}"))
    }
    let mut pos = 0usize;
    let count = take_u32(bytes, &mut pos)? as usize;
    let mut props = Vec::new();
    for _ in 0..count {
        let key = take_str(bytes, &mut pos)?;
        let type_uri = take_str(bytes, &mut pos)?;
        let flags = take_u32(bytes, &mut pos)?;
        let vlen = take_u32(bytes, &mut pos)? as usize;
        let value = take(bytes, &mut pos, vlen)?.to_vec();
        props.push(Lv2Property { key, type_uri, flags, value });
    }
    Ok(props)
}

/// Convenience for the LV2 host bridge: full state -> APST-ready blob.
pub fn lv2_state_blob(
    instance: &livi::Instance,
    features: &livi::Features,
) -> Result<Option<StateBlob>, String> {
    Ok(save_lv2_state(instance, features)?
        .map(|props| StateBlob { kind: KIND_LV2_PROPS, data: encode_lv2_props(&props) }))
}

/// Convenience for the LV2 host bridge: APST blob -> instance.
pub fn apply_lv2_state_blob(
    instance: &mut livi::Instance,
    features: &livi::Features,
    blob: &StateBlob,
) -> Result<(), String> {
    if blob.kind != KIND_LV2_PROPS {
        return Err(format!("blob kind {} is not an LV2 property list", blob.kind));
    }
    let props = decode_lv2_props(&blob.data)?;
    restore_lv2_state(instance, features, &props)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::project;
    use crate::audio::types::Store;
    use crate::midi::MidiStore;
    use crate::plugins::PluginRegistry;

    fn tmp_parent(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-plugin-state-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn new_session() -> Session {
        Session::new(Store::default(), MidiStore::default())
    }

    /// Test-only doc-level "instantiate": mints a `"stub"` row + an empty
    /// param entry directly into `session.plugins`, mirroring the retired
    /// `PluginRegistry::instantiate`'s pure (no host call) semantics — these
    /// tests are about PERSISTENCE, not host activation (that's
    /// `instantiate_and_activate`'s job, exercised where a real/fake host is
    /// available, further down this module).
    fn stub_instantiate(session: &mut Session, uid: &str) -> PluginInstanceInfo {
        let info = PluginInstanceInfo {
            id: uuid::Uuid::new_v4().to_string(),
            uid: uid.into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "stub".into(),
            track_id: None,
        };
        session.plugins.instances.push(info.clone());
        session.plugins.params.insert(info.id.clone(), Vec::new());
        info
    }

    // ---- APST blob format --------------------------------------------------

    #[test]
    fn apst_roundtrip_and_corruption_rejection() {
        let blob = StateBlob { kind: KIND_LV2_PROPS, data: vec![1, 2, 3, 250] };
        let bytes = encode_state("lv2:urn:x", &blob);
        let (uid, back) = decode_state(&bytes).unwrap();
        assert_eq!(uid, "lv2:urn:x");
        assert_eq!(back, blob);

        // Empty payload is legal.
        let empty = StateBlob { kind: KIND_OPAQUE, data: vec![] };
        let bytes = encode_state("clap:/a.clap#x", &empty);
        assert_eq!(decode_state(&bytes).unwrap().1, empty);

        // Corruption: bad magic, newer version, truncated, oversized lens.
        assert!(decode_state(&[0u8; 8]).is_err(), "bad magic");
        let mut newer = encode_state("u", &blob);
        newer[4] = 0xFF;
        assert!(decode_state(&newer).is_err(), "newer version rejected");
        let full = encode_state("u", &blob);
        assert!(decode_state(&full[..full.len() - 2]).is_err(), "truncated");
        let mut evil = encode_state("u", &blob);
        evil[8] = 0xFF;
        evil[9] = 0xFF;
        evil[10] = 0xFF;
        evil[11] = 0xFF; // uid_len = u32::MAX
        assert!(decode_state(&evil).is_err(), "oversized uid length");
    }

    // ---- project save / restore -------------------------------------------

    #[test]
    fn plugins_save_restore_roundtrip_with_blobs_and_params() {
        let parent = tmp_parent("roundtrip");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let mut session = new_session();
        let a = stub_instantiate(&mut session, "lv2:urn:test:synth");
        let b = stub_instantiate(&mut session, "lv2:urn:test:synth");
        session.plugins.params.get_mut(&a.id).unwrap().push(ParamInfo {
            id: 3,
            name: "param 3".into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            value: 0.75,
            steps: 0,
        });
        // Instance b has a pending blob (as if restored for a missing host).
        let blob = StateBlob { kind: KIND_OPAQUE, data: vec![9; 100] };
        session.plugins.pending_state.insert(b.id.clone(), encode_state("lv2:urn:test:synth", &blob));

        save_snapshot_into_project(&dir, &session.plugins, true).unwrap();
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["schemaVersion"], 2, "plugin save upgrades to v2");
        assert!(dir.join("project.json.v1.bak").exists(), "v1 backup kept");
        let rows = raw["plugins"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // b's pending blob became a referenced .state file.
        let b_row = rows.iter().find(|r| r["id"] == b.id.as_str()).unwrap();
        let sref = b_row["stateRef"].as_str().unwrap();
        assert_eq!(sref, format!("plugins/{}.state", b.id));
        let (uid, on_disk) = read_state_file(&dir.join(sref)).unwrap();
        assert_eq!(uid, "lv2:urn:test:synth");
        assert_eq!(on_disk, blob);
        // a has params but no blob.
        let a_row = rows.iter().find(|r| r["id"] == a.id.as_str()).unwrap();
        assert!(a_row.get("stateRef").is_none());
        assert_eq!(a_row["params"][0]["id"], 3);

        // Restore into a fresh session: everything comes back, stub status.
        let mut fresh = new_session();
        let (n, prior) = restore_into_session(&dir, &mut fresh).unwrap();
        assert_eq!(n, 2);
        assert!(prior.is_empty());
        let ra = fresh.plugins.instances.iter().find(|r| r.id == a.id).expect("a restored");
        assert_eq!(ra.status, "stub");
        assert_eq!(ra.uid, "lv2:urn:test:synth");
        assert_eq!(ra.name, "TestSynth");
        assert_eq!(fresh.plugins.params.get(&a.id).unwrap()[0].value, 0.75);
        let stored = fresh.plugins.pending_state.get(&b.id).expect("blob preserved");
        assert_eq!(decode_state(stored).unwrap().1, blob);

        // Re-saving from the restored session keeps the blob (no host
        // bridge registered): data survives a full open/save cycle.
        save_snapshot_into_project(&dir, &fresh.plugins, true).unwrap();
        assert_eq!(read_state_file(&dir.join(sref)).unwrap().1, blob);
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn projects_without_plugins_field_leave_document_alone() {
        let parent = tmp_parent("no-field");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut session = new_session();
        let inst = stub_instantiate(&mut session, "lv2:urn:test:synth");
        let (n, prior) = restore_into_session(&dir, &mut session).unwrap();
        assert_eq!((n, prior.len()), (0, 0));
        assert!(
            session.plugins.instances.iter().any(|r| r.id == inst.id),
            "document untouched"
        );
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn malicious_state_ref_is_rejected_and_instance_still_restores() {
        let parent = tmp_parent("evil");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let doc = PluginDoc::default();
        save_snapshot_into_project(&dir, &doc, true).unwrap();
        let mut raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        raw["plugins"] = serde_json::json!([
            {"id": "inst-1", "uid": "lv2:urn:x",
             "stateRef": "plugins/../../../etc/passwd.state"},
            {"id": "../evil", "uid": "lv2:urn:y"}
        ]);
        fs::write(dir.join("project.json"), serde_json::to_vec(&raw).unwrap()).unwrap();

        let mut fresh = new_session();
        let (n, _) = restore_into_session(&dir, &mut fresh).unwrap();
        assert_eq!(n, 1, "unsafe id skipped, traversal ref tolerated");
        assert!(fresh.plugins.instances.iter().any(|r| r.id == "inst-1"));
        assert!(fresh.plugins.pending_state.is_empty(), "no blob read through traversal");
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn corrupt_blob_restores_instance_without_state_and_keeps_file() {
        let parent = tmp_parent("corrupt");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut session = new_session();
        let inst = stub_instantiate(&mut session, "lv2:urn:test:synth");
        session.plugins.pending_state.insert(
            inst.id.clone(),
            encode_state("lv2:urn:test:synth", &StateBlob { kind: KIND_OPAQUE, data: vec![7; 10] }),
        );
        save_snapshot_into_project(&dir, &session.plugins, true).unwrap();
        // Corrupt the blob on disk.
        let blob_file = state_dir(&dir).join(format!("{}.state", inst.id));
        fs::write(&blob_file, b"garbage").unwrap();

        let mut fresh = new_session();
        let (n, _) = restore_into_session(&dir, &mut fresh).unwrap();
        assert_eq!(n, 1, "instance restored despite corrupt blob");
        assert!(fresh.plugins.pending_state.is_empty());
        // The (still referenced) corrupt file survives the next save's GC —
        // a future version might read it.
        save_snapshot_into_project(&dir, &fresh.plugins, true).unwrap();
        assert!(blob_file.exists(), "referenced blob never GC'd");
        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn removed_instances_get_their_blobs_gcd_on_resave() {
        let parent = tmp_parent("gc");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut session = new_session();
        let inst = stub_instantiate(&mut session, "lv2:urn:test:synth");
        session.plugins.pending_state.insert(
            inst.id.clone(),
            encode_state("lv2:urn:test:synth", &StateBlob { kind: KIND_OPAQUE, data: vec![1] }),
        );
        save_snapshot_into_project(&dir, &session.plugins, true).unwrap();
        let blob_file = state_dir(&dir).join(format!("{}.state", inst.id));
        assert!(blob_file.exists());

        session.plugins.instances.retain(|r| r.id != inst.id);
        session.plugins.params.remove(&inst.id);
        session.plugins.pending_state.remove(&inst.id);
        save_snapshot_into_project(&dir, &session.plugins, true).unwrap();
        assert!(!blob_file.exists(), "orphan blob GC'd");
        let raw: Value =
            serde_json::from_slice(&fs::read(dir.join("project.json")).unwrap()).unwrap();
        assert_eq!(raw["plugins"].as_array().unwrap().len(), 0);
        let _ = fs::remove_dir_all(&parent);
    }

    /// A bridge takes precedence and its state lands on disk. Uses the
    /// explicit-bridge entry point — the process-global bridge slot belongs
    /// to `plugins::init` (production `FormatStateBridge`), never to tests.
    #[test]
    fn bridge_state_is_preferred_over_pending_and_disk() {
        struct OneBridge(String, StateBlob);
        impl HostStateBridge for OneBridge {
            fn save_state(&self, id: &str) -> Result<Option<StateBlob>, String> {
                Ok((id == self.0).then(|| self.1.clone()))
            }
            fn load_state(&self, _id: &str, _b: &StateBlob) -> Result<(), String> {
                Ok(())
            }
        }

        let fresh_blob = StateBlob { kind: KIND_LV2_PROPS, data: vec![42; 8] };
        let parent = tmp_parent("bridge");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        let mut session = new_session();
        let inst = stub_instantiate(&mut session, "lv2:urn:test:synth");
        let bridge = OneBridge(inst.id.clone(), fresh_blob.clone());
        session.plugins.pending_state.insert(
            inst.id.clone(),
            encode_state("lv2:urn:test:synth", &StateBlob { kind: KIND_OPAQUE, data: vec![1] }),
        );
        save_snapshot_into_project_with(&dir, &session.plugins, true, Some(&bridge)).unwrap();
        let (_uid, on_disk) =
            read_state_file(&state_dir(&dir).join(format!("{}.state", inst.id))).unwrap();
        assert_eq!(on_disk, fresh_blob, "bridge state written");

        // Param-path saves (with_host_state=false) keep the existing file.
        save_snapshot_into_project_with(&dir, &session.plugins, false, Some(&bridge)).unwrap();
        let (_uid, still) =
            read_state_file(&state_dir(&dir).join(format!("{}.state", inst.id))).unwrap();
        assert_eq!(still, fresh_blob);
        let _ = fs::remove_dir_all(&parent);
    }

    /// ARCHITECT-MERGE ACCEPTANCE (worklist item 2): opaque Zyn patches flow
    /// through the REAL save/open path. Instantiate Zyn through the host +
    /// unified plugin main thread, save the project with the production
    /// [`FormatStateBridge`] (serializes live state via `state:interface` on
    /// the plugin main thread), then open into a fresh session: the blob
    /// restores, reactivation loads it back into the host, a SECOND save
    /// round-trips it, and a real RT node still builds (state applied).
    /// Skips cleanly when zynaddsubfx-lv2 is not installed.
    #[test]
    fn zyn_state_roundtrips_through_real_project_save_open() {
        const ZYN_UID: &str = "lv2:http://zynaddsubfx.sourceforge.net";
        let scanned = crate::plugins::scan::scan_lv2();
        if !scanned.iter().any(|d| d.uid == ZYN_UID) {
            eprintln!("skipping: zynaddsubfx-lv2 not installed");
            return;
        }
        let parent = tmp_parent("zyn-roundtrip");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        // Registry (scan cache only) + host activation (the
        // plugin_instantiate path).
        let registry = Arc::new(Mutex::new(PluginRegistry::default()));
        registry.lock().scanned = Some(scanned);
        let (info, params) = crate::plugins::instantiate_and_activate(&registry, ZYN_UID)
            .expect("Zyn instantiates and activates");
        assert_eq!(info.status, "active");

        let session = Arc::new(Mutex::new(new_session()));
        {
            let mut s = session.lock();
            s.plugins.instances.push(info.clone());
            s.plugins.params.insert(info.id.clone(), params);
        }

        // SAVE through the real path with the production bridge.
        let bridge = FormatStateBridge;
        save_snapshot_into_project_with(&dir, &session.lock().plugins, true, Some(&bridge)).unwrap();
        let blob_file = state_dir(&dir).join(format!("{}.state", info.id));
        let (uid_on_disk, blob) = read_state_file(&blob_file).expect("Zyn blob written");
        assert_eq!(uid_on_disk, ZYN_UID);
        assert_eq!(blob.kind, KIND_LV2_PROPS, "LV2 state saved as property list");
        assert!(
            !decode_lv2_props(&blob.data).unwrap().is_empty(),
            "Zyn state has properties"
        );

        // OPEN into a fresh session: restore + reactivate + state load.
        let fresh = Arc::new(Mutex::new(new_session()));
        let (n, _) = restore_into_session(&dir, &mut fresh.lock()).unwrap();
        assert_eq!(n, 1);
        let stored = fresh.lock().plugins.pending_state.get(&info.id).cloned().unwrap();
        assert_eq!(decode_state(&stored).unwrap().1, blob);
        reactivate_restored_with(&fresh, Some(&bridge));
        assert_eq!(
            fresh.lock().plugins.instances.iter().find(|r| r.id == info.id).unwrap().status,
            "active",
            "restored Zyn re-activates through the unified host thread"
        );
        // A real RT node builds from the restored registration (the loaded
        // blob is applied to it — audible-patch path).
        match super::super::lv2_host::global().make_node(&info.id, 48_000) {
            Ok(node) => drop(node),
            Err(e) => panic!("restored instance yields an RT node: {e}"),
        }

        // A SECOND save from the restored session round-trips the state.
        save_snapshot_into_project_with(&dir, &fresh.lock().plugins, true, Some(&bridge)).unwrap();
        let (_uid, again) = read_state_file(&blob_file).unwrap();
        assert_eq!(again.kind, KIND_LV2_PROPS);
        assert!(!decode_lv2_props(&again.data).unwrap().is_empty());

        super::super::lv2_host::global().unregister_instance(&info.id);
        let _ = fs::remove_dir_all(&parent);
    }

    /// CLAP flavor of the same acceptance: an installed CLAP instrument's
    /// opaque state (KIND_OPAQUE via the CLAP state extension) flows through
    /// the real save/open path and the instance re-activates on open. Skips
    /// cleanly when no CLAP instrument is installed.
    #[test]
    fn clap_state_roundtrips_through_real_project_save_open() {
        use crate::plugins::scan::{clap_search_paths, find_clap_bundles};
        // Scan through the sacrificial worker (production path): an
        // in-process scan dlopens every bundle, and Cardinal's teardown
        // corrupts the test process at exit (see
        // `scan_worker::test_worker_command`). Same reason we never pick
        // Cardinal as the instrument to instantiate here.
        let bundles = find_clap_bundles(&clap_search_paths());
        let scanned = if bundles.is_empty() {
            Vec::new()
        } else {
            crate::plugins::scan_worker::scan_with_worker(
                &bundles,
                &crate::plugins::scan_worker::test_worker_command(),
            )
        };
        let Some(desc) = scanned
            .iter()
            .find(|d| d.is_instrument && !d.uid.to_lowercase().contains("cardinal"))
            .cloned()
        else {
            eprintln!("skipping: no CLAP instrument installed");
            return;
        };
        let parent = tmp_parent("clap-roundtrip");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let registry = Arc::new(Mutex::new(PluginRegistry::default()));
        registry.lock().scanned = Some(scanned);
        let (info, params) = crate::plugins::instantiate_and_activate(&registry, &desc.uid)
            .expect("CLAP instrument instantiates");
        assert_eq!(info.status, "active");

        let session = Arc::new(Mutex::new(new_session()));
        {
            let mut s = session.lock();
            s.plugins.instances.push(info.clone());
            s.plugins.params.insert(info.id.clone(), params);
        }

        let bridge = FormatStateBridge;
        save_snapshot_into_project_with(&dir, &session.lock().plugins, true, Some(&bridge)).unwrap();
        let blob_file = state_dir(&dir).join(format!("{}.state", info.id));
        let has_state_ext = blob_file.exists();
        if has_state_ext {
            let (uid_on_disk, blob) = read_state_file(&blob_file).unwrap();
            assert_eq!(uid_on_disk, desc.uid);
            assert_eq!(blob.kind, KIND_OPAQUE, "CLAP state saved as opaque host stream");
            assert!(!blob.data.is_empty());
        } else {
            eprintln!("note: {} has no state extension; snapshot-only path", desc.name);
        }

        // Open into a fresh session: restore + re-activate through the
        // shared plugin main thread (state loaded when a blob exists).
        let fresh = Arc::new(Mutex::new(new_session()));
        let (n, _) = restore_into_session(&dir, &mut fresh.lock()).unwrap();
        assert_eq!(n, 1);
        // The first activation still owns the instance id on the host; drop
        // it (as `open_project` supersedes the old session) before
        // reactivating the restored one.
        let _ = crate::plugins::clap_host::remove(&info.id);
        reactivate_restored_with(&fresh, Some(&bridge));
        assert_eq!(
            fresh.lock().plugins.instances.iter().find(|r| r.id == info.id).unwrap().status,
            "active",
            "restored CLAP instance re-activates"
        );
        if has_state_ext {
            // Second save round-trips the opaque stream.
            save_snapshot_into_project_with(&dir, &fresh.lock().plugins, true, Some(&bridge)).unwrap();
            let (_uid, again) = read_state_file(&blob_file).unwrap();
            assert_eq!(again.kind, KIND_OPAQUE);
            assert!(!again.data.is_empty());
        }
        let _ = crate::plugins::clap_host::remove(&info.id);
        let _ = fs::remove_dir_all(&parent);
    }

    // ---- LV2 property-list encoding ---------------------------------------

    #[test]
    fn lv2_props_roundtrip_and_bounds_checks() {
        let props = vec![
            Lv2Property {
                key: "urn:zyn:state".into(),
                type_uri: "http://lv2plug.in/ns/ext/atom#Chunk".into(),
                flags: LV2_STATE_IS_POD,
                value: b"<xml>patch</xml>".to_vec(),
            },
            Lv2Property {
                key: "urn:other".into(),
                type_uri: "http://www.w3.org/2001/XMLSchema#int".into(),
                flags: LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE,
                value: 7i32.to_le_bytes().to_vec(),
            },
        ];
        let bytes = encode_lv2_props(&props);
        assert_eq!(decode_lv2_props(&bytes).unwrap(), props);
        assert_eq!(decode_lv2_props(&encode_lv2_props(&[])).unwrap(), vec![]);
        assert!(decode_lv2_props(&bytes[..bytes.len() - 3]).is_err(), "truncated");
        let mut evil = bytes.clone();
        evil[4] = 0xFF;
        evil[5] = 0xFF;
        evil[6] = 0xFF;
        evil[7] = 0xFF; // key_len = u32::MAX
        assert!(decode_lv2_props(&evil).is_err(), "oversized string length");
    }

    /// The restore-on-open lifecycle end-to-end: persisted rows come back as
    /// stub instances, then [`reactivate_restored`] re-instantiates through
    /// the REAL LV2 host (zone P2) — flipping available plugins to
    /// `"active"` with real params — while a plugin missing on this machine
    /// stays `"stub"` with params AND state blob preserved (never lost).
    /// The Zyn half self-gates: without zynaddsubfx-lv2 the instance simply
    /// exercises the missing-plugin branch too.
    #[test]
    fn restored_instances_reactivate_through_host_and_missing_stay_stub() {
        const ZYN_UID: &str = "lv2:http://zynaddsubfx.sourceforge.net";
        let zyn_installed = Path::new("/usr/lib/lv2/ZynAddSubFX.lv2/").is_dir();

        let parent = tmp_parent("reactivate");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();
        // Handcraft a saved project: one Zyn row, one unknown-plugin row,
        // both with param snapshots and state blobs.
        let blob = StateBlob { kind: KIND_OPAQUE, data: vec![5; 16] };
        fs::create_dir_all(state_dir(&dir)).unwrap();
        write_state_file(&state_dir(&dir).join("zyn-1.state"), ZYN_UID, &blob).unwrap();
        write_state_file(&state_dir(&dir).join("ghost-1.state"), "lv2:urn:aura:ghost", &blob)
            .unwrap();
        crate::midi::persist::update_project_v2(&dir, |obj| {
            obj.insert(
                "plugins".into(),
                serde_json::json!([
                    {"id": "zyn-1", "uid": ZYN_UID, "name": "ZynAddSubFX", "format": "lv2",
                     "stateRef": "plugins/zyn-1.state",
                     "params": [{"id": 0, "value": 0.5}]},
                    {"id": "ghost-1", "uid": "lv2:urn:aura:ghost", "format": "lv2",
                     "stateRef": "plugins/ghost-1.state",
                     "params": [{"id": 2, "value": 42.0}]}
                ]),
            );
            Ok(())
        })
        .unwrap();

        let session = Arc::new(Mutex::new(new_session()));
        let (n, _) = restore_into_session(&dir, &mut session.lock()).unwrap();
        assert_eq!(n, 2);
        reactivate_restored(&session);

        let s = session.lock();
        // The missing plugin: graceful, nothing lost.
        let ghost = s.plugins.instances.iter().find(|r| r.id == "ghost-1").expect("ghost kept");
        assert_eq!(ghost.status, "stub", "missing plugin stays stub");
        assert_eq!(s.plugins.params.get("ghost-1").unwrap()[0].value, 42.0, "snapshot kept");
        let ghost_blob = s.plugins.pending_state.get("ghost-1").expect("blob kept");
        assert_eq!(decode_state(ghost_blob).unwrap().1, blob);

        let zyn = s.plugins.instances.iter().find(|r| r.id == "zyn-1").expect("zyn kept");
        if zyn_installed {
            assert_eq!(zyn.status, "active", "installed plugin re-activated");
            assert!(
                !s.plugins.params.get("zyn-1").unwrap().is_empty(),
                "real control-port params installed"
            );
            let zyn_blob = s.plugins.pending_state.get("zyn-1").expect("blob preserved");
            assert_eq!(decode_state(zyn_blob).unwrap().1, blob);
        } else {
            eprintln!("note: zynaddsubfx-lv2 not installed; missing-branch asserted");
            assert_eq!(zyn.status, "stub");
        }
        drop(s);

        // A save from the restored session keeps both blobs on disk.
        save_snapshot_into_project(&dir, &session.lock().plugins, false).unwrap();
        assert!(state_dir(&dir).join("zyn-1.state").exists());
        assert!(state_dir(&dir).join("ghost-1.state").exists());
        if zyn_installed {
            super::super::lv2_host::global().unregister_instance("zyn-1");
        }
        let _ = fs::remove_dir_all(&parent);
    }

    // ---- real LV2 state via raw lilv (gated on ZynAddSubFX) ----------------

    /// The zone-P4 acceptance for LV2 state: ZynAddSubFX's `state:interface`
    /// is driven through raw lilv on top of livi handles — save yields
    /// properties, they round-trip through the APST payload encoding, and a
    /// SECOND instance accepts the restore. Skips cleanly when
    /// zynaddsubfx-lv2 is not installed.
    #[test]
    fn zyn_state_saves_and_restores_via_raw_lilv() {
        const ZYN_URI: &str = "http://zynaddsubfx.sourceforge.net";
        let bundle = Path::new("/usr/lib/lv2/ZynAddSubFX.lv2/");
        if !bundle.is_dir() {
            eprintln!("skipping: zynaddsubfx-lv2 not installed");
            return;
        }
        let world = livi::World::with_load_bundle("file:///usr/lib/lv2/ZynAddSubFX.lv2/");
        let Some(plugin) = world.iter_plugins().find(|p| p.uri() == ZYN_URI) else {
            eprintln!("skipping: Zyn bundle present but plugin not loadable");
            return;
        };
        let features = livi::FeaturesBuilder::default().build(&world);
        // SAFETY: running third-party plugin code, single-threaded here (the
        // owning thread), which is exactly the state-interface contract.
        let inst =
            unsafe { plugin.instantiate(features.clone(), 48_000.0) }.expect("Zyn instantiates");

        let props = save_lv2_state(&inst, &features)
            .expect("state save succeeds")
            .expect("Zyn implements state:interface");
        assert!(!props.is_empty(), "Zyn state has at least one property");
        for p in &props {
            assert!(!p.key.is_empty() && !p.type_uri.is_empty());
        }

        // Payload encoding round-trips exactly.
        let blob = StateBlob { kind: KIND_LV2_PROPS, data: encode_lv2_props(&props) };
        assert_eq!(decode_lv2_props(&blob.data).unwrap(), props);

        // A fresh instance accepts the restore and can save again.
        let mut inst2 =
            unsafe { plugin.instantiate(features.clone(), 48_000.0) }.expect("second instance");
        apply_lv2_state_blob(&mut inst2, &features, &blob).expect("state restore succeeds");
        let again = save_lv2_state(&inst2, &features)
            .expect("re-save succeeds")
            .expect("still has state");
        assert!(!again.is_empty());
    }

    // ---- Task 9 review round 1 regressions ---------------------------------

    /// Critical-1: `adopt_open_project` must not deadlock when driven
    /// through the REAL production call chain with a REAL session global
    /// registered. Unit tests that build their own private `Session`
    /// (everywhere else in this suite) never exercise `midi::playback::
    /// registered_store() == Some`, which is exactly the blind spot the
    /// review flagged — this test closes it by registering a session (the
    /// same `midi::playback::register_store` production wiring uses) and
    /// driving the SAME two-step call chain `ControlPlane::open_project_
    /// epoch` uses (Task 6 superseded the old lazy `with_synced_store`/
    /// `notify_project_opened` resync path with eager epoch adoption —
    /// `crate::midi::adopt_midi_from_dir` under the session lock, THEN
    /// `adopt_open_project` after it drops). Before the round-1 fix —
    /// `adopt_open_project` re-locking the SAME session mutex its caller
    /// already held — this hung forever; now the whole chain drops its
    /// guard before adoption runs, so this completes and the row lands.
    ///
    /// `midi::playback::register_store` is a process-global `OnceLock`
    /// (first registration wins). NOTE (merge of Tasks 6+9): mainline's own
    /// `midi::mod::midi_purity_fixture` (midi/mod.rs) ALSO calls
    /// `MidiState::shared` — which registers too — from tests in this same
    /// binary, so this test cannot assume it wins the race. Mirrors the
    /// accepted fix `plugins::automation`'s own
    /// `adopt_open_project_writes_into_the_registered_session` test uses:
    /// operate on and assert against WHOEVER is actually registered (via
    /// `registered_store()`), and identify the row by its unique id rather
    /// than assuming an otherwise-empty session.
    #[test]
    fn adopt_open_project_end_to_end_with_a_registered_session_does_not_deadlock() {
        let parent = tmp_parent("adopt-e2e");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let blob = StateBlob { kind: KIND_OPAQUE, data: vec![3; 8] };
        fs::create_dir_all(state_dir(&dir)).unwrap();
        write_state_file(&state_dir(&dir).join("ghost-e2e.state"), "lv2:urn:aura:ghost-e2e", &blob)
            .unwrap();
        crate::midi::persist::update_project_v2(&dir, |obj| {
            obj.insert(
                "plugins".into(),
                serde_json::json!([
                    {"id": "ghost-e2e", "uid": "lv2:urn:aura:ghost-e2e", "format": "lv2",
                     "stateRef": "plugins/ghost-e2e.state",
                     "params": [{"id": 0, "value": 0.25}]}
                ]),
            );
            Ok(())
        })
        .unwrap();

        let session = Arc::new(Mutex::new(new_session()));
        crate::midi::playback::register_store(session.clone());
        // First registration wins: use whichever session actually holds the
        // global (ours, or one registered by another test in this binary).
        let registered =
            crate::midi::playback::registered_store().expect("a session is registered");

        // THE regression check: this call chain used to deadlock. If it
        // hangs, this test times out instead of failing cleanly — that IS
        // the signal (cargo test's own timeout / CI kill). Mirrors
        // `ControlPlane::open_project_epoch`: adopt midi under the session
        // lock, then adopt plugins AFTER it drops — both against `registered`
        // so this test is race-proof even when it loses `register_store`.
        crate::midi::adopt_midi_from_dir(&mut registered.lock().midi, &dir, 120.0);
        adopt_open_project(&dir);

        let s = registered.lock();
        assert!(
            s.plugins.instances.iter().any(|r| r.id == "ghost-e2e"),
            "plugin row adopted through the real, lock-holding call chain"
        );
        assert_eq!(s.plugins.params.get("ghost-e2e").unwrap()[0].value, 0.25);
        assert_eq!(s.midi.loaded_dir.as_deref(), Some(dir.as_path()), "midi side ALSO synced");
        drop(s);
        let _ = fs::remove_dir_all(&parent);
    }

    /// Critical-2: a SECOND `PluginSetState` for the same instance (the
    /// `zyn_load_patch` shape — load a different patch into an
    /// already-patched instance) must land on disk, not get silently
    /// dropped because the FIRST write already produced a `.state` file
    /// that "exists" (the pre-fix ladder let step-2 "keep the file"
    /// unconditionally beat step-3 "write the pending bytes").
    #[test]
    fn plugin_set_state_persists_a_second_write_not_just_the_first() {
        let parent = tmp_parent("setstate-durable");
        let (_p, dir) = project::create(&parent, "Song", 48_000, 120.0).unwrap();

        let m = parking_lot::Mutex::new(new_session());
        let row = PluginInstanceInfo {
            id: "p-1".into(),
            uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "active".into(),
            track_id: None,
        };
        m.lock().plugins.instances.push(row);

        let first = encode_state("lv2:urn:test:synth", &StateBlob { kind: KIND_OPAQUE, data: vec![1; 4] });
        crate::control::Session::transact(&m, crate::control::op::TxMeta::user("patch 1"), |tx| {
            tx.apply(crate::control::op::Op::PluginSetState { instance: "p-1".into(), state: first.clone() })
        })
        .unwrap();
        let doc1 = m.lock().plugin_snapshot();
        save_snapshot_into_project(&dir, &doc1, false).unwrap();
        let file = state_dir(&dir).join("p-1.state");
        assert_eq!(fs::read(&file).unwrap(), first, "first patch landed on disk");

        let second = encode_state("lv2:urn:test:synth", &StateBlob { kind: KIND_OPAQUE, data: vec![2; 4] });
        crate::control::Session::transact(&m, crate::control::op::TxMeta::user("patch 2"), |tx| {
            tx.apply(crate::control::op::Op::PluginSetState { instance: "p-1".into(), state: second.clone() })
        })
        .unwrap();
        let doc2 = m.lock().plugin_snapshot();
        save_snapshot_into_project(&dir, &doc2, false).unwrap();
        assert_eq!(
            fs::read(&file).unwrap(),
            second,
            "SECOND patch also landed on disk (Critical-2 regression — used to stay `first`)"
        );
        let _ = fs::remove_dir_all(&parent);
    }
}
