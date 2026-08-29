/**
 * Panel resize math — pure, so the piano roll's top-edge and the dock's
 * left-edge drag handles can be tested without a DOM or pointer events.
 *
 * Sizes are CSS px. Maxima are viewport fractions (a panel may never eat
 * the whole window), minima absolute px (below that the content is junk).
 */

export interface ResizeSpec {
  /** Smallest useful size, absolute px. Wins over the max in tiny windows. */
  minPx: number;
  /** Largest allowed size as a fraction of the viewport's relevant axis. */
  maxViewportFraction: number;
}

/** Piano roll height: 200px … 80vh. */
export const ROLL_RESIZE: ResizeSpec = { minPx: 200, maxViewportFraction: 0.8 };
/** Right dock width: 260px … 60vw. */
export const DOCK_RESIZE: ResizeSpec = { minPx: 260, maxViewportFraction: 0.6 };
/** Track rail width: 312px … 45vw. The minimum is not a taste call — it is
 * where the routing row's own floors stop fitting, and below it the chips
 * would have to overlap again, which is the bug this handle exists to help
 * you avoid rather than reintroduce. Worst case that row must survive is
 * two 38px name chips (the group, the output bus), 22px of plugin status
 * dots, 132px of fixed chips and five 6px gaps = 260 — inside a row that
 * is the rail minus 12px of side padding and then indented another 20px.
 * Two drafts of this number were wrong (260, then 292, the second missing
 * exactly that indent); the test below is what caught both, and it spells
 * the arithmetic out so a third is caught too. */
export const RAIL_RESIZE: ResizeSpec = { minPx: 312, maxViewportFraction: 0.45 };

/** Which edge of the panel the drag handle sits on. "start" is the edge
 * nearest the app centre — the piano roll's top, the dock's left — where
 * the panel grows as the pointer coordinate DECREASES. The track rail is
 * anchored to the window's left instead, so its handle is on the far edge
 * and the sign flips. Getting this wrong is not subtle: the panel runs
 * away from the pointer. */
export type PanelEdge = "start" | "end";

/**
 * Clamp a requested size into the spec's range for the given viewport, as a
 * whole px. NaN degrades to the minimum rather than propagating into CSS.
 */
export function clampSize(px: number, spec: ResizeSpec, viewportPx: number): number {
  if (Number.isNaN(px)) return spec.minPx;
  const max = Math.max(spec.minPx, viewportPx * spec.maxViewportFraction);
  return Math.round(Math.min(max, Math.max(spec.minPx, px)));
}

export interface PanelDrag {
  /** Size for the pointer now at `coord`, clamped for `viewportPx`. */
  update(coord: number, viewportPx: number): number;
}

/**
 * One resize gesture. A handle on the panel's "start" edge (roll: top,
 * dock: left — the edge nearest the app centre) grows the panel as the
 * pointer coordinate DECREASES: size = startSize + (startCoord − coord).
 * On an "end" edge (the track rail's right side) the sign flips. Deltas
 * are taken from the gesture start, never accumulated, so clamping
 * mid-drag cannot drift — returning the pointer to its start coordinate
 * returns the start size.
 */
export function createPanelDrag(
  spec: ResizeSpec,
  startSize: number,
  startCoord: number,
  edge: PanelEdge = "start",
): PanelDrag {
  const sign = edge === "end" ? 1 : -1;
  return {
    update(coord: number, viewportPx: number): number {
      return clampSize(startSize + sign * (coord - startCoord), spec, viewportPx);
    },
  };
}
