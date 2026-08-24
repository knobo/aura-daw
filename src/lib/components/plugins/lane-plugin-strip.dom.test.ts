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
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import { tick } from "svelte";
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
const { paramFollow } = await import("../../state/param-follow.svelte");

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
  paramFollow.reset();
});

afterEach(() => {
  cleanup();
  plugins.instances = [];
  plugins.catalog = emptyCatalog();
  plugins.paramCache = {};
  project.tracks = [];
  modulation.bindings = [];
  paramFollow.reset();
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

  it("a pinned chip paints the driven value, not the document's, while automation holds it", async () => {
    // Same read-back the param panel follows (Track D ruling 2). If only the
    // panel followed it, this chip would read 0.50 while the panel's chip for
    // the very same param read 0.80 — the confusion this closed, on a second
    // surface.
    paramFollow.apply([{ instanceId: "i1", index: 3, value: 0.8 }]);
    render(LanePluginStrip, { props: { track: track() } });
    expect(screen.getByRole("button", { name: "Gain, 0.80" })).toBeTruthy();

    paramFollow.apply([]); // transport stops
    expect(await screen.findByRole("button", { name: "Gain, 0.50" })).toBeTruthy();
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

  it("titles a chip by whether the click creates or jumps to the lane", () => {
    // Gain (pinned, no binding) is "plain" — a click MINTS the lane.
    render(LanePluginStrip, { props: { track: track() } });
    expect(screen.getByRole("button", { name: /gain/i }).getAttribute("title")).toBe(
      "Create an automation lane for Gain",
    );
  });

  it("titles an already-automated chip as a jump, not a create", () => {
    modulation.bindings = [
      {
        id: "b1",
        source: { kind: "curve", curveId: "c1" },
        target: { kind: "pluginParam", instanceId: "i1", paramId: 3 },
        mode: "absolute",
        depth: 1,
      },
    ];
    render(LanePluginStrip, { props: { track: track() } });
    expect(screen.getByRole("button", { name: /gain/i }).getAttribute("title")).toBe(
      "Jump to Gain's lane",
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

  it("Alt+click on the instrument name does nothing — no bypass, no params open", async () => {
    render(LanePluginStrip, { props: { track: track() } });

    await fireEvent.click(screen.getByRole("button", { name: /^synth$/i }), { altKey: true });

    expect(insertSetBypass).not.toHaveBeenCalled();
    expect(openPluginParams).not.toHaveBeenCalled();
  });

  it("shows +N for the overflow and hands its click to the parent", async () => {
    const onoverflow = vi.fn();
    render(LanePluginStrip, { props: { track: track(), onoverflow } });

    const overflowBtn = screen.getByRole("button", { name: "+1" });
    await fireEvent.click(overflowBtn);

    expect(onoverflow).toHaveBeenCalledTimes(1);
  });

  it("re-requests ensureParams only when the strip's device SET changes — not on a chip click or a paramCache write", async () => {
    // Same membership-only-derived + `untrack` shape as
    // `AutomationMatrix.svelte` (see that component's dom test, the model
    // for this one) — the ledger records an earlier version of this exact
    // pattern as the branch's most expensive mistake, so this is the
    // regression test the matrix already has and the strip didn't.
    const ensureParamsSpy = vi.spyOn(plugins, "ensureParams");
    try {
      const { rerender } = render(LanePluginStrip, { props: { track: track() } });
      await waitFor(() => expect(ensureParamsSpy).toHaveBeenCalledTimes(3));
      expect(ensureParamsSpy).toHaveBeenCalledWith("i1");
      expect(ensureParamsSpy).toHaveBeenCalledWith("i2");
      expect(ensureParamsSpy).toHaveBeenCalledWith("i3");

      // A chip click flips a lane's visibility (via the mocked
      // `revealParamLane`) — it must not touch device membership.
      await fireEvent.click(screen.getByRole("button", { name: /gain/i }));
      await tick();
      expect(ensureParamsSpy).toHaveBeenCalledTimes(3);

      // A paramCache write (what a knob drag on an open param panel does)
      // changes chip VALUES, not which instances are on this chain.
      plugins.paramCache = {
        ...plugins.paramCache,
        i1: [{ id: 3, name: "Gain", min: 0, max: 1, default: 0.5, value: 0.9 }],
      };
      await tick();
      expect(ensureParamsSpy).toHaveBeenCalledTimes(3);

      // Adding a fourth device to the chain DOES change membership — this
      // must re-run ensureParams (the effect loop re-iterates the whole
      // current set, not just the newcomer, same as the matrix's test).
      plugins.instances = [...plugins.instances, inst("i4", "u-new", "New", "active")];
      const grown: TrackState = {
        ...track(),
        inserts: [...(track().inserts ?? []), { id: "s3", instanceId: "i4", bypassed: false }],
      };
      await rerender({ track: grown });

      await waitFor(() => expect(ensureParamsSpy).toHaveBeenCalledWith("i4"));
      expect(ensureParamsSpy.mock.calls.length).toBeGreaterThan(3);
    } finally {
      ensureParamsSpy.mockRestore();
    }
  });

  it("renders nothing for a track with no plugin devices", () => {
    const bare: TrackState = { ...track(), instrumentId: null, inserts: [] };
    const { container } = render(LanePluginStrip, { props: { track: bare } });

    expect(container.querySelector('[role="group"]')).toBeNull();
    expect(container.querySelectorAll(".dot").length).toBe(0);
  });
});
