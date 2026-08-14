/**
 * Project store: clip placement. `moveClip` is a frontend-only preview used
 * DURING a drag/arrow-key gesture; `commitClipMove` persists the store's
 * CURRENT position through the `move_clip` channel at the end of the
 * gesture — mirrors midi.svelte.ts's moveClip/commitBounds split (Plan E
 * Task 4).
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Clip } from "../types/ipc";

const moveClip = vi.fn(() => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri" as const,
    on: () => () => {},
    moveClip,
  },
}));

const { project } = await import("./project.svelte");

function testClip(overrides: Partial<Clip> = {}): Clip {
  return {
    id: "c-1",
    trackId: "t-1",
    name: "Clip",
    sourcePath: "audio/c-1.wav",
    sourceChannels: 2,
    sourceSampleRate: 48000,
    sourceLengthSamples: 96000,
    timelineStartSamples: 0,
    offsetSamples: 0,
    lengthSamples: 96000,
    gainDb: 0,
    fadeInSamples: 0,
    fadeOutSamples: 0,
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  project.clips = [];
});

describe("commitClipMove", () => {
  it("invokes move_clip with the store's CURRENT timelineStartSamples", async () => {
    project.clips = [testClip({ id: "c-1", timelineStartSamples: 0 })];
    project.moveClip("c-1", 48_000);

    await project.commitClipMove("c-1");

    expect(moveClip).toHaveBeenCalledWith("c-1", 48_000);
  });

  it("does not invoke when the clip id is unknown", async () => {
    project.clips = [testClip({ id: "c-1" })];

    await project.commitClipMove("no-such-clip");

    expect(moveClip).not.toHaveBeenCalled();
  });
});
