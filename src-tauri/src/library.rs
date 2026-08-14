//! Library & browser backend (Track E) — directory listing, the default
//! library root, and the audition decode.
//!
//! Deliberately tauri-free at its core (`scan_dir`, `default_library_root`,
//! `compile_audition`) with thin `#[tauri::command]` wrappers, the same shape
//! `control/import.rs` uses, so every rule below unit-tests without a webview.
//!
//! SCAN MODEL (plan ruling 1): ONE directory level per call, expanded lazily
//! by the panel. No recursion, no background scanner, no events. A folder of
//! 5000 samples is one `read_dir` sweep plus a sort.
//!
//! METADATA (plan ruling 2): filesystem metadata only — never a decode probe.
//! Duration/channels/rate cost a full decode in this crate, so they surface
//! where the user asks for the file (audition, import), not on every scan.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

/// Extensions the panel offers. Pinned to `control::import::decode_audio`'s
/// accepted set by `audio_exts_match_the_import_decoder_exactly` — every row
/// the user can see must be a row they can drop onto a track.
pub const AUDIO_EXTS: [&str; 7] = ["wav", "mp3", "flac", "ogg", "aac", "m4a", "mp4"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LibraryEntryKind {
    Dir,
    Audio,
}

/// One row of a library listing. Filesystem metadata only (ruling 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntry {
    /// File or directory name (no path).
    pub name: String,
    /// Absolute path.
    pub path: String,
    pub kind: LibraryEntryKind,
    /// Lowercased extension without the dot; empty for directories.
    pub ext: String,
    pub size_bytes: u64,
    /// Modification time, milliseconds since the Unix epoch (0 when unknown).
    pub modified_ms: u64,
}

/// List ONE directory level: sub-directories and supported audio files.
/// Hidden entries (leading `.`) and unsupported files are skipped. Sorted
/// directories-first, then case-insensitively by name.
pub fn scan_dir(dir: &Path) -> Result<Vec<LibraryEntry>, String> {
    if !dir.is_absolute() {
        return Err(format!("library path must be absolute: {}", dir.display()));
    }
    let read = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;

    let mut out = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // `metadata()` follows symlinks on purpose: a symlinked sample
        // folder is a normal way to organise a library.
        let Ok(meta) = entry.metadata() else { continue };
        let path = entry.path();
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        if meta.is_dir() {
            out.push(LibraryEntry {
                name,
                path: path.display().to_string(),
                kind: LibraryEntryKind::Dir,
                ext: String::new(),
                size_bytes: 0,
                modified_ms,
            });
            continue;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !AUDIO_EXTS.contains(&ext.as_str()) {
            continue;
        }
        out.push(LibraryEntry {
            name,
            path: path.display().to_string(),
            kind: LibraryEntryKind::Audio,
            ext,
            size_bytes: meta.len(),
            modified_ms,
        });
    }

    out.sort_by(|a, b| {
        let kind_rank = |k: &LibraryEntryKind| match k {
            LibraryEntryKind::Dir => 0,
            LibraryEntryKind::Audio => 1,
        };
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// The library dir AURA ships material into and always shows first:
/// `<audio dir>/AURA/Library`, created on demand. Mirrors
/// `audio::project::default_project_parent`'s convention one level deeper.
pub fn default_library_root() -> Result<PathBuf, String> {
    let dir = dirs::audio_dir()
        .or_else(dirs::home_dir)
        .ok_or("cannot determine a directory for the default library")?
        .join("AURA")
        .join("Library");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// List one directory of the library (additive command).
///
/// `async` + `spawn_blocking` on purpose: sync commands run on the MAIN
/// thread, and on Linux WebKitGTK shares the GTK main loop — a `read_dir` on
/// a slow network mount would freeze the UI (the same rationale
/// `seed_demo_project` documents).
#[tauri::command]
pub async fn library_scan(dir: String) -> Result<Vec<LibraryEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || scan_dir(Path::new(&dir)))
        .await
        .map_err(|e| e.to_string())?
}

/// Absolute path of the default library dir, created if missing (additive).
#[tauri::command]
pub fn library_default_root() -> Result<String, String> {
    Ok(default_library_root()?.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// House idiom (see `audio/sampler_engine.rs`'s own `temp_dir`): a
    /// process- and uuid-tagged dir under the system temp root, no dev-dep.
    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-library-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"not really audio").unwrap();
    }

    #[test]
    fn scan_dir_lists_dirs_before_audio_files_each_case_insensitively_sorted() {
        let d = temp_dir("sort");
        std::fs::create_dir_all(d.join("Zebra")).unwrap();
        std::fs::create_dir_all(d.join("alpha")).unwrap();
        touch(&d, "Snare.wav");
        touch(&d, "kick.WAV");

        let got = scan_dir(&d).unwrap();
        let names: Vec<&str> = got.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "Zebra", "kick.WAV", "Snare.wav"]);
        assert_eq!(got[0].kind, LibraryEntryKind::Dir);
        assert_eq!(got[2].kind, LibraryEntryKind::Audio);
        assert_eq!(got[2].ext, "wav", "ext is lowercased");
        assert_eq!(got[0].ext, "", "directories carry no extension");
        assert!(got[2].path.ends_with("kick.WAV"), "path is absolute and exact");
        assert_eq!(got[2].size_bytes, 16);
    }

    #[test]
    fn scan_dir_skips_hidden_entries_and_unsupported_files() {
        let d = temp_dir("filter");
        touch(&d, "loop.flac");
        touch(&d, "notes.txt");
        touch(&d, "song.aiff"); // decode_audio rejects it — do not offer it
        touch(&d, ".hidden.wav");
        std::fs::create_dir_all(d.join(".git")).unwrap();

        let names: Vec<String> = scan_dir(&d).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, vec!["loop.flac".to_string()]);
    }

    #[test]
    fn scan_dir_rejects_relative_paths_and_reports_unreadable_dirs() {
        let rel = scan_dir(std::path::Path::new("samples")).unwrap_err();
        assert!(rel.contains("absolute"), "relative path is rejected: {rel}");

        let missing = temp_dir("missing").join("nope");
        let err = scan_dir(&missing).unwrap_err();
        assert!(err.contains("nope"), "the error names the directory: {err}");
    }

    #[test]
    fn audio_exts_match_the_import_decoder_exactly() {
        // Anti-drift: every row the panel offers must be importable. If
        // `decode_audio` grows a format, this test fails until AUDIO_EXTS
        // follows.
        let mut expected = vec!["wav".to_string()];
        expected.extend(crate::control::import::SYMPHONIA_EXTS.iter().map(|s| s.to_string()));
        let mut actual: Vec<String> = AUDIO_EXTS.iter().map(|s| s.to_string()).collect();
        expected.sort();
        actual.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn library_entry_serializes_camel_case_with_lowercase_kind() {
        let e = LibraryEntry {
            name: "kick.wav".into(),
            path: "/tmp/kick.wav".into(),
            kind: LibraryEntryKind::Audio,
            ext: "wav".into(),
            size_bytes: 42,
            modified_ms: 7,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "audio");
        assert_eq!(v["sizeBytes"], 42);
        assert_eq!(v["modifiedMs"], 7);
        assert!(v.get("size_bytes").is_none(), "camelCase only on the wire");
    }

    #[test]
    fn default_library_root_is_the_aura_library_dir_and_exists_after_the_call() {
        let root = default_library_root().unwrap();
        assert!(root.is_dir(), "created on demand");
        assert_eq!(root.file_name().unwrap(), "Library");
        assert_eq!(root.parent().unwrap().file_name().unwrap(), "AURA");
    }
}
