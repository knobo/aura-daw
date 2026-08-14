import { describe, expect, it } from "vitest";
import { canvasPos } from "./canvas-pos";

/** Minimal stand-in for a canvas under a given interface-zoom factor:
 * layout size is clientWidth/Height, getBoundingClientRect reports the
 * zoomed (visual) rect the way CSS `zoom` does. */
function el(opts: {
  left: number;
  top: number;
  clientWidth: number;
  clientHeight: number;
  zoom?: number;
}) {
  const zoom = opts.zoom ?? 1;
  return {
    clientWidth: opts.clientWidth,
    clientHeight: opts.clientHeight,
    getBoundingClientRect: () => ({
      left: opts.left,
      top: opts.top,
      width: opts.clientWidth * zoom,
      height: opts.clientHeight * zoom,
    }),
  };
}

describe("canvasPos", () => {
  it("maps client coordinates to canvas-local layout px at zoom 1", () => {
    const c = el({ left: 100, top: 50, clientWidth: 640, clientHeight: 280 });
    expect(canvasPos(c, 100, 50)).toEqual({ x: 0, y: 0 });
    expect(canvasPos(c, 420, 190)).toEqual({ x: 320, y: 140 });
  });

  it("divides out the visual/layout scale under interface zoom", () => {
    // zoom 1.25: the rect is 25% larger than the canvas layout size, so a
    // pointer at the rect's far corner must still map to (640, 280).
    const c = el({ left: 100, top: 50, clientWidth: 640, clientHeight: 280, zoom: 1.25 });
    expect(canvasPos(c, 100 + 800, 50 + 350)).toEqual({ x: 640, y: 280 });
    expect(canvasPos(c, 100 + 400, 50 + 175)).toEqual({ x: 320, y: 140 });
  });

  it("also corrects when the UI is scaled down", () => {
    const c = el({ left: 0, top: 0, clientWidth: 400, clientHeight: 200, zoom: 0.5 });
    expect(canvasPos(c, 100, 50)).toEqual({ x: 200, y: 100 });
  });

  it("falls back to raw offsets when the element has no layout size", () => {
    const c = el({ left: 10, top: 20, clientWidth: 0, clientHeight: 0 });
    expect(canvasPos(c, 15, 26)).toEqual({ x: 5, y: 6 });
  });
});
