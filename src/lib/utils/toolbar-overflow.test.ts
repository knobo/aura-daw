import { describe, expect, it } from "vitest";
import { computeVisible, type OverflowItem } from "./toolbar-overflow";

const chip = (id: string, width: number, priority: number): OverflowItem => ({
  id,
  width,
  priority,
});
const divider = (id: string, width = 1): OverflowItem => ({
  id,
  width,
  priority: 0,
  divider: true,
});

const GAP = 8;
const MORE = 30;

describe("computeVisible", () => {
  it("shows everything when it fits", () => {
    const items = [chip("a", 50, 2), chip("b", 50, 1)];
    // 50 + 8 + 50 = 108
    const r = computeVisible(items, 108, MORE, GAP);
    expect(r.visible).toEqual(new Set(["a", "b"]));
    expect(r.overflow).toEqual([]);
  });

  it("collapses the lowest-priority chip first and reserves room for the more button", () => {
    const items = [chip("a", 50, 3), chip("b", 50, 1), chip("c", 50, 2)];
    // all: 50*3 + 8*2 = 166; give less
    const r = computeVisible(items, 160, MORE, GAP);
    // b (lowest priority) goes first: a + c + more = 50+8+50+8+30 = 146 <= 160
    expect(r.visible).toEqual(new Set(["a", "c"]));
    expect(r.overflow).toEqual(["b"]);
  });

  it("keeps dropping until the row plus the more button fits", () => {
    const items = [chip("a", 50, 3), chip("b", 50, 2), chip("c", 50, 1)];
    // only one chip + more fits: 50 + 8 + 30 = 88
    const r = computeVisible(items, 100, MORE, GAP);
    expect(r.visible).toEqual(new Set(["a"]));
    expect(r.overflow).toEqual(["b", "c"]);
  });

  it("breaks priority ties by dropping the rightmost first", () => {
    const items = [chip("a", 50, 1), chip("b", 50, 1), chip("c", 50, 1)];
    const r = computeVisible(items, 160, MORE, GAP);
    expect(r.visible).toEqual(new Set(["a", "b"]));
    expect(r.overflow).toEqual(["c"]);
  });

  it("keeps display order in the overflow list regardless of priority", () => {
    const items = [chip("a", 50, 1), chip("b", 50, 2), chip("c", 50, 3)];
    const r = computeVisible(items, 100, MORE, GAP);
    expect(r.visible).toEqual(new Set(["c"]));
    expect(r.overflow).toEqual(["a", "b"]);
  });

  it("hides a divider when everything on one side of it collapsed", () => {
    const items = [chip("a", 50, 2), divider("d1"), chip("b", 50, 1)];
    // all: 50 + 8 + 1 + 8 + 50 = 117; force b out
    const r = computeVisible(items, 110, MORE, GAP);
    expect(r.visible).toEqual(new Set(["a"]));
    expect(r.overflow).toEqual(["b"]);
  });

  it("collapses adjacent dividers to one", () => {
    const items = [
      chip("a", 50, 3),
      divider("d1"),
      chip("b", 50, 1),
      divider("d2"),
      chip("c", 50, 2),
    ];
    // drop b: a | d? | c — only one divider between a and c survives
    const r = computeVisible(items, 160, MORE, GAP);
    expect(r.visible.has("a")).toBe(true);
    expect(r.visible.has("c")).toBe(true);
    expect(r.overflow).toEqual(["b"]);
    const dividersShown = ["d1", "d2"].filter((d) => r.visible.has(d));
    expect(dividersShown.length).toBe(1);
  });

  it("never puts dividers in the overflow list", () => {
    const items = [chip("a", 50, 1), divider("d1"), chip("b", 50, 1)];
    const r = computeVisible(items, 10, MORE, GAP);
    expect(r.overflow).toEqual(["a", "b"]);
    expect(r.visible.size).toBe(0);
  });

  it("shows nothing but the more button when nothing fits", () => {
    const items = [chip("a", 50, 1), chip("b", 50, 1)];
    const r = computeVisible(items, 20, MORE, GAP);
    expect(r.visible.size).toBe(0);
    expect(r.overflow).toEqual(["a", "b"]);
  });

  it("handles an empty item list", () => {
    const r = computeVisible([], 100, MORE, GAP);
    expect(r.visible.size).toBe(0);
    expect(r.overflow).toEqual([]);
  });
});
