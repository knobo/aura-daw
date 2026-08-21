/**
 * Sampler audition is a gate: the note sounds while the pointer is down
 * and releases on up, so a long instrument can actually be heard.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokes = {
  samplerPreviewNote: vi.fn(async () => {}),
  samplerPreviewNoteOn: vi.fn(async () => {}),
  samplerPreviewNoteOff: vi.fn(async () => {}),
};

vi.mock("../tauri", () => ({
  backend: { mode: "tauri", on: () => () => {}, ...invokes },
}));

const { instruments } = await import("./instruments.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  instruments.previewingId = null;
});

describe("instruments hold-to-play", () => {
  it("previewDown starts a held C3 and previewUp releases it", async () => {
    await instruments.previewDown("piano", 60, 100);
    expect(invokes.samplerPreviewNoteOn).toHaveBeenCalledWith("piano", 60, 100);
    expect(instruments.previewingId).toBe("piano");

    await instruments.previewUp();
    expect(invokes.samplerPreviewNoteOff).toHaveBeenCalledWith(60);
    expect(instruments.previewingId).toBeNull();
  });

  it("a new previewDown releases the previous key first", async () => {
    await instruments.previewDown("piano", 60);
    await instruments.previewDown("organ", 64);
    expect(invokes.samplerPreviewNoteOff).toHaveBeenCalledWith(60);
    expect(invokes.samplerPreviewNoteOn).toHaveBeenLastCalledWith("organ", 64, 100);
  });
});
