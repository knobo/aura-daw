/**
 * The lane plugin strip (design §3.4, plan §6.3), mounted for real: order,
 * folded dots-only degradation, the chip → `revealParamLane` jump, the
 * plain-click/Alt+click split on a device name, and the `+N` overflow
 * handoff to the parent's `InsertChain` popover.
 *
 * See Global Constraints in
 * docs/superpowers/plans/2026-08-18-dom-test-environment.md — this
 * component drives no pointer-capture / getBoundingClientRect path, so
 * none of that plan's jsdom workarounds are needed here.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import type { PluginInstanceInfo, TrackState } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const insertSetBypass = vi.fn(async (_trackId: string, _slotId: string, _bypassed: boolean) => {});
const revealParamLane = vi.fn(async (_trackId: string, _target: unknown, _initialNormalized?: number) => {});
const openPluginParams = vi.fn(async (_instanceId: string) => {});

vi.mock("../../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    insertSetBypass: (...args: [string, string, boolean]) => insertSetBypass(...args),
    pluginGetParams: () => Promise.resolve([]),
    pluginList: () => Promise.resolve({ plugins: [], instances: [], scanned: true }),
  },
}));

// Seams — the same convention `plugin-param-panel.dom.test.ts` uses for
// `revealParamLane`: this component's job is to call these with the right
// arguments, not to re-prove what they do internally.
vi.mock("../../utils/lane-reveal", () => ({
  revealParamLane: (...args: [string, unknown, number | undefined]) => revealParamLane(...args),
}));
vi.mock("../../state/plugin-panel", () => ({
  openPluginParams: (...args: [string]) => openPluginParams(...args),
}));

const { default: LanePluginStrip } = await import("./LanePluginStrip.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { project } = await import("../../state/project.svelte");
const { modulation } = await import("../../state/modulation.svelte");

function inst(id: string, uid: string, name: string, status: PluginInstanceInfo["status"]): PluginInstanceInfo {
  return { id, uid, name, format: "clap", status };
}

function track(): TrackState {
  return {
    id: "t1",
    name: "Bass",
    kind: "midi",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#38bdf8",
    instrumentId: "plugin:i1",
    inserts: [
      { id: "s1", instanceId: "i2", bypassed: false },
      { id: "s2", instanceId: "i3", bypassed: false },
    ],
  };
}

function emptyCatalog() {
  return { version: 1, scan: { entries: [] }, favorites: [], recents: [], tags: {}, pinnedParams: {} };
}

beforeEach(() => {
  insertSetBypass.mockClear();
  revealParamLane.mockClear();
  openPluginParams.mockClear();
  plugins.instances = [
    inst("i1", "u-synth", "Synth", "active"),
    inst("i2", "u-verb", "IRVerb", "stub"),
    inst("i3", "u-comp", "Comp", "crashed"),
  ];
  plugins.catalog = { ...emptyCatalog(), pinnedParams: { "u-synth": [3] } };
  plugins.paramCache = { i1: [{ id: 3, name: "Gain", min: 0, max: 1, default: 0.5, value: 0.5 }] };
  project.tracks = [track()];
  modulation.bindings = [];
});

afterEach(() => {
  cleanup();
  plugins.instances = [];
  plugins.catalog = emptyCatalog();
  plugins.paramCache = {};
  project.tracks = [];
  modulation.bindings = [];
});

describe("LanePluginStrip", () => {
  it("renders the instrument then its inserts in order, each with a status dot", () => {
    const { container } = render(LanePluginStrip, { props: { track: track() } });

    expect(screen.getByRole("button", { name: /^synth$/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^irverb$/i })).toBeTruthy();
    // maxEntries: 2 unfolded — Comp is the third device, pushed to overflow.
    expect(screen.queryByRole("button", { name: /^comp$/i })).toBeNull();

    const dots = container.querySelectorAll(".dot");
    expect(dots.length).toBe(2);
    expect(dots[0].className).toContain("active"); // Synth
    expect(dots[1].className).toContain("stub"); // IRVerb
  });

  it("folded renders dots only — no name buttons, no chips", () => {
    const { container } = render(LanePluginStrip, { props: { track: track(), folded: true } });

    // maxEntries: 4 folded — all three devices fit.
    const dots = screen.getAllByRole("img");
    expect(dots.length).toBe(3);
    expect(container.querySelectorAll("button").length).toBe(0);
  });

  it("a chip click reaches revealParamLane with the right target", async () => {
    render(LanePluginStrip, { props: { track: track() } });

    await fireEvent.click(screen.getByRole("button", { name: /gain/i }));

    expect(revealParamLane).toHaveBeenCalledTimes(1);
    expect(revealParamLane).toHaveBeenCalledWith(
      "t1",
      { kind: "pluginParam", instanceId: "i1", paramId: 3 },
      0.5,
    );
  });

  it("a plain click on an insert name opens the params panel", async () => {
    render(LanePluginStrip, { props: { track: track() } });

    await fireEvent.click(screen.getByRole("button", { name: /^irverb$/i }));

    expect(openPluginParams).toHaveBeenCalledWith("i2");
    expect(insertSetBypass).not.toHaveBeenCalled();
  });

  it("Alt+click on an insert name toggles bypass instead of opening params", async () => {
    render(LanePluginStrip, { props: { track: track() } });

    await fireEvent.click(screen.getByRole("button", { name: /^irverb$/i }), { altKey: true });

    expect(insertSetBypass).toHaveBeenCalledWith("t1", "s1", true);
    expect(openPluginParams).not.toHaveBeenCalled();
  });

  it("Alt+click on the instrument name does nothing bypass-related — it opens params", async () => {
    render(LanePluginStrip, { props: { track: track() } });

    await fireEvent.click(screen.getByRole("button", { name: /^synth$/i }), { altKey: true });

    expect(insertSetBypass).not.toHaveBeenCalled();
    expect(openPluginParams).toHaveBeenCalledWith("i1");
  });

  it("shows +N for the overflow and hands its click to the parent", async () => {
    const onoverflow = vi.fn();
    render(LanePluginStrip, { props: { track: track(), onoverflow } });

    const overflowBtn = screen.getByRole("button", { name: "+1" });
    await fireEvent.click(overflowBtn);

    expect(onoverflow).toHaveBeenCalledTimes(1);
  });

  it("renders nothing for a track with no plugin devices", () => {
    const bare: TrackState = { ...track(), instrumentId: null, inserts: [] };
    const { container } = render(LanePluginStrip, { props: { track: bare } });

    expect(container.querySelector('[role="group"]')).toBeNull();
    expect(container.querySelectorAll(".dot").length).toBe(0);
  });
});
