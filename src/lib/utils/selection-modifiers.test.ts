import { describe, expect, it } from "vitest";
import { selectionModeFor } from "./selection-modifiers";

const ev = (shiftKey = false, ctrlKey = false, metaKey = false) => ({
  shiftKey,
  ctrlKey,
  metaKey,
});

describe("selectionModeFor", () => {
  it("plain click replaces", () => {
    expect(selectionModeFor(ev())).toBe("replace");
  });
  it("shift adds", () => {
    expect(selectionModeFor(ev(true))).toBe("add");
  });
  it("ctrl toggles", () => {
    expect(selectionModeFor(ev(false, true))).toBe("toggle");
  });
  it("meta toggles too (macOS)", () => {
    expect(selectionModeFor(ev(false, false, true))).toBe("toggle");
  });
  it("shift+ctrl subtracts", () => {
    expect(selectionModeFor(ev(true, true))).toBe("subtract");
  });
  it("shift+meta subtracts too", () => {
    expect(selectionModeFor(ev(true, false, true))).toBe("subtract");
  });
});
