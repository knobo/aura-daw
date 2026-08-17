//! User themes on disk.
//!
//! Files live at `dirs::config_dir()/aura/themes/*.json`, the same
//! per-machine config carve-out `midi_out/persist.rs` uses. This module
//! does NOT understand what a theme is: it hands the frontend raw JSON
//! text and lets `src/lib/theme/parse.ts` be the single validator. The
//! frontend has to validate untrusted disk state anyway (the contract
//! `utils/prefs.ts` states), and a second rule set in Rust would drift.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserThemeFile {
    /// Filename stem — `my-theme.json` → `my-theme`. The theme's id.
    pub id: String,
    /// Unparsed file contents.
    pub raw: String,
}

/// One directory level, `*.json` only, sorted by id. A missing or unreadable
/// directory yields an empty list — "no user themes", never an error.
pub fn scan_dir(dir: &Path) -> Vec<UserThemeFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<UserThemeFile> = entries
        .flatten()
        .filter(|e| e.path().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_stem()?.to_str()?.to_string();
            match fs::read_to_string(&path) {
                Ok(raw) => Some(UserThemeFile { id, raw }),
                Err(err) => {
                    eprintln!("theme: skipping {}: {err}", path.display());
                    None
                }
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// A filename-safe id: lowercase, `[a-z0-9-]`, runs of anything else
/// collapsed to a single dash, no leading or trailing dash. `None` when
/// nothing survives — which is also what makes traversal impossible, since
/// `.` and `/` are not in the surviving alphabet.
pub fn sanitize_id(id: &str) -> Option<String> {
    let mut out = String::new();
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Write `<dir>/<sanitized id>.json`, creating the directory.
pub fn write_theme_to(dir: &Path, id: &str, json: &str) -> Result<PathBuf, String> {
    let safe = sanitize_id(id).ok_or_else(|| format!("\"{id}\" has no usable characters"))?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{safe}.json"));
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

fn themes_dir() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|d| d.join("aura").join("themes"))
        .ok_or_else(|| "no config directory on this platform".to_string())
}

#[tauri::command]
pub fn list_user_themes() -> Result<Vec<UserThemeFile>, String> {
    Ok(scan_dir(&themes_dir()?))
}

#[tauri::command]
pub fn write_user_theme(id: String, json: String) -> Result<String, String> {
    Ok(write_theme_to(&themes_dir()?, &id, &json)?.display().to_string())
}

#[tauri::command]
pub fn user_themes_dir() -> Result<String, String> {
    Ok(themes_dir()?.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aura-theme-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scan_returns_files_by_stem_with_contents_unparsed() {
        let d = tmpdir("scan");
        fs::write(d.join("sunny.json"), r#"{"name":"Sunny"}"#).unwrap();
        let found = scan_dir(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "sunny");
        assert_eq!(found[0].raw, r#"{"name":"Sunny"}"#);
    }

    #[test]
    fn scan_returns_malformed_json_untouched_because_the_frontend_judges_it() {
        let d = tmpdir("malformed");
        fs::write(d.join("broken.json"), "{not json").unwrap();
        let found = scan_dir(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].raw, "{not json");
    }

    #[test]
    fn scan_ignores_non_json_and_subdirectories() {
        let d = tmpdir("filter");
        fs::write(d.join("notes.txt"), "hello").unwrap();
        fs::write(d.join("theme.json"), "{}").unwrap();
        fs::create_dir_all(d.join("nested")).unwrap();
        fs::write(d.join("nested/deep.json"), "{}").unwrap();
        let found = scan_dir(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "theme");
    }

    #[test]
    fn scan_sorts_by_id_so_the_picker_order_is_stable() {
        let d = tmpdir("sorted");
        fs::write(d.join("zulu.json"), "{}").unwrap();
        fs::write(d.join("alpha.json"), "{}").unwrap();
        let ids: Vec<String> = scan_dir(&d).into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec!["alpha", "zulu"]);
    }

    #[test]
    fn scan_of_a_missing_directory_is_empty_not_an_error() {
        let d = tmpdir("missing").join("nope");
        assert!(scan_dir(&d).is_empty());
    }

    #[test]
    fn sanitize_id_lowercases_and_keeps_only_safe_characters() {
        assert_eq!(sanitize_id("My Theme"), Some("my-theme".to_string()));
        assert_eq!(sanitize_id("Solarized_Dark!"), Some("solarized-dark".to_string()));
    }

    #[test]
    fn sanitize_id_refuses_path_traversal_and_empty_names() {
        assert_eq!(sanitize_id("../../etc/passwd"), Some("etc-passwd".to_string()));
        assert_eq!(sanitize_id(""), None);
        assert_eq!(sanitize_id("///"), None);
    }

    #[test]
    fn write_creates_the_directory_and_lands_inside_it() {
        let d = tmpdir("write").join("themes");
        let path = write_theme_to(&d, "My Theme", r#"{"name":"My Theme"}"#).unwrap();
        assert_eq!(path, d.join("my-theme.json"));
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"name":"My Theme"}"#);
    }

    #[test]
    fn write_refuses_an_id_that_sanitizes_to_nothing() {
        let d = tmpdir("write-bad");
        assert!(write_theme_to(&d, "///", "{}").is_err());
    }
}
