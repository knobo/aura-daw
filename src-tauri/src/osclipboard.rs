//! The OS clipboard's TEXT slot, for cross-instance clip copy/paste.
//!
//! WHY A COMMAND AND NOT THE WEBVIEW: `navigator.clipboard.readText()` needs
//! a `clipboard-read` permission WebKitGTK does not grant, and reading is
//! precisely the cross-instance case this exists for. WHY NOT
//! `tauri-plugin-clipboard-manager`: registering a plugin requires an edit
//! to `capabilities/default.json`, which is a frozen file.
//!
//! WHY TEXT ONLY: the slot a Tauri v2 desktop app can portably own is plain
//! text. The `application/x-aura-clips` MIME name therefore rides INSIDE the
//! JSON envelope (see `control::clipboard`), not as a clipboard flavor —
//! which is also why SMF interchange is an export-to-file action rather than
//! a second clipboard flavor.
//!
//! NOT UNIT-TESTED on purpose: a headless test environment has no clipboard,
//! so a test here would either be skipped or flaky. The CODEC on both sides
//! (`control::clipboard`'s payload tests, `src/lib/utils/aura-clips.test.ts`)
//! is what carries the coverage.

/// Put `text` on the OS clipboard.
#[tauri::command]
pub fn os_clipboard_write_text(text: String) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    cb.set_text(text).map_err(|e| format!("clipboard write failed: {e}"))
}

/// Read the OS clipboard's text slot. An empty or non-text clipboard is an
/// empty string, not an error — "nothing to paste" is a normal state and the
/// caller falls back to its in-memory clipboard.
#[tauri::command]
pub fn os_clipboard_read_text() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    Ok(cb.get_text().unwrap_or_default())
}
