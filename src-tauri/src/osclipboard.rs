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
//! WHY A WRITE IS NEVER VERIFIED BY READING IT BACK: on a desktop running a
//! clipboard manager (observed with GNOME's `gpaste-daemon`), X11 selection
//! ownership is handed from the writing process to the manager asynchronously
//! — a same-process read immediately after a write can see nothing for as
//! long as ~1.5s after the write returns. A verifying read-back would either
//! block the copy path for that long or produce false negatives, and it
//! would make a write's reported success depend on a clipboard manager's own
//! timing. So `clips_copy`'s frontend caller must keep the payload it just
//! built in memory rather than reading the OS clipboard to confirm it landed.
//! Cross-process reads (a second AURA instance, started after the first one
//! wrote) are NOT subject to this delay in testing — which is the case this
//! module exists to serve.
//!
//! WHAT `Ok(())` FROM `os_clipboard_write_text` ACTUALLY MEANS: that
//! `arboard` accepted the text and this process became the X11 selection
//! owner. It does NOT mean the payload reached a clipboard manager, is
//! durable past this process's next clipboard operation, or will be
//! readable by another process — see the size ceiling below, where the
//! write reports success and the content is then lost in transit anyway.
//!
//! MEASURED SIZE CEILING (this is a property of the TEST ENVIRONMENT, not
//! of this module or of AURA — see below): on X11 with `gpaste-daemon`,
//! text payloads up to ~165KB round-tripped byte-for-byte through a real
//! cross-process read every time tested; at ~200KB and above the write
//! still reports success, but the cross-process read comes back completely
//! EMPTY (not truncated) even after waits up to 10s. In `clips_copy`
//! payload terms (which zero every `noteId`, so this is the payload at its
//! most compact) the marginal cost is ~80 bytes/MIDI note, so the ceiling
//! sits around 2,000-2,500 notes — reachable by ordinary use: one
//! multi-minute recorded MIDI performance in a single clip, or a
//! multi-track song-section selection at a few hundred notes per track.
//!
//! THE CONSEQUENCE THAT MATTERS MOST: because the local write succeeds
//! before the manager handoff is attempted, AURA has already taken
//! selection ownership away from whatever previously held it — including a
//! different application entirely. When the handoff to the manager then
//! fails, that prior clipboard content is gone, not restored. A large copy
//! that silently exceeds this ceiling can therefore destroy clipboard
//! content the user never asked AURA to touch, while reporting success.
//!
//! WHY THIS MODULE DELIBERATELY ADDS NO SIZE THRESHOLD OR REFUSAL: the
//! ceiling above is a property of THIS machine's X11-plus-clipboard-manager
//! combination, not of the OS clipboard in general — a session with no
//! clipboard manager routinely handles multi-megabyte X11 INCR transfers,
//! and macOS/Windows have entirely different limits. Hard-coding one
//! desktop's measurement into a cross-platform text command would produce
//! false refusals where a larger payload would have gone through fine, and
//! false confidence anywhere the real limit is tighter. A size guard is
//! legitimate, but it belongs at the CALLER that knows the payload's
//! meaning — `clips_copy`'s frontend orchestration, which can say "this
//! selection is too large to send to another instance" — not here, where
//! all that's known is a byte length.
//!
//! SCOPING NOTE FOR THE FRONTEND ORCHESTRATION (clips_copy/clips_paste):
//! keeping the just-copied payload in memory for same-instance paste (see
//! above) sidesteps the write-then-read asynchrony, but it does NOT fix
//! cross-instance paste of a large selection — the size ceiling above still
//! applies to that path, which is this feature's whole reason to exist.
//! Treat the in-memory payload as a same-instance MITIGATION, not as a fix
//! for the cross-instance ceiling.
//!
//! WHY NOT UNIT-TESTED AGAINST A REAL CLIPBOARD: a headless test environment
//! has none, so a test here would either be skipped or flaky. The CODEC on
//! both sides (`control::clipboard`'s payload tests,
//! `src/lib/utils/aura-clips.test.ts`) carries that coverage. The one thing
//! that IS a real branch — how an `arboard::Error` maps onto this command's
//! `Result<String, String>` — is pure and is tested below without touching
//! a clipboard at all.

/// Offer `text` to the OS clipboard: `arboard` accepts it and this process
/// becomes the X11 selection owner. This does NOT assert that the text
/// reaches a clipboard manager or will be readable by another process — see
/// the module doc's measured size ceiling, where that handoff is exactly
/// what fails while this call still reports success.
#[tauri::command]
pub fn os_clipboard_write_text(text: String) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    cb.set_text(text).map_err(|e| format!("clipboard write failed: {e}"))
}

/// Read the OS clipboard's text slot. An empty or unavailable-format
/// clipboard is an empty string, not an error — "nothing to paste" is a
/// normal state and the caller falls back to its in-memory clipboard. Every
/// OTHER clipboard fault (occupied by another party, unsupported on this
/// session, a failed conversion, or an opaque backend error) is reported,
/// not swallowed — see `map_read_result` for why collapsing them all to
/// empty was wrong.
#[tauri::command]
pub fn os_clipboard_read_text() -> Result<String, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("clipboard unavailable: {e}"))?;
    map_read_result(cb.get_text())
}

/// The `arboard::Error` → command-result mapping, pulled out of
/// `os_clipboard_read_text` so it can be tested without a clipboard.
/// `ContentNotAvailable` is the only variant that means "nothing to paste" —
/// everything else is a real fault (most reachably `ClipboardOccupied`,
/// which a running clipboard manager makes MORE likely, not less: a paste
/// that lands mid-handoff must not be told "nothing was copied").
fn map_read_result(result: Result<String, arboard::Error>) -> Result<String, String> {
    match result {
        Ok(text) => Ok(text),
        Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
        Err(e) => Err(format!("clipboard read failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_not_available_is_the_only_silent_empty_string() {
        assert_eq!(map_read_result(Ok("hi".into())), Ok("hi".to_string()));
        assert_eq!(map_read_result(Err(arboard::Error::ContentNotAvailable)), Ok(String::new()));
    }

    #[test]
    fn every_other_clipboard_fault_is_reported_not_swallowed() {
        for err in [
            arboard::Error::ClipboardOccupied,
            arboard::Error::ClipboardNotSupported,
            arboard::Error::ConversionFailure,
            arboard::Error::Unknown { description: "boom".into() },
        ] {
            let msg = format!("{err}");
            let mapped = map_read_result(Err(err));
            assert!(mapped.is_err(), "expected an error for {msg:?}, got {mapped:?}");
            assert!(
                mapped.unwrap_err().contains(&msg),
                "mapped error should carry the underlying description"
            );
        }
    }
}
