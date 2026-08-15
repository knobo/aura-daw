import { describe, expect, it } from "vitest";
import { automationClipGesture } from "./automation-clip-gesture";

describe("automation clip gesture", () => {
  it("a body click that misses every point moves the clip", () => {
    expect(automationClipGesture({ nearRight: false, hit: -1, erase: false })).toBe("move");
  });

  it("a hit on a point edits the point", () => {
    expect(automationClipGesture({ nearRight: false, hit: 0, erase: false })).toBe("point");
  });

  it("the right edge resizes", () => {
    expect(automationClipGesture({ nearRight: true, hit: -1, erase: false })).toBe("resize");
    expect(automationClipGesture({ nearRight: true, hit: 2, erase: false })).toBe("resize");
  });

  it("alt/right-click deletes a hit point and ignores empty body", () => {
    expect(automationClipGesture({ nearRight: false, hit: 1, erase: true })).toBe("delete");
    expect(automationClipGesture({ nearRight: false, hit: -1, erase: true })).toBe("ignore");
  });
});
