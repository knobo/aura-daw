import { describe, expect, it, vi } from "vitest";
import { analyzePitchTrack, type PitchAnalysisState } from "./analyze";

describe("analyzePitchTrack", () => {
  it("reports pending before the persisted frame count", async () => {
    const states: PitchAnalysisState[] = [];
    const analyze = vi.fn(async () => 321);

    await analyzePitchTrack("clip-1", analyze, (state) => states.push(state));

    expect(analyze).toHaveBeenCalledWith("clip-1");
    expect(states).toEqual([{ phase: "analyzing" }, { phase: "done", frames: 321 }]);
  });

  it("normalizes backend failures into visible state", async () => {
    const states: PitchAnalysisState[] = [];
    const analyze = vi.fn(async () => {
      throw new Error("audio is missing");
    });

    await analyzePitchTrack("clip-2", analyze, (state) => states.push(state));

    expect(states).toEqual([
      { phase: "analyzing" },
      { phase: "error", message: "Error: audio is missing" },
    ]);
  });
});
