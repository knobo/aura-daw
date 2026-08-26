import { describe, expect, it } from "vitest";
import { placePopover } from "./popover";

const VIEW = { width: 1600, height: 950 };

describe("placePopover", () => {
  it("opens downward when there is room", () => {
    const p = placePopover({ top: 100, bottom: 120, left: 200 }, { width: 260, height: 400 }, VIEW);
    expect(p.side).toBe("below");
    expect(p.top).toBe(126);
    expect(p.left).toBe(200);
  });

  it("flips above a trigger docked at the bottom of the window", () => {
    // The surface panel's header sits low; below it there is nothing.
    const p = placePopover({ top: 553, bottom: 577, left: 244 }, { width: 260, height: 740 }, VIEW);
    expect(p.side).toBe("above");
    expect(p.top).toBeGreaterThanOrEqual(8);
    expect(p.top + p.maxHeight).toBeLessThanOrEqual(553);
  });

  it("caps the height so a tall menu scrolls instead of leaving the viewport", () => {
    const p = placePopover({ top: 553, bottom: 577, left: 244 }, { width: 260, height: 740 }, VIEW);
    // 740 does not fit in either gap: 8..547 above is what is left
    expect(p.maxHeight).toBe(553 - 6 - 8);
    expect(p.top).toBe(8);
  });

  it("keeps a wide popover inside the right edge", () => {
    const p = placePopover({ top: 100, bottom: 120, left: 1550 }, { width: 300, height: 100 }, VIEW);
    expect(p.left).toBe(1600 - 8 - 300);
  });

  it("never places the popover off the left edge", () => {
    const p = placePopover({ top: 100, bottom: 120, left: -40 }, { width: 300, height: 100 }, VIEW);
    expect(p.left).toBe(8);
  });
});
