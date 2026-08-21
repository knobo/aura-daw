//! Machine-global plugin catalog: persistent scan cache + user curation
//! (favorites / recents / tags / pinned params). ARCHITECTURE §15.4.
//!
//! One file at `dirs::config_dir()/aura/plugin-catalog.json` — NOT per
//! project, the same per-machine config carve-out `theme.rs` uses for user
//! themes. Written atomically (temp file in the same directory, then
//! `rename` — same-filesystem renames are atomic on every platform we ship,
//! so a crash between write and rename leaves either the OLD file intact or
//! nothing, never a truncated one) and schema-versioned. Loading is NEVER
//! FATAL: a missing, unreadable, malformed, or wrong-version file logs and
//! yields an empty catalog — the same "AURA must always start" contract
//! `theme.rs::scan_dir` follows for user themes.
//!
//! The reducers below (`apply_patch`, `note_used`, `plan_rescan`) are pure —
//! plain values in, plain values out, no filesystem — so the cap/de-dupe
//! rules and the incremental-rescan classification are unit-testable
//! without a temp directory.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::descriptor::PluginDescriptor;

/// Bump when the on-disk shape changes incompatibly; [`load_from`] refuses
/// (empties) anything else rather than guess at a migration.
pub const CURRENT_VERSION: u32 = 1;
/// Most-recently-used list length (spec 4.1).
pub const RECENTS_CAP: usize = 40;
/// Pinned-parameter list length, per plugin uid (spec 4.1).
pub const PINNED_PARAMS_CAP: usize = 8;

/// One cached scan result: a descriptor plus the bundle stat it was read
/// from, so a later scan can tell "unchanged" from "needs a re-read"
/// without dlopening anything. LV2 descriptors (no bundle FILE — `lilv`
/// reads a Turtle world, not a single stat-able path) carry `path: ""` and
/// `mtime/size: 0`; they are never matched by [`plan_rescan`], which only
/// classifies CLAP entries — LV2 rescanning is cheap and safe in-process
/// (see `scan.rs`), so it simply always re-runs and always overwrites its
/// slice of the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub descriptor: PluginDescriptor,
    pub path: String,
    pub mtime: u64,
    pub size: u64,
}

// `PluginDescriptor` (owned by a different zone — `descriptor.rs`) does not
// derive `PartialEq`, so the ordinary `#[derive(PartialEq)]` is unavailable
// here. A manual, field-by-field impl instead — every one of its fields is
// itself `PartialEq` — is enough for the round-trip/cap tests below without
// adding a dependency on that file's derive list.
impl PartialEq for CatalogEntry {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.mtime == other.mtime
            && self.size == other.size
            && self.descriptor.uid == other.descriptor.uid
            && self.descriptor.format == other.descriptor.format
            && self.descriptor.name == other.descriptor.name
            && self.descriptor.vendor == other.descriptor.vendor
            && self.descriptor.version == other.descriptor.version
            && self.descriptor.path == other.descriptor.path
            && self.descriptor.is_instrument == other.descriptor.is_instrument
            && self.descriptor.audio_inputs == other.descriptor.audio_inputs
            && self.descriptor.audio_outputs == other.descriptor.audio_outputs
            && self.descriptor.has_note_input == other.descriptor.has_note_input
            && self.descriptor.categories == other.descriptor.categories
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanCache {
    /// Unix seconds of the last successful scan; `None` before AURA has
    /// ever scanned on this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scanned_at: Option<u64>,
    #[serde(default)]
    pub entries: Vec<CatalogEntry>,
}

/// One entry in the recents list — spec 4.1: `{ "uid": "…", "usedAt": … }`,
/// capped at [`RECENTS_CAP`] and de-duplicated by uid (a re-use moves the
/// existing entry to the front rather than adding a second one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRecent {
    pub uid: String,
    pub used_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalog {
    pub version: u32,
    #[serde(default)]
    pub scan: ScanCache,
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default)]
    pub recents: Vec<PluginRecent>,
    /// uid -> free-text tags.
    #[serde(default)]
    pub tags: BTreeMap<String, Vec<String>>,
    /// uid -> pinned param ids, capped at [`PINNED_PARAMS_CAP`].
    #[serde(default)]
    pub pinned_params: BTreeMap<String, Vec<u32>>,
}

impl Default for PluginCatalog {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            scan: ScanCache::default(),
            favorites: Vec::new(),
            recents: Vec::new(),
            tags: BTreeMap::new(),
            pinned_params: BTreeMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Patch (additive command D: plugin_catalog_update)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFavorite {
    pub uid: String,
    pub on: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTags {
    pub uid: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPinnedParams {
    pub uid: String,
    pub param_ids: Vec<u32>,
}

/// All-optional so the command surface stays at ONE additive entry instead
/// of growing a command per field (spec 4.1's stated reason). Emitter
/// contract, D-06: no `additionalProperties:false`, readers ignore unknown
/// fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCatalogPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_favorite: Option<SetFavorite>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_used: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_tags: Option<SetTags>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_pinned_params: Option<SetPinnedParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forget_recent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_recents: Option<bool>,
}

/// Move `uid` to the front of `recents` with a fresh `usedAt`, de-duped and
/// capped at [`RECENTS_CAP`]. The cap and de-dupe live HERE — not at call
/// sites — so `plugin_catalog_update` and any future caller share the same
/// guarantee for free (spec 4.1: "caps enforced in the reducer").
pub fn note_used(mut recents: Vec<PluginRecent>, uid: &str, now: u64) -> Vec<PluginRecent> {
    recents.retain(|r| r.uid != uid);
    recents.insert(0, PluginRecent { uid: uid.to_string(), used_at: now });
    recents.truncate(RECENTS_CAP);
    recents
}

/// De-dupe (first occurrence wins — the caller's own priority order) and
/// cap pinned param ids at [`PINNED_PARAMS_CAP`].
fn cap_pinned(ids: Vec<u32>) -> Vec<u32> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        if out.len() == PINNED_PARAMS_CAP {
            break;
        }
        if seen.insert(id) {
            out.push(id);
        }
    }
    out
}

/// Apply one patch to a catalog value, pure (no I/O — the command handler
/// owns loading/saving). `now` is injected rather than read from the clock
/// in here so tests can assert exact `usedAt` values.
pub fn apply_patch(mut cat: PluginCatalog, patch: &PluginCatalogPatch, now: u64) -> PluginCatalog {
    if let Some(sf) = &patch.set_favorite {
        if sf.on {
            if !cat.favorites.iter().any(|u| u == &sf.uid) {
                cat.favorites.push(sf.uid.clone());
            }
        } else {
            cat.favorites.retain(|u| u != &sf.uid);
        }
    }
    if let Some(uid) = &patch.note_used {
        cat.recents = note_used(cat.recents, uid, now);
    }
    if let Some(st) = &patch.set_tags {
        if st.tags.is_empty() {
            cat.tags.remove(&st.uid);
        } else {
            cat.tags.insert(st.uid.clone(), st.tags.clone());
        }
    }
    if let Some(sp) = &patch.set_pinned_params {
        let capped = cap_pinned(sp.param_ids.clone());
        if capped.is_empty() {
            cat.pinned_params.remove(&sp.uid);
        } else {
            cat.pinned_params.insert(sp.uid.clone(), capped);
        }
    }
    if let Some(uid) = &patch.forget_recent {
        cat.recents.retain(|r| &r.uid != uid);
    }
    if patch.clear_recents == Some(true) {
        cat.recents.clear();
    }
    cat
}

// ---------------------------------------------------------------------------
// Incremental rescan planning (command C: plugin_scan stays incremental)
// ---------------------------------------------------------------------------

/// One filesystem probe of a discovered CLAP bundle — the identity
/// [`plan_rescan`] compares against the cache. Pure input so the planner is
/// testable without touching a filesystem; `scan.rs::stat_bundle` is the
/// real probe.
#[derive(Debug, Clone, PartialEq)]
pub struct BundleStat {
    pub path: String,
    pub mtime: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RescanPlan {
    /// Cached entries whose bundle stat is UNCHANGED — reused verbatim,
    /// never handed to the (dlopen-risking) scan worker.
    pub reused: Vec<CatalogEntry>,
    /// Bundle paths that are new or changed — these alone go to the worker.
    pub to_scan: Vec<String>,
    /// Cached entries whose bundle no longer exists on disk at all —
    /// dropped from the catalog (neither reused nor rescanned).
    pub missing: usize,
}

/// Classify each discovered CLAP bundle against the cached CLAP catalog
/// entries by `(path, mtime, size)`. A bundle with no matching cached entry
/// (new, or an existing one whose mtime/size moved) goes to `to_scan`. A
/// cached entry whose path is absent from `discovered` altogether (the
/// bundle was removed from disk since the last scan) is counted in
/// `missing` and dropped — silently, since "the plugin is gone" is not an
/// error.
pub fn plan_rescan(cached: &[CatalogEntry], discovered: &[BundleStat]) -> RescanPlan {
    let discovered_paths: HashSet<&str> = discovered.iter().map(|s| s.path.as_str()).collect();
    let missing = cached
        .iter()
        .filter(|e| !discovered_paths.contains(e.path.as_str()))
        .count();

    let mut plan = RescanPlan { missing, ..Default::default() };
    for stat in discovered {
        let matches: Vec<&CatalogEntry> = cached
            .iter()
            .filter(|e| e.path == stat.path && e.mtime == stat.mtime && e.size == stat.size)
            .collect();
        if matches.is_empty() {
            plan.to_scan.push(stat.path.clone());
        } else {
            plan.reused.extend(matches.into_iter().cloned());
        }
    }
    plan
}

// ---------------------------------------------------------------------------
// Additive command E: plugin_scan_status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginScanStatus {
    pub scanned_at: Option<u64>,
    /// Descriptors the cache can still vouch for (bundle stat unchanged).
    pub cached: usize,
    /// Bundle PATHS that are new or changed since the cached scan.
    pub stale: usize,
    /// Cached entries whose bundle no longer exists on disk.
    pub missing: usize,
}

// ---------------------------------------------------------------------------
// Disk I/O
// ---------------------------------------------------------------------------

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn catalog_path() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|d| d.join("aura").join("plugin-catalog.json"))
        .ok_or_else(|| "no config directory on this platform".to_string())
}

/// Load from an explicit path (the file-system-free half is exercised via
/// [`load_from`] directly in tests; [`load`] is the production entry
/// point). Never fails: see the module doc's "never fatal" contract.
pub fn load_from(path: &Path) -> PluginCatalog {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return PluginCatalog::default(),
        Err(e) => {
            log::warn!("plugin catalog: unreadable {}: {e}; starting empty", path.display());
            return PluginCatalog::default();
        }
    };
    match serde_json::from_str::<PluginCatalog>(&raw) {
        Ok(cat) if cat.version == CURRENT_VERSION => cat,
        Ok(cat) => {
            log::warn!(
                "plugin catalog: {} is version {}, expected {CURRENT_VERSION}; starting empty",
                path.display(),
                cat.version
            );
            PluginCatalog::default()
        }
        Err(e) => {
            log::warn!("plugin catalog: malformed {}: {e}; starting empty", path.display());
            PluginCatalog::default()
        }
    }
}

pub fn load() -> PluginCatalog {
    match catalog_path() {
        Ok(p) => load_from(&p),
        Err(e) => {
            log::warn!("plugin catalog: {e}; starting empty");
            PluginCatalog::default()
        }
    }
}

/// Atomic write: serialize, write to a temp file in the SAME directory (so
/// the rename below is same-filesystem and therefore atomic), then rename
/// over `path`. A crash between the two steps leaves either the old file
/// (untouched) or nothing at all — never a truncated catalog.
pub fn save_to(path: &Path, cat: &PluginCatalog) -> Result<(), String> {
    let dir = path.parent().ok_or("catalog path has no parent directory")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(cat).map_err(|e| e.to_string())?;
    let file_name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("plugin-catalog.json");
    let tmp = dir.join(format!(".{file_name}.tmp-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
    fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn save(cat: &PluginCatalog) -> Result<(), String> {
    save_to(&catalog_path()?, cat)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-plugin-catalog-test-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn desc(uid: &str) -> PluginDescriptor {
        PluginDescriptor {
            uid: uid.to_string(),
            format: "clap".into(),
            name: uid.to_string(),
            vendor: None,
            version: None,
            path: Some(format!("/x/{uid}.clap")),
            is_instrument: false,
            audio_inputs: 2,
            audio_outputs: 2,
            has_note_input: false,
            categories: vec![],
        }
    }

    fn entry(path: &str, mtime: u64, size: u64) -> CatalogEntry {
        CatalogEntry { descriptor: desc(path), path: path.to_string(), mtime, size }
    }

    // ── round-trip / never-fatal ────────────────────────────────────────

    #[test]
    fn round_trips_through_disk() {
        let d = tmpdir("roundtrip");
        let path = d.join("plugin-catalog.json");
        let mut cat = PluginCatalog::default();
        cat.favorites.push("clap:/x#a".into());
        cat.scan.scanned_at = Some(1_755_648_000);
        cat.scan.entries.push(entry("/x/a.clap", 111, 222));
        save_to(&path, &cat).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded, cat);
    }

    #[test]
    fn missing_file_yields_default_catalog_not_an_error() {
        let d = tmpdir("missing").join("nope").join("plugin-catalog.json");
        let cat = load_from(&d);
        assert_eq!(cat, PluginCatalog::default());
        assert_eq!(cat.version, CURRENT_VERSION);
    }

    #[test]
    fn malformed_json_yields_default_catalog() {
        let d = tmpdir("malformed");
        let path = d.join("plugin-catalog.json");
        fs::write(&path, "{ not json at all").unwrap();
        assert_eq!(load_from(&path), PluginCatalog::default());
    }

    #[test]
    fn wrong_version_yields_default_catalog() {
        let d = tmpdir("wrong-version");
        let path = d.join("plugin-catalog.json");
        fs::write(&path, r#"{"version":999,"favorites":["x"]}"#).unwrap();
        assert_eq!(load_from(&path), PluginCatalog::default());
    }

    #[test]
    fn save_is_atomic_no_leftover_temp_file_and_content_lands_whole() {
        let d = tmpdir("atomic");
        let path = d.join("plugin-catalog.json");
        let mut cat = PluginCatalog::default();
        cat.favorites.push("clap:/x#a".into());
        save_to(&path, &cat).unwrap();
        // No stray temp files left behind after a successful save.
        let leftovers: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
        assert_eq!(load_from(&path), cat);
    }

    // ── favorites / recents / tags / pinned reducers ────────────────────

    #[test]
    fn set_favorite_toggles_on_and_off_without_duplicates() {
        let cat = PluginCatalog::default();
        let patch_on = PluginCatalogPatch {
            set_favorite: Some(SetFavorite { uid: "u1".into(), on: true }),
            ..Default::default()
        };
        let cat = apply_patch(cat, &patch_on, 0);
        let cat = apply_patch(cat, &patch_on, 0); // idempotent
        assert_eq!(cat.favorites, vec!["u1".to_string()]);

        let patch_off = PluginCatalogPatch {
            set_favorite: Some(SetFavorite { uid: "u1".into(), on: false }),
            ..Default::default()
        };
        let cat = apply_patch(cat, &patch_off, 0);
        assert!(cat.favorites.is_empty());
    }

    #[test]
    fn note_used_dedupes_and_moves_to_front() {
        let recents = note_used(Vec::new(), "a", 1);
        let recents = note_used(recents, "b", 2);
        let recents = note_used(recents, "a", 3);
        let uids: Vec<&str> = recents.iter().map(|r| r.uid.as_str()).collect();
        assert_eq!(uids, vec!["a", "b"], "re-used uid moved to front, not duplicated");
        assert_eq!(recents[0].used_at, 3);
    }

    #[test]
    fn note_used_caps_at_40() {
        let mut recents = Vec::new();
        for i in 0..50u64 {
            recents = note_used(recents, &format!("u{i}"), i);
        }
        assert_eq!(recents.len(), RECENTS_CAP);
        // Most-recent-first: the last 40 noted (u10..u49), newest at front.
        assert_eq!(recents[0].uid, "u49");
        assert_eq!(recents[RECENTS_CAP - 1].uid, "u10");
    }

    #[test]
    fn forget_recent_and_clear_recents() {
        let mut recents = Vec::new();
        for i in 0..3u64 {
            recents = note_used(recents, &format!("u{i}"), i);
        }
        let mut cat = PluginCatalog { recents, ..PluginCatalog::default() };
        cat = apply_patch(cat, &PluginCatalogPatch { forget_recent: Some("u1".into()), ..Default::default() }, 0);
        assert!(!cat.recents.iter().any(|r| r.uid == "u1"));
        assert_eq!(cat.recents.len(), 2);

        cat = apply_patch(cat, &PluginCatalogPatch { clear_recents: Some(true), ..Default::default() }, 0);
        assert!(cat.recents.is_empty());
    }

    #[test]
    fn set_tags_stores_and_empty_tags_clears_the_entry() {
        let cat = PluginCatalog::default();
        let cat = apply_patch(
            cat,
            &PluginCatalogPatch {
                set_tags: Some(SetTags { uid: "u1".into(), tags: vec!["Bass".into(), "Live rig".into()] }),
                ..Default::default()
            },
            0,
        );
        assert_eq!(cat.tags.get("u1").unwrap(), &vec!["Bass".to_string(), "Live rig".to_string()]);

        let cat = apply_patch(
            cat,
            &PluginCatalogPatch {
                set_tags: Some(SetTags { uid: "u1".into(), tags: vec![] }),
                ..Default::default()
            },
            0,
        );
        assert!(!cat.tags.contains_key("u1"));
    }

    #[test]
    fn pinned_params_cap_at_8_and_dedupe() {
        let cat = PluginCatalog::default();
        let ids: Vec<u32> = (0..20).chain(0..5).collect(); // dupes + overflow
        let cat = apply_patch(
            cat,
            &PluginCatalogPatch {
                set_pinned_params: Some(SetPinnedParams { uid: "u1".into(), param_ids: ids }),
                ..Default::default()
            },
            0,
        );
        let pinned = cat.pinned_params.get("u1").unwrap();
        assert_eq!(pinned.len(), PINNED_PARAMS_CAP);
        assert_eq!(pinned, &vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn set_pinned_params_with_empty_list_clears_the_entry() {
        let cat = PluginCatalog::default();
        let cat = apply_patch(
            cat,
            &PluginCatalogPatch {
                set_pinned_params: Some(SetPinnedParams { uid: "u1".into(), param_ids: vec![1, 2] }),
                ..Default::default()
            },
            0,
        );
        assert!(cat.pinned_params.contains_key("u1"));
        let cat = apply_patch(
            cat,
            &PluginCatalogPatch {
                set_pinned_params: Some(SetPinnedParams { uid: "u1".into(), param_ids: vec![] }),
                ..Default::default()
            },
            0,
        );
        assert!(!cat.pinned_params.contains_key("u1"));
    }

    // ── incremental rescan planning ──────────────────────────────────────

    #[test]
    fn plan_rescan_hit_unchanged_bundle_is_reused_not_rescanned() {
        let cached = vec![entry("/a.clap", 100, 200)];
        let discovered = vec![BundleStat { path: "/a.clap".into(), mtime: 100, size: 200 }];
        let plan = plan_rescan(&cached, &discovered);
        assert_eq!(plan.reused.len(), 1);
        assert!(plan.to_scan.is_empty());
        assert_eq!(plan.missing, 0);
    }

    #[test]
    fn plan_rescan_miss_changed_mtime_or_size_goes_to_scan() {
        let cached = vec![entry("/a.clap", 100, 200)];
        let discovered = vec![BundleStat { path: "/a.clap".into(), mtime: 101, size: 200 }];
        let plan = plan_rescan(&cached, &discovered);
        assert!(plan.reused.is_empty());
        assert_eq!(plan.to_scan, vec!["/a.clap".to_string()]);
        assert_eq!(plan.missing, 0);

        let cached = vec![entry("/a.clap", 100, 200)];
        let discovered = vec![BundleStat { path: "/a.clap".into(), mtime: 100, size: 999 }];
        let plan = plan_rescan(&cached, &discovered);
        assert_eq!(plan.to_scan, vec!["/a.clap".to_string()]);
    }

    #[test]
    fn plan_rescan_new_bundle_not_in_cache_goes_to_scan() {
        let cached: Vec<CatalogEntry> = vec![];
        let discovered = vec![BundleStat { path: "/new.clap".into(), mtime: 1, size: 2 }];
        let plan = plan_rescan(&cached, &discovered);
        assert_eq!(plan.to_scan, vec!["/new.clap".to_string()]);
        assert_eq!(plan.missing, 0);
    }

    #[test]
    fn plan_rescan_missing_bundle_is_dropped_not_scanned_or_reused() {
        let cached = vec![entry("/gone.clap", 1, 2)];
        let discovered: Vec<BundleStat> = vec![];
        let plan = plan_rescan(&cached, &discovered);
        assert!(plan.reused.is_empty());
        assert!(plan.to_scan.is_empty());
        assert_eq!(plan.missing, 1);
    }

    #[test]
    fn plan_rescan_multiple_descriptors_sharing_one_bundle_path_all_reused() {
        let cached = vec![
            CatalogEntry { descriptor: desc("Cardinal"), path: "/c.clap".into(), mtime: 5, size: 6 },
            CatalogEntry { descriptor: desc("CardinalFX"), path: "/c.clap".into(), mtime: 5, size: 6 },
        ];
        let discovered = vec![BundleStat { path: "/c.clap".into(), mtime: 5, size: 6 }];
        let plan = plan_rescan(&cached, &discovered);
        assert_eq!(plan.reused.len(), 2, "both descriptors from the one unchanged bundle are reused");
        assert!(plan.to_scan.is_empty());
    }
}
