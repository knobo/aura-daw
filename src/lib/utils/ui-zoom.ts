/**
 * Interface zoom, DOM side. CSS `zoom` (not `transform: scale`) is the
 * mechanism on purpose: zoom reflows layout, so `clientWidth` /
 * `getBoundingClientRect` measurements — which size every canvas backing
 * store (timeline, piano roll, meters) — keep tracking what is actually
 * on screen, and pointer math stays in the same coordinate space.
 * A transform would scale pixels after layout and blur every canvas.
 *
 * That "same coordinate space" claim has one hole `canvasPos` (canvas-pos.ts)
 * already papers over for canvases: `getBoundingClientRect`/`clientX` report
 * VISUAL px (already multiplied by zoom) while `clientWidth`/`offsetWidth`
 * stay in LAYOUT px (what canvas backing stores and `view.spp` are sized
 * from). Anything that takes a raw `clientX` delta and multiplies it by a
 * layout-px quantity — or writes a visual-space coordinate into a `style`
 * property on a node that lives inside the zoomed subtree, where the
 * browser will re-multiply it — needs `uiZoomFactor()` to cross back into
 * layout space first.
 */

import { prefs } from "../prefs/prefs.svelte";

/** Structural slice of HTMLElement so `applyUiZoom` is testable without a
 * DOM. `uiZoomFactor` below, unlike `applyUiZoom`, reads `prefs` directly
 * rather than the DOM — the two halves of this module have different
 * dependencies, not a shared testability property. */
type ZoomTarget = {
  style: {
    setProperty(name: string, value: string): void;
    removeProperty(name: string): void;
  };
};

/** Apply the interface zoom factor to a root element (the app shell). */
export function applyUiZoom(el: ZoomTarget, factor: number) {
  if (factor === 1) el.style.removeProperty("zoom");
  else el.style.setProperty("zoom", String(factor));
}

/** The zoom factor `applyUiZoom` last applied to `document.body` (App.svelte). */
export function uiZoomFactor(): number {
  return prefs.values.uiZoom || 1;
}
