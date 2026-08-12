//! Plugin discovery: CLAP search paths + LV2 world (phase 3).
//!
//! CRASH-SAFETY STATUS (zone P1, landed): the CLAP descriptor read
//! `dlopen`s bundles, so [`scan_all`] routes it through the sacrificial
//! scan subprocess (`scan_worker` — `AURA_SCAN_WORKER=1` re-exec, NDJSON
//! over stdout); a crashing bundle kills only the worker, is skipped, and
//! the scan resumes. The in-process functions below are the WORKER body
//! (and the last-resort fallback when no worker can be spawned). LV2
//! discovery is safe in-process (lilv reads Turtle metadata; plugin
//! binaries are NOT loaded until instantiation).

use std::path::{Path, PathBuf};

use super::descriptor::{clap_uid, lv2_uid, PluginDescriptor};

// ---------------------------------------------------------------------------
// Search paths
// ---------------------------------------------------------------------------

/// Standard CLAP search paths (Linux, per clap entry.h) plus `$CLAP_PATH`
/// (colon-separated). Scanned recursively for `*.clap` files.
pub fn clap_search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".clap"));
    }
    paths.push(PathBuf::from("/usr/lib/clap"));
    paths.push(PathBuf::from("/usr/local/lib/clap"));
    if let Ok(env) = std::env::var("CLAP_PATH") {
        paths.extend(env.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    paths.retain(|p| p.is_dir());
    paths.dedup();
    paths
}

/// All `*.clap` bundle files under the given roots.
pub fn find_clap_bundles(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        for entry in walkdir::WalkDir::new(root)
            .follow_links(true)
            .max_depth(6)
            .into_iter()
            .filter_map(Result::ok)
        {
            let p = entry.path();
            if p.is_file() && p.extension().is_some_and(|e| e == "clap") {
                out.push(p.to_path_buf());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// ---------------------------------------------------------------------------
// CLAP descriptor read (clack-host)
// ---------------------------------------------------------------------------

/// Read the plugin descriptors of ONE .clap bundle. In-process `dlopen` —
/// see the module-level crash-safety note.
pub fn scan_clap_bundle(bundle: &Path) -> Result<Vec<PluginDescriptor>, String> {
    let entry = unsafe { clack_host::prelude::PluginEntry::load(bundle) }
        .map_err(|e| format!("{}: {e}", bundle.display()))?;
    let factory = entry
        .get_plugin_factory()
        .ok_or_else(|| format!("{}: no plugin factory", bundle.display()))?;
    let path_str = bundle.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for d in factory.plugin_descriptors() {
        let Some(id) = d.id() else { continue };
        let features: Vec<String> = d
            .features()
            .map(|f| f.to_string_lossy().into_owned())
            .collect();
        let is_instrument = features.iter().any(|f| f == "instrument");
        out.push(PluginDescriptor {
            uid: clap_uid(&path_str, &id.to_string_lossy()),
            format: "clap".into(),
            name: d
                .name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.to_string_lossy().into_owned()),
            vendor: d.vendor().map(|v| v.to_string_lossy().into_owned()),
            version: d.version().map(|v| v.to_string_lossy().into_owned()),
            path: Some(path_str.clone()),
            is_instrument,
            // CLAP port counts are only known after instantiation (audio-ports
            // extension); descriptor-level scan reports 0 — zone P1 refines.
            audio_inputs: 0,
            audio_outputs: 0,
            has_note_input: is_instrument,
            categories: features,
        });
    }
    Ok(out)
}

/// Scan every CLAP bundle under the standard paths. Per-bundle errors are
/// collected as log warnings, never abort the scan. In-process — this is
/// the WORKER body (`scan_worker::worker_main` runs it bundle by bundle);
/// AURA itself scans through `scan_worker::scan_clap_subprocess`.
pub fn scan_clap_paths(roots: &[PathBuf]) -> Vec<PluginDescriptor> {
    scan_clap_bundles_in_process(&find_clap_bundles(roots))
}

/// In-process scan of an explicit bundle list (worker body / last-resort
/// fallback when no worker can be spawned).
pub fn scan_clap_bundles_in_process(bundles: &[PathBuf]) -> Vec<PluginDescriptor> {
    let mut out = Vec::new();
    for bundle in bundles {
        match scan_clap_bundle(bundle) {
            Ok(mut d) => out.append(&mut d),
            Err(e) => log::warn!("plugin scan (clap): {e}"),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// LV2 discovery (livi/lilv — metadata only, no plugin code loaded)
// ---------------------------------------------------------------------------

/// Enumerate the LV2 world (default LV2_PATH: `~/.lv2:/usr/local/lib/lv2:`
/// `/usr/lib/lv2`). Only plugins livi can host are listed (livi filters by
/// its supported feature set — urid/options/bufsize/worker — which covers
/// ZynAddSubFX & friends).
pub fn scan_lv2() -> Vec<PluginDescriptor> {
    let world = livi::World::new();
    world
        .iter_plugins()
        .map(|p| {
            let counts = *p.port_counts();
            PluginDescriptor {
                uid: lv2_uid(&p.uri()),
                format: "lv2".into(),
                name: p.name(),
                vendor: None,
                version: None,
                path: None,
                is_instrument: p.is_instrument(),
                audio_inputs: counts.audio_inputs as u32,
                audio_outputs: counts.audio_outputs as u32,
                has_note_input: counts.atom_sequence_inputs > 0,
                categories: p.classes().map(|c| c.to_string()).collect(),
            }
        })
        .collect()
}

/// Full scan: LV2 world (metadata-only, safe in-process) + CLAP standard
/// paths through the sacrificial scan subprocess (crash containment —
/// PHASE3-PLAN contract 8).
pub fn scan_all() -> Vec<PluginDescriptor> {
    let mut out = scan_lv2();
    out.extend(super::scan_worker::scan_clap_subprocess(&clap_search_paths()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// LV2 discovery works against the machine's real LV2 world; skipped
    /// (with a note) when no plugins are installed.
    #[test]
    fn lv2_world_enumerates_installed_plugins() {
        let plugins = scan_lv2();
        if plugins.is_empty() {
            eprintln!("skipping: no LV2 plugins installed");
            return;
        }
        for p in &plugins {
            assert!(p.uid.starts_with("lv2:"), "uid prefixed: {}", p.uid);
            assert_eq!(p.format, "lv2");
            assert!(!p.name.is_empty());
        }
        // If ZynAddSubFX is installed it must be recognized as an instrument
        // with a note input and stereo out (the phase-3 acceptance target).
        if let Some(zyn) = plugins
            .iter()
            .find(|p| p.uid == "lv2:http://zynaddsubfx.sourceforge.net")
        {
            assert!(zyn.is_instrument, "Zyn is an instrument");
            assert!(zyn.has_note_input, "Zyn accepts MIDI");
            assert_eq!(zyn.audio_outputs, 2);
        } else {
            eprintln!("note: ZynAddSubFX not installed; instrument checks skipped");
        }
    }

    /// CLAP descriptor read against real bundles when present (Ubuntu:
    /// zam-plugins ships /usr/lib/clap/*.clap); skipped otherwise. Scans
    /// through the sacrificial worker (production path) so third-party
    /// bundles are never dlopened into the shared test process — the same
    /// descriptor code runs inside the child (see
    /// `scan_worker::test_worker_command` for why: Cardinal teardown).
    #[test]
    fn clap_bundles_scan_when_present() {
        let roots = clap_search_paths();
        let bundles = find_clap_bundles(&roots);
        if bundles.is_empty() {
            eprintln!("skipping: no CLAP bundles installed");
            return;
        }
        let plugins = crate::plugins::scan_worker::scan_with_worker(
            &bundles,
            &crate::plugins::scan_worker::test_worker_command(),
        );
        assert!(!plugins.is_empty(), "installed bundles yield descriptors");
        for p in &plugins {
            assert!(p.uid.starts_with("clap:"));
            assert!(p.uid.contains('#'), "uid carries the clap plugin id");
            assert_eq!(p.format, "clap");
            assert!(!p.name.is_empty());
        }
    }

    #[test]
    fn clap_search_paths_honor_env_and_existence() {
        // Non-existent dirs are filtered out.
        for p in clap_search_paths() {
            assert!(p.is_dir());
        }
    }
}
