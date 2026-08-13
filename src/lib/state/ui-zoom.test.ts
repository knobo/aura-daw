/**
 * Interface zoom: a global scale factor for the whole UI so low-vision
 * users can grow text, dots and buttons. These tests pin the store logic:
 * the 0.8–2.0 clamp, the 0.1 step without float drift, and reset.
 */

import { beforeEach, describe, expect, it } from "vitest";
import { UI_ZOOM_MAX, UI_ZOOM_MIN, resetUiZoom, setUiZoom, ui, zoomUiIn, zoomUiOut } from "./ui.svelte";

beforeEach(() => {
  resetUiZoom();
});

describe("interface zoom store", () => {
  it("defaults to 1.0", () => {
    expect(ui.zoom).toBe(1);
  });

  it("steps up by 0.1", () => {
    zoomUiIn();
    expect(ui.zoom).toBe(1.1);
  });

  it("steps down by 0.1", () => {
    zoomUiOut();
    expect(ui.zoom).toBe(0.9);
  });

  it("accumulates steps without float drift", () => {
    // 1.0 + 10 × 0.1 must land exactly on 2.0, not 1.9999999999999998
    for (let i = 0; i < 10; i++) zoomUiIn();
    expect(ui.zoom).toBe(2);
  });

  it("clamps at the maximum", () => {
    for (let i = 0; i < 30; i++) zoomUiIn();
    expect(ui.zoom).toBe(UI_ZOOM_MAX);
  });

  it("clamps at the minimum", () => {
    for (let i = 0; i < 30; i++) zoomUiOut();
    expect(ui.zoom).toBe(UI_ZOOM_MIN);
  });

  it("setUiZoom clamps arbitrary values into range", () => {
    setUiZoom(5);
    expect(ui.zoom).toBe(UI_ZOOM_MAX);
    setUiZoom(0.1);
    expect(ui.zoom).toBe(UI_ZOOM_MIN);
  });

  it("setUiZoom rounds to the 0.1 grid", () => {
    setUiZoom(1.2345);
    expect(ui.zoom).toBe(1.2);
  });

  it("setUiZoom ignores non-finite input", () => {
    setUiZoom(1.5);
    setUiZoom(NaN);
    expect(ui.zoom).toBe(1.5);
    setUiZoom(Infinity);
    expect(ui.zoom).toBe(1.5);
  });

  it("resets to 1.0", () => {
    zoomUiIn();
    zoomUiIn();
    resetUiZoom();
    expect(ui.zoom).toBe(1);
  });
});
