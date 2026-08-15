/**
 * The `application/x-aura-clips` clipboard envelope.
 *
 * The MIME name lives INSIDE the payload, and the text begins with a magic
 * line, because the OS clipboard slot a Tauri v2 desktop app can portably
 * own is plain text — arbitrary MIME flavors are not available (plan scope
 * ruling C). The magic line also means a human who pastes this into a text
 * editor immediately sees what it is.
 *
 * `parseAuraClips` NEVER throws: the clipboard holds whatever the user last
 * copied, and a paste must degrade to "not ours" rather than error.
 */
import type { AuraClipsPayload } from "../types/ipc";

export const AURA_CLIPS_MIME = "application/x-aura-clips";
const MAGIC = "AURA-CLIPS/1";

export function encodeAuraClips(payload: AuraClipsPayload): string {
  return `${MAGIC}\n${JSON.stringify(payload)}`;
}

export function parseAuraClips(text: string): AuraClipsPayload | null {
  if (!text.startsWith(`${MAGIC}\n`)) return null;
  try {
    const parsed = JSON.parse(text.slice(MAGIC.length + 1)) as unknown;
    if (
      typeof parsed !== "object" ||
      parsed === null ||
      (parsed as AuraClipsPayload).mime !== AURA_CLIPS_MIME ||
      !Array.isArray((parsed as AuraClipsPayload).clips)
    ) {
      return null;
    }
    return parsed as AuraClipsPayload;
  } catch {
    return null;
  }
}
