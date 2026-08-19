import { describe, expect, it } from "vitest";
import { deletePoint, hitTest, insertPoint, movePoint, positiveValueCeiling, type Pt } from "./automation-edit";

const pts: Pt[] = [
  { tick: 0, value: 1 },
  { tick: 960, value: 0.5 },
  { tick: 3840, value: 0 },
];

describe("positiveValueCeiling", () => {
  it("always leaves boost-authoring headroom", () => {
    expect(positiveValueCeiling([])).toBe(2);
    expect(positiveValueCeiling(pts)).toBe(2);
  });
  it("expands boost lanes without clamping their values", () => {
    expect(positiveValueCeiling([{ tick: 0, value: 1.995 }])).toBe(2);
    expect(positiveValueCeiling([{ tick: 0, value: 2 }])).toBe(4);
    expect(positiveValueCeiling([{ tick: 0, value: 3 }])).toBe(4);
  });
});

describe("hitTest", () => {
  it("finds the point under the cursor within the radius", () => {
    // 10 ticks/px horizontally, 0.01 value/px vertically, 6 px radius
    expect(hitTest(pts, 955, 0.5, 10, 0.01, 6)).toBe(1);
  });
  it("misses when either axis is outside the radius", () => {
    expect(hitTest(pts, 960, 0.9, 10, 0.01, 6)).toBe(-1);
    expect(hitTest(pts, 1200, 0.5, 10, 0.01, 6)).toBe(-1);
  });
  it("returns -1 on an empty lane", () => {
    expect(hitTest([], 0, 0, 10, 0.01, 6)).toBe(-1);
  });
});

describe("insertPoint", () => {
  it("keeps the array sorted by tick", () => {
    expect(insertPoint(pts, { tick: 1920, value: 0.25 }).map((p) => p.tick)).toEqual([
      0, 960, 1920, 3840,
    ]);
  });
  it("replaces an exact tick collision (last write wins, like normalize_lane)", () => {
    const out = insertPoint(pts, { tick: 960, value: 0.75 });
    expect(out).toHaveLength(3);
    expect(out[1]).toEqual({ tick: 960, value: 0.75 });
  });
  it("does not mutate its input", () => {
    const before = structuredClone(pts);
    insertPoint(pts, { tick: 5, value: 0.5 });
    expect(pts).toEqual(before);
  });
});

describe("movePoint", () => {
  it("clamps tick to >= 0 and value to [min,max]", () => {
    const { points } = movePoint(pts, 0, -500, 3, 0, 1);
    expect(points[0]).toEqual({ tick: 0, value: 1 });
  });
  it("re-sorts and reports the moved point's new index", () => {
    const { points, index } = movePoint(pts, 0, 2000, 0.2, 0, 1);
    expect(points.map((p) => p.tick)).toEqual([960, 2000, 3840]);
    expect(index).toBe(1);
    expect(points[index]).toEqual({ tick: 2000, value: 0.2 });
  });
  it("is a no-op for an out-of-range index", () => {
    expect(movePoint(pts, 9, 0, 0, 0, 1)).toEqual({ points: pts, index: 9 });
  });

  it("does not delete a neighbour when the drag lands on its tick", () => {
    const { points, index } = movePoint(pts, 0, 960, 0.8, 0, 1);
    expect(points).toHaveLength(3);
    expect(points.map((p) => p.tick)).toEqual([0, 960, 3840]);
    expect(points[0]).toEqual({ tick: 0, value: 0.8 });
    expect(points[1]).toEqual({ tick: 960, value: 0.5 });
    expect(index).toBe(0);
  });
});

describe("deletePoint", () => {
  it("removes the point", () => {
    expect(deletePoint(pts, 1).map((p) => p.tick)).toEqual([0, 3840]);
  });
  it("ignores an out-of-range index", () => {
    expect(deletePoint(pts, 9)).toEqual(pts);
  });
});
