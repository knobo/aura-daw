/**
 * Interface zoom, DOM side. CSS `zoom` (not `transform: scale`) is the
 * mechanism on purpose: zoom reflows layout, so `clientWidth` /
 * `getBoundingClientRect` measurements — which size every canvas backing
 * store (timeline, piano roll, meters) — keep tracking what is actually
 * on screen, and pointer math stays in the same coordinate space.
 * A transform would scale pixels after layout and blur every canvas.
 */

/** Structural slice of HTMLElement so the helper is testable without a DOM. */
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
