/**
 * `view.revealSamples` — the one rule both follow paths share: bring a
 * sample position on screen, and do nothing at all when it is already
 * there. Without the second half, following while stopped would fight the
 * user's own scrolling on every frame.
 */

import { beforeEach, describe, expect, it } from "vitest";
import { view } from "./view.svelte";

beforeEach(() => {
  view.spp = 1000;
  view.width = 1000; // 1_000_000 samples wide
  view.viewStart = 0;
});

describe("revealSamples", () => {
  it("leaves the view alone when the position is comfortably on screen", () => {
    view.viewStart = 500_000;
    view.revealSamples(1_000_000);
    expect(view.viewStart).toBe(500_000);
  });

  it("pages forward when the position is past the right edge", () => {
    view.viewStart = 0;
    view.revealSamples(2_000_000);
    // lands at LEAD_FRAC (8%) from the left edge
    expect(view.viewStart).toBe(2_000_000 - 0.08 * 1000 * 1000);
  });

  it("pages back when the position is left of the view", () => {
    view.viewStart = 5_000_000;
    view.revealSamples(1_000_000);
    expect(view.viewStart).toBe(1_000_000 - 0.08 * 1000 * 1000);
  });

  it("clamps at zero, so rewinding to the start shows the start", () => {
    view.viewStart = 5_000_000;
    view.revealSamples(0);
    expect(view.viewStart).toBe(0);
  });

  it("treats a position inside the trailing margin as off screen", () => {
    // 96% across the viewport — visible, but about to leave.
    view.viewStart = 0;
    view.revealSamples(960_000);
    expect(view.viewStart).toBeGreaterThan(0);
  });

  it("is a no-op at the left edge once already clamped to zero", () => {
    view.viewStart = 0;
    view.revealSamples(0);
    expect(view.viewStart).toBe(0);
  });
});
