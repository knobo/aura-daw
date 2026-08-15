/**
 * Multi-clip copy/paste orchestration.
 *
 * The PAYLOAD is built and consumed backend-side (`clips_copy`/`clips_paste`)
 * — that is what keeps the in-app and cross-instance paths byte-identical
 * and keeps every placement decision out of the renderer (ADR 0006). This
 * store only routes: it splits the selection into two id lists, parks the
 * payload in memory, mirrors it onto the OS clipboard, and on paste PREFERS
 * a valid AURA envelope found on the OS clipboard (so a copy in one AURA
 * instance pastes in another) over its own memory.
 *
 * Never round-trips through the OS clipboard to CONFIRM a copy
 * (`osclipboard.rs`'s module doc: a same-process write can be invisible to a
 * same-process read for up to ~1.5s under a clipboard manager) — the payload
 * `clipsCopy` returns is kept in memory directly. That in-memory copy is a
 * same-instance mitigation only: it does not fix cross-instance paste of a
 * selection that exceeds the measured OS-clipboard size ceiling (same doc),
 * which is what a failed large copy silently drops (and can take a PRIOR,
 * unrelated clipboard owner's content down with it — see the copy-time
 * toast below).
 *
 * `paste()` ALWAYS tries the OS clipboard before falling back to the
 * in-memory payload — the caller must never gate that scan behind "is
 * anything selected right now", because a fresh window has nothing
 * selected and nothing in memory, and that is exactly the cross-instance
 * case this store exists to serve (fix round 1, plan defect #14: the
 * previous caller-side guard skipped the OS-clipboard read entirely in
 * that window). `paste()` reports back whether it found a payload
 * ANYWHERE, so a caller knows whether to fall back to the legacy
 * single-clip stamp instead — not whether the paste fully succeeded.
 *
 * `pasteAtPlayhead` is the one Ctrl+V entry point and OWNS that fallback:
 * it is the only place that may show a "nothing to paste" toast, and only
 * AFTER the legacy stamp has also come back empty (fix round 2). `paste()`
 * itself must never toast on "nothing found" — a MIDI take recording
 * (`midi.adoptTake` → `midi.select`) sets `midi.selectedClipId` without
 * touching `clipSelection`, so `clipSelection.count()` can be 0 while
 * `midi.clipboard` (filled by the legacy `Ctrl+C` branch) is not; `paste()`
 * has no way to know a legacy fallback exists, so toasting from inside it
 * produced a real "NOTHING TO PASTE" toast in the same keystroke that the
 * legacy stamp silently succeeded — the user watched a clip land while
 * being told there was nothing to paste.
 */

import { backend } from "../tauri";
import { encodeAuraClips, parseAuraClips } from "../utils/aura-clips";
import { clipSelection } from "./clip-selection.svelte";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import { toasts } from "./toasts.svelte";
import type { AuraClipsPayload } from "../types/ipc";

/** Above this many BYTES (UTF-8 — a clip/track name may be CJK or emoji,
 * where `.length`'s UTF-16 code units undercount), a copy that must ride the
 * OS clipboard is told the truth up front rather than silently risking the
 * measured cross-instance ceiling (osclipboard.rs: ~165KB round-trips
 * reliably, ~200KB+ reports write-success and then vanishes in transit —
 * total loss, not truncation). The in-memory payload still lets the SAME
 * instance paste past this size; only the cross-instance OS-clipboard path
 * is at risk, so the write is still attempted and only a toast warns of the
 * risk — this store must never refuse a paste the user can still get in the
 * same window. */
const OS_CLIPBOARD_RISK_BYTES = 165_000;

/** UTF-8 byte length, not `.length`'s UTF-16 code units — a clip or track
 * name is user text, and CJK/emoji names are 3-4 bytes per code unit, which
 * would otherwise make the size guard warn later than the actual transfer
 * risk. Exported for its own unit test (fix round 1, minor 1). */
export function payloadByteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

class ClipClipboardStore {
  /** In-memory fallback: what THIS window last copied. The OS clipboard is
   * the cross-instance channel; this is the one that always works. */
  payload = $state<AuraClipsPayload | null>(null);

  /** Monotonic token guarding against two rapid Ctrl+C presses resolving
   * out of order (fix round 1, minor 4): without it, a slower FIRST copy
   * that resolves AFTER a faster second one would overwrite `payload` with
   * the stale selection. Only the call that is still the most-recently-
   * STARTED one when its backend round trip resolves gets to apply its
   * result — an older one that resolves late is discarded, not applied. */
  private copySeq = 0;

  async copy(): Promise<void> {
    const audioIds = clipSelection.audioIds();
    const midiIds = clipSelection.midiIds();
    if (audioIds.length === 0 && midiIds.length === 0) return;
    const seq = ++this.copySeq;
    try {
      const p = await backend.clipsCopy?.(audioIds, midiIds);
      if (!p || seq !== this.copySeq) return;
      this.payload = p;
      const encoded = encodeAuraClips(p);
      try {
        await backend.osClipboardWriteText?.(encoded);
        if (seq === this.copySeq && payloadByteLength(encoded) > OS_CLIPBOARD_RISK_BYTES) {
          // "Ok" from the write above only means arboard accepted the text
          // and this process took selection ownership — NOT that another
          // AURA instance will ever see it (osclipboard.rs's measured
          // ceiling). The harm worth naming is not "this copy replaced the
          // old clipboard" (true of every copy, and read as boilerplate) —
          // it is that if the cross-instance transfer fails, the SYSTEM
          // clipboard ends up with NEITHER the old content NOR this one:
          // not a truncated paste, an empty one, and the loss reaches
          // outside AURA — any other application that pastes afterwards
          // gets nothing. Same-instance paste is unaffected (it reads
          // `this.payload`).
          toasts.info(
            "COPY MAY NOT REACH ANOTHER WINDOW",
            `${audioIds.length + midiIds.length} clips is a lot of data for the OS clipboard — pasting HERE will work regardless, ` +
              "but the transfer to another AURA window can fail silently, and if it does the system clipboard ends up with " +
              "nothing at all, in any application: not this copy, and not whatever was on it before.",
          );
        }
      } catch (err) {
        // A machine without a usable clipboard still gets in-app paste.
        console.warn("[aura] OS clipboard write failed:", err);
      }
    } catch (err) {
      console.error("[aura] clips_copy failed:", err);
      toasts.error("COPY FAILED", String(err));
    }
  }

  /** Paste at `atSamples`. Returns whether a payload was found anywhere (OS
   * clipboard or in-memory) — NOT whether every clip landed (see
   * `PasteResult.skipped` for that). The caller uses the return value only
   * to decide whether to fall back to the legacy single-clip stamp; it must
   * not skip calling this in the first place (see the module doc above). */
  async paste(atSamples: number, toNewTracks: boolean): Promise<boolean> {
    let payload = this.payload;
    try {
      const text = await backend.osClipboardReadText?.();
      const fromOs = text ? parseAuraClips(text) : null;
      if (fromOs) payload = fromOs;
    } catch (err) {
      console.warn("[aura] OS clipboard read failed:", err);
    }
    if (!payload) return false;
    try {
      const res = await backend.clipsPaste?.({
        payload,
        atSamples: Math.max(0, Math.round(atSamples)),
        toNewTracks,
      });
      if (!res) {
        // `backend.clipsPaste` itself is missing (a partial/demo backend,
        // same convention as `moveClip?`) — a payload WAS found (the whole
        // reason this method returns `true` here), there is simply no
        // command to apply it with. Nothing to apply, nothing to undo.
        return true;
      }
      // `clipsPaste` commits `project://changed` INSIDE its transaction, so
      // `project.applyProject` can already have replaced `tracks` with the
      // authoritative post-paste list by the time THIS await resolves —
      // `upsertTrack` (not a blind append) is what keeps a created track
      // from landing twice (fix round 1, Important 1).
      for (const t of res.createdTracks) project.upsertTrack(t);
      for (const c of res.audioClips) project.upsertClip(c);
      // Provably safe as a blind append TODAY — `project://changed` carries
      // only the audio-shaped `Project` (`midi.svelte.ts` doc), so nothing
      // races this — but `midi.upsert` costs nothing extra and keeps every
      // apply-loop in this method idempotent rather than leaving one kind
      // as the odd one out for the next reader to imitate (fix round 2,
      // minor 5).
      for (const c of res.midiClips) midi.upsert(c);
      clipSelection.apply(
        [
          ...res.audioClips.map((c) => ({ kind: "audio" as const, id: c.id })),
          ...res.midiClips.map((c) => ({ kind: "midi" as const, id: c.id })),
        ],
        "replace",
      );
      if (res.skipped.length > 0) {
        toasts.error(
          "SOME CLIPS COULD NOT BE PASTED",
          res.skipped.map((s) => `${s.name}: ${s.reason}`).join("\n"),
        );
      }
    } catch (err) {
      console.error("[aura] clips_paste failed:", err);
      toasts.error("PASTE FAILED", String(err));
    }
    return true;
  }

  /** The one Ctrl+V entry point. Always tries the multi-clip clipboard
   * (`paste`, which scans the OS clipboard unconditionally) first; falls
   * back to the legacy single-clip note stamp only when `paste` found
   * nothing anywhere AND this is not a to-new-tracks paste, which the
   * legacy stamp has no concept of (so it is skipped, not attempted and
   * ignored). Toasts "nothing to paste" whenever every path that COULD
   * have pasted came back empty — see the module doc for why that toast
   * cannot live inside `paste()` itself. */
  async pasteAtPlayhead(atSamples: number, toNewTracks: boolean): Promise<void> {
    const found = await this.paste(atSamples, toNewTracks);
    if (found) return;
    const pasted = toNewTracks ? null : await midi.pasteAtPlayhead(midi.samplesToTicks(atSamples));
    if (!pasted) {
      // Lead with the ordinary cause (nothing was ever copied) — the
      // cross-instance transfer failure is the rare case, and a toast that
      // explains the exotic case to someone in the everyday one is how
      // toasts get trained into background noise (fix round 2, minor 3).
      toasts.info(
        "NOTHING TO PASTE",
        "copy something first — Ctrl+C a clip here, or in another AURA window. " +
          "(A very large copy can occasionally fail to cross between windows.)",
      );
    }
  }
}

export const clipClipboard = new ClipClipboardStore();
