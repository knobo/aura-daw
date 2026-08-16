/**
 * Lane geometry. The plan's cases verbatim, plus the boundaries they leave
 * open. One plan expectation was corrected — see `autoFitRange` below.
 */
import { describe, it, expect } from "vitest";
import { autoFitRange, yOfKey, centsToTarget, bandFor, trailSegments } from "./lane";
import type { PitchFrame } from "../types/ipc";

const note = (key: number) => ({ noteId: key, startSample: 0, endSample: 480, key });
const f = (sample: number, voiced: boolean): PitchFrame => ({
  sample,
  midi: 57,
  hz: 220,
  clarity: 0.9,
  rms: 0.2,
  voiced,
});

describe("autoFitRange", () => {
  it("pads two semitones either side of the phrase, then to the minimum span", () => {
    // The plan's table said {58, 66} here AND "widen to a minimum span of
    // 12" two paragraphs later; those cannot both hold, since 66-58 is 8.
    // The minimum wins: a four-semitone phrase drawn 8 semitones tall puts
    // the trail of anyone who cannot hit a pitch off the lane entirely.
    expect(autoFitRange([note(60), note(64)])).toEqual({ lowKey: 56, highKey: 68 });
  });

  it("pads without widening once the phrase is wider than the minimum", () => {
    expect(autoFitRange([note(55), note(72)])).toEqual({ lowKey: 53, highKey: 74 });
  });

  it("falls back to a usable range when there are no notes", () => {
    const r = autoFitRange([], 60);
    expect(r.highKey - r.lowKey).toBeGreaterThanOrEqual(12);
    expect(r.lowKey).toBeLessThanOrEqual(60);
    expect(r.highKey).toBeGreaterThanOrEqual(60);
  });

  it("keeps a minimum span so a single note does not fill the lane", () => {
    const r = autoFitRange([note(60)]);
    expect(r.highKey - r.lowKey).toBeGreaterThanOrEqual(12);
  });

  it("centres the widened lane on the phrase", () => {
    const r = autoFitRange([note(60)]);
    expect((r.lowKey + r.highKey) / 2).toBeCloseTo(60);
  });
});

describe("yOfKey", () => {
  it("puts the high key at the top and the low key at the bottom", () => {
    const r = { lowKey: 55, highKey: 67 };
    expect(yOfKey(67, r, 120)).toBeLessThan(yOfKey(55, r, 120));
    expect(yOfKey(67, r, 120)).toBeCloseTo(0, 0);
    expect(yOfKey(55, r, 120)).toBeCloseTo(120, 0);
  });

  it("places a float key between its neighbours, not on one of them", () => {
    // The whole point of drawing PitchFrame.midi rather than the note name.
    const r = { lowKey: 55, highKey: 67 };
    expect(yOfKey(61.5, r, 120)).toBeCloseTo(55, 5);
  });

  it("survives a degenerate range instead of dividing by zero", () => {
    expect(yOfKey(60, { lowKey: 60, highKey: 60 }, 120)).toBe(60);
  });
});

describe("centsToTarget", () => {
  it("is zero on the note", () => {
    expect(centsToTarget(60, 60)).toBeCloseTo(0);
  });

  it("is signed", () => {
    expect(centsToTarget(60.25, 60)).toBeCloseTo(25);
    expect(centsToTarget(59.75, 60)).toBeCloseTo(-25);
  });

  it("folds octaves, so an octave slip reads as on-pitch", () => {
    expect(centsToTarget(72, 60)).toBeCloseTo(0);
    expect(centsToTarget(48, 60)).toBeCloseTo(0);
  });

  it("never exceeds half an octave after folding", () => {
    for (let m = 40; m <= 90; m += 0.5) {
      expect(Math.abs(centsToTarget(m, 60))).toBeLessThanOrEqual(600.01);
    }
  });

  it("takes the shorter way round a tritone", () => {
    expect(centsToTarget(65.5, 60)).toBeCloseTo(550);
    expect(centsToTarget(66.5, 60)).toBeCloseTo(-550);
  });
});

describe("bandFor", () => {
  it("bands by the active tolerance", () => {
    expect(bandFor(10, 50)).toBe("in");
    expect(bandFor(-49, 50)).toBe("in");
    expect(bandFor(80, 50)).toBe("near");
    expect(bandFor(400, 50)).toBe("far");
  });

  it("is inclusive at both edges, so the tolerance means what it says", () => {
    expect(bandFor(50, 50)).toBe("in");
    expect(bandFor(-50, 50)).toBe("in");
    expect(bandFor(100, 50)).toBe("near");
    expect(bandFor(100.1, 50)).toBe("far");
  });

  it("tracks a stricter tolerance", () => {
    expect(bandFor(30, 20)).toBe("near");
    expect(bandFor(30, 33)).toBe("in");
  });
});

describe("trailSegments", () => {
  it("breaks the trail on unvoiced frames rather than interpolating", () => {
    const segs = trailSegments([f(0, true), f(1, true), f(2, false), f(3, true)]);
    expect(segs.map((s) => s.length)).toEqual([2, 1]);
  });

  it("returns nothing for an all-unvoiced run", () => {
    expect(trailSegments([f(0, false), f(1, false)])).toEqual([]);
  });

  it("returns nothing for no frames", () => {
    expect(trailSegments([])).toEqual([]);
  });

  it("keeps each run's frames in order", () => {
    const segs = trailSegments([f(0, true), f(1, true), f(2, false), f(3, true), f(4, true)]);
    expect(segs.map((s) => s.map((x) => x.sample))).toEqual([
      [0, 1],
      [3, 4],
    ]);
  });
});
