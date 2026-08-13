/**
 * DOM side of the interface zoom: applying the factor to a root element
 * via the CSS `zoom` property. Tested against a structural stub — no DOM
 * environment needed, matching the node-env test setup.
 */

import { describe, expect, it, vi } from "vitest";
import { applyUiZoom } from "./ui-zoom";

function stubEl() {
  return { style: { setProperty: vi.fn(), removeProperty: vi.fn() } };
}

describe("applyUiZoom", () => {
  it("sets the css zoom property for a non-default factor", () => {
    const el = stubEl();
    applyUiZoom(el, 1.3);
    expect(el.style.setProperty).toHaveBeenCalledWith("zoom", "1.3");
    expect(el.style.removeProperty).not.toHaveBeenCalled();
  });

  it("removes the property at the default factor, leaving the DOM clean", () => {
    const el = stubEl();
    applyUiZoom(el, 1);
    expect(el.style.removeProperty).toHaveBeenCalledWith("zoom");
    expect(el.style.setProperty).not.toHaveBeenCalled();
  });
});
