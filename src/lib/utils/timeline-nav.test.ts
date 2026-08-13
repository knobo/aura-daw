import { describe, expect, it } from "vitest";
import { clipEdges, edgeJump, gridStep } from "./timeline-nav";

const BAR = 96_000; // 2 s at 48 kHz, 120 bpm 4/4

describe("gridStep", () => {
  it("walks bar lines from a position already on one", () => {
    expect(gridStep(0, BAR, 1)).toBe(BAR);
    expect(gridStep(BAR, BAR, 1)).toBe(2 * BAR);
    expect(gridStep(2 * BAR, BAR, -1)).toBe(BAR);
  });

  it("snaps to the next line from between lines instead of adding a full step", () => {
    expect(gridStep(BAR + 1000, BAR, 1)).toBe(2 * BAR);
    expect(gridStep(BAR + 1000, BAR, -1)).toBe(BAR);
    expect(gridStep(BAR - 1000, BAR, 1)).toBe(BAR);
  });

  it("never walks off the start of the timeline", () => {
    expect(gridStep(0, BAR, -1)).toBe(0);
    expect(gridStep(500, BAR, -1)).toBe(0);
  });

  it("survives a degenerate grid", () => {
    expect(gridStep(1234, 0, 1)).toBe(1234);
    expect(gridStep(1234, -5, -1)).toBe(1234);
  });

  it("is not fooled by float dust on a line", () => {
    // A position a hair past a line must still advance a whole bar.
    expect(gridStep(BAR + 1e-9, BAR, 1)).toBe(2 * BAR);
  });
});

describe("edgeJump", () => {
  const edges = [0, 48_000, 48_000, 96_000, 150_000];

  it("finds the nearest edge in each direction", () => {
    expect(edgeJump(edges, 0, 1)).toBe(48_000);
    expect(edgeJump(edges, 50_000, 1)).toBe(96_000);
    expect(edgeJump(edges, 96_000, -1)).toBe(48_000);
  });

  it("is strict, so repeated presses keep moving", () => {
    expect(edgeJump(edges, 48_000, 1)).toBe(96_000);
    expect(edgeJump(edges, 48_000, -1)).toBe(0);
  });

  it("returns null past the last edge rather than jumping somewhere odd", () => {
    expect(edgeJump(edges, 200_000, 1)).toBeNull();
    expect(edgeJump(edges, 0, -1)).toBeNull();
    expect(edgeJump([], 1000, 1)).toBeNull();
  });

  it("ignores negative edges", () => {
    expect(edgeJump([-500, 1000], 0, 1)).toBe(1000);
    expect(edgeJump([-500], 0, -1)).toBeNull();
  });
});

describe("clipEdges", () => {
  it("yields both bounds of every clip", () => {
    expect(
      clipEdges([
        { timelineStartSamples: 0, lengthSamples: 100 },
        { timelineStartSamples: 500, lengthSamples: 250 },
      ]),
    ).toEqual([0, 100, 500, 750]);
  });
});
