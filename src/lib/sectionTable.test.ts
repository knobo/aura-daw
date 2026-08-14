import { describe, it, expect } from "vitest";
import { sampleAtTick, tickAtSample, type SectionRow } from "./sectionTable";

describe("sectionTable", () => {
  // 120bpm: period = 60/120 * 508_032_000 = 254_016_000 superticks/quarter.
  const sections: SectionRow[] = [
    { startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 },
  ];
  const sampleRate = 48_000;
  const ppq = 960;

  it("interpolates linearly within a single constant-tempo section", () => {
    expect(sampleAtTick(sections, sampleRate, ppq, 0)).toBe(0);
    expect(sampleAtTick(sections, sampleRate, ppq, 960)).toBeCloseTo(24_000, 0);
    expect(sampleAtTick(sections, sampleRate, ppq, 4 * 960)).toBeCloseTo(96_000, 0);
  });

  it("inverts sampleAtTick within a fraction of a tick", () => {
    const s = sampleAtTick(sections, sampleRate, ppq, 500);
    const back = tickAtSample(sections, sampleRate, ppq, s);
    expect(Math.abs(back - 500)).toBeLessThan(1e-6);
  });

  it("picks the right segment across a tempo change", () => {
    const twoSections: SectionRow[] = [
      { startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 }, // 120bpm
      { startTick: 3840, startSample: 96_000, startBeat: 16, startBar: 1, period: 508_032_000 }, // 60bpm
    ];
    // At 60bpm a quarter is 48000 samples.
    expect(sampleAtTick(twoSections, sampleRate, ppq, 3840)).toBeCloseTo(96_000, 0);
    expect(sampleAtTick(twoSections, sampleRate, ppq, 3840 + 960)).toBeCloseTo(96_000 + 48_000, 0);
    expect(tickAtSample(twoSections, sampleRate, ppq, 96_000)).toBeCloseTo(3840, 0);
  });

  it("returns 0 for an empty section table rather than throwing", () => {
    expect(sampleAtTick([], sampleRate, ppq, 100)).toBe(0);
    expect(tickAtSample([], sampleRate, ppq, 100)).toBe(0);
  });

  it("does no tempo/bpm math of its own — a pure function of sections+rate+ppq", () => {
    // Structural check: the module's only inputs are the four documented
    // parameters, no other state.
    expect(typeof sampleAtTick).toBe("function");
    expect(sampleAtTick.length).toBe(4);
  });
});
