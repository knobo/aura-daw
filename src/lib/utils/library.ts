/**
 * Pure helpers behind the library panel. Chrome only: the ONE path operation
 * the thin-renderer rule allows here is trimming a trailing segment for the
 * "up one folder" button — no filesystem access ever happens frontend-side
 * (ADR 0006; scanning is `library_scan`).
 */

import type { ZynPatch } from "../types/ipc";

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
