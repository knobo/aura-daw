/**
 * `AutomationMatrix`, mounted for real (design §6.1, plan §6.1) — grouped
 * rows, a row click revealing the lane through the real `modulation` store
 * (not a spy on `revealParamLane`), and the empty state.
 *
 * See Global Constraints in
 * docs/superpowers/plans/2026-08-18-dom-test-environment.md — this
 * component touches neither pointer capture nor getBoundingClientRect, so
 * none of that plan's jsdom workarounds apply here.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/svelte";
import { tick } from "svelte";
import type { Binding, PluginInstanceInfo, PluginParamInfo, TrackState } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const pluginGetParams = vi.fn(async (instanceId: string): Promise<PluginParamInfo[]> => {
  if (instanceId === "i-alpha" || instanceId === "i-zeta") {
    return [
      { id: 12, name: "Filter / Cutoff", min: 20, max: 20000, default: 1000, value: 1500, steps: 0 },
    ];
  }
  return [];
});

vi.mock("../../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    pluginGetParams: (...args: [string]) => pluginGetParams(...args),
  },
}));

const { default: AutomationMatrix } = await import("./AutomationMatrix.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { project } = await import("../../state/project.svelte");
const { lanes } = await import("../../state/lanes.svelte");
const { modulation } = await import("../../state/modulation.svelte");

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

function inst(id: string, uid: string, name: string): PluginInstanceInfo {
  return { id, uid, name, format: "clap", status: "active" };
}

function pluginBinding(id: string, instanceId: string, paramId: number): Binding {
  return {
    id,
    source: { kind: "curve", curveId: `c-${id}` },
    target: { kind: "pluginParam", instanceId, paramId },
    mode: "absolute",
    depth: 1,
  };
}

function trackGainBinding(id: string, trackId: string): Binding {
  return {
    id,
    source: { kind: "curve", curveId: `c-${id}` },
    target: { kind: "trackParam", trackId, param: "gain" },
    mode: "absolute",
    depth: 1,
  };
}

beforeEach(() => {
  pluginGetParams.mockClear();
  plugins.instances = [];
  plugins.paramCache = {};
  project.tracks = [];
  project.projectDir = null;
  lanes.clearSelection();
  lanes.collapsedTracks = new Set();
  modulation.bindings = [];
  modulation.visible = new Map();
});

afterEach(() => {
  cleanup();
  plugins.instances = [];
  plugins.paramCache = {};
  project.tracks = [];
  modulation.bindings = [];
  modulation.visible = new Map();
});

describe("AutomationMatrix", () => {
  it("shows the empty state when nothing is automated", () => {
    render(AutomationMatrix);
    expect(
      screen.getByText("Nothing is automated yet. Automate a parameter and it shows up here."),
    ).toBeTruthy();
  });

  it("groups rows by parameter — two plugins' Cutoff land in one section", async () => {
    plugins.instances = [inst("i-alpha", "u-a", "Alpha"), inst("i-zeta", "u-z", "Zeta")];
    project.tracks = [
      track("t1", "Bass", { instrumentId: "plugin:i-alpha" }),
      track("t2", "Lead", { instrumentId: "plugin:i-zeta" }),
    ];
    modulation.bindings = [pluginBinding("b1", "i-alpha", 12), pluginBinding("b2", "i-zeta", 12)];

    const { container } = render(AutomationMatrix);

    // Param names fill in once `ensureParams`'s mocked backend call
    // resolves (an $effect after mount) — the initial paint shows "#12",
    // grouped under its own numeric-id section, before it flips to
    // "Cutoff". "Cutoff" itself renders more than once (the section
    // label AND each row's ParamChip), so assert on the section id
    // rather than an ambiguous getByRole/getByText name match.
    const section = await waitFor(() => {
      const el = container.querySelector('[id="browser-row-group-Cutoff"]');
      if (!el) throw new Error("Cutoff section not rendered yet");
      return el;
    });
    expect(section.textContent).toContain("2");
    expect(within(section.parentElement as HTMLElement).getByText("Bass")).toBeTruthy();
    expect(within(section.parentElement as HTMLElement).getByText("Lead")).toBeTruthy();
  });

  it("clicking a row reveals that binding's lane through the real modulation store", async () => {
    plugins.instances = [];
    project.tracks = [track("t1", "Bass", { gainDb: -6 })];
    modulation.bindings = [trackGainBinding("b1", "t1")];

    render(AutomationMatrix);
    expect(modulation.isBindingVisible("t1", "b1")).toBe(false);

    await fireEvent.click(screen.getByText("Bass"));

    expect(modulation.isBindingVisible("t1", "b1")).toBe(true);
  });

  it("re-requests ensureParams only when the automated instance SET changes — not on a row click or a paramCache write", async () => {
    plugins.instances = [inst("i-alpha", "u-a", "Alpha")];
    project.tracks = [track("t1", "Bass", { instrumentId: "plugin:i-alpha" })];
    modulation.bindings = [pluginBinding("b1", "i-alpha", 12)];

    const ensureParamsSpy = vi.spyOn(plugins, "ensureParams");
    try {
      render(AutomationMatrix);
      await waitFor(() => expect(ensureParamsSpy).toHaveBeenCalledTimes(1));
      expect(ensureParamsSpy).toHaveBeenCalledWith("i-alpha");

      // A row click flips `modulation.visible` (via `revealParamLane` →
      // `modulation.show`), which changes `rows` (laneVisible) but not
      // the SET of automated instance ids — must not re-run ensureParams.
      await fireEvent.click(screen.getByText("Bass"));
      await tick();
      expect(ensureParamsSpy).toHaveBeenCalledTimes(1);

      // A paramCache write (what a knob drag on an open param panel does)
      // changes `rows` (valueText) but not instance-id membership either.
      plugins.paramCache = {
        ...plugins.paramCache,
        "i-alpha": [
          { id: 12, name: "Filter / Cutoff", min: 20, max: 20000, default: 1000, value: 999, steps: 0 },
        ],
      };
      await tick();
      expect(ensureParamsSpy).toHaveBeenCalledTimes(1);

      // Adding a new automated instance DOES change membership — this
      // must re-run ensureParams (for both ids — the effect loop always
      // iterates the current set, not just what's new).
      plugins.instances = [...plugins.instances, inst("i-zeta", "u-z", "Zeta")];
      project.tracks = [...project.tracks, track("t2", "Lead", { instrumentId: "plugin:i-zeta" })];
      modulation.bindings = [...modulation.bindings, pluginBinding("b2", "i-zeta", 12)];

      await waitFor(() => expect(ensureParamsSpy).toHaveBeenCalledWith("i-zeta"));
      expect(ensureParamsSpy.mock.calls.length).toBeGreaterThan(1);
    } finally {
      ensureParamsSpy.mockRestore();
    }
  });
});
