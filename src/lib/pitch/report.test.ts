import { describe, it, expect } from "vitest";
import { centsBarGeometry, sortNotes } from "./report";
import type { NoteScore } from "../types/ipc";

const n = (id: number, start: number, hit: number, extra: Partial<NoteScore> = {}): NoteScore => ({
  noteId: id,
  startSample: start,
  endSample: start + 480,
  key: 60,
  hitFraction: hit,
  coverage: 1,
  meanCents: 0,
  medianCents: 0,
  onsetOffsetMs: 0,
  stabilityCents: 0,
  vibratoRateHz: 0,
  vibratoExtentCents: 0,
  ambiguous: false,
  ...extra,
});

describe("centsBarGeometry", () => {
  it("draws from the centre, rightwards when sharp", () => {
    const g = centsBarGeometry(50, 50, 200);
    expect(g.x).toBeGreaterThanOrEqual(100);
    expect(g.w).toBeGreaterThan(0);
  });

  it("draws leftwards when flat", () => {
    const g = centsBarGeometry(-50, 50, 200);
    expect(g.x).toBeLessThan(100);
    expect(g.x + g.w).toBeCloseTo(100, 0);
  });

  it("clamps a wild reading inside the bar", () => {
    const g = centsBarGeometry(5000, 50, 200);
    expect(g.x + g.w).toBeLessThanOrEqual(200);
    expect(g.band).toBe("far");
  });

  it("is a zero-width mark exactly on pitch", () => {
    expect(centsBarGeometry(0, 50, 200).w).toBeLessThanOrEqual(2);
  });

  it("bands the bar the same way the lane bands the trail", () => {
    // One tolerance vocabulary across the whole feature: a note the lane
    // coloured cyan while singing must not turn amber in the report.
    expect(centsBarGeometry(20, 25, 200).band).toBe("in");
    expect(centsBarGeometry(40, 25, 200).band).toBe("near");
    expect(centsBarGeometry(-40, 25, 200).band).toBe("near");
  });

  it("scales to the tolerance, so a tighter tier reads as a longer bar", () => {
    // The same 25 cents is a near miss at the strict tier and nothing at
    // the loose one. A fixed cents-per-pixel scale would draw them alike.
    const strict = centsBarGeometry(25, 10, 200);
    const loose = centsBarGeometry(25, 50, 200);
    expect(strict.w).toBeGreaterThan(loose.w);
  });

  it("survives a zero tolerance instead of dividing by it", () => {
    const g = centsBarGeometry(30, 0, 200);
    expect(Number.isFinite(g.x)).toBe(true);
    expect(Number.isFinite(g.w)).toBe(true);
    expect(g.x + g.w).toBeLessThanOrEqual(200);
  });
});

describe("sortNotes", () => {
  it("orders by time by default", () => {
    expect(sortNotes([n(2, 960, 1), n(1, 0, 0.2)], "time").map((x) => x.noteId)).toEqual([1, 2]);
  });

  it("puts the worst notes first when asked, so practice starts there", () => {
    expect(sortNotes([n(2, 960, 1), n(1, 0, 0.2)], "worst").map((x) => x.noteId)).toEqual([1, 2]);
  });

  it("breaks a tie by time, so the order is stable between renders", () => {
    const rows = sortNotes([n(3, 1920, 0.5), n(1, 0, 0.5), n(2, 960, 0.5)], "worst");
    expect(rows.map((x) => x.noteId)).toEqual([1, 2, 3]);
  });

  it("does not mutate the array it was given", () => {
    const input = [n(2, 960, 1), n(1, 0, 0.2)];
    sortNotes(input, "worst");
    expect(input.map((x) => x.noteId)).toEqual([2, 1]);
  });

  it("ranks a note that was never sung below one that was sung badly", () => {
    // Both score 0 on hitFraction, and the advice differs: one needs
    // tuning, the other needs singing at all. Coverage is the tie-break.
    const rows = sortNotes([n(1, 0, 0, { coverage: 0.9 }), n(2, 960, 0, { coverage: 0 })], "worst");
    expect(rows.map((x) => x.noteId)).toEqual([2, 1]);
  });
});
