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
 * One resize gesture. Both handles sit on the edge nearest the app center
 * (roll: top, dock: left), so the panel grows as the pointer coordinate
 * decreases: size = startSize + (startCoord − coord). Deltas are taken from
 * the gesture start, never accumulated, so clamping mid-drag cannot drift —
 * returning the pointer to its start coordinate returns the start size.
 */
export function createPanelDrag(
  spec: ResizeSpec,
  startSize: number,
  startCoord: number,
): PanelDrag {
  return {
    update(coord: number, viewportPx: number): number {
      return clampSize(startSize + (startCoord - coord), spec, viewportPx);
    },
  };
}
