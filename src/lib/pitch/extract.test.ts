import { describe, expect, it, vi } from "vitest";
import { extractMelodyFromAudio, type MelodyExtractionState } from "./extract";
import type { ExtractMelodyReply, ExtractMelodyRequest } from "../types/ipc";

describe("extractMelodyFromAudio", () => {
  it("reports pending before the extracted reply", async () => {
    const states: MelodyExtractionState[] = [];
    const reply: ExtractMelodyReply = {
      trackId: "track-m1",
      clipId: "clip-m1",
      noteCount: 12,
      createdTrack: true,
    };
    const extract = vi.fn(async (_req: ExtractMelodyRequest) => reply);
    const req: ExtractMelodyRequest = { clipId: "clip-audio-1", setAsPitchReference: true };

    const result = await extractMelodyFromAudio(req, extract, (s) => states.push(s));

    expect(extract).toHaveBeenCalledWith(req);
    expect(result).toEqual(reply);
    expect(states).toEqual([
      { phase: "extracting" },
      { phase: "done", reply },
    ]);
  });

  it("normalizes backend errors into error state", async () => {
    const states: MelodyExtractionState[] = [];
    const extract = vi.fn(async () => {
      throw new Error("no melody detected in clip");
    });
    const req: ExtractMelodyRequest = { clipId: "clip-audio-2" };

    const result = await extractMelodyFromAudio(req, extract, (s) => states.push(s));

    expect(result).toBeNull();
    expect(states).toEqual([
      { phase: "extracting" },
      { phase: "error", message: "Error: no melody detected in clip" },
    ]);
  });
});
