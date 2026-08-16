/**
 * Transport-bar tempo / meter writes. The production change that would
 * fail these: setTempo flattening the call into a local assignment
 * without setTempoMap, or setMeter omitting the meter argument so the
 * backend keeps 4/4.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TempoMapState } from "../types/ipc";

const setTempoMap = vi.fn(
  async (
    _ppq: number | null,
    events: { tick: number; bpm: number }[],
    meter?: { tick: number; num: number; den: number }[] | null,
  ): Promise<TempoMapState> => ({
    ppq: 960,
    events: events.map((e) => ({ ...e })),
    meterMap: meter?.length ? meter.map((m) => ({ ...m })) : [{ tick: 0, num: 4, den: 4 }],
    periodEvents: [],
    sectionTable: [{ startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254016000 }],
    sectionTableRuleVersion: 1,
  }),
);

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    setTempoMap: (...a: Parameters<typeof setTempoMap>) => setTempoMap(...a),
    getProjectState: () =>
      Promise.resolve({
        transport: { sampleRate: 48000, tempoBpm: 120 },
        tracks: [],
        clips: [],
        ppq: 960,
        tempoEvents: [{ tick: 0, bpm: 120 }],
        midiClips: [],
        meterMap: [{ tick: 0, num: 4, den: 4 }],
        periodEvents: [],
        sectionTable: [],
        sectionTableRuleVersion: 1,
      }),
  },
}));

const { midi } = await import("./midi.svelte");
const { project } = await import("./project.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  project.tempoBpm = 120;
  project.timeSignature = [4, 4];
  project.sampleRate = 48_000;
  midi.ppq = 960;
  midi.tempoEvents = [{ tick: 0, bpm: 120 }];
  midi.sectionTable = [];
});

describe("setTempo", () => {
  it("writes a single tick-0 event and updates the bpm mirror", async () => {
    await midi.setTempo(140);
    expect(setTempoMap).toHaveBeenCalledWith(null, [{ tick: 0, bpm: 140 }]);
    expect(project.tempoBpm).toBe(140);
    expect(midi.tempoEvents).toEqual([{ tick: 0, bpm: 140 }]);
    expect(midi.sectionTable).toHaveLength(1);
  });

  it("clamps out-of-range typed values before they hit the wire", async () => {
    await midi.setTempo(12);
    expect(setTempoMap).toHaveBeenCalledWith(null, [{ tick: 0, bpm: 40 }]);
    expect(project.tempoBpm).toBe(40);
  });
});

describe("setMeter", () => {
  it("sends the current tempo events plus the new meter", async () => {
    midi.tempoEvents = [{ tick: 0, bpm: 90 }];
    project.tempoBpm = 90;
    await midi.setMeter(3, 4);
    expect(setTempoMap).toHaveBeenCalledWith(
      null,
      [{ tick: 0, bpm: 90 }],
      [{ tick: 0, num: 3, den: 4 }],
    );
    expect(project.timeSignature).toEqual([3, 4]);
  });

  it("rejects an invalid signature without invoking the backend", async () => {
    await midi.setMeter(0, 4);
    expect(setTempoMap).not.toHaveBeenCalled();
    expect(project.timeSignature).toEqual([4, 4]);
  });
});

describe("applySnapshot meter", () => {
  it("mirrors meterMap[0] onto project.timeSignature", () => {
    midi.applySnapshot({
      transport: { sampleRate: 48000, tempoBpm: 100, state: "stopped", positionSamples: 0, loopEnabled: false, loopStartSamples: 0, loopEndSamples: 0, songEndSamples: 0, stopAtEnd: false },
      tracks: [],
      clips: [],
      midiClips: [],
      ppq: 960,
      tempoEvents: [{ tick: 0, bpm: 100 }],
      meterMap: [{ tick: 0, num: 6, den: 8 }],
      periodEvents: [],
      sectionTable: [],
      sectionTableRuleVersion: 1,
    });
    expect(project.timeSignature).toEqual([6, 8]);
    expect(project.tempoBpm).toBe(100);
  });
});

describe("bar length follows the denominator", () => {
  it("makes a 6/8 bar three quarter-notes, not six", () => {
    project.tempoBpm = 120;
    project.sampleRate = 48_000;
    project.timeSignature = [6, 8];
    midi.ppq = 960;
    expect(project.samplesPerBar).toBe(72_000);
    expect(midi.ticksPerBar).toBe(2880);
  });
});
