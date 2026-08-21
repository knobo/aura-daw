/**
 * Ctrl+P quick picker (plan §5.3): one ranked list, Enter adds as
 * instrument, Shift+Enter as insert, Esc closes.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import type { PluginDescriptor, TrackState } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const instantiate = vi.fn(() => Promise.resolve(null));
const insertEffect = vi.fn(() => Promise.resolve());

vi.mock("../../tauri", () => ({
  backend: { mode: "demo", on: () => () => {} },
}));

const { default: PluginQuickPick } = await import("./PluginQuickPick.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { project } = await import("../../state/project.svelte");
const { lanes } = await import("../../state/lanes.svelte");
const { ui } = await import("../../state/ui.svelte");

function desc(
  uid: string,
  name: string,
  extra: Partial<PluginDescriptor> = {},
): PluginDescriptor {
  return {
    uid,
    format: "clap",
    name,
    isInstrument: true,
    audioInputs: 0,
    audioOutputs: 2,
    hasNoteInput: true,
    ...extra,
  };
}

function track(id: string, name: string): TrackState {
  return {
    id,
    name,
    kind: "midi",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#38bdf8",
  };
}

const origInstantiate = plugins.instantiate.bind(plugins);
const origInsertEffect = plugins.insertEffect.bind(plugins);

beforeEach(() => {
  instantiate.mockClear();
  insertEffect.mockClear();
  plugins.instantiate = instantiate as typeof plugins.instantiate;
  plugins.insertEffect = insertEffect as typeof plugins.insertEffect;
  plugins.descriptors = [
    desc("u-surge", "Surge XT"),
    desc("u-verb", "Calf Reverb", { isInstrument: false }),
    desc("u-vital", "Vital"),
  ];
  plugins.catalog = {
    version: 1,
    scan: { entries: [] },
    favorites: ["u-vital"],
    recents: [],
    tags: {},
    pinnedParams: {},
  };
  project.tracks = [track("t1", "Bass")];
  lanes.selectOnly("t1");
  ui.pluginPickerOpen = true;
});

afterEach(() => {
  cleanup();
  ui.pluginPickerOpen = false;
  plugins.descriptors = [];
  plugins.instantiate = origInstantiate;
  plugins.insertEffect = origInsertEffect;
  lanes.clearSelection();
});

describe("PluginQuickPick", () => {
  it("lists favourites first", () => {
    render(PluginQuickPick);
    const options = screen.getAllByRole("option").map((el) => el.textContent ?? "");
    expect(options[0]).toMatch(/Vital/);
    expect(options[0]).toMatch(/Bass/);
  });

  it("Enter instantiates the highlighted plugin onto the selected track", async () => {
    render(PluginQuickPick);
    const dialog = screen.getByRole("dialog");
    await fireEvent.keyDown(dialog, { key: "Enter" });
    expect(instantiate).toHaveBeenCalledWith("u-vital", "t1");
  });

  it("Shift+Enter adds as an insert", async () => {
    render(PluginQuickPick);
    const dialog = screen.getByRole("dialog");
    await fireEvent.keyDown(dialog, { key: "Enter", shiftKey: true });
    expect(insertEffect).toHaveBeenCalledWith("t1", "u-vital");
  });

  it("Escape closes the picker", async () => {
    render(PluginQuickPick);
    await fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(ui.pluginPickerOpen).toBe(false);
  });
});
