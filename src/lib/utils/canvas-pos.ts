/**
 * Pointer → canvas-local coordinates in LAYOUT px, the space every canvas
 * paints in (backing stores are sized from clientWidth/clientHeight).
 * `getBoundingClientRect()` and `clientX/Y` are visual coordinates: under
 * interface zoom (CSS `zoom` on the app shell) or any ancestor transform
 * they run ahead of layout px by the accumulated scale, so raw
 * `clientX - rect.left` drifts down-right the further the pointer is from
 * the canvas origin. Dividing by the rect/layout ratio removes exactly
 * that scale, whatever produced it.
 */

export interface Measurable {
  getBoundingClientRect(): { left: number; top: number; width: number; height: number };
  clientWidth: number;
  clientHeight: number;
}

export function canvasPos(el: Measurable, clientX: number, clientY: number): { x: number; y: number } {
  const rect = el.getBoundingClientRect();
  const sx = el.clientWidth > 0 ? rect.width / el.clientWidth : 1;
  const sy = el.clientHeight > 0 ? rect.height / el.clientHeight : 1;
  return { x: (clientX - rect.left) / (sx || 1), y: (clientY - rect.top) / (sy || 1) };
}
