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
 * which is what a failed large copy silently drops.
 */

import { backend } from "../tauri";
import { encodeAuraClips, parseAuraClips } from "../utils/aura-clips";
import { clipSelection } from "./clip-selection.svelte";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import { toasts } from "./toasts.svelte";
import type { AuraClipsPayload } from "../types/ipc";

/** Above this, a copy that must ride the OS clipboard is told the truth
 * up front rather than silently risking the measured cross-instance size
 * ceiling (osclipboard.rs: ~165KB round-trips reliably, ~200KB+ reports
 * write-success and then vanishes in transit — total loss, not truncation).
 * The in-memory payload still lets the SAME instance paste past this size;
 * only the cross-instance OS-clipboard path is at risk, so the write is
 * still attempted and only a toast warns of the risk — this store must
 * never refuse a paste the user can still get in the same window. */
const OS_CLIPBOARD_RISK_BYTES = 165_000;

class ClipClipboardStore {
  /** In-memory fallback: what THIS window last copied. The OS clipboard is
   * the cross-instance channel; this is the one that always works. */
  payload = $state<AuraClipsPayload | null>(null);

  async copy(): Promise<void> {
    const audioIds = clipSelection.audioIds();
    const midiIds = clipSelection.midiIds();
    if (audioIds.length === 0 && midiIds.length === 0) return;
    try {
      const p = await backend.clipsCopy?.(audioIds, midiIds);
      if (!p) return;
      this.payload = p;
      const encoded = encodeAuraClips(p);
      try {
        await backend.osClipboardWriteText?.(encoded);
        if (encoded.length > OS_CLIPBOARD_RISK_BYTES) {
          // The write above reported success, but success here only means
          // arboard accepted the text and this process took selection
          // ownership — NOT that another AURA instance will ever see it
          // (osclipboard.rs's measured ceiling). Same-instance paste is
          // unaffected (it reads `this.payload`), so this is a warning
          // about the OTHER instance, not a failure of this one.
          toasts.info(
            "LARGE SELECTION COPIED",
            `${audioIds.length + midiIds.length} clips is a lot of data for the OS clipboard — ` +
              "pasting in THIS window will work, but a paste in another AURA instance may come back empty.",
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

  async paste(atSamples: number, toNewTracks: boolean): Promise<void> {
    let payload = this.payload;
    try {
      const text = await backend.osClipboardReadText?.();
      const fromOs = text ? parseAuraClips(text) : null;
      if (fromOs) payload = fromOs;
    } catch (err) {
      console.warn("[aura] OS clipboard read failed:", err);
    }
    if (!payload) return;
    try {
      const res = await backend.clipsPaste?.({
        payload,
        atSamples: Math.max(0, Math.round(atSamples)),
        toNewTracks,
      });
      if (!res) return;
      for (const t of res.createdTracks) project.tracks = [...project.tracks, t];
      for (const c of res.audioClips) project.upsertClip(c);
      if (res.midiClips.length > 0) midi.clips = [...midi.clips, ...res.midiClips];
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
  }
}

export const clipClipboard = new ClipClipboardStore();
