/**
 * The circle widget's geometry. Worth its own test because the rotation is the
 * thing that makes ONE component correct in every key: the backend says which
 * slot holds the tonic, and this puts that slot at the top.
 */
import { describe, expect, it } from "vitest";
import { annulus, circleLayout, polar } from "./circle-geometry";

describe("polar", () => {
  it("puts zero degrees straight up and grows clockwise", () => {
    const up = polar(100, 50, 0);
    expect(up.x).toBeCloseTo(100);
    expect(up.y).toBeCloseTo(50);
    const right = polar(100, 50, 90);
    expect(right.x).toBeCloseTo(150);
    expect(right.y).toBeCloseTo(100);
    const down = polar(100, 50, 180);
    expect(down.y).toBeCloseTo(150);
  });
});

describe("circleLayout", () => {
  it("spaces twelve wedges evenly", () => {
    const { wedges } = circleLayout(12, 0, 240);
    expect(wedges).toHaveLength(12);
    const angles = wedges.map((w) => w.angle).sort((a, b) => a - b);
    expect(angles).toEqual([0, 30, 60, 90, 120, 150, 180, 210, 240, 270, 300, 330]);
  });

  it("rotates the tonic's slot to the top, whatever its index", () => {
    for (const tonic of [0, 1, 5, 11]) {
      const { wedges } = circleLayout(12, tonic, 240);
      expect(wedges[tonic].angle).toBe(0);
      // The slot one step clockwise of the tonic (the dominant) sits at 30°.
      expect(wedges[(tonic + 1) % 12].angle).toBe(30);
      // …and one step counter-clockwise (the subdominant) at 330°.
      expect(wedges[(tonic + 11) % 12].angle).toBe(330);
    }
  });

  it("keeps every label inside the circle", () => {
    const { wedges, size, centre } = circleLayout(12, 3, 200);
    expect(size).toBe(200);
    for (const w of wedges) {
      for (const p of [w.outerLabel, w.innerLabel]) {
        const d = Math.hypot(p.x - centre, p.y - centre);
        expect(d).toBeLessThan(centre);
        expect(d).toBeGreaterThan(0);
      }
    }
  });

  it("puts the inner ring inside the outer one", () => {
    const { wedges, centre } = circleLayout(12, 0, 240);
    const outer = Math.hypot(wedges[0].outerLabel.x - centre, wedges[0].outerLabel.y - centre);
    const inner = Math.hypot(wedges[0].innerLabel.x - centre, wedges[0].innerLabel.y - centre);
    expect(inner).toBeLessThan(outer);
  });

  it("survives a degenerate count instead of dividing by zero", () => {
    const { wedges } = circleLayout(0, 0, 240);
    expect(wedges).toEqual([]);
    expect(circleLayout(1, 0, 240).wedges[0].angle).toBe(0);
  });
});

describe("annulus", () => {
  it("emits a closed two-arc path", () => {
    const path = annulus(120, 40, 100, -15, 15);
    expect(path.startsWith("M ")).toBe(true);
    expect(path.endsWith("Z")).toBe(true);
    // Out, arc, in, arc back: exactly two elliptical arcs and one line.
    expect(path.match(/A /g)).toHaveLength(2);
    expect(path.match(/L /g)).toHaveLength(1);
    // Sweep flags: 1 going clockwise on the outer edge, 0 coming back.
    expect(path).toContain("0 0 1 ");
    expect(path).toContain("0 0 0 ");
  });

  it("rounds coordinates so the DOM is not full of float noise", () => {
    const path = annulus(120, 40, 100, 0, 30);
    for (const n of path.match(/-?\d+\.?\d*/g) ?? []) {
      const decimals = n.split(".")[1] ?? "";
      expect(decimals.length).toBeLessThanOrEqual(2);
    }
  });

  it("sets the large-arc flag only for a segment over a half turn", () => {
    expect(annulus(100, 30, 90, 0, 30)).toContain("0 0 1 ");
    expect(annulus(100, 30, 90, 0, 200)).toContain("0 1 1 ");
  });
});
