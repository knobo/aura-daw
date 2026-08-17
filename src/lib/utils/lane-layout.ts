/**
 * The arrangement's ROW MODEL: the ordered list of visual rows the timeline
 * paints, with each row's `top`/`height` in layout px.
 *
 * Before lane groups and collapse, "row i is at `i * TRACK_HEIGHT_PX`" was
 * true everywhere and the geometry lived as that one multiplication. It is
 * not true anymore — a row is a full lane, a collapsed strip, or a group
 * header — so the multiplication is replaced by this table, built ONCE per
 * render and consumed by everything that needs to know where a lane sits:
 * the rail, the lane column, the grid canvas, the marquee hit-test and the
 * launch-mark overlay. One table means those five cannot drift apart, which
 * is the same contract `TRACK_HEIGHT_PX` used to carry.
 *
 * Pure — no DOM, no stores — so the maths test without a browser.
 */

import type { TrackState } from "../types/ipc";

/** A full-height lane, in LAYOUT px. Mirrors `--track-height` in app.css. */
export const LANE_HEIGHT_PX = 88;

/** A collapsed lane: one thin strip, tall enough for the name, the index
 * and the mute/solo pair, and nothing else. Mirrors `--lane-collapsed`. */
export const LANE_COLLAPSED_PX = 22;

/** A group's own header strip. Same height as a collapsed lane so a folded
 * group and a folded lane read as the same kind of object. Mirrors
 * `--lane-group-height`. */
export const GROUP_HEADER_PX = 22;

/** One visual row. `top` is measured from the top of the lane column. */
export type LaneRow =
  | {
      kind: "group";
      /** The group's name — its identity (see `TrackState.group`). */
      group: string;
      top: number;
      height: number;
      /** Member track ids, in display order. Never empty: a group with no
       * members does not exist, because membership IS the group. */
      trackIds: string[];
      collapsed: boolean;
      /** Colour of the first member — a folded group still needs to be
       * recognisable at a glance, and it has no colour of its own. */
      color: string;
    }
  | {
      kind: "track";
      track: TrackState;
      top: number;
      height: number;
      collapsed: boolean;
      /** The group this lane displays under, or null. */
      group: string | null;
      /** Index into `project.tracks` — the LANE INDEX every sample-space
       * helper (`buildLaneBoxes`, launch regions) is keyed by. Rows and
       * track indices are NOT the same sequence once groups exist, so this
       * is carried explicitly rather than re-derived from row position. */
      trackIndex: number;
    };

export interface LaneLayout {
  rows: LaneRow[];
  /** Total painted height — what the grid canvas and the lane column size
   * themselves to. */
  totalHeight: number;
  /** trackId -> its row, for the rows that are actually painted. A track
   * inside a folded group has NO entry: it is not on screen, so nothing
   * may try to position a clip against it. */
  byTrackId: Map<string, Extract<LaneRow, { kind: "track" }>>;
}

export interface BuildLaneLayoutArgs {
  tracks: TrackState[];
  /** Track ids the user has folded to a strip. */
  collapsedTracks: ReadonlySet<string>;
  /** Group names the user has folded away. */
  collapsedGroups: ReadonlySet<string>;
}

/** Normalized group name for a track: trimmed, and "" treated as ungrouped
 * — matching what the backend's write side stores. The UI must agree with
 * it, or a stale `Some("")` from anywhere would render a nameless group. */
export function groupOf(track: TrackState): string | null {
  const g = track.group?.trim();
  return g ? g : null;
}

/**
 * Build the row table.
 *
 * A group's rows are the MAXIMAL RUN of consecutive tracks that share its
 * name. Runs, not "all tracks with this name": order is the document's, and
 * a non-contiguous group would have to either reorder the arrangement
 * behind the user's back or draw one group in two places. Both are worse
 * than showing the second run as its own header — which also makes the
 * fix obvious (drag them together). `arrangeLanes` keeps runs contiguous
 * for every gesture the UI offers, so a split run only appears if a project
 * arrives that way from elsewhere.
 */
export function buildLaneLayout(args: BuildLaneLayoutArgs): LaneLayout {
  const { tracks, collapsedTracks, collapsedGroups } = args;
  const rows: LaneRow[] = [];
  const byTrackId = new Map<string, Extract<LaneRow, { kind: "track" }>>();
  let top = 0;

  const pushTrack = (track: TrackState, trackIndex: number, group: string | null) => {
    const collapsed = collapsedTracks.has(track.id);
    const row = {
      kind: "track" as const,
      track,
      top,
      height: collapsed ? LANE_COLLAPSED_PX : LANE_HEIGHT_PX,
      collapsed,
      group,
      trackIndex,
    };
    rows.push(row);
    byTrackId.set(track.id, row);
    top += row.height;
  };

  let i = 0;
  while (i < tracks.length) {
    const group = groupOf(tracks[i]);
    if (group === null) {
      pushTrack(tracks[i], i, null);
      i += 1;
      continue;
    }
    // Extend to the end of this run.
    let end = i;
    while (end + 1 < tracks.length && groupOf(tracks[end + 1]) === group) end += 1;
    const members = tracks.slice(i, end + 1);
    const collapsed = collapsedGroups.has(group);
    rows.push({
      kind: "group",
      group,
      top,
      height: GROUP_HEADER_PX,
      trackIds: members.map((t) => t.id),
      collapsed,
      color: members[0].color,
    });
    top += GROUP_HEADER_PX;
    if (!collapsed) {
      for (let k = i; k <= end; k++) pushTrack(tracks[k], k, group);
    }
    i = end + 1;
  }

  return { rows, totalHeight: top, byTrackId };
}

/**
 * The TRACK INDEX at a lane-column y coordinate — the replacement for the
 * old `floor(y / TRACK_HEIGHT_PX)`.
 *
 * Returns `null` when y falls on a group header or past the last row,
 * because "no lane here" is a real answer that the old clamping version
 * could not express: a marquee dragged across a group header should not
 * silently select the lane above it.
 */
export function trackIndexAtY(layout: LaneLayout, y: number): number | null {
  for (const row of layout.rows) {
    if (y < row.top) break;
    if (y < row.top + row.height) return row.kind === "track" ? row.trackIndex : null;
  }
  return null;
}

/**
 * The same, but CLAMPED to the nearest visible lane — for gestures that
 * must always land somewhere (dragging a launch region, marquee bounds).
 * Falls back to the nearest lane above, then the first lane below, so a
 * drag over a group header behaves as if it were over the lane the user
 * can see rather than aborting.
 */
export function nearestTrackIndexAtY(layout: LaneLayout, y: number): number | null {
  let last: number | null = null;
  for (const row of layout.rows) {
    if (row.kind !== "track") continue;
    if (y < row.top) return last ?? row.trackIndex;
    if (y < row.top + row.height) return row.trackIndex;
    last = row.trackIndex;
  }
  return last;
}

/** Top/height of one track's row, or null when it is not painted (its
 * group is folded). Used to place overlays that span lanes. */
export function trackBand(
  layout: LaneLayout,
  trackId: string,
): { top: number; height: number } | null {
  const row = layout.byTrackId.get(trackId);
  return row ? { top: row.top, height: row.height } : null;
}

/**
 * The pixel band covering lanes `lo..hi` (track indices, inclusive) — the
 * launch-mark overlay's geometry. Lanes inside a folded group contribute
 * nothing, so a region spanning a folded group collapses onto the rows that
 * are actually visible instead of drawing over the fold.
 *
 * Returns null when NO lane in the range is painted.
 */
export function bandForTrackRange(
  layout: LaneLayout,
  tracks: TrackState[],
  lo: number,
  hi: number,
): { top: number; height: number } | null {
  let top = Infinity;
  let bottom = -Infinity;
  for (let i = Math.max(0, lo); i <= Math.min(hi, tracks.length - 1); i++) {
    const row = layout.byTrackId.get(tracks[i].id);
    if (!row) continue;
    top = Math.min(top, row.top);
    bottom = Math.max(bottom, row.top + row.height);
  }
  return bottom > top ? { top, height: bottom - top } : null;
}
