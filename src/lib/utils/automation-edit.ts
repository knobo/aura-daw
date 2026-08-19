/**
 * Pure point-set edits for an automation lane. Kept out of the component so
 * the geometry is testable without a DOM (the split `note-ops.ts` uses for
 * the piano roll). Values are domain values; the caller converts pixels
 * through `view`/`midi` before calling in.
 *
 * The collision rule mirrors the backend's `normalize_lane`: points are
 * sorted by tick and a duplicate tick keeps the LATER value, so the frontend
 * never shows a shape `automation_set` would silently change under it.
 */

export interface Pt {
  tick: number;
  value: number;
}

/** Positive power-of-two display ceiling for native gain multipliers.
 * The editor always exposes boost space (minimum 2x), and when the current
 * peak reaches a ceiling it advances to the next one so upward drags remain
 * possible without first changing another control. */
export function positiveValueCeiling(points: Pt[]): number {
  const peak = points.reduce(
    (max, point) => (Number.isFinite(point.value) ? Math.max(max, point.value) : max),
    1,
  );
  const ceiling = Math.max(2, 2 ** Math.ceil(Math.log2(peak)));
  return ceiling <= peak ? ceiling * 2 : ceiling;
}

/** Index of the point within `radiusPx` of (tick, value) in SCREEN space,
 * or -1. `tickPerPx`/`valuePerPx` convert the radius into domain units. */
export function hitTest(
  points: Pt[],
  tick: number,
  value: number,
  tickPerPx: number,
  valuePerPx: number,
  radiusPx: number,
): number {
  const dt = radiusPx * tickPerPx;
  const dv = radiusPx * valuePerPx;
  for (let i = 0; i < points.length; i++) {
    if (Math.abs(points[i].tick - tick) <= dt && Math.abs(points[i].value - value) <= dv) {
      return i;
    }
  }
  return -1;
}

function sorted(points: Pt[]): Pt[] {
  return [...points].sort((a, b) => a.tick - b.tick);
}

/** Insert `p`, keeping the array sorted by tick; an exact tick collision
 * REPLACES the existing point (the same last-wins rule `normalize_lane`
 * applies backend-side). Returns a new array. */
export function insertPoint(points: Pt[], p: Pt): Pt[] {
  const out = points.filter((q) => q.tick !== p.tick);
  out.push(p);
  return sorted(out);
}

/** Move `points[index]` to (tick, value), clamped to tick >= 0 and value in
 * [min, max], re-sorted. Returns the new array AND the moved point's new
 * index. */
export function movePoint(
  points: Pt[],
  index: number,
  tick: number,
  value: number,
  min: number,
  max: number,
): { points: Pt[]; index: number } {
  if (index < 0 || index >= points.length) return { points, index };
  const moved: Pt = {
    tick: Math.max(0, Math.round(tick)),
    value: Math.min(max, Math.max(min, value)),
  };
  const rest = points.filter((_, i) => i !== index);
  // A drag onto another point's tick must not eat the neighbour — insert
  // last-wins is for a click-to-place, not for a move. Keep the original
  // tick (value still updates) so a vertical drag still works.
  if (rest.some((q) => q.tick === moved.tick)) {
    moved.tick = points[index].tick;
  }
  const out = sorted([...rest, moved]);
  return { points: out, index: out.findIndex((q) => q === moved) };
}

/** Remove `points[index]`; out-of-range is a no-op. */
export function deletePoint(points: Pt[], index: number): Pt[] {
  if (index < 0 || index >= points.length) return points;
  return points.filter((_, i) => i !== index);
}
