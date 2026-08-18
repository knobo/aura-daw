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

/** The bits of a MIDI clip the Library's usage tools need — a structural
 * subset so this stays testable without the full `MidiClip` IPC type. */
export interface ClipUsageEntry {
  id: string;
  name: string;
  trackId: string;
  timelineStartTicks: number;
}

/** Every placement grouped by name, restricted to tracks that still exist —
 * a track delete leaves its MIDI clips behind (orphaned, invisible on any
 * timeline) rather than removing them, so "still exists" is not implied by
 * being in `clips`. One pass over `clips` regardless of how many distinct
 * names the caller looks up (a Library render does one lookup per row), so
 * this is the single source of truth for a render — build it once and
 * reuse the map, don't call this per row. Each group keeps the incoming
 * array's order (first-created / timeline-appended first), so
 * double-click-to-jump lands on the first one. */
export function groupLiveUsages(
  clips: readonly ClipUsageEntry[],
  liveTrackIds: ReadonlySet<string>,
): Map<string, ClipUsageEntry[]> {
  const groups = new Map<string, ClipUsageEntry[]>();
  for (const c of clips) {
    if (!liveTrackIds.has(c.trackId)) continue;
    const group = groups.get(c.name);
    if (group) group.push(c);
    else groups.set(c.name, [c]);
  }
  return groups;
}

/** The location after `currentId` in `locations`, wrapping around. The
 * first location when nothing is current yet, or when `currentId` no
 * longer appears (its track was deleted since). `null` for an empty group. */
export function nextUsage(
  locations: readonly ClipUsageEntry[],
  currentId: string | null,
): ClipUsageEntry | null {
  if (locations.length === 0) return null;
  const i = currentId == null ? -1 : locations.findIndex((c) => c.id === currentId);
  return locations[(i + 1) % locations.length];
}
