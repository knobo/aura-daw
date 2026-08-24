/**
 * Plugin Manager (plan §5 / design §9): browse + rack in one shell, RACK
 * first when the project has instances, ALL/INST/FX chips, collapse-all.
 *
 * See Global Constraints in
 * docs/superpowers/plans/2026-08-18-dom-test-environment.md — this file
 * does not touch pointer capture or getBoundingClientRect.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import type { PluginDescriptor, PluginInstanceInfo, TrackState } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const setTrackInstrument = vi.fn(async (trackId: string, instrumentId: string | null) => ({
  id: trackId,
  name: "Bass",
  kind: "midi" as const,
  gainDb: 0,
  pan: 0,
  muted: false,
  soloed: false,
  armed: false,
  automationMode: "read" as const,
  color: "#38bdf8",
  instrumentId,
}));

const pluginInstantiate = vi.fn(async (uid: string) => ({
  id: "new-1",
  uid,
  name: "Surge XT",
  format: "clap" as const,
  status: "active" as const,
}));

const insertAdd = vi.fn(async (trackId: string, uid: string) => ({
  id: "slot-1",
  instanceId: "fx-1",
  bypassed: false,
  trackId,
  uid,
}));

const pluginShowGui = vi.fn(async (_instanceId: string) => {});
const pluginPreviewNote = vi.fn(async (_instanceId: string, _key: number, _velocity: number) => {});

vi.mock("../../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    pluginScan: () => Promise.resolve([]),
    pluginInstantiate: (...args: [string]) => pluginInstantiate(...args),
    insertAdd: (...args: [string, string]) => insertAdd(...args),
    pluginPreviewNote: (...args: [string, number, number]) => pluginPreviewNote(...args),
    pluginRemove: () => Promise.resolve(),
    insertSetBypass: () => Promise.resolve(),
    setTrackInstrument: (...args: [string, string | null]) => setTrackInstrument(...args),
    pluginList: () => Promise.resolve({ plugins: [], instances: [], scanned: true }),
    pluginShowGui: (...args: [string]) => pluginShowGui(...args),
  },
}));

const { default: PluginManager } = await import("./PluginManager.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { project } = await import("../../state/project.svelte");
const { lanes } = await import("../../state/lanes.svelte");
const { modulation } = await import("../../state/modulation.svelte");
const { audition } = await import("../../state/audition.svelte");

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

function inst(id: string, uid: string, name: string): PluginInstanceInfo {
  return { id, uid, name, format: "clap", status: "active" };
}

function track(id: string, name: string, extra: Partial<TrackState> = {}): TrackState {
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
    ...extra,
  };
}

function seedCatalog(over: Partial<(typeof plugins)["catalog"]> = {}) {
  plugins.catalog = {
    version: 1,
    scan: { entries: [] },
    favorites: [],
    recents: [],
    tags: {},
    pinnedParams: {},
    ...over,
  };
}

beforeEach(() => {
  setTrackInstrument.mockClear();
  pluginInstantiate.mockClear();
  insertAdd.mockClear();
  pluginShowGui.mockClear();
  pluginPreviewNote.mockClear();
  plugins.guiById = {};
  plugins.descriptors = [
    desc("u-surge", "Surge XT", { categories: ["Synth"] }),
    desc("u-verb", "Calf Reverb", { isInstrument: false, categories: ["Reverb"] }),
  ];
  plugins.instances = [];
  plugins.scanned = true;
  plugins.scanning = false;
  plugins.scanError = null;
  plugins.error = null;
  seedCatalog();
  project.tracks = [track("t1", "Bass")];
  project.projectDir = null;
  lanes.clearSelection();
  modulation.bindings = [];
  audition.enabled = false;
  audition.lastSilentReason = null;
});

afterEach(() => {
  cleanup();
  plugins.descriptors = [];
  plugins.instances = [];
  plugins.scanned = false;
  audition.enabled = false;
  audition.lastSilentReason = null;
});

describe("PluginManager default mode (winner spec §3.3)", () => {
  it("opens split when the project has live instances — rack and catalog both visible", () => {
    plugins.instances = [inst("i1", "u-surge", "Surge XT")];
    project.tracks = [track("t1", "Bass", { instrumentId: "plugin:i1" })];
    render(PluginManager);

    expect(screen.getByRole("button", { name: /^split$/i }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getAllByText("Surge XT").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Bass").length).toBeGreaterThan(0);
    expect(screen.getByText("Instruments")).toBeTruthy();
  });

  it("opens on browse when nothing is instantiated", () => {
    render(PluginManager);

    expect(screen.getByRole("button", { name: /^browse$/i }).getAttribute("aria-pressed")).toBe(
      "true",
    );
    expect(screen.getByText("Instruments")).toBeTruthy();
    expect(screen.getByText("Surge XT")).toBeTruthy();
  });
});

describe("PluginManager kind chips (design §9.2)", () => {
  it("drops type chips that the current kind cannot produce", async () => {
    render(PluginManager);
    expect(screen.getByRole("button", { name: /^reverb$/i })).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: /^inst$/i }));

    expect(screen.queryByRole("button", { name: /^reverb$/i })).toBeNull();
    expect(screen.getByRole("button", { name: /^synth$/i })).toBeTruthy();
  });

  it("offers ALL / INST / FX, and FX hides instruments", async () => {
    render(PluginManager);

    expect(screen.getByRole("button", { name: /^all$/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^inst$/i })).toBeTruthy();
    const fx = screen.getByRole("button", { name: /^fx$/i });
    await fireEvent.click(fx);

    expect(screen.getByText("Calf Reverb")).toBeTruthy();
    expect(screen.queryByText("Surge XT")).toBeNull();
    expect(screen.queryByText("Instruments")).toBeNull();
    expect(screen.getByText("Effects")).toBeTruthy();
  });
});

describe("PluginManager browse place", () => {
  it("lets you pick which MIDI track an instrument lands on", async () => {
    render(PluginManager);
    const place = screen.getByRole("combobox", { name: /add surge xt to a midi track/i });
    expect(screen.queryByRole("button", { name: /^add$/i })).toBeNull();
    await fireEvent.change(place, { target: { value: "t1" } });
    await waitFor(() => expect(pluginInstantiate).toHaveBeenCalledWith("u-surge"));
    await waitFor(() => expect(setTrackInstrument).toHaveBeenCalledWith("t1", "plugin:new-1"));
  });

  it("lets you pick which track an effect inserts on", async () => {
    render(PluginManager);
    const place = screen.getByRole("combobox", { name: /insert calf reverb on a track/i });
    await fireEvent.change(place, { target: { value: "t1" } });
    await waitFor(() => expect(insertAdd).toHaveBeenCalledWith("t1", "u-verb"));
  });
});

describe("PluginManager rack bind", () => {
  it("lets an unbound instrument bind to a midi track from the rack", async () => {
    plugins.instances = [inst("i1", "u-surge", "Surge XT")];
    render(PluginManager);

    expect(screen.getByText("Not on any track")).toBeTruthy();
    const bind = screen.getByRole("combobox", { name: /bind to a midi track/i });
    await fireEvent.change(bind, { target: { value: "t1" } });
    await waitFor(() => expect(setTrackInstrument).toHaveBeenCalledWith("t1", "plugin:i1"));
  });
});

describe("PluginManager native GUI", () => {
  it("shows a GUI button next to PARAMS when the hosted instance has a native GUI", () => {
    plugins.instances = [inst("i1", "u-glbars", "glBars")];
    plugins.guiById = { i1: true };
    project.tracks = [track("t1", "Bass", { instrumentId: "plugin:i1" })];
    render(PluginManager);
    expect(screen.getByRole("button", { name: /^gui$/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^params$/i })).toBeTruthy();
  });

  it("hides the GUI button when the plugin has no native GUI", () => {
    plugins.instances = [inst("i1", "u-surge", "Surge XT")];
    plugins.guiById = { i1: false };
    project.tracks = [track("t1", "Bass", { instrumentId: "plugin:i1" })];
    render(PluginManager);
    expect(screen.queryByRole("button", { name: /^gui$/i })).toBeNull();
    expect(screen.getByRole("button", { name: /^params$/i })).toBeTruthy();
  });

  it("opens the hosted instance GUI, not a standalone process", async () => {
    plugins.instances = [inst("i1", "u-glbars", "glBars")];
    plugins.guiById = { i1: true };
    project.tracks = [track("t1", "Bass", { instrumentId: "plugin:i1" })];
    render(PluginManager);
    await fireEvent.click(screen.getByRole("button", { name: /^gui$/i }));
    expect(pluginShowGui).toHaveBeenCalledWith("i1");
  });
});

describe("PluginManager rack tools", () => {
  it("keeps favourites and collapse-all when focused on the rack", async () => {
    plugins.instances = [inst("i1", "u-surge", "Surge XT")];
    project.tracks = [track("t1", "Bass", { instrumentId: "plugin:i1" })];
    render(PluginManager);
    await fireEvent.click(screen.getByRole("button", { name: /^rack$/i }));

    expect(screen.getByRole("button", { name: /collapse all|expand all/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: "★" })).toBeTruthy();
  });
});

describe("PluginManager collapse-all", () => {
  it("folds every browse section", async () => {
    render(PluginManager);
    expect(screen.getByText("Surge XT")).toBeTruthy();

    await fireEvent.click(screen.getByRole("button", { name: /collapse all groups/i }));

    expect(screen.queryByText("Surge XT")).toBeNull();
    expect(screen.getByText("Instruments")).toBeTruthy();
  });
});

describe("PluginManager audition (design §8.2, ruling R-3)", () => {
  it("clears a stale silent reason left by a different browser on mount", () => {
    // A double-click in, say, Presets can set this global singleton and
    // never clear it (no decay timer — see audition.svelte.ts). Without a
    // mount-time reset, the manager would open already showing someone
    // else's stale reason.
    audition.lastSilentReason = "open a Zyn instance to audition its patches";
    render(PluginManager);

    expect(audition.lastSilentReason).toBeNull();
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("double-clicking a catalog row with no live instance goes silent, never instantiates (R-3)", async () => {
    audition.enabled = true;
    render(PluginManager);

    const row = screen.getByText("Surge XT").closest(".row");
    if (!row) throw new Error("catalog row not found");
    await fireEvent.dblClick(row);

    expect(pluginInstantiate).not.toHaveBeenCalled();
    expect(audition.lastSilentReason).toBe("no live instance of this plugin to audition");
    expect(screen.getByRole("status").textContent).toBe(
      "no live instance of this plugin to audition",
    );
  });

  it("Shift+Enter on a catalog row reaches the preview path in split mode — the default whenever instances exist", async () => {
    // Regression: onAudition/onActivate used to branch on `mode ===
    // "browse"`, but the catalog BrowserShell renders under `showBrowse`
    // (browse OR split), and split is the default here. That left
    // Shift+Enter silently falling into the (unreachable) rack branch.
    plugins.instances = [inst("i1", "u-surge", "Surge XT")];
    project.tracks = [track("t1", "Bass", { instrumentId: "plugin:i1" })];
    audition.enabled = true;
    render(PluginManager);
    expect(screen.getByRole("button", { name: /^split$/i }).getAttribute("aria-pressed")).toBe(
      "true",
    );

    const tree = screen.getByRole("tree");
    const row = within(tree).getByText("Surge XT").closest(".row");
    if (!row) throw new Error("catalog row not found");
    await fireEvent.click(row);
    await fireEvent.keyDown(tree, { key: "Enter", shiftKey: true });

    await waitFor(() => expect(pluginPreviewNote).toHaveBeenCalledWith("i1", 60, 100));
  });
});
