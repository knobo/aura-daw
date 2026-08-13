/**
 * Panel resize math — clamping and the drag helper behind the piano roll's
 * top-edge and the dock's left-edge grab handles. Pure px-in/px-out, so the
 * whole gesture can be pinned down without a DOM.
 */

import { describe, expect, it } from "vitest";
import {
  clampSize,
  createPanelDrag,
  DOCK_RESIZE,
  ROLL_RESIZE,
  type ResizeSpec,
} from "./panel-resize";

const SPEC: ResizeSpec = { minPx: 200, maxViewportFraction: 0.8 };
const VIEWPORT = 1000; // → max 800px

describe("clampSize", () => {
  it("passes sizes inside the range through (rounded to whole px)", () => {
    expect(clampSize(340, SPEC, VIEWPORT)).toBe(340);
    expect(clampSize(340.4, SPEC, VIEWPORT)).toBe(340);
    expect(clampSize(340.6, SPEC, VIEWPORT)).toBe(341);
  });

  it("clamps below to minPx", () => {
    expect(clampSize(10, SPEC, VIEWPORT)).toBe(200);
    expect(clampSize(-500, SPEC, VIEWPORT)).toBe(200);
  });

  it("clamps above to the viewport fraction", () => {
    expect(clampSize(5000, SPEC, VIEWPORT)).toBe(800);
  });

  it("lets minPx win over the max when the viewport is tiny", () => {
    // 0.8 × 100 = 80 < minPx: a shrunken window must not squash the panel
    // below its usable minimum.
    expect(clampSize(500, SPEC, 100)).toBe(200);
  });

  it("falls back to minPx on non-finite input", () => {
    expect(clampSize(Number.NaN, SPEC, VIEWPORT)).toBe(200);
    expect(clampSize(Infinity, SPEC, VIEWPORT)).toBe(800); // +∞ is just "past max"
  });
});

describe("createPanelDrag", () => {
  // Both panels grow when the pointer moves toward the panel's far edge:
  // the roll's top handle drags UP (coord decreases) to grow, the dock's
  // left handle drags LEFT likewise. So: size = startSize + (start − coord).
  it("grows as the pointer coordinate decreases", () => {
    const drag = createPanelDrag(SPEC, 340, 600);
    expect(drag.update(500, VIEWPORT)).toBe(440);
  });

  it("shrinks as the pointer coordinate increases", () => {
    const drag = createPanelDrag(SPEC, 340, 600);
    expect(drag.update(700, VIEWPORT)).toBe(240);
  });

  it("clamps at both ends of the range", () => {
    const drag = createPanelDrag(SPEC, 340, 600);
    expect(drag.update(-2000, VIEWPORT)).toBe(800);
    expect(drag.update(2000, VIEWPORT)).toBe(200);
  });

  it("is relative to the gesture start — overshooting then returning does not drift", () => {
    const drag = createPanelDrag(SPEC, 340, 600);
    drag.update(2000, VIEWPORT); // clamped at min mid-gesture
    expect(drag.update(600, VIEWPORT)).toBe(340); // back at start coord → start size
  });
});

describe("panel specs", () => {
  it("piano roll resizes between 200px and 80vh", () => {
    expect(ROLL_RESIZE).toEqual({ minPx: 200, maxViewportFraction: 0.8 });
  });

  it("dock resizes between 260px and 60vw", () => {
    expect(DOCK_RESIZE).toEqual({ minPx: 260, maxViewportFraction: 0.6 });
  });
});

describe("ui store defaults", () => {
  it("start inside their clamp ranges, so first render needs no correction", async () => {
    const { ui } = await import("../state/ui.svelte");
    const viewport = 1200;
    expect(clampSize(ui.rollHeight, ROLL_RESIZE, viewport)).toBe(ui.rollHeight);
    expect(clampSize(ui.dockWidth, DOCK_RESIZE, viewport)).toBe(ui.dockWidth);
  });
});
