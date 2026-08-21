/**
 * Collapse-all has to show up on a REAL browser, not just BrowserShell's
 * harness. The shell's button is gated on `onFoldAll`; if a caller forgets
 * to pass it, the control vanishes and the user is right to say they
 * don't see collapse in the UI.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import type { InstrumentInfo } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const samplerPreviewNoteOn = vi.fn(async (_id: string, _key: number, _vel: number) => {});
const samplerPreviewNoteOff = vi.fn(async (_key: number) => {});

vi.mock("../../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    samplerPreviewNote: () => Promise.resolve(),
    samplerPreviewNoteOn: (...args: [string, number, number]) => samplerPreviewNoteOn(...args),
    samplerPreviewNoteOff: (...args: [number]) => samplerPreviewNoteOff(...args),
    setTrackInstrument: () => Promise.resolve({ instrumentId: null }),
  },
}));

const { default: InstrumentBrowser } = await import("./InstrumentBrowser.svelte");
const { instruments } = await import("../../state/instruments.svelte");
const { project } = await import("../../state/project.svelte");

function inst(id: string, name: string, folder: string): InstrumentInfo {
  return {
    id,
    name,
    sfzPath: `/banks/${folder}/${name}.sfz`,
    regionCount: 1,
  };
}

beforeEach(() => {
  samplerPreviewNoteOn.mockClear();
  samplerPreviewNoteOff.mockClear();
  instruments.list = [
    inst("a", "Piano", "keys"),
    inst("b", "Organ", "keys"),
    inst("c", "Kick", "drums"),
  ];
  instruments.error = null;
  instruments.loading = false;
  project.tracks = [];
});

afterEach(() => {
  cleanup();
  instruments.list = [];
});

describe("InstrumentBrowser collapse-all", () => {
  const foldAll = () => screen.getByRole("button", { name: /collapse all|expand all/i });

  it("offers a collapse-all button next to search", () => {
    render(InstrumentBrowser);
    expect(foldAll()).toBeTruthy();
  });

  it("folds every bank group on press", async () => {
    render(InstrumentBrowser);
    expect(screen.getByText("Piano")).toBeTruthy();
    expect(screen.getByText("Kick")).toBeTruthy();

    await fireEvent.click(foldAll());

    expect(screen.queryByText("Piano")).toBeNull();
    expect(screen.queryByText("Kick")).toBeNull();
    expect(foldAll().getAttribute("aria-label")).toMatch(/expand all/i);
  });
});

describe("InstrumentBrowser hold-to-play", () => {
  it("holds C3 while the pointer is down on a piano key and releases on up", async () => {
    render(InstrumentBrowser);
    const c4 = screen.getAllByRole("button", { name: /^c4$/i })[0];
    await fireEvent.pointerDown(c4);
    await waitFor(() => expect(samplerPreviewNoteOn).toHaveBeenCalledWith("a", 60, 100));
    await fireEvent.pointerUp(c4);
    await waitFor(() => expect(samplerPreviewNoteOff).toHaveBeenCalledWith(60));
  });
});

