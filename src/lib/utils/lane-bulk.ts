/**
 * Pure half of bulk M/S/A (4.5) — the value/tri-state maths, kept apart from
 * `project.setTracksState` so the "what should pressing this button do"
 * question tests without a backend or a gesture.
 */
import type { TrackState } from "../types/ipc";

export type BulkField = "muted" | "soloed" | "armed";

/**
 * What a bulk M/S/A press should set the whole group to.
 *
 * Ruling: if ANY of them is off, the press turns them ALL on; only when
 * every one is already on does it turn them all off. A press must never
 * leave a mixed selection mixed — "mute all of these" that flips half of
 * them off (because they read their OWN current state) is the bug this
 * function exists to not have.
 */
export function nextBulkValue(currentValues: boolean[]): boolean {
  return currentValues.some((v) => !v);
}

/** What the group looks like right now, for the button's own paint. */
export function bulkTriState(currentValues: boolean[]): "on" | "off" | "mixed" {
  if (currentValues.every((v) => !v)) return "off";
  if (currentValues.every((v) => v)) return "on";
  return "mixed";
}

/**
 * The tracks a bulk press can actually touch: automation lanes carry no
 * mute/solo/arm control (TrackHeader never renders one for them), so a
 * selection or group that happens to include one silently drops it here
 * rather than the caller having to special-case it three times. With no
 * `ids`, every eligible track in the project counts — the master-bar case.
 */
export function bulkableTracks(
  tracks: TrackState[],
  ids?: ReadonlySet<string> | readonly string[],
): TrackState[] {
  if (!ids) return tracks.filter((t) => t.kind !== "automation");
  const idSet = ids instanceof Set ? ids : new Set(ids);
  return tracks.filter((t) => idSet.has(t.id) && t.kind !== "automation");
}

export function fieldValues(tracks: TrackState[], field: BulkField): boolean[] {
  return tracks.map((t) => t[field]);
}
