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
      { rev: 9, materialized: false, chargedBytes: 128, label: "set gain", actor: "You" },
      { rev: 8, materialized: true, chargedBytes: 256, label: "move clip", actor: "You" },
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

  it("does not let a stale materialization overwrite a newer selection", async () => {
    await historyBrowser.load();
    let release!: (value: Awaited<ReturnType<typeof historyVersion>>) => void;
    historyVersion.mockReturnValueOnce(new Promise((resolve) => (release = resolve)));

    const oldSelection = historyBrowser.select(8);
    historyVersion.mockResolvedValueOnce({
      rev: 9, projectName: "Song", trackCount: 9, audioClipCount: 0, midiClipCount: 0, automationLaneCount: 0,
    });
    await historyBrowser.select(9);
    release({
      rev: 8, projectName: "Old", trackCount: 8, audioClipCount: 0, midiClipCount: 0, automationLaneCount: 0,
    });
    await oldSelection;

    expect(historyBrowser.selectedRev).toBe(9);
    expect(historyBrowser.detail?.rev).toBe(9);
  });

  it("coalesces tab activation and component mount into one automatic load", async () => {
    let release!: (value: Awaited<ReturnType<typeof historyOverview>>) => void;
    historyOverview.mockReturnValueOnce(new Promise((resolve) => (release = resolve)));

    const fromTab = historyBrowser.load();
    const fromMount = historyBrowser.load();

    expect(historyOverview).toHaveBeenCalledOnce();
    release({
      undoDepth: 0, redoDepth: 0, retainedBytes: 0, materialized: 0, replayOnly: 0, versions: [],
    });
    await Promise.all([fromTab, fromMount]);
    expect(historyOverview).toHaveBeenCalledOnce();
  });
});
