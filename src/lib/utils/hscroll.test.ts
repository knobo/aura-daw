import { describe, expect, test } from "vitest";
import { totalExtent, thumbGeometry, startFromThumbX } from "./hscroll";

// Units are deliberately abstract: the timeline feeds samples, the piano
// roll feeds ticks. Only ratios matter.

describe("totalExtent", () => {
  test("content longer than the viewport wins", () => {
    expect(totalExtent(0, 100, 500)).toBe(500);
  });

  test("a viewport scrolled past the content extends the extent", () => {
    expect(totalExtent(450, 100, 500)).toBe(550);
  });

  test("empty content still spans one viewport", () => {
    expect(totalExtent(0, 100, 0)).toBe(100);
  });
});

describe("thumbGeometry", () => {
  test("thumb spans the view:total ratio, at the left edge when unscrolled", () => {
    const g = thumbGeometry(0, 100, 200, 200, 24);
    expect(g.w).toBeCloseTo(100);
    expect(g.x).toBeCloseTo(0);
    expect(g.scrollable).toBe(true);
  });

  test("fully scrolled puts the thumb against the right edge", () => {
    const g = thumbGeometry(100, 100, 200, 200, 24);
    expect(g.x).toBeCloseTo(200 - g.w);
  });

  test("thumb never shrinks below minThumb", () => {
    const g = thumbGeometry(0, 100, 1_000_000, 200, 24);
    expect(g.w).toBe(24);
  });

  test("content that fits is not scrollable: full-width thumb at 0", () => {
    const g = thumbGeometry(0, 200, 150, 300, 24);
    expect(g.scrollable).toBe(false);
    expect(g.x).toBe(0);
    expect(g.w).toBe(300);
  });

  test("degenerate track narrower than minThumb stays finite", () => {
    const g = thumbGeometry(50, 100, 400, 10, 24);
    expect(Number.isFinite(g.x)).toBe(true);
    expect(Number.isFinite(g.w)).toBe(true);
  });
});

describe("startFromThumbX", () => {
  test("round-trips through thumbGeometry", () => {
    const start = 130;
    const g = thumbGeometry(start, 100, 500, 240, 24);
    expect(startFromThumbX(g.x, 100, 500, 240, 24)).toBeCloseTo(start);
  });

  test("round-trips when the thumb is clamped to minThumb", () => {
    const start = 4000;
    const g = thumbGeometry(start, 100, 10_000, 240, 24);
    expect(startFromThumbX(g.x, 100, 10_000, 240, 24)).toBeCloseTo(start);
  });

  test("clamps below zero", () => {
    expect(startFromThumbX(-50, 100, 500, 240, 24)).toBe(0);
  });

  test("clamps past the right edge to total - viewSpan", () => {
    expect(startFromThumbX(9999, 100, 500, 240, 24)).toBeCloseTo(400);
  });

  test("unscrollable content pins start to 0", () => {
    expect(startFromThumbX(120, 200, 150, 240, 24)).toBe(0);
  });
});
