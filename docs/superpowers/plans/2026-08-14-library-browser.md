# Track E — Library & Browser Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking. Execution mode is the owner's
> call at run start and is recorded in the execution note at the bottom.

**Goal:** Give AURA a side panel with three roots — **samples** (user-configured
folders plus a default library dir, with click-to-audition), **project clips**
(the open project's audio and MIDI clips), and **presets/instruments** (Zyn
patches, sampler instruments) — where dragging a row onto a timeline track
lands on the existing, already-channel-routed import/clip-add commands, so
every drop is undoable for free.

**Architecture:** The panel is **chrome** (ADR 0006, thin renderer): it holds
no authoritative state and does no filesystem walking. Directory listing and
audio decoding are backend work behind two new additive commands
(`library_scan`, `library_default_root`); auditioning is two more
(`library_audition`, `library_audition_stop`) that reuse the existing
`sampler_preview` voice path — the document-decoupled, Gate-safe preview
category MIDI slice 1's live monitoring established (no op, no document field,
no dirty flag). Every **mutation** the panel can cause is an existing command
(`import_audio_clip`, `midi_add_clip`/`midi_set_notes`, `set_track_instrument`,
`zyn_load_patch`), reached through existing frontend stores — this plan adds
**no new mutation path** and therefore no new exposure to the now-persisted op
log. The panel mounts as a new right-dock tab, so `App.svelte` is not touched
at all.

**Tech Stack:** Rust (src-tauri crate: serde, `dirs`, `hound`/`symphonia` via
the existing `decode_audio`, cpal via the existing preview stream),
TypeScript/Svelte 5 frontend (`src/lib/`), vitest.

**Spec:** `docs/backlog/library-and-browser.md` (the backlog spec this plan
implements) + `next-prompt.md` §3 "Track E" (scope definition) + §2 "Standing
constraints now in force". Supporting: `docs/PHASE4-PLAN.md`'s "Plan E handoff"
(the channel-routed import/clip-add commands this builds on), ADR 0006 (thin
renderer), ADR 0007 (evidence policy — corrections and deviations are marked,
never silent).

---

## Global Constraints

Every task's requirements implicitly include this section.

- **The op log is ON.** `journal.ndjson` is a persisted format;
  `OP_FORMAT_VERSION` is **2**. **This plan adds no ops and no op kinds.** Its
  entire exposure is the rule: *do not route any mutation outside an existing
  command*. If a task ever seems to need a direct `Session` write, a new op, or
  a new `PropPath`, that task is out of scope — stop and raise it.
- **Thin renderer (ADR 0006).** The panel is chrome. No filesystem walking
  frontend-side, no `readDir`, no `@tauri-apps/plugin-fs` browsing, no new
  authoritative state, no new time math. Scanning and metadata are backend.
  The one sanctioned frontend path operation is `parentDir()` (string trim for
  the "up one folder" button) and `@tauri-apps/api/path`'s `join` for turning a
  project-relative `Clip.sourcePath` into an absolute path — the same import
  the landed `projectops.defaultParentDir` already uses.
- **Frozen command/event names stay frozen; new commands are additive.** New
  commands in this plan, exactly four:
  `library_scan`, `library_default_root`, `library_audition`,
  `library_audition_stop`. **No new events.**
- **Audition stays document-decoupled.** The audition path must not take a
  `Session`, a `ControlPlane`, or a `Committer`; must emit no op, write no
  document field, and set no dirty flag. This is the established Gate-safe
  category (`sampler_preview_note`, `midi_input`'s live monitoring). Task 2
  pins it with a test.
- **No `engine.rs`.** Nothing in this plan touches
  `src-tauri/src/audio/engine.rs`. That file belongs to Tracks A/B/D.
- **Prepare-outside.** Unbounded work (directory reads, audio decode) happens
  on a blocking pool via `tauri::async_runtime::spawn_blocking`, never on the
  command's calling thread — sync Tauri commands run on the MAIN thread, and on
  Linux WebKitGTK shares the GTK main loop, so a slow scan on a network mount
  would freeze the UI (the rationale `seed_demo_project`'s doc comment already
  spells out).
- **Foreground test runs only, `timeout`-guarded:**
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` and
  `timeout 300 npx vitest run`. Never background a test run and move on.
- **Baseline at branch base** (`3340aa8`): **527 backend + 206 frontend, all
  green** — both halves run and verified in this worktree on 2026-08-14 while
  this plan was written (backend: the sum of every `test result: ok` line;
  frontend: `206 passed`, 23 files). Task 1 step 0 re-runs both anyway; if
  either number differs, record the real number in the ledger and use *that*
  as the baseline — do not silently edit the number, and do not proceed until
  you know why it moved.
- **Dated test-count convention.** The task that changes test counts updates
  `README.md` and `CONTRIBUTING.md` in the same commit with the date. This plan
  concentrates that in Task 9, which re-runs both suites and writes the final
  numbers — intermediate tasks do NOT touch the count lines (nine competing
  edits to one line is churn for nothing).
- **Evidence policy (ADR 0007).** The scope rulings below are decided now and
  travel verbatim into `docs/PHASE4-PLAN.md`'s eventual Track E handoff. A
  deviation discovered mid-flight becomes a recorded ruling in the ledger, not
  a silent change.

---

## Scope rulings (decided now, so no task stalls)

These settle the four design questions the track brief named, plus the ones
the code raised while this plan was written.

1. **Scan model: synchronous, one directory per call, lazily expanded — no
   background scanner, no new events.** `library_scan(dir)` reads exactly ONE
   directory level and returns its entries; the panel expands folders one at a
   time as the user navigates, exactly like every file browser. This bounds any
   single call to one `read_dir` (a 5000-file folder is one syscall sweep and a
   sort, not a tree walk), so it cannot freeze the UI even before the
   `spawn_blocking` wrapper — and the wrapper keeps it off the main thread
   regardless. A recursive/background scanner with progress events was
   considered and **deferred**: it needs cancellation, a persisted cache, and
   at least one new event name, and it buys nothing until search/tagging exists
   (backlog "Later"). Recorded, not silent.

2. **Metadata depth: filesystem metadata only — no decode probe during a
   scan.** An entry carries `name`, `path`, `kind` (`dir`/`audio`), `ext`,
   `sizeBytes`, `modifiedMs`. **No duration, channels, or sample rate.** The
   only decoder in the crate (`control::import::decode_audio`) decodes the
   *whole* file to get any of that — probing a folder of 500 samples would mean
   500 full decodes. Duration and format surface where the user actually asks
   for the file: the audition (which decodes) and the import (which probes and
   writes them onto the `Clip`). A cheap header-only probe command
   (`library_probe(path)`) is **deferred** and recorded. Waveform thumbnails
   (backlog cut item 4) are likewise **deferred** — they need the pyramid cache
   keyed by source, which only exists for imported material.

3. **Default library dir: `<audio dir>/AURA/Library`, created on demand,
   resolved by the backend.** It follows the landed
   `project::default_project_parent` convention exactly
   (`dirs::audio_dir().or_else(dirs::home_dir).join("AURA")`), one level
   deeper. The frontend never computes it (thin renderer): it asks
   `library_default_root()`. It is always shown first under SAMPLES and cannot
   be removed.

4. **The folder list is a preference, and the prefs schema grows a
   `"pathList"` kind to hold it.** `PrefValues.librarySampleFolders: string[]`,
   category `"library"` (a new `PrefCategoryId`). The schema-driven module has
   exactly three kinds today (boolean/number/enum) and none holds a list, so
   the additive fourth kind is the way to "follow the existing pattern" rather
   than route around it — one entry in `PrefValues` + `PREF_SCHEMA`, one
   `coercePref` case, one dialog branch, and the store/persistence come free.
   Storing it backend-side in a config file was rejected: it is UI
   configuration in exactly the sense `uiZoom` is, `aura.prefs` already
   persists per-machine, and a backend config file would need its own
   read/write/validate surface for one string array.

5. **Drag-and-drop is in-app HTML5 DnD on a private MIME type; the OS
   file-drop path is not touched.** Payloads travel as JSON under
   `application/x-aura-library`. `ImportDropZone.svelte` keys its handlers on
   `dataTransfer.types.includes("Files")`, which an in-app drag never sets, so
   the two paths cannot collide and the drop-zone overlay never appears for a
   panel drag. The lane drop handler lives on the per-track `.lane` element
   (the track is known from the `{#each}` closure, exactly as `onLaneDblClick`
   already does), so **no track hit-testing is needed** — only x→samples, and
   that goes through `canvasPos()` per the standing zoom-pointer rule.
   `dragover` may only read `dataTransfer.types` (browsers withhold
   `getData()` until `drop`), so the util exposes `hasLibraryDrag(dt)` for
   `dragover` and `decodeLibraryDrag(dt)` for `drop` — two functions on
   purpose, not an oversight.

6. **Nothing in this plan invents a mutation.** Every drop calls a landed
   command through a landed store: `import_audio_clip` (samples, project audio
   clips), `midi_add_clip` + `midi_set_notes` via `midi.stamp` (project MIDI
   clips), `set_track_instrument` (sampler instruments), `zyn_load_patch` via
   `zyn.load` (Zyn patches). Multi-command drops are wrapped in the landed
   `gesture_begin`/`gesture_end` IPC so they fold into ONE undo entry — this
   works for mixed op kinds because `fold_ops` passes non-`Set` ops through
   into the same batch (`session.rs:1259`), and a gesture batch becomes one
   `HistoryEntry` via `HistoryEntry::from_gesture`.

7. **`lib.rs` is edited additively, following the `midi_input` precedent.**
   `src-tauri/src/lib.rs` carries a "FROZEN FILE" banner from phase 2. The
   hardware-MIDI slice (PR #17, on `main`) added `pub mod midi_input;` plus a
   commented additive block in `generate_handler!` with a note explaining it is
   outside the frozen phase-2 surface. This plan does the same, in the same
   style, for `pub mod library;` and four command entries. Recorded as a
   marked deviation from the banner, per ADR 0007.

8. **The audition plays the file's own samples at the file's own rate — no
   resampling.** `compile_audition` builds a one-region `CompiledInstrument`
   whose `rate` is the *file* rate, not the engine rate.
   `sampler_voice::pitch_ratio` folds `sample.rate / out_rate` in, so playback
   pitch is correct on any output stream. `CompiledInstrument.rate`'s doc
   comment says "resampled to at load time", which is true of the SFZ loader
   and not of this path; the audition path documents the difference at its own
   definition. **Do not "fix" this by resampling** — it would burn CPU to
   arrive at the same audio.

9. **Presets root v1 lists and applies; it does not browse `.sfz`/`.xiz` on
   disk.** Sampler instruments come from `sampler_list_instruments` (already
   loaded into the bank), Zyn patches from `zyn_list_patches`. Scanning the
   filesystem for uninstalled instruments is a separate capability and is
   **deferred**. A Zyn patch drop requires the target track to already host a
   Zyn instance; when it does not, the drop toasts and does nothing (it must
   not silently instantiate a plugin behind the user's back).

10. **MIDI files (`.mid`/`.smf`) are not listed by the sample scan.**
    `AUDIO_EXTS` mirrors exactly what `decode_audio` accepts, so every listed
    row is guaranteed importable. Dragging SMF in from the library (backlog
    "Later", `midi_import_file` already exists) is **deferred** and recorded.

11. **Demo mode degrades, it does not fake.** All four new backend methods are
    **optional** on the `Backend` interface (the landed `moveClip?`,
    `seedDemoProject?`, `midiListInputPorts?` idiom). `DemoBackend` does not
    implement them; the panel shows a "desktop only" note. No synthetic
    filesystem.

---

## File structure

**Backend (new):**

| File | Responsibility |
|---|---|
| `src-tauri/src/library.rs` | The whole backend half except the two audition commands: `LibraryEntry`/`LibraryEntryKind` wire types, `AUDIO_EXTS`, `scan_dir`, `default_library_root`, `compile_audition`, and the `library_scan`/`library_default_root` commands. Self-contained, tauri-free core + thin command wrappers (the house pattern). |

**Backend (modified):**

| File | Change |
|---|---|
| `src-tauri/src/lib.rs` | `pub mod library;` + four additive `generate_handler!` entries (ruling 7). |
| `src-tauri/src/audio/sampler_preview.rs` | `Msg::PlayHeld` + `PreviewHandle::play_held` — the reply-carrying sibling of `note_on`. |
| `src-tauri/src/audio/mod.rs` | `library_audition` / `library_audition_stop` commands (they live here because `AudioState`'s `preview` field is private to this module). |
| `src-tauri/src/control/import.rs` | `SYMPHONIA_EXTS` becomes `pub(crate)` so `library.rs`'s anti-drift test can compare against it. |
| `src-tauri/tests/pure_readers.rs` | One test: the audition core leaves the document byte-identical. |

**Frontend (new):**

| File | Responsibility |
|---|---|
| `src/lib/utils/library.ts` | Pure helpers, no DOM, no stores: `parentDir`, `LIBRARY_DRAG_MIME`, `LibraryDragPayload`, `encodeLibraryDrag`, `hasLibraryDrag`, `decodeLibraryDrag`. |
| `src/lib/utils/library.test.ts` | Its tests. |
| `src/lib/state/library.svelte.ts` | The panel's store: root selection, folder navigation + per-folder cache, audition, and `dropOnTrack` (which dispatches to the landed stores/commands). |
| `src/lib/state/library.test.ts` | Its tests. |
| `src/lib/components/library/LibraryPanel.svelte` | Root switcher + shared chrome. |
| `src/lib/components/library/LibraryRow.svelte` | One draggable row (icon, name, meta) — shared by all three roots. |
| `src/lib/components/library/SamplesRoot.svelte` | Folder list / entry list / breadcrumb + audition click. |
| `src/lib/components/library/ClipsRoot.svelte` | The open project's audio + MIDI clips. |
| `src/lib/components/library/PresetsRoot.svelte` | Sampler instruments + Zyn patches. |

**Frontend (modified):**

| File | Change |
|---|---|
| `src/lib/types/ipc.ts` | `LibraryEntryKind`, `LibraryEntry`. |
| `src/lib/tauri.ts` | Four optional `Backend` methods + their `TauriBackend` implementations. |
| `src/lib/prefs/schema.ts` | `"library"` category, `PathListDef`, `librarySampleFolders`, `coercePref` case, array-safe `schemaDefaults`. |
| `src/lib/prefs/schema.test.ts` | Tests for the above. |
| `src/lib/components/prefs/PreferencesDialog.svelte` | One `pathList` control branch. |
| `src/lib/utils/pick-directory.ts` (new, tiny) | The replaceable native folder-picker seam the dialog uses. |
| `src/lib/state/ui.svelte.ts` | `DockTab` gains `"library"`. |
| `src/lib/components/Dock.svelte` | One `TABS` entry + one render branch. |
| `src/lib/components/TransportBar.svelte` | One chip entry. |
| `src/lib/components/Timeline.svelte` | `ondragover`/`ondragleave`/`ondrop` on `.lane` + a drop-highlight class. |
| `src/lib/state/midi.svelte.ts` | `stampClipTo` — a public entry to the existing private `stamp`. |
| `README.md`, `CONTRIBUTING.md`, `docs/backlog/library-and-browser.md`, `next-prompt.md` | Task 9. |

**`App.svelte` is not modified.** The panel mounts as a dock tab. This is the
whole conflict story with the other four tracks: they may add their own panels
to `App.svelte`; this branch will not conflict there. The only shared surfaces
that remain are `Dock.svelte`/`ui.svelte.ts`/`TransportBar.svelte` (one line
each) and `schema.ts` (one entry). See the execution note's rebase paragraph.

---

## Task 1: Backend — directory scan + default library root

**Files:**
- Create: `src-tauri/src/library.rs`
- Modify: `src-tauri/src/lib.rs` (module declaration + handler entries)
- Modify: `src-tauri/src/control/import.rs` (`SYMPHONIA_EXTS` → `pub(crate)`)
- Test: `src-tauri/src/library.rs` (`#[cfg(test)] mod tests`, house style)

**Interfaces:**
- Consumes: `crate::control::import::SYMPHONIA_EXTS` (made `pub(crate)` here).
- Produces:
  ```rust
  pub const AUDIO_EXTS: [&str; 7] = ["wav", "mp3", "flac", "ogg", "aac", "m4a", "mp4"];

  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub enum LibraryEntryKind { Dir, Audio }

  #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
  #[serde(rename_all = "camelCase")]
  pub struct LibraryEntry {
      pub name: String,
      pub path: String,
      pub kind: LibraryEntryKind,
      pub ext: String,
      pub size_bytes: u64,
      pub modified_ms: u64,
  }

  pub fn scan_dir(dir: &std::path::Path) -> Result<Vec<LibraryEntry>, String>;
  pub fn default_library_root() -> Result<std::path::PathBuf, String>;

  #[tauri::command] pub async fn library_scan(dir: String) -> Result<Vec<LibraryEntry>, String>;
  #[tauri::command] pub fn library_default_root() -> Result<String, String>;
  ```

- [ ] **Step 0: Verify the baseline before writing anything**

Run both, foreground, in this worktree:

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```

Expected: **527 backend + 206 frontend, all green.** If the backend number
differs, record the real number in the SDD ledger and treat it as the baseline
— do not proceed until you know what changed.

- [ ] **Step 1: Write the failing tests**

Create `src-tauri/src/library.rs` with the module doc and this test module
only (no implementation yet — the file will not compile, which is the point):

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml library::`
Expected: FAIL — compile errors (`scan_dir` not found, `LibraryEntry` not
found) and `SYMPHONIA_EXTS` private.

- [ ] **Step 3: Write the implementation**

In `src-tauri/src/control/import.rs`, change the const's visibility (leave the
comment):

```rust
/// Formats decoded through symphonia (everything except the WAV fast path).
pub(crate) const SYMPHONIA_EXTS: [&str; 6] = ["mp3", "flac", "ogg", "aac", "m4a", "mp4"];
```

In `src-tauri/src/library.rs`, above the test module:

```rust
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
        // `metadata()` follows symlinks on purpose: a symlinked sample folder
        // is a normal way to organise a library.
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
/// thread, and on Linux WebKitGTK shares the GTK main loop — a `read_dir` on a
/// slow network mount would freeze the UI (the same rationale
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
```

- [ ] **Step 4: Wire the module and the commands into `lib.rs`**

In `src-tauri/src/lib.rs`, after `pub mod ids;` (keeping alphabetical order
among the additive modules), add — the comment matters, it is what keeps
ruling 7 auditable:

```rust
// Library & browser panel backend (Track E: directory scan, default library
// root, file audition). Self-contained; wired here purely additively, the
// same carve-out `midi_input` takes. Not part of the frozen phase-2 command
// surface described above.
pub mod library;
```

And in `generate_handler!`, after the `midi_input` block:

```rust
            // ---- library & browser (Track E, additive) ----
            library::library_scan,
            library::library_default_root,
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — baseline + 6 new backend tests, all green, no warnings from
the new file (`cargo build` must be clean; an unused import is a review
finding).

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/library.rs src-tauri/src/lib.rs src-tauri/src/control/import.rs
git commit -m "feat(library): backend directory scan + default library root"
```

---

## Task 2: Backend — file audition through the sampler-preview voice path

**Files:**
- Modify: `src-tauri/src/audio/sampler_preview.rs` (add `Msg::PlayHeld` + `PreviewHandle::play_held`)
- Modify: `src-tauri/src/library.rs` (add `AUDITION_MAX_SECS`, `compile_audition`)
- Modify: `src-tauri/src/audio/mod.rs` (two commands, next to `sampler_preview_note`)
- Modify: `src-tauri/src/lib.rs` (two handler entries)
- Test: `src-tauri/src/audio/sampler_preview.rs`, `src-tauri/src/library.rs`, `src-tauri/tests/pure_readers.rs`

**Interfaces:**
- Consumes: `library::AUDIO_EXTS` (Task 1); `control::import::decode_audio`
  (existing, `pub(crate)`); `audio::sampler_engine::CompiledInstrument`;
  `audio::sampler_voice::{CompiledRegion, SampleData}`;
  `sampler_preview::{start, PreviewHandle}`.
- Produces:
  ```rust
  // library.rs
  pub const AUDITION_MAX_SECS: f32 = 12.0;
  pub fn compile_audition(path: &std::path::Path, max_seconds: f32)
      -> Result<crate::audio::sampler_engine::CompiledInstrument, String>;

  // sampler_preview.rs
  impl PreviewHandle {
      pub fn play_held(&self, instrument: std::sync::Arc<CompiledInstrument>, key: u8, velocity: u8)
          -> Result<(), String>;
  }

  // audio/mod.rs
  #[tauri::command] pub async fn library_audition(
      path: String, max_seconds: Option<f32>, state: State<'_, AudioState>) -> Result<(), String>;
  #[tauri::command] pub fn library_audition_stop(state: State<'_, AudioState>) -> Result<(), String>;
  ```

**Why `play_held` and not the existing `play`:** `play` auto-releases after
`PREVIEW_HOLD_SECS` (0.6 s) — right for a note audition, wrong for hearing a
sample. `note_on` holds until an explicit release (which is what we want) but
is fire-and-forget with no reply channel, so a stream-open failure would never
reach the user. `play_held` is `note_on`'s behaviour with `play`'s reply
channel. The voice ends itself at the end of the sample data
(`sampler_voice.rs:311` sets `Stage::Idle` when the read position passes the
last frame), so a held, non-looped one-shot stops on its own.

- [ ] **Step 1: Write the failing tests**

In `src-tauri/src/library.rs`'s `mod tests`, add:

```rust
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
        let err = compile_audition(&d.join("notes.txt"), 1.0).unwrap_err();
        assert!(!err.is_empty(), "the failure is reported, never a panic");
    }
```

In `src-tauri/src/audio/sampler_preview.rs`'s `mod tests`, add — this is the
headless RT-callback proof that a held note outlives the 0.6 s auto-release
window and that `AllOff` ends it:

```rust
    /// `play_held`'s wire behaviour at the callback: a note pushed with
    /// `hold_frames: 0` (HOLD_MANUAL) is still sounding well past
    /// PREVIEW_HOLD_SECS, and `NoteCmd::AllOff` releases it to silence.
    #[test]
    fn a_held_preview_note_outlives_the_auto_release_window_until_all_off() {
        let (mut swap_tx, swap_rx) = rtrb::RingBuffer::new(4);
        let (retire_tx, _retire_rx) = rtrb::RingBuffer::new(4);
        let mut cb = PreviewCb { swap_rx, retire_tx, node: None, channels: 2, rate: RATE };

        let mut node = Box::new(SamplerNode::new(compiled("held", 440.0)));
        let mut note_tx = node.command_queue(8);
        node.prepare(RATE, 512);
        swap_tx.push(NodePtr(Box::into_raw(node))).unwrap();
        note_tx.push(NoteCmd::On { key: 69, velocity: 110, hold_frames: 0 }).unwrap();

        // Render past PREVIEW_HOLD_SECS (0.6 s) — `compiled()` is a 1 s sample.
        let block = 512usize;
        let blocks = ((RATE as f64 * 0.8) / block as f64) as usize;
        let mut last = vec![0.0f32; block * 2];
        for _ in 0..blocks {
            last.fill(0.0);
            cb.render(&mut last);
        }
        assert!(last.iter().any(|s| s.abs() > 0.01), "still sounding after 0.8 s");

        note_tx.push(NoteCmd::AllOff).unwrap();
        for _ in 0..20 {
            last.fill(0.0);
            cb.render(&mut last);
        }
        assert!(last.iter().all(|s| s.abs() < 1.0e-4), "AllOff released it to silence");
    }
```

In `src-tauri/tests/pure_readers.rs`, add the Gate-safe proof (copy the file's
existing `fixture()` / `tmp_parent()` helpers — they are already there):

```rust
/// The audition path is DOCUMENT-DECOUPLED (Track E, the Gate-safe preview
/// category MIDI slice 1's monitoring established): compiling an audition
/// decodes a file and touches nothing the document can see. This test pins the
/// non-RT half — the whole half that could ever grow a `Session` reference.
/// The command wrapper adds only `PreviewHandle` calls, and `PreviewHandle`
/// takes no `Session`, no `ControlPlane` and no `Committer` by construction.
#[test]
fn library_audition_core_leaves_the_document_and_the_history_untouched() {
    let (cp, _engine) = fixture();
    let parent = tmp_parent("library-audition");
    cp.create_project_epoch("AuditionProbe", Some(parent.display().to_string())).unwrap();

    let wav = parent.join("probe.wav");
    {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        for i in 0..48_000 {
            w.write_sample((i as f32 / 48_000.0).sin() * 0.3).unwrap();
        }
        w.finalize().unwrap();
    }

    let before = serde_json::to_value(cp.project_state()).unwrap();
    let depths_before = cp.history_depths();

    let inst = aura_lib::library::compile_audition(&wav, 1.0).unwrap();
    assert_eq!(inst.regions.len(), 1);

    let after = serde_json::to_value(cp.project_state()).unwrap();
    assert_eq!(before, after, "audition mutated the document");
    assert_eq!(depths_before, cp.history_depths(), "audition created a history entry");
}
```

> Implementer note: `create_project_epoch`'s exact signature is whatever
> `pure_readers.rs`'s existing tests already call — copy the call from
> `project_state_after_an_epoch_boundary_is_stable_across_repeated_reads`
> rather than guessing, and add `hound` to the test's imports only if the file
> does not already have it.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: FAIL — `compile_audition` not found; the sampler-preview test fails
or the pure-readers test does not compile.

- [ ] **Step 3: Implement `play_held` in `sampler_preview.rs`**

Add the variant to `enum Msg` (right after `Play`):

```rust
    /// Sustained audition with a reply channel: `note_on`'s hold semantics
    /// (no auto-release — the voice ends itself at the end of the sample
    /// data) with `play`'s error reporting, for user-initiated commands that
    /// have a result to surface. Used by `library_audition`.
    PlayHeld {
        instrument: Arc<CompiledInstrument>,
        key: u8,
        velocity: u8,
        reply: Sender<Result<(), String>>,
    },
```

Add the method to `impl PreviewHandle`:

```rust
    /// Audition a whole sample: a sustained note that holds until the sample
    /// data runs out (or an [`PreviewHandle::all_off`]). Blocks briefly for
    /// the thread's reply, so stream-open and install errors reach the caller
    /// — unlike [`PreviewHandle::note_on`], which is fire-and-forget for
    /// non-RT callback threads.
    pub fn play_held(
        &self,
        instrument: Arc<CompiledInstrument>,
        key: u8,
        velocity: u8,
    ) -> Result<(), String> {
        let (reply, rx) = bounded(1);
        self.tx
            .send(Msg::PlayHeld { instrument, key, velocity, reply })
            .map_err(|_| "preview thread is gone".to_string())?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| "preview thread did not respond".to_string())?
    }
```

Add the arm to `preview_thread`'s `match`, next to `Msg::Play`:

```rust
            Ok(Msg::PlayHeld { instrument, key, velocity, reply }) => {
                let _ = reply.send(handle_note_on(&mut bundle, instrument, key, velocity));
            }
```

(`handle_note_on` already pushes `hold_frames: 0`, which `SamplerNode` maps to
`HOLD_MANUAL` — no new handler is needed.)

- [ ] **Step 4: Implement `compile_audition` in `library.rs`**

```rust
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
```

- [ ] **Step 5: Add the two commands in `audio/mod.rs`**

Immediately after `sampler_preview_note`:

```rust
/// Audition a LIBRARY FILE — no project, no clip, no document. The Gate-safe
/// preview category (`sampler_preview_note`, `midi_input`'s live monitoring):
/// no op, no document field, no dirty flag. Plays at most
/// `library::AUDITION_MAX_SECS` of the file once through the shared "ui"
/// preview stream; whatever was auditioning is released first, so a click
/// cancels the previous one.
///
/// `async` + `spawn_blocking`: decoding is disk + CPU work and sync commands
/// run on the MAIN thread (see `seed_demo_project`).
#[tauri::command]
pub async fn library_audition(
    path: String,
    max_seconds: Option<f32>,
    state: State<'_, AudioState>,
) -> Result<(), String> {
    let handle = state.preview.get_or_init(|| sampler_preview::start("ui")).clone();
    let secs = max_seconds.unwrap_or(crate::library::AUDITION_MAX_SECS);
    tauri::async_runtime::spawn_blocking(move || {
        let inst = crate::library::compile_audition(std::path::Path::new(&path), secs)?;
        // Cancel whatever is sounding before installing the next sample.
        handle.all_off()?;
        handle.play_held(std::sync::Arc::new(inst), 60, 100)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Release whatever the audition path is sounding. A no-op (never an error)
/// when nothing has ever been auditioned — the preview stream is lazy.
#[tauri::command]
pub fn library_audition_stop(state: State<'_, AudioState>) -> Result<(), String> {
    match state.preview.get() {
        Some(h) => h.all_off(),
        None => Ok(()),
    }
}
```

Register both in `src-tauri/src/lib.rs`, in the Track E block from Task 1:

```rust
            audio::library_audition,
            audio::library_audition_stop,
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS — Task 1's count + 4 new backend tests.

- [ ] **Step 7: Commit**

```bash
git add src-tauri/src/library.rs src-tauri/src/audio/sampler_preview.rs \
        src-tauri/src/audio/mod.rs src-tauri/src/lib.rs src-tauri/tests/pure_readers.rs
git commit -m "feat(library): file audition through the sampler-preview voice path"
```

---

## Task 3: Preferences — a `pathList` kind and the sample-folder list

**Files:**
- Modify: `src/lib/prefs/schema.ts`
- Modify: `src/lib/prefs/schema.test.ts`
- Create: `src/lib/utils/pick-directory.ts`
- Modify: `src/lib/components/prefs/PreferencesDialog.svelte`
- Test: `src/lib/prefs/schema.test.ts`

**Interfaces:**
- Consumes: the landed `PrefValues` / `PREF_SCHEMA` / `coercePref` /
  `schemaDefaults` contract in `src/lib/prefs/schema.ts`.
- Produces:
  ```ts
  export type PrefCategoryId = "editing" | "interface" | "library" | "ai";
  export type PathListDef = BaseDef & { kind: "pathList"; default: string[]; addLabel: string };
  // PrefValues gains:
  librarySampleFolders: string[];
  // src/lib/utils/pick-directory.ts
  export const directoryPicker: { pick(title: string): Promise<string | null> };
  ```

- [ ] **Step 1: Write the failing tests**

Append to `src/lib/prefs/schema.test.ts`:

```ts
describe("pathList preferences", () => {
  it("declares librarySampleFolders as an empty pathList in the library category", () => {
    const def = PREF_SCHEMA.librarySampleFolders;
    expect(def.kind).toBe("pathList");
    expect(def.default).toEqual([]);
    expect(def.category).toBe("library");
    expect(PREF_CATEGORIES.map((c) => c.id)).toContain("library");
  });

  it("coerces a pathList: keeps strings, drops junk, de-dupes, preserves order", () => {
    const def = PREF_SCHEMA.librarySampleFolders;
    expect(coercePref(def, ["/a", "/b", "/a", "", 7, null, "/c"])).toEqual(["/a", "/b", "/c"]);
  });

  it("rejects a non-array pathList value whole", () => {
    const def = PREF_SCHEMA.librarySampleFolders;
    expect(coercePref(def, "/a")).toBeUndefined();
    expect(coercePref(def, 3)).toBeUndefined();
    expect(coercePref(def, null)).toBeUndefined();
  });

  it("hands out a FRESH array from schemaDefaults so the schema default cannot be mutated", () => {
    const a = schemaDefaults().librarySampleFolders;
    const b = schemaDefaults().librarySampleFolders;
    expect(a).toEqual([]);
    expect(a).not.toBe(b);
    expect(a).not.toBe(PREF_SCHEMA.librarySampleFolders.default);
  });
});
```

> The file's existing imports may not include `PREF_CATEGORIES` or
> `schemaDefaults` — add them to the existing import statement.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `timeout 300 npx vitest run src/lib/prefs/schema.test.ts`
Expected: FAIL — `PREF_SCHEMA.librarySampleFolders` is undefined.

- [ ] **Step 3: Extend the schema**

In `src/lib/prefs/schema.ts`:

```ts
export type PrefCategoryId = "editing" | "interface" | "library" | "ai";

export const PREF_CATEGORIES: readonly { id: PrefCategoryId; label: string }[] = [
  { id: "editing", label: "EDITING" },
  { id: "interface", label: "INTERFACE" },
  { id: "library", label: "LIBRARY" },
  { id: "ai", label: "AI AGENTS" },
];
```

In `PrefValues`, add:

```ts
  /** Absolute folders listed under the library panel's SAMPLES root. The
   * default library dir is supplied by the backend and is not stored here. */
  librarySampleFolders: string[];
```

Add the def type after `EnumDef`:

```ts
/** An ordered list of absolute filesystem paths (the library's sample
 * folders). Rendered by the dialog as a removable list plus an "add" button
 * driven by the native directory picker. */
export type PathListDef = BaseDef & {
  kind: "pathList";
  default: string[];
  /** Copy on the dialog's add button, e.g. "ADD FOLDER". */
  addLabel: string;
};
```

Extend `DefFor` — `string[]` is matched before the bare-`string` (enum) arm so
the intent is readable at a glance:

```ts
type DefFor<V> = [V] extends [boolean]
  ? BooleanDef
  : [V] extends [number]
    ? NumberDef
    : [V] extends [string[]]
      ? PathListDef
      : [V] extends [string]
        ? EnumDef<V>
        : never;
```

Add the schema entry (after `uiZoom`, before `mcpDefaultMode`):

```ts
  librarySampleFolders: {
    kind: "pathList",
    default: [],
    addLabel: "ADD FOLDER",
    category: "library",
    label: "Sample folders",
    blurb: "Folders browsed under SAMPLES in the library panel. The default AURA library folder is always listed first.",
  },
```

Add the `coercePref` case:

```ts
    case "pathList": {
      // Reject a non-array whole (the schema's own convention); inside an
      // array, drop junk entries and duplicates rather than losing the list.
      if (!Array.isArray(value)) return undefined;
      const paths = value.filter((p): p is string => typeof p === "string" && p.length > 0);
      return [...new Set(paths)] as D["default"];
    }
```

Make `schemaDefaults` array-safe — without this, every store shares (and could
mutate) the schema's own default array:

```ts
/** A fresh { id → default } object covering the whole schema. Array defaults
 * are COPIED: the store's value must never alias the schema's own default. */
export function schemaDefaults(): PrefValues {
  return Object.fromEntries(
    (Object.keys(PREF_SCHEMA) as PrefId[]).map((id) => {
      const d = PREF_SCHEMA[id].default;
      return [id, Array.isArray(d) ? [...d] : d];
    }),
  ) as unknown as PrefValues;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `timeout 300 npx vitest run src/lib/prefs`
Expected: PASS — existing prefs tests plus 4 new ones.

- [ ] **Step 5: Add the directory-picker seam**

Create `src/lib/utils/pick-directory.ts`:

```ts
/**
 * Native directory picker, behind a replaceable seam so components stay
 * testable outside Tauri. Mirrors `projectops.pickDirectory`'s idiom (a
 * replaceable member, not a mutable export) — that one stays where it is;
 * this module exists for callers that are not the project flow.
 */
export const directoryPicker = {
  async pick(title: string): Promise<string | null> {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ directory: true, title });
    return typeof picked === "string" ? picked : null;
  },
};
```

- [ ] **Step 6: Render `pathList` in the preferences dialog**

In `src/lib/components/prefs/PreferencesDialog.svelte`, import the seam:

```ts
  import { directoryPicker } from "../../utils/pick-directory";
```

and add a handler next to `commitNumber`:

```ts
  /** Append a folder to a pathList preference (no-op when cancelled or a
   * duplicate — `coercePref` de-dupes, but not adding is quieter). */
  async function addPath(id: PrefId, def: PrefDef) {
    if (def.kind !== "pathList") return;
    const picked = await directoryPicker.pick(def.label);
    if (!picked) return;
    const current = prefs.values[id] as string[];
    if (current.includes(picked)) return;
    prefs.set(id, [...current, picked]);
  }

  function removePath(id: PrefId, path: string) {
    prefs.set(
      id,
      (prefs.values[id] as string[]).filter((p) => p !== path),
    );
  }
```

Add the markup branch after the existing `boolean` / `enum` / `number`
branches, in the body area under `.prefhead` (a list does not fit on the
header row):

```svelte
                {#if def.kind === "pathList"}
                  <div class="pathlist">
                    {#each prefs.values[id] as string[] as p (p)}
                      <div class="pathrow">
                        <span class="pathtext silk" title={p}>{p}</span>
                        <button
                          class="pathx mono"
                          title="Remove this folder"
                          aria-label="Remove {p}"
                          onclick={() => removePath(id, p)}>×</button
                        >
                      </div>
                    {:else}
                      <div class="pathempty silk">no extra folders — only the default library</div>
                    {/each}
                    <button class="pathadd mono" onclick={() => void addPath(id, def)}>
                      + {def.addLabel}
                    </button>
                  </div>
                {/if}
```

Style it with the dialog's existing tokens (`--text-faint`, `--glass-border`,
`--cyan`); do not introduce new colour values.

- [ ] **Step 7: Verify the suites and the typecheck**

```bash
timeout 300 npx vitest run
npm run check
```
Expected: 210 frontend tests green (206 + 4), `svelte-check` clean.

- [ ] **Step 8: Commit**

```bash
git add src/lib/prefs/schema.ts src/lib/prefs/schema.test.ts \
        src/lib/utils/pick-directory.ts src/lib/components/prefs/PreferencesDialog.svelte
git commit -m "feat(prefs): pathList preference kind + library sample folders"
```

---

## Task 4: Frontend — IPC types, backend adapter, and the library store

**Files:**
- Modify: `src/lib/types/ipc.ts`
- Modify: `src/lib/tauri.ts`
- Create: `src/lib/utils/library.ts`
- Create: `src/lib/utils/library.test.ts`
- Create: `src/lib/state/library.svelte.ts`
- Create: `src/lib/state/library.test.ts`

**Interfaces:**
- Consumes: Task 1/2's four commands; Task 3's
  `prefs.values.librarySampleFolders`.
- Produces:
  ```ts
  // src/lib/types/ipc.ts
  export type LibraryEntryKind = "dir" | "audio";
  export interface LibraryEntry {
    name: string; path: string; kind: LibraryEntryKind;
    ext: string; sizeBytes: number; modifiedMs: number;
  }

  // src/lib/tauri.ts — all OPTIONAL on Backend (demo mode has no filesystem)
  libraryScan?(dir: string): Promise<LibraryEntry[]>;
  libraryDefaultRoot?(): Promise<string>;
  libraryAudition?(path: string, maxSeconds?: number | null): Promise<void>;
  libraryAuditionStop?(): Promise<void>;

  // src/lib/utils/library.ts
  export function parentDir(path: string): string | null;

  // src/lib/state/library.svelte.ts
  export type LibraryRoot = "samples" | "clips" | "presets";
  export const library: LibraryStore;   // root, defaultRoot, cwd, entries,
                                        // loading, error, auditioning,
                                        // available, folders, init, open,
                                        // up, refresh, audition, stopAudition
  ```

- [ ] **Step 1: Write the failing tests for the path helper**

Create `src/lib/utils/library.test.ts`:

```ts
/**
 * Pure helpers behind the library panel. No DOM, no stores — the vitest
 * environment is "node" (see vitest.config.ts).
 */
import { describe, expect, it } from "vitest";
import { parentDir } from "./library";

describe("parentDir", () => {
  it("walks up a POSIX path", () => {
    expect(parentDir("/home/u/Music/AURA/Library/drums")).toBe("/home/u/Music/AURA/Library");
    expect(parentDir("/home/u/Music/AURA/Library/drums/")).toBe("/home/u/Music/AURA/Library");
  });

  it("walks up a Windows path", () => {
    expect(parentDir("C:\\Users\\u\\Samples\\drums")).toBe("C:\\Users\\u\\Samples");
  });

  it("returns null at a filesystem root, so the UI can hide the up button", () => {
    expect(parentDir("/")).toBeNull();
    expect(parentDir("C:\\")).toBeNull();
    expect(parentDir("")).toBeNull();
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `timeout 300 npx vitest run src/lib/utils/library.test.ts`
Expected: FAIL — cannot resolve `./library`.

- [ ] **Step 3: Implement the path helper**

Create `src/lib/utils/library.ts`:

```ts
/**
 * Pure helpers behind the library panel. Chrome only: the ONE path operation
 * the thin-renderer rule allows here is trimming a trailing segment for the
 * "up one folder" button — no filesystem access ever happens frontend-side
 * (ADR 0006; scanning is `library_scan`).
 */

/**
 * The parent of an absolute path, or null when it is already a root.
 * Handles both separators because the backend hands back whatever the host
 * OS uses.
 */
export function parentDir(path: string): string | null {
  const trimmed = path.replace(/[/\\]+$/, "");
  if (!trimmed) return null;
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  if (cut < 0) return null;
  if (cut === 0) return "/"; // "/drums" -> "/"
  const parent = trimmed.slice(0, cut);
  // "C:" -> a Windows drive root, not a relative path.
  return /^[A-Za-z]:$/.test(parent) ? `${parent}\\` : parent;
}
```

- [ ] **Step 4: Add the IPC types and the adapter methods**

In `src/lib/types/ipc.ts`, in the phase-2/3 section near `InstrumentInfo`:

```ts
// ── library & browser (Track E, src-tauri/src/library.rs) ──────────────────

export type LibraryEntryKind = "dir" | "audio";

/** One row of a `library_scan` listing. Filesystem metadata only — duration
 * and format are NOT probed during a scan (plan ruling 2). */
export interface LibraryEntry {
  /** File or directory name (no path). */
  name: string;
  /** Absolute path. */
  path: string;
  kind: LibraryEntryKind;
  /** Lowercased extension without the dot; empty for directories. */
  ext: string;
  sizeBytes: number;
  /** Modification time, ms since the Unix epoch (0 when unknown). */
  modifiedMs: number;
}
```

In `src/lib/tauri.ts`, add `LibraryEntry` to the type import, then in the
`Backend` interface (near the other optional members):

```ts
  // ── library & browser (Track E, additive; desktop only) ──
  /** List ONE directory level of the sample library. */
  libraryScan?(dir: string): Promise<LibraryEntry[]>;
  /** Absolute path of the default library dir, created on demand. */
  libraryDefaultRoot?(): Promise<string>;
  /** One-shot audition of a library file. Document-decoupled: no op, no
   * document field, no dirty flag. */
  libraryAudition?(path: string, maxSeconds?: number | null): Promise<void>;
  /** Release whatever the audition path is sounding. */
  libraryAuditionStop?(): Promise<void>;
```

and in `TauriBackend`:

```ts
  libraryScan(dir: string) {
    return invoke<LibraryEntry[]>("library_scan", { dir });
  }
  libraryDefaultRoot() {
    return invoke<string>("library_default_root");
  }
  async libraryAudition(path: string, maxSeconds: number | null = null) {
    await invoke("library_audition", { path, maxSeconds });
  }
  async libraryAuditionStop() {
    await invoke("library_audition_stop");
  }
```

`DemoBackend` implements none of them (ruling 11) — it satisfies `Backend`
because all four are optional.

- [ ] **Step 5: Write the failing store tests**

Create `src/lib/state/library.test.ts` (the `vi.mock("../tauri")` idiom is
copied from `src/lib/state/projectops.test.ts`):

```ts
/**
 * Library store: folder navigation, the per-folder cache, and audition.
 * Drag/drop behaviour is Task 6's; these pin the browsing half.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LibraryEntry } from "../types/ipc";

const dirEntry = (name: string, path: string): LibraryEntry => ({
  name,
  path,
  kind: "dir",
  ext: "",
  sizeBytes: 0,
  modifiedMs: 0,
});
const fileEntry = (name: string, path: string): LibraryEntry => ({
  name,
  path,
  kind: "audio",
  ext: "wav",
  sizeBytes: 100,
  modifiedMs: 0,
});

const invokes = {
  libraryScan: vi.fn(async (dir: string): Promise<LibraryEntry[]> =>
    dir === "/lib" ? [dirEntry("drums", "/lib/drums"), fileEntry("kick.wav", "/lib/kick.wav")] : [],
  ),
  libraryDefaultRoot: vi.fn(async () => "/lib"),
  libraryAudition: vi.fn(async (_p: string, _s?: number | null) => {}),
  libraryAuditionStop: vi.fn(async () => {}),
};

const mockBackend = { mode: "tauri" as const, on: () => () => {}, ...invokes };
vi.mock("../tauri", () => ({ backend: mockBackend }));

const { prefs } = await import("../prefs/prefs.svelte");
const { library } = await import("./library.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  prefs.set("librarySampleFolders", []);
  library.reset();
});

describe("library store", () => {
  it("resolves the default root once and lists it first among the folders", async () => {
    await library.init();
    expect(invokes.libraryDefaultRoot).toHaveBeenCalledTimes(1);
    prefs.set("librarySampleFolders", ["/extra", "/lib"]);
    expect(library.folders).toEqual(["/lib", "/extra"]);

    await library.init();
    expect(invokes.libraryDefaultRoot).toHaveBeenCalledTimes(1);
  });

  it("opens a folder, exposes its entries, and serves a re-open from cache", async () => {
    await library.init();
    await library.open("/lib");
    expect(library.cwd).toBe("/lib");
    expect(library.entries.map((e) => e.name)).toEqual(["drums", "kick.wav"]);
    expect(library.loading).toBe(false);
    expect(library.error).toBeNull();

    await library.open("/lib/drums");
    await library.open("/lib");
    expect(invokes.libraryScan).toHaveBeenCalledTimes(2);
  });

  it("refresh() busts the cache for the current folder", async () => {
    await library.init();
    await library.open("/lib");
    await library.refresh();
    expect(invokes.libraryScan).toHaveBeenCalledTimes(2);
  });

  it("surfaces a scan failure as an error and leaves the entries empty", async () => {
    invokes.libraryScan.mockRejectedValueOnce(new Error("cannot read /nope"));
    await library.open("/nope");
    expect(library.error).toContain("cannot read /nope");
    expect(library.entries).toEqual([]);
    expect(library.loading).toBe(false);
  });

  it("up() walks to the parent and clears at a configured root", async () => {
    await library.init();
    await library.open("/lib/drums");
    await library.up();
    expect(library.cwd).toBe("/lib");
    await library.up();
    expect(library.cwd).toBeNull();
  });

  it("audition() plays a file and records which one is sounding", async () => {
    await library.audition("/lib/kick.wav");
    expect(invokes.libraryAudition).toHaveBeenCalledWith("/lib/kick.wav", null);
    expect(library.auditioning).toBe("/lib/kick.wav");

    await library.stopAudition();
    expect(invokes.libraryAuditionStop).toHaveBeenCalledTimes(1);
    expect(library.auditioning).toBe("");
  });

  it("clears the auditioning marker when the backend rejects", async () => {
    invokes.libraryAudition.mockRejectedValueOnce(new Error("no output device"));
    await library.audition("/lib/kick.wav");
    expect(library.auditioning).toBe("");
    expect(library.error).toContain("no output device");
  });
});
```

- [ ] **Step 6: Run them to verify they fail**

Run: `timeout 300 npx vitest run src/lib/state/library.test.ts`
Expected: FAIL — cannot resolve `./library.svelte`.

- [ ] **Step 7: Implement the store**

Create `src/lib/state/library.svelte.ts`:

```ts
/**
 * Library & browser panel state (Track E).
 *
 * Chrome only (ADR 0006): every listing comes from `library_scan`, every
 * mutation goes through an existing command via an existing store. The store
 * owns navigation, a per-folder cache, and which file is auditioning.
 *
 * `up()` walking above a configured folder deliberately returns to the ROOTS
 * list rather than to the filesystem parent — the configured folders are what
 * the user chose to see, and browsing out of them sideways is a file manager's
 * job, not a DAW's.
 */

import { backend } from "../tauri";
import { prefs } from "../prefs/prefs.svelte";
import { parentDir } from "../utils/library";
import type { LibraryEntry } from "../types/ipc";

export type LibraryRoot = "samples" | "clips" | "presets";

class LibraryStore {
  /** Which of the three roots the panel shows. */
  root = $state<LibraryRoot>("samples");
  /** Absolute path of the backend's default library dir (null until init). */
  defaultRoot = $state<string | null>(null);
  /** Folder being listed; null = the configured-folders list. */
  cwd = $state<string | null>(null);
  entries = $state<LibraryEntry[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);
  /** Path of the file currently auditioning ("" = nothing). */
  auditioning = $state("");

  /** path → listing. In memory, per session; `refresh()` busts one entry. */
  private cache = new Map<string, LibraryEntry[]>();

  /** False in demo mode (no filesystem) — the panel shows a desktop-only note. */
  get available(): boolean {
    return !!backend.libraryScan;
  }

  /** The default library dir first, then the user's folders (no duplicates). */
  get folders(): string[] {
    const configured = prefs.values.librarySampleFolders;
    if (!this.defaultRoot) return [...configured];
    return [this.defaultRoot, ...configured.filter((p) => p !== this.defaultRoot)];
  }

  /** Resolve the default root once. Safe to call on every panel open. */
  async init(): Promise<void> {
    if (this.defaultRoot !== null || !backend.libraryDefaultRoot) return;
    try {
      this.defaultRoot = await backend.libraryDefaultRoot();
      this.error = null;
    } catch (err) {
      this.error = String(err);
    }
  }

  /** List `path` (cached), making it the current folder. */
  async open(path: string): Promise<void> {
    this.cwd = path;
    const hit = this.cache.get(path);
    if (hit) {
      this.entries = hit;
      this.error = null;
      return;
    }
    if (!backend.libraryScan) return;
    this.loading = true;
    try {
      const listing = await backend.libraryScan(path);
      this.cache.set(path, listing);
      this.entries = listing;
      this.error = null;
    } catch (err) {
      this.entries = [];
      this.error = String(err);
    } finally {
      this.loading = false;
    }
  }

  /** Up one level, or back to the roots list when already at a configured one. */
  async up(): Promise<void> {
    const here = this.cwd;
    if (!here) return;
    const parent = this.folders.includes(here) ? null : parentDir(here);
    if (!parent) {
      this.cwd = null;
      this.entries = [];
      return;
    }
    await this.open(parent);
  }

  /** Re-list the current folder, ignoring (and replacing) the cache. */
  async refresh(): Promise<void> {
    const here = this.cwd;
    if (!here) return;
    this.cache.delete(here);
    await this.open(here);
  }

  /** Audition a file — no document coupling; the previous one is cancelled. */
  async audition(path: string): Promise<void> {
    if (!backend.libraryAudition) return;
    this.auditioning = path;
    try {
      await backend.libraryAudition(path, null);
      this.error = null;
    } catch (err) {
      this.auditioning = "";
      this.error = String(err);
    }
  }

  async stopAudition(): Promise<void> {
    this.auditioning = "";
    try {
      await backend.libraryAuditionStop?.();
    } catch {
      // Stopping a preview that is not running is not worth a message.
    }
  }

  /** Back to a fresh store — tests only; the app never resets the panel. */
  reset(): void {
    this.root = "samples";
    this.defaultRoot = null;
    this.cwd = null;
    this.entries = [];
    this.loading = false;
    this.error = null;
    this.auditioning = "";
    this.cache.clear();
  }
}

export const library = new LibraryStore();
```

- [ ] **Step 8: Run the tests to verify they pass**

```bash
timeout 300 npx vitest run
npm run check
```
Expected: 220 frontend tests green (210 + 3 path + 7 store), `svelte-check`
clean.

- [ ] **Step 9: Commit**

```bash
git add src/lib/types/ipc.ts src/lib/tauri.ts src/lib/utils/library.ts \
        src/lib/utils/library.test.ts src/lib/state/library.svelte.ts \
        src/lib/state/library.test.ts
git commit -m "feat(library): IPC types, backend adapter and the library store"
```

---

## Task 5: Panel shell, dock tab, and the samples root

**Files:**
- Create: `src/lib/components/library/LibraryPanel.svelte`
- Create: `src/lib/components/library/LibraryRow.svelte`
- Create: `src/lib/components/library/SamplesRoot.svelte`
- Modify: `src/lib/state/ui.svelte.ts`
- Modify: `src/lib/components/Dock.svelte`
- Modify: `src/lib/components/TransportBar.svelte`

**Interfaces:**
- Consumes: Task 4's `library` store; the landed `ui.dock` / `toggleDock`.
- Produces:
  - `DockTab` gains `"library"`.
  - `LibraryRow` props:
    ```ts
    let {
      label,          // string  — primary text
      meta = "",      // string  — right-aligned dim text
      icon = "",      // string  — leading glyph
      active = false, // boolean — highlight (auditioning / selected)
      draggable = false,
      onclick = undefined,       // () => void
      ondragstart = undefined,   // (e: DragEvent) => void
    }: LibraryRowProps = $props();
    ```

**Testing note:** vitest runs in the `node` environment and this repo tests
stores, not components (`vitest.config.ts`'s own comment). This task therefore
adds no unit test; its verification is `timeout 300 npx vitest run` unchanged
at 220, `npm run check` clean, and the manual smoke below. That is the house
convention, not a gap — do not invent a DOM harness for one panel.

- [ ] **Step 1: Add the dock tab to the UI state**

In `src/lib/state/ui.svelte.ts`:

```ts
export type DockTab = "" | "generate" | "hum" | "library" | "instruments" | "plugins" | "mcp";
```

- [ ] **Step 2: Write `LibraryRow.svelte`**

```svelte
<script lang="ts">
  /**
   * One row of any library root: glyph, label, dim meta, optional drag.
   * Shared by SAMPLES / CLIPS / PRESETS so the three roots cannot drift apart
   * visually or behaviourally.
   */
  interface Props {
    label: string;
    meta?: string;
    icon?: string;
    active?: boolean;
    draggable?: boolean;
    onclick?: () => void;
    ondragstart?: (e: DragEvent) => void;
  }
  let {
    label,
    meta = "",
    icon = "",
    active = false,
    draggable = false,
    onclick,
    ondragstart,
  }: Props = $props();
</script>

<button
  class="row"
  class:on={active}
  {draggable}
  type="button"
  title={label}
  onclick={() => onclick?.()}
  ondragstart={(e) => ondragstart?.(e)}
>
  {#if icon}<span class="icon" aria-hidden="true">{icon}</span>{/if}
  <span class="label">{label}</span>
  {#if meta}<span class="meta silk">{meta}</span>{/if}
</button>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 5px 6px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text-dim);
    text-align: left;
    font: inherit;
    cursor: pointer;
  }
  .row:hover {
    background: rgba(122, 160, 220, 0.08);
    color: var(--text);
  }
  .row.on {
    color: var(--cyan);
    background: rgba(82, 229, 255, 0.08);
  }
  .icon {
    flex: none;
    width: 14px;
    text-align: center;
    opacity: 0.8;
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .meta {
    flex: none;
    letter-spacing: 0.08em;
  }
</style>
```

- [ ] **Step 3: Write `SamplesRoot.svelte`**

```svelte
<script lang="ts">
  /**
   * SAMPLES root: the configured folders (default library dir first) and one
   * directory level at a time. Click an audio row to audition it — the
   * audition never touches the project (no op, no dirty flag).
   *
   * Drag-out is wired in Task 6; this file only renders and auditions.
   */
  import { onMount } from "svelte";
  import { library } from "../../state/library.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import LibraryRow from "./LibraryRow.svelte";

  onMount(() => void library.init());

  function sizeLabel(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} K`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} M`;
  }

  function folderName(path: string): string {
    return path.split(/[/\\]/).filter(Boolean).pop() ?? path;
  }
</script>

{#if !library.available}
  <div class="note silk">the sample library is available in the desktop app</div>
{:else if library.cwd === null}
  <div class="list">
    {#each library.folders as folder (folder)}
      <LibraryRow
        icon="▸"
        label={folderName(folder)}
        meta={folder === library.defaultRoot ? "DEFAULT" : ""}
        onclick={() => void library.open(folder)}
      />
    {/each}
  </div>
  <button class="cfg mono" onclick={() => (prefs.dialogOpen = true)}>+ CONFIGURE FOLDERS</button>
{:else}
  <div class="crumbs">
    <button class="up mono" onclick={() => void library.up()}>↑ UP</button>
    <span class="here silk" title={library.cwd}>{library.cwd}</span>
    <button class="up mono" onclick={() => void library.refresh()} title="Rescan">⟳</button>
  </div>
  {#if library.loading}
    <div class="note silk">scanning…</div>
  {:else if library.error}
    <div class="note err silk">{library.error}</div>
  {:else}
    <div class="list">
      {#each library.entries as e (e.path)}
        <LibraryRow
          icon={e.kind === "dir" ? "▸" : "♪"}
          label={e.name}
          meta={e.kind === "dir" ? "" : sizeLabel(e.sizeBytes)}
          active={library.auditioning === e.path}
          onclick={() =>
            e.kind === "dir" ? void library.open(e.path) : void library.audition(e.path)}
        />
      {:else}
        <div class="note silk">nothing here</div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--glass-border);
    margin-bottom: 6px;
  }
  .here {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .up,
  .cfg {
    flex: none;
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 3px 6px;
    cursor: pointer;
  }
  .up:hover,
  .cfg:hover {
    color: var(--cyan);
    border-color: var(--cyan);
  }
  .cfg {
    margin-top: 8px;
    align-self: flex-start;
  }
  .note {
    padding: 10px 4px;
    letter-spacing: 0.1em;
  }
  .note.err {
    color: var(--red);
  }
</style>
```

- [ ] **Step 4: Write `LibraryPanel.svelte`**

```svelte
<script lang="ts">
  /**
   * Library & browser panel (Track E): three roots — SAMPLES (folders on
   * disk), CLIPS (the open project's clips), PRESETS (instruments and
   * patches). The panel is chrome: it lists, auditions and drags; every
   * mutation it can cause is an existing, channel-routed command, so a drop
   * is undoable for free.
   */
  import { library, type LibraryRoot } from "../../state/library.svelte";
  import SamplesRoot from "./SamplesRoot.svelte";

  const ROOTS: { id: LibraryRoot; label: string }[] = [
    { id: "samples", label: "SAMPLES" },
    { id: "clips", label: "CLIPS" },
    { id: "presets", label: "PRESETS" },
  ];
</script>

<div class="panel">
  <div class="roots" role="tablist">
    {#each ROOTS as r (r.id)}
      <button
        class="root mono"
        class:on={library.root === r.id}
        role="tab"
        aria-selected={library.root === r.id}
        onclick={() => (library.root = r.id)}
      >
        {r.label}
      </button>
    {/each}
  </div>

  <div class="body">
    {#if library.root === "samples"}
      <SamplesRoot />
    {/if}
  </div>
</div>

<style>
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .roots {
    flex: none;
    display: flex;
    gap: 4px;
  }
  .root {
    flex: 1;
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 4px 2px;
    cursor: pointer;
  }
  .root.on {
    color: var(--cyan);
    border-color: var(--cyan);
    background: rgba(82, 229, 255, 0.06);
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
</style>
```

(The CLIPS and PRESETS branches are added by Tasks 7 and 8; until then those
tabs render nothing, which is deliberate — each task lands a working root.)

- [ ] **Step 5: Mount it in the dock**

In `src/lib/components/Dock.svelte`: import
`LibraryPanel from "./library/LibraryPanel.svelte"`, add to `TABS` (between
`hum` and `instruments`):

```ts
    { id: "library", label: "LIBRARY" },
```

and add the render branch (before the `instruments` branch):

```svelte
      {:else if ui.dock === "library"}
        <LibraryPanel />
```

- [ ] **Step 6: Add the transport-bar chip**

In `src/lib/components/TransportBar.svelte`, in the right-item list before the
`inst` entry:

```ts
    {
      id: "lib",
      kind: "chip",
      priority: 5,
      label: "LIB",
      title: "Library — samples, project clips, presets",
      on: ui.dock === "library",
      onClick: () => toggleDock("library"),
    },
```

Priority 5 sits below `plug` (6), so LIB is the first dock chip to collapse
into the ⋯ menu on a narrow window (PR #16's overflow behaviour, unchanged).

- [ ] **Step 7: Verify**

```bash
timeout 300 npx vitest run
npm run check
```
Expected: 220 green (unchanged — this task adds no test), `svelte-check` clean.

Smoke it manually (the `run-aura` skill covers launching): open the LIB chip,
see the default library folder, enter it, click a `.wav`, hear it; click
another and hear the first one cut off.

- [ ] **Step 8: Commit**

```bash
git add src/lib/components/library src/lib/state/ui.svelte.ts \
        src/lib/components/Dock.svelte src/lib/components/TransportBar.svelte
git commit -m "feat(library): panel shell, dock tab and the samples root"
```

---

## Task 6: Drag a sample onto a track → `import_audio_clip`

**Files:**
- Modify: `src/lib/utils/library.ts` (drag payload + codec)
- Modify: `src/lib/utils/library.test.ts`
- Modify: `src/lib/state/library.svelte.ts` (`dropOnTrack`)
- Modify: `src/lib/state/library.test.ts`
- Modify: `src/lib/components/library/SamplesRoot.svelte` (drag source)
- Modify: `src/lib/components/Timeline.svelte` (drop target)

**Interfaces:**
- Consumes: `backend.importAudioClip` (landed, channel-routed, undoable);
  `view.samplesAt`/`view.snapSamples`; `canvasPos`; `project.reload`;
  `toasts`.
- Produces:
  ```ts
  // src/lib/utils/library.ts
  export const LIBRARY_DRAG_MIME = "application/x-aura-library";
  export type LibraryDragPayload =
    | { kind: "sampleFile"; path: string; name: string }
    | { kind: "projectAudioClip"; clipId: string }
    | { kind: "projectMidiClip"; clipId: string }
    | { kind: "samplerInstrument"; instrumentId: string; name: string }
    | { kind: "zynPatch"; patch: ZynPatch };
  export function encodeLibraryDrag(dt: DragWriter, payload: LibraryDragPayload): void;
  export function hasLibraryDrag(dt: { types: readonly string[] } | null | undefined): boolean;
  export function decodeLibraryDrag(dt: DragReader | null | undefined): LibraryDragPayload | null;

  // src/lib/state/library.svelte.ts
  async dropOnTrack(payload: LibraryDragPayload, trackId: string, atSamples: number): Promise<void>;
  ```
  The full five-variant union is declared **now**, in this task, so Tasks 7
  and 8 add only their `dropOnTrack` branches and their drag sources.

**Why two functions for one MIME type:** during `dragover` browsers expose
`dataTransfer.types` but withhold `getData()` (drag-data protected mode). The
drop-target uses `hasLibraryDrag` to decide whether to accept, and
`decodeLibraryDrag` only in the `drop` handler, where the data is readable.

- [ ] **Step 1: Write the failing codec tests**

Append to `src/lib/utils/library.test.ts`:

```ts
import {
  LIBRARY_DRAG_MIME,
  decodeLibraryDrag,
  encodeLibraryDrag,
  hasLibraryDrag,
} from "./library";

/** Minimal stand-in for DataTransfer (vitest runs in "node" — no DOM). */
function fakeDataTransfer() {
  const store = new Map<string, string>();
  return {
    types: [] as string[],
    setData(type: string, data: string) {
      store.set(type, data);
      if (!this.types.includes(type)) this.types.push(type);
    },
    getData(type: string) {
      return store.get(type) ?? "";
    },
    effectAllowed: "" as string,
  };
}

describe("library drag payloads", () => {
  it("round-trips a sample-file payload", () => {
    const dt = fakeDataTransfer();
    encodeLibraryDrag(dt, { kind: "sampleFile", path: "/lib/kick.wav", name: "kick.wav" });
    expect(dt.types).toContain(LIBRARY_DRAG_MIME);
    expect(hasLibraryDrag(dt)).toBe(true);
    expect(decodeLibraryDrag(dt)).toEqual({
      kind: "sampleFile",
      path: "/lib/kick.wav",
      name: "kick.wav",
    });
  });

  it("round-trips every payload kind", () => {
    const payloads = [
      { kind: "sampleFile", path: "/a.wav", name: "a.wav" },
      { kind: "projectAudioClip", clipId: "c1" },
      { kind: "projectMidiClip", clipId: "m1" },
      { kind: "samplerInstrument", instrumentId: "i1", name: "Piano" },
      { kind: "zynPatch", patch: { bank: "Arpeggios", name: "Arp 1", program: 1, path: "/a.xiz" } },
    ] as const;
    for (const p of payloads) {
      const dt = fakeDataTransfer();
      encodeLibraryDrag(dt, p);
      expect(decodeLibraryDrag(dt)).toEqual(p);
    }
  });

  it("ignores foreign drags — an OS file drop is never mistaken for ours", () => {
    const dt = fakeDataTransfer();
    dt.types.push("Files");
    expect(hasLibraryDrag(dt)).toBe(false);
    expect(decodeLibraryDrag(dt)).toBeNull();
    expect(hasLibraryDrag(null)).toBe(false);
    expect(decodeLibraryDrag(null)).toBeNull();
  });

  it("refuses malformed or unknown payloads instead of throwing", () => {
    const bad = fakeDataTransfer();
    bad.setData(LIBRARY_DRAG_MIME, "{not json");
    expect(decodeLibraryDrag(bad)).toBeNull();

    const unknown = fakeDataTransfer();
    unknown.setData(LIBRARY_DRAG_MIME, JSON.stringify({ kind: "somethingElse" }));
    expect(decodeLibraryDrag(unknown)).toBeNull();
  });
});
```

- [ ] **Step 2: Run to verify failure**

Run: `timeout 300 npx vitest run src/lib/utils/library.test.ts`
Expected: FAIL — `encodeLibraryDrag` is not exported.

- [ ] **Step 3: Implement the codec**

Append to `src/lib/utils/library.ts`:

```ts
import type { ZynPatch } from "../types/ipc";

/**
 * Private MIME type for in-app library drags. Deliberately NOT "Files":
 * `ImportDropZone.svelte` keys its OS-file handlers on
 * `dataTransfer.types.includes("Files")`, so an in-app drag never raises the
 * import overlay and the two paths cannot collide.
 */
export const LIBRARY_DRAG_MIME = "application/x-aura-library";

/** What a library row carries when it is dragged onto a track. */
export type LibraryDragPayload =
  | { kind: "sampleFile"; path: string; name: string }
  | { kind: "projectAudioClip"; clipId: string }
  | { kind: "projectMidiClip"; clipId: string }
  | { kind: "samplerInstrument"; instrumentId: string; name: string }
  | { kind: "zynPatch"; patch: ZynPatch };

const KINDS = [
  "sampleFile",
  "projectAudioClip",
  "projectMidiClip",
  "samplerInstrument",
  "zynPatch",
] as const;

/** Structural slices of DataTransfer, so these are testable without a DOM. */
type DragWriter = { setData(type: string, data: string): void; effectAllowed?: string };
type DragReader = { types: readonly string[]; getData(type: string): string };

export function encodeLibraryDrag(dt: DragWriter, payload: LibraryDragPayload): void {
  dt.setData(LIBRARY_DRAG_MIME, JSON.stringify(payload));
  dt.effectAllowed = "copy";
}

/**
 * True when a drag carries a library payload. This is the ONLY check a
 * `dragover` handler can make: browsers withhold `getData()` until `drop`,
 * exposing just `types` while the drag is in flight.
 */
export function hasLibraryDrag(dt: { types: readonly string[] } | null | undefined): boolean {
  return !!dt && Array.from(dt.types).includes(LIBRARY_DRAG_MIME);
}

/** Read a library payload from a `drop`. Null for anything else — malformed
 * JSON and unknown kinds included; a drop must never throw into the timeline. */
export function decodeLibraryDrag(dt: DragReader | null | undefined): LibraryDragPayload | null {
  if (!hasLibraryDrag(dt)) return null;
  try {
    const parsed: unknown = JSON.parse(dt!.getData(LIBRARY_DRAG_MIME));
    if (!parsed || typeof parsed !== "object") return null;
    const kind = (parsed as { kind?: unknown }).kind;
    if (typeof kind !== "string" || !(KINDS as readonly string[]).includes(kind)) return null;
    return parsed as LibraryDragPayload;
  } catch {
    return null;
  }
}
```

- [ ] **Step 4: Write the failing `dropOnTrack` test**

Append to `src/lib/state/library.test.ts` — extend `invokes` with
`importAudioClip`, `getProjectState`, and add:

```ts
describe("dropOnTrack — sample file", () => {
  it("imports the file onto the target track at the drop position", async () => {
    project.tracks = [
      { id: "t1", name: "Audio 1", kind: "audio", gainDb: 0, pan: 0, muted: false, soloed: false, armed: false, color: "#888888" },
    ];
    await library.dropOnTrack(
      { kind: "sampleFile", path: "/lib/kick.wav", name: "kick.wav" },
      "t1",
      96000,
    );
    expect(invokes.importAudioClip).toHaveBeenCalledWith({
      path: "/lib/kick.wav",
      trackId: "t1",
      atSamples: 96000,
    });
  });

  it("refuses a MIDI track and never calls the import command", async () => {
    project.tracks = [
      { id: "m1", name: "Synth", kind: "midi", gainDb: 0, pan: 0, muted: false, soloed: false, armed: false, color: "#888888" },
    ];
    await library.dropOnTrack(
      { kind: "sampleFile", path: "/lib/kick.wav", name: "kick.wav" },
      "m1",
      0,
    );
    expect(invokes.importAudioClip).not.toHaveBeenCalled();
    expect(lastToast().title).toContain("AUDIO TRACK");
  });
});
```

Add the imports the block needs (`project` from `./project.svelte`, `toasts`
from `./toasts.svelte`, and a `lastToast()` helper copied from
`projectops.test.ts`).

- [ ] **Step 5: Run to verify failure**

Run: `timeout 300 npx vitest run src/lib/state/library.test.ts`
Expected: FAIL — `library.dropOnTrack is not a function`.

- [ ] **Step 6: Implement `dropOnTrack`'s sample-file branch**

In `src/lib/state/library.svelte.ts`, add the imports (`project`, `toasts`,
`LibraryDragPayload`) and the method:

```ts
  /**
   * A library row was dropped on a track. EVERY branch calls an existing,
   * channel-routed command — this store never mutates the document itself, so
   * every drop is undoable for free and adds no op-log exposure.
   */
  async dropOnTrack(
    payload: LibraryDragPayload,
    trackId: string,
    atSamples: number,
  ): Promise<void> {
    const track = project.trackById(trackId);
    if (!track) return;

    switch (payload.kind) {
      case "sampleFile": {
        if (track.kind !== "audio") {
          toasts.info("NEEDS AN AUDIO TRACK", `${payload.name} is audio — drop it on an audio track`);
          return;
        }
        try {
          await backend.importAudioClip({ path: payload.path, trackId, atSamples });
          await project.reload();
        } catch (err) {
          toasts.error("IMPORT FAILED", String(err));
        }
        return;
      }
      default:
        return; // remaining kinds land in Tasks 7 and 8
    }
  }
```

- [ ] **Step 7: Make sample rows draggable**

In `SamplesRoot.svelte`, import the codec and pass a drag handler to audio
rows:

```ts
  import { encodeLibraryDrag } from "../../utils/library";
```

```svelte
        <LibraryRow
          icon={e.kind === "dir" ? "▸" : "♪"}
          label={e.name}
          meta={e.kind === "dir" ? "" : sizeLabel(e.sizeBytes)}
          active={library.auditioning === e.path}
          draggable={e.kind === "audio"}
          ondragstart={(ev) => {
            if (e.kind === "audio" && ev.dataTransfer)
              encodeLibraryDrag(ev.dataTransfer, {
                kind: "sampleFile",
                path: e.path,
                name: e.name,
              });
          }}
          onclick={() =>
            e.kind === "dir" ? void library.open(e.path) : void library.audition(e.path)}
        />
```

- [ ] **Step 8: Add the timeline drop target**

In `src/lib/components/Timeline.svelte`'s `<script>`:

```ts
  import { canvasPos } from "../utils/canvas-pos";
  import { decodeLibraryDrag, hasLibraryDrag } from "../utils/library";
  import { library } from "../state/library.svelte";

  /** Track id currently under a library drag ("" = none). */
  let dropTrackId = $state("");

  /** Drop position in samples. Uses canvasPos (NOT clientX - rect.left) so
   * the mapping is correct under interface zoom — the standing rule for all
   * canvas/lane pointer math. */
  function dropSamples(e: DragEvent): number {
    const { x } = canvasPos(e.currentTarget as HTMLElement, e.clientX, e.clientY);
    return Math.max(0, Math.round(view.snapSamples(view.samplesAt(x))));
  }

  function onLaneDragOver(track: TrackState, e: DragEvent) {
    // `types` is all a dragover may read — see hasLibraryDrag's doc.
    if (!hasLibraryDrag(e.dataTransfer)) return; // OS file drags fall through
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    dropTrackId = track.id;
  }

  function onLaneDrop(track: TrackState, e: DragEvent) {
    dropTrackId = "";
    const payload = decodeLibraryDrag(e.dataTransfer);
    if (!payload) return;
    e.preventDefault();
    void library.dropOnTrack(payload, track.id, dropSamples(e));
  }
```

and on the `.lane` div, alongside the existing `ondblclick`:

```svelte
          class:droptarget={dropTrackId === track.id}
          ondragover={(e) => onLaneDragOver(track, e)}
          ondragleave={() => (dropTrackId = "")}
          ondrop={(e) => onLaneDrop(track, e)}
```

plus the style:

```css
  .lane.droptarget {
    box-shadow: inset 0 0 0 1px var(--cyan);
    background: rgba(82, 229, 255, 0.05);
  }
```

- [ ] **Step 9: Run the tests to verify they pass**

```bash
timeout 300 npx vitest run
npm run check
```
Expected: 226 green (220 + 4 codec + 2 drop), `svelte-check` clean.

- [ ] **Step 10: Manual smoke**

Drag a `.wav` from the panel onto an audio lane; the clip appears at the drop
position. Ctrl+Z removes it (undoable for free — the proof that this went
through the channel). Drag an OS file over the timeline: the old import
overlay still appears, unchanged.

- [ ] **Step 11: Commit**

```bash
git add src/lib/utils/library.ts src/lib/utils/library.test.ts \
        src/lib/state/library.svelte.ts src/lib/state/library.test.ts \
        src/lib/components/library/SamplesRoot.svelte src/lib/components/Timeline.svelte
git commit -m "feat(library): drag a sample onto a track through import_audio_clip"
```

---

## Task 7: Project-clips root

**Files:**
- Create: `src/lib/components/library/ClipsRoot.svelte`
- Modify: `src/lib/components/library/LibraryPanel.svelte`
- Modify: `src/lib/state/midi.svelte.ts` (`stampClipTo`)
- Modify: `src/lib/state/library.svelte.ts` (two `dropOnTrack` branches)
- Modify: `src/lib/state/library.test.ts`

**Interfaces:**
- Consumes: `project.clips`, `project.projectDir`, `midi.clips`,
  `midi.samplesToTicks`, `backend.importAudioClip`, `backend.gestureBegin?` /
  `gestureEnd?`.
- Produces:
  ```ts
  // src/lib/state/midi.svelte.ts
  async stampClipTo(clipId: string, trackId: string, timelineStartTicks: number): Promise<MidiClip | null>;
  ```

**Design notes the implementer needs:**
- An audio project clip's `sourcePath` is **relative to the `.aura` dir**.
  Turning it absolute uses `@tauri-apps/api/path`'s `join` with
  `project.projectDir` — the same import `projectops.defaultParentDir` already
  uses. Re-importing that absolute path is correct and cheap: the backend mints
  the asset's `SourceId` as a UUIDv5 of the normalised path, so re-importing
  the same file reuses the same source identity rather than duplicating audio.
- A MIDI clip copy is `midi_add_clip` + optional `midi_set_clip_bounds` +
  `midi_set_notes` — which is exactly the landed private `midi.stamp` behind
  copy/paste and duplicate. Expose it rather than writing a second stamping
  path (ruling 6). Wrap the call in `gesture_begin`/`gesture_end` so the three
  commits fold into ONE undo entry: `fold_ops` passes non-`Set` ops through
  into the same batch, and a closed gesture becomes one `HistoryEntry`.

- [ ] **Step 1: Write the failing tests**

Append to `src/lib/state/library.test.ts` (extend `invokes` with
`midiAddClip`, `midiSetNotes`, `midiSetClipBounds`, `gestureBegin`,
`gestureEnd`, and mock `@tauri-apps/api/path`'s `join`):

```ts
vi.mock("@tauri-apps/api/path", () => ({
  join: async (...parts: string[]) => parts.join("/"),
}));

describe("dropOnTrack — project clips", () => {
  it("re-imports an audio project clip from its absolute source path", async () => {
    project.projectDir = "/home/u/Music/AURA/Song.aura";
    project.tracks = [audioTrack("t1")];
    project.clips = [
      { id: "c1", trackId: "t1", name: "loop", sourcePath: "audio/abc.wav",
        sourceChannels: 2, sourceSampleRate: 48000, sourceLengthSamples: 48000,
        timelineStartSamples: 0, offsetSamples: 0, lengthSamples: 48000,
        gainDb: 0, fadeInSamples: 0, fadeOutSamples: 0 },
    ];

    await library.dropOnTrack({ kind: "projectAudioClip", clipId: "c1" }, "t1", 24000);

    expect(invokes.importAudioClip).toHaveBeenCalledWith({
      path: "/home/u/Music/AURA/Song.aura/audio/abc.wav",
      trackId: "t1",
      atSamples: 24000,
    });
  });

  it("stamps a MIDI project clip inside ONE gesture, so it is one undo step", async () => {
    project.tracks = [midiTrack("m1")];
    midi.clips = [
      { id: "k1", trackId: "m1", name: "riff", timelineStartTicks: 0, lengthTicks: 1920,
        notes: [{ tick: 0, key: 60, velocity: 100, lengthTicks: 480 }] },
    ];

    await library.dropOnTrack({ kind: "projectMidiClip", clipId: "k1" }, "m1", 0);

    expect(invokes.gestureBegin).toHaveBeenCalledTimes(1);
    expect(invokes.midiAddClip).toHaveBeenCalledTimes(1);
    expect(invokes.midiSetNotes).toHaveBeenCalledTimes(1);
    expect(invokes.gestureEnd).toHaveBeenCalledTimes(1);
  });

  it("closes the gesture even when the stamp fails", async () => {
    project.tracks = [midiTrack("m1")];
    midi.clips = [
      { id: "k1", trackId: "m1", name: "riff", timelineStartTicks: 0, lengthTicks: 1920, notes: [] },
    ];
    invokes.midiAddClip.mockRejectedValueOnce(new Error("boom"));

    await library.dropOnTrack({ kind: "projectMidiClip", clipId: "k1" }, "m1", 0);
    expect(invokes.gestureEnd).toHaveBeenCalledTimes(1);
  });

  it("refuses a MIDI clip on an audio track and an audio clip on a MIDI track", async () => {
    project.tracks = [audioTrack("t1"), midiTrack("m1")];
    midi.clips = [
      { id: "k1", trackId: "m1", name: "riff", timelineStartTicks: 0, lengthTicks: 1920, notes: [] },
    ];
    await library.dropOnTrack({ kind: "projectMidiClip", clipId: "k1" }, "t1", 0);
    expect(invokes.midiAddClip).not.toHaveBeenCalled();
  });
});
```

Add the `audioTrack(id)` / `midiTrack(id)` helpers next to the existing
fixtures at the top of the file.

- [ ] **Step 2: Run to verify failure**

Run: `timeout 300 npx vitest run src/lib/state/library.test.ts`
Expected: FAIL — the `projectAudioClip` branch returns without doing anything.

- [ ] **Step 3: Expose the MIDI stamp**

In `src/lib/state/midi.svelte.ts`, immediately above the private `stamp`:

```ts
  /** Public entry to the copy-stamp behind paste/duplicate: place an
   * independent copy of `clipId` on `trackId` at `timelineStartTicks`. The
   * library panel's project-clips drag lands here, so there is exactly ONE
   * stamping path in the app. */
  async stampClipTo(
    clipId: string,
    trackId: string,
    timelineStartTicks: number,
  ): Promise<MidiClip | null> {
    const src = this.clipById(clipId);
    if (!src) return null;
    return this.stamp(src, trackId, Math.max(0, Math.round(timelineStartTicks)));
  }
```

- [ ] **Step 4: Implement the two `dropOnTrack` branches**

Replace the `default:` arm's neighbours in `library.svelte.ts`:

```ts
      case "projectAudioClip": {
        if (track.kind !== "audio") {
          toasts.info("NEEDS AN AUDIO TRACK", "audio clips drop on audio tracks");
          return;
        }
        const clip = project.clips.find((c) => c.id === payload.clipId);
        if (!clip || !project.projectDir) return;
        try {
          // `sourcePath` is relative to the .aura dir; join it with the
          // platform separator rather than string-concatenating.
          const { join } = await import("@tauri-apps/api/path");
          const abs = await join(project.projectDir, clip.sourcePath);
          await backend.importAudioClip({ path: abs, trackId, atSamples });
          await project.reload();
        } catch (err) {
          toasts.error("CLIP COPY FAILED", String(err));
        }
        return;
      }

      case "projectMidiClip": {
        if (track.kind !== "midi") {
          toasts.info("NEEDS A MIDI TRACK", "MIDI clips drop on MIDI tracks");
          return;
        }
        // One gesture = one undo entry, even though the stamp is up to three
        // commands (add + bounds + notes). `fold_ops` keeps non-Set ops in
        // the same batch, and a closed gesture becomes one HistoryEntry.
        project.beginGesture("drag midi clip");
        try {
          await midi.stampClipTo(payload.clipId, trackId, midi.samplesToTicks(atSamples));
        } finally {
          project.endGesture();
        }
        return;
      }
```

Import `midi` from `./midi.svelte` at the top of the store.

> Import-cycle check: `library.svelte.ts` importing `midi.svelte.ts` is safe —
> `midi` imports `project`, `clipEditLoop` and `transport`, none of which
> import `library`. If `npm run check` reports a cycle, stop and raise it
> rather than papering over it with a dynamic import.

- [ ] **Step 5: Write `ClipsRoot.svelte`**

```svelte
<script lang="ts">
  /**
   * CLIPS root: the open project's own material — audio clips and MIDI clips
   * — draggable back onto any track. A drop re-imports (audio) or stamps a
   * copy (MIDI) through the landed commands, so it is undoable for free.
   */
  import { project } from "../../state/project.svelte";
  import { midi } from "../../state/midi.svelte";
  import { encodeLibraryDrag } from "../../utils/library";
  import LibraryRow from "./LibraryRow.svelte";

  function seconds(samples: number): string {
    return `${(samples / project.sampleRate).toFixed(1)}s`;
  }
</script>

<div class="list">
  {#each project.clips as c (c.id)}
    <LibraryRow
      icon="▤"
      label={c.name}
      meta={seconds(c.lengthSamples)}
      draggable
      ondragstart={(e) =>
        e.dataTransfer &&
        encodeLibraryDrag(e.dataTransfer, { kind: "projectAudioClip", clipId: c.id })}
      onclick={() => project.select(c.id)}
    />
  {/each}
  {#each midi.clips as c (c.id)}
    <LibraryRow
      icon="♫"
      label={c.name}
      meta={`${c.notes.length} n`}
      draggable
      ondragstart={(e) =>
        e.dataTransfer &&
        encodeLibraryDrag(e.dataTransfer, { kind: "projectMidiClip", clipId: c.id })}
      onclick={() => midi.select(c.id)}
    />
  {/each}
  {#if project.clips.length === 0 && midi.clips.length === 0}
    <div class="note silk">no clips in this project yet</div>
  {/if}
</div>

<style>
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .note {
    padding: 10px 4px;
    letter-spacing: 0.1em;
  }
</style>
```

Wire it into `LibraryPanel.svelte`:

```svelte
    {:else if library.root === "clips"}
      <ClipsRoot />
```

- [ ] **Step 6: Run the tests to verify they pass**

```bash
timeout 300 npx vitest run
npm run check
```
Expected: 230 green (226 + 4), `svelte-check` clean.

- [ ] **Step 7: Commit**

```bash
git add src/lib/components/library src/lib/state/midi.svelte.ts \
        src/lib/state/library.svelte.ts src/lib/state/library.test.ts
git commit -m "feat(library): project-clips root with drag-back-onto-tracks"
```

---

## Task 8: Presets root — sampler instruments and Zyn patches

**Files:**
- Create: `src/lib/components/library/PresetsRoot.svelte`
- Modify: `src/lib/components/library/LibraryPanel.svelte`
- Modify: `src/lib/state/library.svelte.ts` (two `dropOnTrack` branches)
- Modify: `src/lib/state/library.test.ts`

**Interfaces:**
- Consumes: `instruments.list` / `instruments.refresh` (from
  `sampler_list_instruments`), `zyn.patches` / `zyn.refresh` / `zyn.load`
  (from `zyn_list_patches` / `zyn_load_patch`), `isZynInstance`,
  `plugins.byId`, `backend.setTrackInstrument`,
  `backend.samplerPreviewNote`.
- Produces: no new exported symbols — two more `dropOnTrack` branches and one
  component.

- [ ] **Step 1: Write the failing tests**

Append to `src/lib/state/library.test.ts` (extend `invokes` with
`setTrackInstrument`, `samplerPreviewNote`, `zynLoadPatch`,
`samplerListInstruments`, `zynListPatches`, `pluginList`):

```ts
describe("dropOnTrack — presets", () => {
  it("binds a sampler instrument to a MIDI track", async () => {
    project.tracks = [midiTrack("m1")];
    await library.dropOnTrack(
      { kind: "samplerInstrument", instrumentId: "inst-1", name: "Piano" },
      "m1",
      0,
    );
    expect(invokes.setTrackInstrument).toHaveBeenCalledWith("m1", "inst-1");
  });

  it("refuses to bind an instrument to an audio track", async () => {
    project.tracks = [audioTrack("t1")];
    await library.dropOnTrack(
      { kind: "samplerInstrument", instrumentId: "inst-1", name: "Piano" },
      "t1",
      0,
    );
    expect(invokes.setTrackInstrument).not.toHaveBeenCalled();
    expect(lastToast().title).toContain("MIDI TRACK");
  });

  it("loads a Zyn patch into the instance the target track already hosts", async () => {
    project.tracks = [{ ...midiTrack("m1"), instrumentId: "plugin:zyn-1" }];
    plugins.instances = [
      { instanceId: "zyn-1", uid: "lv2:http://zynaddsubfx.sf.net", name: "ZynAddSubFX", trackId: "m1", status: "active" },
    ];
    const patch = { bank: "Arpeggios", name: "Arp 1", program: 1, path: "/a.xiz" };

    await library.dropOnTrack({ kind: "zynPatch", patch }, "m1", 0);
    expect(invokes.zynLoadPatch).toHaveBeenCalledWith("zyn-1", "/a.xiz");
  });

  it("toasts instead of instantiating a plugin when the track hosts no Zyn", async () => {
    project.tracks = [midiTrack("m1")];
    plugins.instances = [];
    await library.dropOnTrack(
      { kind: "zynPatch", patch: { bank: "B", name: "P", program: 0, path: "/p.xiz" } },
      "m1",
      0,
    );
    expect(invokes.zynLoadPatch).not.toHaveBeenCalled();
    expect(lastToast().title).toContain("ZYN");
  });
});
```

> The `PluginInstanceInfo` literal must match the landed interface in
> `src/lib/types/ipc.ts` — copy the field set from there rather than from this
> plan if they disagree.

- [ ] **Step 2: Run to verify failure**

Run: `timeout 300 npx vitest run src/lib/state/library.test.ts`
Expected: FAIL — the preset branches do nothing.

- [ ] **Step 3: Implement the two branches**

In `library.svelte.ts` (importing `instruments`, `plugins`, `zyn`,
`isZynInstance`):

```ts
      case "samplerInstrument": {
        if (track.kind !== "midi") {
          toasts.info("NEEDS A MIDI TRACK", `${payload.name} is an instrument — drop it on a MIDI track`);
          return;
        }
        try {
          await backend.setTrackInstrument(trackId, payload.instrumentId);
          project.patchTrackLocal(trackId, { instrumentId: payload.instrumentId });
        } catch (err) {
          toasts.error("BIND FAILED", String(err));
        }
        return;
      }

      case "zynPatch": {
        // A patch needs a live Zyn instance. Instantiating one behind the
        // user's back on a drop would be a surprise mutation — toast instead
        // (ruling 9).
        const ref = track.instrumentId ?? "";
        const instanceId = ref.startsWith("plugin:") ? ref.slice("plugin:".length) : "";
        if (!instanceId || !isZynInstance(plugins.byId(instanceId))) {
          toasts.info("NO ZYN ON THIS TRACK", "bind a ZynAddSubFX instance first, then drop the patch");
          return;
        }
        const ok = await zyn.load(instanceId, payload.patch);
        if (!ok) toasts.error("PATCH LOAD FAILED", zyn.error ?? "zyn_load_patch failed");
        return;
      }
```

Remove the now-unreachable `default:` arm (every union member is handled;
TypeScript's exhaustiveness check will confirm).

- [ ] **Step 4: Write `PresetsRoot.svelte`**

```svelte
<script lang="ts">
  /**
   * PRESETS root: loaded sampler instruments (sampler_list_instruments) and
   * ZynAddSubFX bank patches (zyn_list_patches). Drag onto a MIDI track to
   * apply — instruments bind through set_track_instrument, patches load into
   * the Zyn instance the track already hosts. Click an instrument to audition
   * middle C.
   */
  import { onMount } from "svelte";
  import { instruments } from "../../state/instruments.svelte";
  import { zyn } from "../../state/zynpatches.svelte";
  import { backend } from "../../tauri";
  import { encodeLibraryDrag } from "../../utils/library";
  import LibraryRow from "./LibraryRow.svelte";

  let section = $state<"instruments" | "patches">("instruments");
  let filter = $state("");

  onMount(() => {
    void instruments.refresh();
    if (zyn.patches.length === 0) void zyn.refresh();
  });

  const shownPatches = $derived(
    filter
      ? zyn.patches.filter((p) =>
          `${p.bank} ${p.name}`.toLowerCase().includes(filter.toLowerCase()),
        )
      : zyn.patches,
  );
</script>

<div class="switch">
  <button class="s mono" class:on={section === "instruments"} onclick={() => (section = "instruments")}>
    SAMPLER
  </button>
  <button class="s mono" class:on={section === "patches"} onclick={() => (section = "patches")}>
    ZYN
  </button>
</div>

{#if section === "instruments"}
  <div class="list">
    {#each instruments.list as i (i.id)}
      <LibraryRow
        icon="◈"
        label={i.name}
        meta={`${i.regionCount}`}
        draggable
        ondragstart={(e) =>
          e.dataTransfer &&
          encodeLibraryDrag(e.dataTransfer, {
            kind: "samplerInstrument",
            instrumentId: i.id,
            name: i.name,
          })}
        onclick={() => void backend.samplerPreviewNote(i.id, 60, 100)}
      />
    {:else}
      <div class="note silk">no instruments loaded</div>
    {/each}
  </div>
{:else}
  <input class="filter" placeholder="filter patches…" bind:value={filter} />
  <div class="list">
    {#each shownPatches as p (p.path)}
      <LibraryRow
        icon="◆"
        label={p.name}
        meta={p.bank}
        draggable
        ondragstart={(e) =>
          e.dataTransfer && encodeLibraryDrag(e.dataTransfer, { kind: "zynPatch", patch: p })}
      />
    {:else}
      <div class="note silk">no Zyn patches found</div>
    {/each}
  </div>
{/if}

<style>
  .switch {
    flex: none;
    display: flex;
    gap: 4px;
    margin-bottom: 6px;
  }
  .s {
    flex: 1;
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 3px 2px;
    cursor: pointer;
  }
  .s.on {
    color: var(--cyan);
    border-color: var(--cyan);
  }
  .filter {
    flex: none;
    margin-bottom: 6px;
    padding: 4px 6px;
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.25);
    color: var(--text);
    font: inherit;
    font-size: 11px;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .note {
    padding: 10px 4px;
    letter-spacing: 0.1em;
  }
</style>
```

Wire it into `LibraryPanel.svelte`:

```svelte
    {:else if library.root === "presets"}
      <PresetsRoot />
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
timeout 300 npx vitest run
npm run check
```
Expected: 234 green (230 + 4), `svelte-check` clean.

- [ ] **Step 6: Manual smoke**

Open PRESETS, click a sampler instrument (hear middle C), drag it onto a MIDI
track (the track's instrument changes), then drag a Zyn patch onto a track
without a Zyn instance (a toast, no mutation).

- [ ] **Step 7: Commit**

```bash
git add src/lib/components/library src/lib/state/library.svelte.ts \
        src/lib/state/library.test.ts
git commit -m "feat(library): presets root — sampler instruments and Zyn patches"
```

---

## Task 9: Documentation, dated test counts, and the backlog/next-prompt update

**Files:**
- Modify: `README.md`
- Modify: `CONTRIBUTING.md`
- Modify: `docs/backlog/library-and-browser.md`
- Modify: `next-prompt.md`

**Interfaces:** none — paperwork, per the dated-count convention.

- [ ] **Step 1: Run both suites and record the exact numbers**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```

Write down the two totals from the output. Do **not** copy the numbers this
plan predicts (527+10 backend, 234 frontend) — those are estimates; the suite
output is the truth. If either differs from the prediction by more than the
tests this plan added, find out why before continuing.

- [ ] **Step 2: Update the dated counts**

- `README.md:389` — `cd src-tauri && cargo test    # <N> tests (counted 2026-08-14): …`
- `README.md:390` — `npm test                      # <M> frontend unit tests (counted 2026-08-14; vitest): …`
- `CONTRIBUTING.md:62` — `cd src-tauri && cargo test   # <N> tests (counted 2026-08-14)`

Keep the trailing descriptions; add "library scan/audition" to the backend
list and "library store" to the frontend list.

- [ ] **Step 3: Document the panel in `README.md`**

Add a short subsection near the instrument-browser material (README ~line 144),
no screenshot required:

```markdown
### Library & browser panel

The **LIB** dock tab browses three roots: **SAMPLES** (the default AURA
library folder plus any folders added under Preferences → LIBRARY; click a file
to audition it without touching the project), **CLIPS** (the open project's
audio and MIDI clips), and **PRESETS** (loaded sampler instruments and
ZynAddSubFX bank patches). Drag any row onto a track and it lands through the
ordinary import/clip-add commands — so every drop is a normal, undoable edit.
Directory listing and decoding happen in the Rust backend (`library_scan`,
`library_audition`); the panel is chrome.
```

- [ ] **Step 4: Update the backlog doc**

In `docs/backlog/library-and-browser.md`, mark the "Suggested cut" items 1-3
as landed (dated 2026-08-14, branch `library-browser`) and rewrite item 4 plus
the "Later" list as the explicit deferrals this plan recorded: recursive
background scan with events, header-only metadata probe
(duration/channels/rate), waveform thumbnails, browsing `.sfz`/`.xiz` on disk,
dragging SMF in from the panel, tagging/favourites/search, and dragging clips
out to the OS. Each one line, each marked deferred — not silent.

- [ ] **Step 5: Update `next-prompt.md`'s Track E entry**

Replace the Track E section's scope paragraph with a landed-status paragraph:
what shipped, the four additive commands, the `pathList` preference kind, and
the deferral list pointer. Keep the file's existing tone and the "trust files
over this file" caveat. Note that the plan document is
`docs/superpowers/plans/2026-08-14-library-browser.md` and that the scope
rulings belong in `docs/PHASE4-PLAN.md`'s Track E handoff when one is written.

- [ ] **Step 6: Verify the suites one last time**

```bash
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
npm run check
```
Expected: the numbers written in Step 2, all green.

- [ ] **Step 7: Commit**

```bash
git add README.md CONTRIBUTING.md docs/backlog/library-and-browser.md next-prompt.md
git commit -m "docs(library): panel documentation, dated test counts, backlog deferrals"
```

---

## Self-review notes (writing-plans skill, run against the spec before Task 1 starts)

1. **Spec coverage** — every element of `docs/backlog/library-and-browser.md`
   maps to a task or a recorded deferral:
   - "Samples — user-configured folders (plus a default library dir)" → Tasks
     1, 3, 4, 5.
   - "audition preview … reuse the sampler-preview voice path" → Task 2 (and
     ruling 8 on why nothing is resampled).
   - "Project clips … draggable back onto tracks" → Task 7.
   - "Presets/instruments … `zyn_list_patches` / `sampler_list_instruments`" →
     Task 8.
   - "Drag out of the panel … the existing import/clip-add commands" → Tasks
     6, 7, 8; ruling 6 makes it binding.
   - "browsing/scanning backend commands (`library_scan(dir)`, cached listing
     with file metadata)" → Task 1; the cache is the panel's per-folder
     in-memory map (Task 4), not a persisted backend cache — recorded in ruling
     1's deferral.
   - "waveform thumbnails can reuse the pyramid cache" → **deferred**, ruling 2.
   - "Config: library folder list is app config … NOT project state" → ruling
     4, Task 3.
   - Backlog cut item 4 ("Folders config UI + persistence; thumbnails") →
     folders config UI + persistence land in Task 3; thumbnails deferred.
   - Backlog "Later" (tagging/favourites, search, SMF drag-in, drag out to the
     OS) → deferred, recorded in Task 9 step 4. Search exists only as the
     PRESETS patch filter, which is local UI, not a library index.
2. **Where the backlog and a standing constraint disagree**, recorded rather
   than silently deviated: the backlog offers "prefs-side **or** backend config
   file" for the folder list; the track brief says prefs — ruling 4 takes prefs
   and says why. The backlog's "cached listing with file metadata" reads like a
   backend cache; ruling 1 takes a frontend per-folder cache and records the
   backend cache as deferred.
3. **Type consistency check.** `LibraryEntry`'s Rust fields
   (`name`/`path`/`kind`/`ext`/`size_bytes`/`modified_ms`, `rename_all =
   "camelCase"`) match the TS interface's
   `name`/`path`/`kind`/`ext`/`sizeBytes`/`modifiedMs` (Task 1 ↔ Task 4).
   `LibraryDragPayload`'s five variants are declared once in Task 6 and
   consumed unchanged by Tasks 7/8. `dropOnTrack(payload, trackId, atSamples)`
   has one signature across Tasks 6/7/8. `play_held` (Task 2) is the name used
   by `library_audition`, not `playHeld`/`play_sustained`. `stampClipTo` (Task
   7) is the name the store calls. `directoryPicker.pick` (Task 3) is the name
   the dialog calls.
4. **Placeholder scan.** No TBDs. Two places defer to in-repo sources rather
   than restating them, each naming the exact file and symbol: Task 2's
   `create_project_epoch` call (copy from `pure_readers.rs`'s existing test)
   and Task 8's `PluginInstanceInfo` literal (copy from `types/ipc.ts`). Both
   are "copy this named thing", not "figure it out".
5. **Known risks, named.**
   (a) *Preference-schema blast radius.* Task 3 changes `DefFor`, which every
   preference's type flows through. If `svelte-check` reports errors on the
   existing four preferences after the change, the conditional-type ordering is
   wrong — fix the type, never widen `PrefValues` to `any`.
   (b) *Drag-and-drop under interface zoom.* The drop position goes through
   `canvasPos`, deliberately, because `Timeline.svelte`'s existing
   `onLaneDblClick` uses raw `clientX - rect.left` and is wrong under zoom.
   Fixing `onLaneDblClick` too is a one-line temptation and is **out of scope**
   — record it as a minor in the ledger instead.
   (c) *Gesture folding of a MIDI stamp.* Task 7 relies on a gesture batch
   containing `MidiClipAdd` + `MidiSetNotes` becoming one history entry. The
   mechanism is read from `session.rs:1259` (`fold_ops` passes non-`Set` ops
   through) and `history.rs:130` (`HistoryEntry::from_gesture`). If a manual
   Ctrl+Z after a MIDI-clip drop takes two presses, that is the deviation —
   record it, do not invent a new command to fix it.
   (d) *Audition on a machine with no output device.* `play_held` surfaces the
   error; the store shows it. Do not make audition failures fatal to browsing.
   (e) *`lib.rs`'s FROZEN banner.* Ruling 7 covers the deviation; a reviewer
   unfamiliar with the `midi_input` precedent will flag it — point them at that
   commit.
6. **Ordering sanity.** 1 (scan) and 2 (audition) are the backend, independent
   of each other but both before any frontend work; 3 (prefs) precedes 4 (the
   store reads `prefs.values.librarySampleFolders`); 5 needs 4; 6 needs 5 (the
   drag source is a row in the samples root) and declares the whole payload
   union so 7 and 8 add only branches; 7 and 8 are independent of each other
   and could run in parallel worktrees if desired; 9 is paperwork and must be
   last because it writes the final test counts.

## Execution note

Executed from the `track-e-library` worktree, branch `library-browser` (cut
from `origin/main` at `3340aa8`). Foreground, `timeout`-guarded test runs only.
Never `git stash` bare; never push from the worktree unless the owner asks.

**Rebase note (the four parallel tracks).** This branch's shared surfaces with
Tracks A–D are deliberately tiny and are listed here so a rebase is mechanical:

- `src/lib/state/ui.svelte.ts` — one member added to the `DockTab` union.
- `src/lib/components/Dock.svelte` — one `TABS` entry, one render branch.
- `src/lib/components/TransportBar.svelte` — one chip entry in the right-item
  list.
- `src/lib/prefs/schema.ts` — one category, one def type, one schema entry, one
  `coercePref` case, and the array-copy in `schemaDefaults`.
- `src/lib/components/Timeline.svelte` — three handlers plus one CSS class on
  `.lane`. Track C (multi-clip selection) also lives in this file; if both are
  in flight, land C's selection work first and rebase this branch's lane
  handlers on top — they attach to the same element but touch no shared state.
- `src-tauri/src/lib.rs` — one module declaration and four handler entries in
  their own commented block.

**`App.svelte` is not touched at all**, which is the point: other tracks may
mount their own panels there without ever conflicting with this branch.

Merge `origin/main` in at a task boundary whenever it advances; re-run both
suites after every merge and re-baseline the counts before Task 9 writes them
down.

Execution mode (subagent-driven vs inline) is the owner's call at run start —
recorded here once made: **[filled in at execution]**.
