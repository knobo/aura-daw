/**
 * Lane drag-and-drop maths: where a dragged lane would land, and what
 * arrangement that produces.
 *
 * Pure and DOM-free. The drop rules are subtle enough that they deserve
 * tests without a browser — "drop between the last member of a group and
 * the next ungrouped lane" is exactly the case a hand-test never repeats.
 */

import type { TrackState } from "../types/ipc";
import { groupOf, type LaneLayout } from "./lane-layout";

/** Where a drag would insert, expressed against the ROW list so the UI can
 * draw the indicator line without re-deriving anything. */
export interface DropTarget {
  /** Insertion point as an index into `tracks` (0..tracks.length). */
  index: number;
  /** Group the lane would join at that point (`null` = ungrouped). */
  group: string | null;
  /** y of the indicator line, in lane-column px. */
  y: number;
}

/** One lane's place in the arrangement sent to the backend. */
export interface LanePlacement {
  trackId: string;
  group: string | null;
}

/**
 * Resolve a pointer y to a drop target.
 *
 * ONE rule, stated so it can be predicted without looking: **the row you
 * are over decides the group; which half of it you are over decides above
 * or below.**
 *
 * That makes joining a group the same gesture as reordering inside it —
 * hover the group's lanes — and leaving one the same gesture as reordering
 * outside it: hover a lane that is not in the group, or the empty space
 * past the last row. There is no separate "drop zone" to find, and no
 * boundary that means two different things depending on approach.
 *
 * A folded group is ONE row and therefore atomic: you can drop above or
 * below it, never inside. Dropping a lane into something the user cannot
 * see is how lanes get lost.
 */
export function dropTargetAtY(
  layout: LaneLayout,
  tracks: TrackState[],
  y: number,
): DropTarget {
  if (tracks.length === 0) return { index: 0, group: null, y: 0 };

  for (const row of layout.rows) {
    const mid = row.top + row.height / 2;
    const bottom = row.top + row.height;
    if (y >= bottom) continue;

    if (row.kind === "group") {
      const firstIndex = tracks.findIndex((t) => t.id === row.trackIds[0]);
      if (y < mid) {
        // Above the header — outside the group entirely.
        return { index: firstIndex, group: null, y: row.top };
      }
      return row.collapsed
        ? // Below a FOLDED header: past the whole group, still outside it.
          { index: firstIndex + row.trackIds.length, group: null, y: bottom }
        : // Below an open header: first lane of the group.
          { index: firstIndex, group: row.group, y: bottom };
    }

    return y < mid
      ? { index: row.trackIndex, group: row.group, y: row.top }
      : { index: row.trackIndex + 1, group: row.group, y: bottom };
  }

  // Past every row: the end of the arrangement, ungrouped.
  return { index: tracks.length, group: null, y: layout.totalHeight };
}

/**
 * Produce the full arrangement for "move `trackId` to `index`, in `group`".
 *
 * `index` is an insertion point in the ORIGINAL list (the drop indicator's
 * position), so it is adjusted for the dragged lane's own removal here
 * rather than at every call site.
 *
 * The result is always group-contiguous: if the move would split a group in
 * two, the dragged lane is placed at the group's edge instead. That keeps
 * the invariant `buildLaneLayout` reads (a group is a run) true by
 * construction, so the UI can never paint a group in two pieces as a result
 * of its own gesture.
 */
export function arrangementForMove(
  tracks: TrackState[],
  trackId: string,
  index: number,
  group: string | null,
): LanePlacement[] {
  const from = tracks.findIndex((t) => t.id === trackId);
  if (from < 0) return tracks.map((t) => ({ trackId: t.id, group: groupOf(t) }));

  const rest = tracks.filter((t) => t.id !== trackId);
  // The insertion index was measured against the list WITH the dragged lane
  // still in it.
  const at = Math.max(0, Math.min(index > from ? index - 1 : index, rest.length));

  const placements: LanePlacement[] = rest.map((t) => ({ trackId: t.id, group: groupOf(t) }));
  placements.splice(at, 0, { trackId, group });
  return normalizeGroupRuns(placements);
}

/**
 * Make every group a single contiguous run, preserving first-appearance
 * order for the groups themselves and document order within each.
 *
 * This runs on every arrangement the UI sends, so a drop that would have
 * split a group instead pulls the stragglers up to the run they belong to.
 * Idempotent: an already-contiguous arrangement comes back unchanged (which
 * is what makes `arrange_lanes`'s group diff emit nothing for a pure
 * reorder).
 */
export function normalizeGroupRuns(placements: LanePlacement[]): LanePlacement[] {
  const out: LanePlacement[] = [];
  const emitted = new Set<string>();
  for (const p of placements) {
    const g = p.group?.trim() ? p.group.trim() : null;
    if (g === null) {
      out.push({ trackId: p.trackId, group: null });
      continue;
    }
    if (emitted.has(g)) continue; // already flushed with its run
    emitted.add(g);
    for (const q of placements) {
      const qg = q.group?.trim() ? q.group.trim() : null;
      if (qg === g) out.push({ trackId: q.trackId, group: g });
    }
  }
  return out;
}

/** Every distinct group name currently in use, in display order. */
export function groupNames(tracks: TrackState[]): string[] {
  const seen: string[] = [];
  for (const t of tracks) {
    const g = groupOf(t);
    if (g && !seen.includes(g)) seen.push(g);
  }
  return seen;
}

/**
 * A group name that is free — "Group 1", "Group 2", … — so "new group"
 * never silently merges into an existing one (which, since the name IS the
 * identity, is exactly what a duplicate would do).
 */
export function nextGroupName(tracks: TrackState[]): string {
  const taken = new Set(groupNames(tracks));
  for (let n = 1; ; n++) {
    const candidate = `Group ${n}`;
    if (!taken.has(candidate)) return candidate;
  }
}

/** The arrangement produced by renaming a group (its members follow it). */
export function arrangementForGroupRename(
  tracks: TrackState[],
  from: string,
  to: string,
): LanePlacement[] {
  const target = to.trim();
  return normalizeGroupRuns(
    tracks.map((t) => ({
      trackId: t.id,
      group: groupOf(t) === from ? (target ? target : null) : groupOf(t),
    })),
  );
}

/** The arrangement produced by dissolving a group (members stay put). */
export function arrangementForGroupDissolve(
  tracks: TrackState[],
  group: string,
): LanePlacement[] {
  return tracks.map((t) => ({
    trackId: t.id,
    group: groupOf(t) === group ? null : groupOf(t),
  }));
}

/** The arrangement produced by putting one lane in (or out of) a group via
 * the menu rather than a drag: the lane MOVES to that group's run, because
 * membership without contiguity would paint a split group. */
export function arrangementForAssign(
  tracks: TrackState[],
  trackId: string,
  group: string | null,
): LanePlacement[] {
  return normalizeGroupRuns(
    tracks.map((t) => ({
      trackId: t.id,
      group: t.id === trackId ? group : groupOf(t),
    })),
  );
}
