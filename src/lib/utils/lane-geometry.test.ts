import { describe, expect, it } from "vitest";
import { buildLaneBoxes, laneIndexAt, TRACK_HEIGHT_PX } from "./lane-geometry";

describe("laneIndexAt", () => {
  it("maps y to the lane it falls in", () => {
    expect(laneIndexAt(0, 3)).toBe(0);
    expect(laneIndexAt(TRACK_HEIGHT_PX - 1, 3)).toBe(0);
    expect(laneIndexAt(TRACK_HEIGHT_PX, 3)).toBe(1);
    expect(laneIndexAt(TRACK_HEIGHT_PX * 2 + 4, 3)).toBe(2);
  });

  it("clamps below zero and above the last track", () => {
    expect(laneIndexAt(-500, 3)).toBe(0);
    expect(laneIndexAt(TRACK_HEIGHT_PX * 99, 3)).toBe(2);
  });

  it("returns 0 for an empty arrangement rather than -1", () => {
    expect(laneIndexAt(0, 0)).toBe(0);
  });
});

describe("buildLaneBoxes", () => {
  const trackIds = ["t1", "t2"];
  const audioClips = [
    { id: "a1", trackId: "t1", timelineStartSamples: 100, lengthSamples: 400 },
    { id: "a2", trackId: "zz", timelineStartSamples: 0, lengthSamples: 10 },
  ];
  const midiClips = [{ id: "m1", trackId: "t2", timelineStartTicks: 960, lengthTicks: 1920 }];
  // a flat 1 tick = 10 samples map, enough to prove the conversion is used
  const ticksToSamples = (t: number) => t * 10;

  it("places each clip on its track's lane index with a sample span", () => {
    const boxes = buildLaneBoxes({ trackIds, audioClips, midiClips, ticksToSamples });
    expect(boxes).toEqual([
      { ref: { kind: "audio", id: "a1" }, laneIndex: 0, startSamples: 100, endSamples: 500 },
      { ref: { kind: "midi", id: "m1" }, laneIndex: 1, startSamples: 9600, endSamples: 28800 },
    ]);
  });

  it("drops clips whose track is not on the timeline", () => {
    const boxes = buildLaneBoxes({ trackIds, audioClips, midiClips, ticksToSamples });
    expect(boxes.some((b) => b.ref.id === "a2")).toBe(false);
  });

  it("converts a MIDI clip's END through the map, not by adding a converted length", () => {
    // a non-linear map: doubling after tick 1000
    const nonLinear = (t: number) => (t <= 1000 ? t : 1000 + (t - 1000) * 2);
    const boxes = buildLaneBoxes({
      trackIds,
      audioClips: [],
      midiClips: [{ id: "m1", trackId: "t2", timelineStartTicks: 900, lengthTicks: 200 }],
      ticksToSamples: nonLinear,
    });
    expect(boxes[0].startSamples).toBe(900);
    expect(boxes[0].endSamples).toBe(1200); // f(1100), not 900 + f(200)
  });
});
