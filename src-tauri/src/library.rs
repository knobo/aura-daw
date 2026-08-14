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
        let path = entry.path();
        // `std::fs::metadata` (unlike `DirEntry::metadata`, which has lstat
        // semantics and reports the *link's* own type/size) follows symlinks
        // on purpose: a symlinked sample folder is a normal way to organise
        // a library, and a symlinked audio file must report the target's
        // size.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
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

/// How much of a file an audition decodes and plays, seconds. Bounded so a
/// 20-minute stem does not decode into RAM on a click.
pub const AUDITION_MAX_SECS: f32 = 12.0;

/// Build a one-shot audition instrument from the head of an audio file.
///
/// RULING 8: the samples keep the FILE's rate — nothing is resampled.
/// `sampler_voice::pitch_ratio` folds `sample.rate / out_rate` in, so the
/// audition plays at the right pitch on any output stream. (The
/// `CompiledInstrument::rate` doc's "resampled at load time" describes the
/// SFZ loader, not this path.) Do not "fix" this by resampling.
///
/// Takes no `Session`, no `ControlPlane`, no `Committer` — the audition path
/// is document-decoupled by construction (pinned by
/// `library_audition_core_leaves_the_document_and_the_history_untouched`).
pub fn compile_audition(
    path: &Path,
    max_seconds: f32,
) -> Result<crate::audio::sampler_engine::CompiledInstrument, String> {
    use std::sync::Arc;

    use crate::audio::sampler_engine::CompiledInstrument;
    use crate::audio::sampler_voice::{CompiledRegion, SampleData};

    let (channels, rate, mut data) = crate::control::import::decode_audio(path)?;
    let ch = channels.max(1) as usize;
    let secs = if max_seconds.is_finite() { max_seconds.clamp(0.1, 60.0) } else { AUDITION_MAX_SECS };
    let max_frames = (rate as f64 * secs as f64) as usize;
    data.truncate(max_frames.saturating_mul(ch));
    if data.is_empty() {
        return Err(format!("{} decoded to no audio", path.display()));
    }

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let sample = Arc::new(SampleData { channels, rate, data });

    Ok(CompiledInstrument {
        // The path IS the identity: the preview thread reinstalls the node
        // only when the id changes, so re-auditioning the same file reuses
        // the loaded node instead of churning an RCU swap.
        id: format!("library-audition:{}", path.display()),
        name,
        rate,
        regions: Arc::new(vec![CompiledRegion {
            lokey: 0,
            hikey: 127,
            lovel: 1,
            hivel: 127,
            pitch_keycenter: 60,
            tune: 0,
            transpose: 0,
            gain_l: 1.0,
            gain_r: 1.0,
            offset: 0,
            looped: false,
            loop_start: 0,
            loop_end: 0,
            // A short attack de-clicks the head; a short release de-clicks
            // the cancel. Neither shapes the sound.
            ampeg_attack: 0.002,
            ampeg_release: 0.05,
            sample,
        }]),
    })
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

    /// `DirEntry::metadata()` has lstat semantics — it reports the *link's*
    /// own type and size, not the target's. A symlinked sample folder is a
    /// normal way to organise a library (the module doc's stated intent),
    /// so `scan_dir` must resolve the link, not just see it exists.
    #[cfg(unix)]
    #[test]
    fn scan_dir_follows_symlinks_to_real_folders_and_files() {
        use std::os::unix::fs::symlink;

        // Real folder + real audio file, living OUTSIDE the scanned dir.
        let real_root = temp_dir("symlink-target");
        let real_subdir = real_root.join("RealSamples");
        std::fs::create_dir_all(&real_subdir).unwrap();
        let real_file = real_root.join("real.wav");
        std::fs::write(&real_file, b"0123456789").unwrap(); // 10 bytes, distinct from symlink's own size

        // The scanned dir contains only symlinks to the above.
        let d = temp_dir("symlink-scan");
        symlink(&real_subdir, d.join("LinkedFolder")).unwrap();
        symlink(&real_file, d.join("linked.wav")).unwrap();

        let got = scan_dir(&d).unwrap();
        let by_name = |n: &str| got.iter().find(|e| e.name == n).unwrap_or_else(|| panic!("missing {n}"));

        let folder = by_name("LinkedFolder");
        assert_eq!(folder.kind, LibraryEntryKind::Dir, "symlinked folder resolves as a Dir, not dropped");

        let file = by_name("linked.wav");
        assert_eq!(file.kind, LibraryEntryKind::Audio);
        assert_eq!(file.size_bytes, 10, "reports the TARGET's size, not the symlink's own");
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

    /// Write a mono 48 kHz WAV of `secs` seconds so the decode path is real.
    fn write_wav(path: &std::path::Path, secs: f64) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        let n = (48_000.0 * secs) as usize;
        for i in 0..n {
            let t = i as f64 / 48_000.0;
            w.write_sample((2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32 * 0.5).unwrap();
        }
        w.finalize().unwrap();
    }

    #[test]
    fn compile_audition_truncates_to_max_seconds_and_keeps_the_native_rate() {
        let d = temp_dir("audition");
        let p = d.join("long.wav");
        write_wav(&p, 3.0);

        let inst = compile_audition(&p, 1.0).unwrap();
        assert_eq!(inst.rate, 48_000, "native file rate, never resampled (ruling 8)");
        assert_eq!(inst.regions.len(), 1, "one region covers the whole keyboard");
        let r = &inst.regions[0];
        assert_eq!((r.lokey, r.hikey), (0, 127));
        assert_eq!(r.pitch_keycenter, 60);
        assert!(!r.looped, "an audition is a one-shot");
        assert_eq!(r.sample.rate, 48_000);
        assert_eq!(r.sample.frames(), 48_000, "1 s of a 3 s file");
        assert!(inst.name.contains("long.wav"), "name is the file name: {}", inst.name);

        // A file SHORTER than the bound is kept whole.
        let short = d.join("short.wav");
        write_wav(&short, 0.25);
        assert_eq!(compile_audition(&short, 1.0).unwrap().regions[0].sample.frames(), 12_000);
    }

    #[test]
    fn compile_audition_rejects_what_the_decoder_rejects() {
        let d = temp_dir("audition-bad");
        touch(&d, "notes.txt");
        // `.unwrap_err()` would need `CompiledInstrument: Debug`, which it
        // deliberately does not derive (out of scope for this task to add) —
        // match instead.
        let err = match compile_audition(&d.join("notes.txt"), 1.0) {
            Err(e) => e,
            Ok(_) => panic!("expected notes.txt to be rejected by the decoder"),
        };
        assert!(!err.is_empty(), "the failure is reported, never a panic");
    }
}
