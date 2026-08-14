/**
 * Geometry for the timeline clip's mini note-preview, looping included.
 * Pure math (no canvas): the component just fills the rects this returns.
 *
 * The load-bearing regression here: px-per-tick must derive from the
 * PLACEMENT length (the canvas spans the whole placement), never from the
 * content length — deriving from content stretches one iteration across
 * the whole clip and pushes every repeat past the right edge.
 */

import { describe, expect, it } from "vitest";
import { midiPreviewLayout } from "./midi-preview";

function note(tick: number, lengthTicks = 240, key = 60, velocity = 100) {
  return { tick, lengthTicks, key, velocity };
}

const base = {
  widthPx: 300,
  offsetPx: 0,
  canvasW: 300,
  canvasH: 60,
};

describe("midiPreviewLayout", () => {
  it("repeats the content across the placement at placement-derived scale", () => {
    // content 960, placement 2880 -> 3 repeats across 300px, 100px each
    const { rects } = midiPreviewLayout({
      ...base,
      notes: [note(0)],
      lengthTicks: 2880,
      contentLengthTicks: 960,
    });
    expect(rects.map((r) => r.x)).toEqual([0, 100, 200]);
  });

  it("draws a separator at every content boundary, none at the leading edge", () => {
    const { separatorXs } = midiPreviewLayout({
      ...base,
      notes: [note(0)],
      lengthTicks: 2880,
      contentLengthTicks: 960,
    });
    expect(separatorXs).toEqual([100, 200]);
  });

  it("crops repeats at the placement end", () => {
    // placement is 1.5 contents: a note late in the content exists once,
    // a note at the content start exists twice.
    const { rects } = midiPreviewLayout({
      ...base,
      notes: [note(0, 240, 64), note(600, 240, 60)],
      lengthTicks: 1440,
      contentLengthTicks: 960,
    });
    const xs = rects.map((r) => r.x).sort((a, b) => a - b);
    // ticks 0, 600, 960 at 300px/1440t -> px 0, 125, 200 (tick 1560 cropped)
    expect(xs).toEqual([0, 125, 200]);
  });

  it("renders a non-looped clip as a single pass with no separators", () => {
    const { rects, separatorXs } = midiPreviewLayout({
      ...base,
      notes: [note(0), note(480)],
      lengthTicks: 960,
      contentLengthTicks: 960,
    });
    expect(rects).toHaveLength(2);
    expect(separatorXs).toEqual([]);
  });

  it("returns an empty layout for an empty clip", () => {
    const { rects, separatorXs } = midiPreviewLayout({
      ...base,
      notes: [],
      lengthTicks: 960,
      contentLengthTicks: 960,
    });
    expect(rects).toEqual([]);
    expect(separatorXs).toEqual([]);
  });

  it("culls rects fully outside the visible canvas window", () => {
    // canvas shows the middle 100px of a 300px clip (offset 100)
    const { rects } = midiPreviewLayout({
      ...base,
      offsetPx: 100,
      canvasW: 100,
      notes: [note(0)],
      lengthTicks: 2880,
      contentLengthTicks: 960,
    });
    // repeats at clip-px 0, 100, 200 -> canvas-px -100 (culled), 0, 100(+w culled)
    expect(rects.map((r) => r.x)).toEqual([0]);
  });

  it("carries velocity through for the component's alpha ramp", () => {
    const { rects } = midiPreviewLayout({
      ...base,
      notes: [note(0, 240, 60, 37)],
      lengthTicks: 960,
      contentLengthTicks: 960,
    });
    expect(rects[0].velocity).toBe(37);
  });
});
