import { beforeEach, describe, expect, it, vi } from "vitest";

const historyOverview = vi.fn();
const historyVersion = vi.fn();

vi.mock("../tauri", () => ({
  backend: { mode: "tauri", historyOverview, historyVersion },
}));

const { historyBrowser } = await import("./history.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  historyBrowser.overview = null;
  historyBrowser.detail = null;
  historyBrowser.selectedRev = null;
  historyBrowser.error = null;
  historyOverview.mockResolvedValue({
    undoDepth: 2,
    redoDepth: 0,
    retainedBytes: 384,
    materialized: 1,
    replayOnly: 1,
    versions: [
      { rev: 9, materialized: false, chargedBytes: 128 },
      { rev: 8, materialized: true, chargedBytes: 256 },
    ],
  });
  historyVersion.mockResolvedValue({
    rev: 9,
    projectName: "Song",
    trackCount: 3,
    audioClipCount: 1,
    midiClipCount: 2,
    automationLaneCount: 1,
  });
});

describe("history browser", () => {
  it("loads newest-first graph metadata and materializes only the selected revision", async () => {
    await historyBrowser.load();

    expect(historyOverview).toHaveBeenCalledOnce();
    expect(historyVersion).toHaveBeenCalledWith(9);
    expect(historyBrowser.selectedRev).toBe(9);
    expect(historyBrowser.detail?.trackCount).toBe(3);
  });

  it("reports when a selected revision was evicted between list and detail reads", async () => {
    historyVersion.mockResolvedValueOnce(null);

    await historyBrowser.load();

    expect(historyBrowser.detail).toBeNull();
    expect(historyBrowser.error).toContain("no longer retained");
  });
});
