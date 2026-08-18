import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";

const historyOverview = vi.fn();
const historyVersion = vi.fn();
const undo = vi.fn(() => Promise.resolve());
const redo = vi.fn(() => Promise.resolve());
let projectChanged: (() => void) | null = null;
const unlisten = vi.fn();
const on = vi.fn((event: string, cb: () => void) => {
  if (event === "project://changed") projectChanged = cb;
  return unlisten;
});

vi.mock("../../tauri", () => ({
  backend: { mode: "tauri", historyOverview, historyVersion, on },
}));
vi.mock("../../state/projectops.svelte", () => ({
  projectops: { undo, redo },
}));

const { default: HistoryPanel } = await import("./HistoryPanel.svelte");
const { historyBrowser } = await import("../../state/history.svelte");

function overview(undoDepth = 2, redoDepth = 1) {
  return {
    undoDepth,
    redoDepth,
    retainedBytes: 384,
    materialized: 1,
    replayOnly: 1,
    versions: [
      { rev: 9, materialized: false, chargedBytes: 128, label: "set gain", actor: "You" },
      { rev: 8, materialized: true, chargedBytes: 256, label: "move clip", actor: "You" },
    ],
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  projectChanged = null;
  historyBrowser.overview = null;
  historyBrowser.detail = null;
  historyBrowser.selectedRev = null;
  historyBrowser.loading = false;
  historyBrowser.error = null;
});

describe("HistoryPanel", () => {
  it("mounts the retained revisions and materializes the newest one for its summary", async () => {
    historyOverview.mockResolvedValue(overview());
    historyVersion.mockResolvedValue({
      rev: 9,
      projectName: "Song",
      trackCount: 3,
      audioClipCount: 1,
      midiClipCount: 2,
      automationLaneCount: 1,
    });

    render(HistoryPanel);

    expect(await screen.findByRole("option", { name: /set gain r9 .* replay/i })).toBeTruthy();
    expect(screen.getByRole("option", { name: /move clip r8 .* snapshot/i })).toBeTruthy();
    expect(await screen.findByText("3")).toBeTruthy();
    expect(historyVersion).toHaveBeenCalledWith(9);
  });

  it("refreshes an open panel when the project announces a committed edit", async () => {
    historyOverview
      .mockResolvedValueOnce(overview())
      .mockResolvedValueOnce({
        ...overview(3, 0),
        versions: [
          { rev: 10, materialized: false, chargedBytes: 96, label: "rename track", actor: "You" },
          ...overview().versions,
        ],
      });
    historyVersion.mockResolvedValue({
      rev: 9, projectName: "Song", trackCount: 3, audioClipCount: 1, midiClipCount: 2, automationLaneCount: 1,
    });

    render(HistoryPanel);
    await screen.findByRole("option", { name: /set gain r9 .* replay/i });
    projectChanged?.();

    expect(await screen.findByRole("option", { name: /rename track r10 .* replay/i })).toBeTruthy();
    expect(historyOverview).toHaveBeenCalledTimes(2);
  });

  it("drives ordinary undo and refreshes the graph without restoring a revision directly", async () => {
    historyOverview.mockResolvedValueOnce(overview()).mockResolvedValueOnce(overview(1, 2));
    historyVersion.mockResolvedValue({
      rev: 9,
      projectName: "Song",
      trackCount: 3,
      audioClipCount: 1,
      midiClipCount: 2,
      automationLaneCount: 1,
    });

    render(HistoryPanel);
    const undoButton = await screen.findByRole("button", { name: /undo\s*2/i });
    await fireEvent.click(undoButton);

    await waitFor(() => expect(undo).toHaveBeenCalledOnce());
    await waitFor(() => expect(historyOverview).toHaveBeenCalledTimes(2));
    expect(historyVersion).toHaveBeenCalledWith(9);
  });
});
