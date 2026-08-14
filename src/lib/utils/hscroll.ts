/**
 * Horizontal-scrollbar geometry — pure, so the same math serves the
 * timeline (samples) and the piano roll (ticks) and can be tested without
 * a DOM. Units are abstract; only ratios reach the screen.
 *
 * The thumb has a minimum pixel width so it stays grabbable at extreme
 * zoom-out; the position mapping distributes the leftover track evenly, so
 * thumb-left = 0 always means start = 0 and thumb-right = track-right
 * always means start = total - viewSpan.
 */

export interface ThumbGeometry {
  /** thumb offset from the track's left edge, CSS px */
  x: number;
  /** thumb width, CSS px */
  w: number;
  /** false when the whole extent fits in the viewport */
  scrollable: boolean;
}

/**
 * The scrollable extent: the content, or the viewport when it is scrolled
 * (or zoomed) past the content's end. Callers freeze this at drag start so
 * the mapping doesn't shift under the pointer mid-gesture.
 */
export function totalExtent(start: number, viewSpan: number, contentEnd: number): number {
  return Math.max(contentEnd, start + viewSpan);
}

export function thumbGeometry(
  start: number,
  viewSpan: number,
  total: number,
  trackW: number,
  minThumb: number,
): ThumbGeometry {
  if (!(total > viewSpan) || !(trackW > 0)) {
    return { x: 0, w: trackW, scrollable: false };
  }
  const w = Math.min(trackW, Math.max(minThumb, (viewSpan / total) * trackW));
  const range = trackW - w;
  if (range <= 0) return { x: 0, w: trackW, scrollable: false };
  const x = (start / (total - viewSpan)) * range;
  return { x: Math.min(range, Math.max(0, x)), w, scrollable: true };
}

/** Inverse of thumbGeometry's position mapping, clamped to [0, total - viewSpan]. */
export function startFromThumbX(
  x: number,
  viewSpan: number,
  total: number,
  trackW: number,
  minThumb: number,
): number {
  const g = thumbGeometry(0, viewSpan, total, trackW, minThumb);
  if (!g.scrollable) return 0;
  const range = trackW - g.w;
  const frac = Math.min(1, Math.max(0, x / range));
  return frac * (total - viewSpan);
}
