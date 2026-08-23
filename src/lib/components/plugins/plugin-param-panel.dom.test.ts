/**
 * Native GUI affordance on the generic param panel: GUI sits next to the
 * status badge and calls plugin_show_gui for the open instance. Also covers
 * task 3 of the plugin-manager PR 6 plan (§6.2): every param row's chip,
 * the Pinned section, and the pin/unpin toggle.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/svelte";
import type { Binding, PluginCatalog, PluginInstanceInfo, PluginParamInfo } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const pluginShowGui = vi.fn(async (_id: string) => {});
const revealParamLane = vi.fn(async (_trackId: string, _target: unknown, _initialNormalized?: number) => {});

vi.mock("../../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    pluginShowGui: (...args: [string]) => pluginShowGui(...args),
    pluginGetParams: () => Promise.resolve([]),
    pluginSetParam: () => Promise.resolve([]),
    pluginList: () => Promise.resolve({ plugins: [], instances: [], scanned: true }),
  },
}));

// `revealParamLane` (Task 1) is exercised by its own unit test — here it's
// a seam: the panel's job is to call it with the right target, not to
// re-prove lane-unfold/scroll behaviour that has no DOM under jsdom anyway.
vi.mock("../../utils/lane-reveal", () => ({
  revealParamLane: (...args: [string, unknown, number | undefined]) => revealParamLane(...args),
}));

const { default: PluginParamPanel } = await import("./PluginParamPanel.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { modulation } = await import("../../state/modulation.svelte");
const { paramFollow } = await import("../../state/param-follow.svelte");
const { toasts } = await import("../../state/toasts.svelte");

const UID = "clap:/usr/lib/clap/glBars.clap#studio.kx.distrho.glBars";

function inst(): PluginInstanceInfo {
  return {
    id: "i1",
    uid: UID,
    name: "glBars",
    format: "clap",
    status: "active",
    trackId: "t1",
  };
}

function emptyCatalog(): PluginCatalog {
  return { version: 1, scan: { entries: [] }, favorites: [], recents: [], tags: {}, pinnedParams: {} };
}

/** "Group / Name" so `paramGroupName`/`shortParamName` split it — matches
 * how real plugin descriptors are named. */
function param(id: number, name: string, value: number): PluginParamInfo {
  return { id, name, min: 0, max: 1, default: 0.5, value };
}

function bindingFor(paramId: number): Binding {
  return {
    id: `b-${paramId}`,
    source: { kind: "automationTrack", trackId: "t1" },
    target: { kind: "pluginParam", instanceId: "i1", paramId },
    mode: "absolute",
    depth: 1,
  };
}

beforeEach(() => {
  pluginShowGui.mockClear();
  revealParamLane.mockClear();
  plugins.instances = [inst()];
  plugins.openInstanceId = "i1";
  plugins.params = [];
  plugins.paramsLoading = false;
  plugins.paramError = null;
  plugins.guiById = {};
  plugins.catalog = emptyCatalog();
  modulation.bindings = [];
  paramFollow.reset();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  plugins.openInstanceId = "";
  plugins.instances = [];
  plugins.guiById = {};
  plugins.catalog = emptyCatalog();
  paramFollow.reset();
});

/**
 * Track D ruling 2 drives plugin-param automation HOST-ONLY, so while a lane
 * holds a param the document value this panel paints is NOT what the plugin
 * has. `paramFollow` carries the engine's own read-back; these cover that the
 * panel paints it, says so, and hands the param back when it stops.
 */
describe("PluginParamPanel follows automation", () => {
  it("paints the driven value, not the document's, while automation holds a param", () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    paramFollow.apply([{ instanceId: "i1", index: 1, value: 0.8 }]);
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: "Level, 0.80" })).toBeTruthy();
    expect(screen.getByRole("slider", { name: "Filter / Level (0.80)" })).toBeTruthy();
  });

  it("flags the row so the fader refusing to stay put is explained, not mysterious", () => {
    plugins.params = [param(1, "Filter / Level", 0.5), param(2, "Filter / Res", 0.25)];
    paramFollow.apply([{ instanceId: "i1", index: 1, value: 0.8 }]);
    render(PluginParamPanel);
    const flags = screen.getAllByText("AUTO");
    expect(flags.length).toBe(1);
    expect(flags[0].getAttribute("title")).toContain("Filter / Level");
  });

  it("leaves an unautomated param on the document value", () => {
    plugins.params = [param(1, "Filter / Level", 0.5), param(2, "Filter / Res", 0.25)];
    paramFollow.apply([{ instanceId: "i1", index: 1, value: 0.8 }]);
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: "Res, 0.25" })).toBeTruthy();
  });

  it("ignores a read-back for a DIFFERENT instance", () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    paramFollow.apply([{ instanceId: "i-other", index: 1, value: 0.8 }]);
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: "Level, 0.50" })).toBeTruthy();
    expect(screen.queryByText("AUTO")).toBeNull();
  });

  it("hands the param back to the document when the transport stops", async () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    paramFollow.apply([{ instanceId: "i1", index: 1, value: 0.8 }]);
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: "Level, 0.80" })).toBeTruthy();

    paramFollow.apply([]); // a stopped transport ships an empty read-back
    expect(await screen.findByRole("button", { name: "Level, 0.50" })).toBeTruthy();
    expect(screen.queryByText("AUTO")).toBeNull();
  });

  it("puts the thumb back where automation has it after a drag on a driven row", async () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    paramFollow.apply([{ instanceId: "i1", index: 1, value: 0.8 }]);
    render(PluginParamPanel);
    const fader = screen.getByRole("slider", { name: /^Filter \/ Level/ }) as HTMLInputElement;

    await fireEvent.input(fader, { target: { value: "0.3" } });
    // A held or flat lane repeats the same value every frame, so nothing
    // reassigns and Svelte will not push the binding back (`set_value`
    // early-returns on an unchanged value). Without an explicit resync the
    // thumb sits at 0.3 while chip, fill and aria-label all say 0.80 — and
    // the plugin snaps back to 0.80 within REASSERT_TICKS with nothing on
    // screen saying so.
    paramFollow.apply([{ instanceId: "i1", index: 1, value: 0.8 }]);
    expect(fader.value).toBe("0.8");
    expect(screen.getByRole("button", { name: "Level, 0.80" })).toBeTruthy();
  });

  it("shows the nearest step on a driven enum row instead of rendering blank", () => {
    // 4 steps over 0..1 => options at 0, 1/3, 2/3, 1. A ramp sits between
    // them, and a <select> whose value matches no option renders with
    // selectedIndex -1, i.e. empty. 0.4 is unambiguously nearest 1/3 —
    // avoid a midpoint, where float error alone decides the winner.
    plugins.params = [{ id: 1, name: "Filter / Mode", min: 0, max: 1, default: 0, value: 0, steps: 4 }];
    paramFollow.apply([{ instanceId: "i1", index: 1, value: 0.4 }]);
    render(PluginParamPanel);
    const select = screen.getByRole("combobox") as HTMLSelectElement;
    expect(select.selectedIndex).not.toBe(-1);
    expect(Number(select.value)).toBeCloseTo(1 / 3, 5);
  });

  it("still writes to the real param id from a driven row", async () => {
    plugins.params = [param(4, "Filter / Level", 0.5)];
    paramFollow.apply([{ instanceId: "i1", index: 4, value: 0.8 }]);
    render(PluginParamPanel);
    const fader = screen.getByRole("slider", { name: /^Filter \/ Level/ });
    await fireEvent.input(fader, { target: { value: "0.3" } });
    expect(plugins.params[0].value).toBe(0.3);
  });
});

describe("PluginParamPanel native GUI", () => {
  it("shows a GUI button when the open instance has a native GUI", () => {
    plugins.guiById = { i1: true };
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: /^gui$/i })).toBeTruthy();
  });

  it("hides the GUI button when the plugin has no native GUI", () => {
    plugins.guiById = { i1: false };
    render(PluginParamPanel);
    expect(screen.queryByRole("button", { name: /^gui$/i })).toBeNull();
  });

  it("calls pluginShowGui for the open instance", async () => {
    plugins.guiById = { i1: true };
    render(PluginParamPanel);
    await fireEvent.click(screen.getByRole("button", { name: /^gui$/i }));
    expect(pluginShowGui).toHaveBeenCalledWith("i1");
  });
});

describe("PluginParamPanel param chips", () => {
  it("renders a ParamChip whose accessible name carries the short name and formatted value", () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: "Level, 0.50" })).toBeTruthy();
  });

  it("reports automated in the accessible name only for a bound param", () => {
    plugins.params = [param(1, "Filter / Level", 0.5), param(2, "Filter / Resonance", 0.25)];
    modulation.bindings = [bindingFor(2)];
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: "Level, 0.50" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Resonance, 0.25, automated" })).toBeTruthy();
  });

  it("clicking a chip reaches revealParamLane with the instance's track and the param target", async () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    render(PluginParamPanel);
    await fireEvent.click(screen.getByRole("button", { name: "Level, 0.50" }));
    expect(revealParamLane).toHaveBeenCalledWith(
      "t1",
      { kind: "pluginParam", instanceId: "i1", paramId: 1 },
      0.5,
    );
  });

  it("carries the chip's title in the fuller 'Group / Name' form, same referent as the A button", () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    render(PluginParamPanel);
    expect(screen.getByRole("button", { name: "Level, 0.50" }).getAttribute("title")).toBe(
      "Create automation lane for Filter / Level",
    );
    expect(screen.getByTitle("Automate Filter / Level")).toBeTruthy();
  });

  it("keeps the full 'Group / Name (id N)' discoverable on a toggle/enum row, which renders no fader", () => {
    // The removed `.pname` used to carry this text; a toggle/enum param has
    // no fader to fall back to, so it must survive somewhere else on the row.
    plugins.params = [{ id: 7, name: "Mode / Bypass", min: 0, max: 1, default: 0, value: 1, steps: 2 }];
    render(PluginParamPanel);
    const toggleBtn = screen.getByTitle("Toggle Bypass");
    expect(toggleBtn.closest(".prow")?.getAttribute("title")).toBe("Mode / Bypass (id 7)");
  });
});

describe("PluginParamPanel pinned section", () => {
  it("renders pinned params in a Pinned section, in pin order, and still in their own group", () => {
    plugins.params = [
      param(1, "Filter / Level", 0.5),
      param(2, "Filter / Resonance", 0.25),
      param(3, "Amp / Gain", 0.75),
    ];
    plugins.catalog = { ...emptyCatalog(), pinnedParams: { [UID]: [2, 1] } };
    render(PluginParamPanel);

    const pinnedSection = screen.getByText("Pinned").closest("section") as HTMLElement;
    const pinnedChips = within(pinnedSection).getAllByRole("button", { name: /,/ });
    expect(pinnedChips.map((c) => c.getAttribute("aria-label"))).toEqual([
      "Resonance, 0.25",
      "Level, 0.50",
    ]);

    // Pinning is a shortcut, not a move — both still show under Filter too
    // (the group holding "Filter / Level" and "Filter / Resonance" is the
    // first group in the plugin's param order, opened by default).
    expect(screen.getAllByRole("button", { name: "Level, 0.50" })).toHaveLength(2);
    expect(screen.getAllByRole("button", { name: "Resonance, 0.25" })).toHaveLength(2);
  });

  it("skips a stale pinned id that is no longer in plugins.params", () => {
    plugins.params = [param(1, "Filter / Level", 0.5)];
    plugins.catalog = { ...emptyCatalog(), pinnedParams: { [UID]: [1, 999] } };
    render(PluginParamPanel);

    const pinnedSection = screen.getByText("Pinned").closest("section") as HTMLElement;
    expect(within(pinnedSection).getAllByRole("button", { name: /,/ })).toHaveLength(1);
  });
});

describe("PluginParamPanel pin toggle", () => {
  it("pins on click, then unpins on a second click", async () => {
    const setPinnedParams = vi.spyOn(plugins, "setPinnedParams");
    plugins.params = [param(1, "Filter / Level", 0.5)];
    render(PluginParamPanel);

    await fireEvent.click(screen.getByRole("button", { name: "Pin Level" }));
    expect(setPinnedParams).toHaveBeenCalledWith(UID, [1]);

    // The real store call above already updated `plugins.catalog`, so the
    // toggle has reactively flipped to its pinned label — and, since the
    // param is now pinned, it also gained a second row in the new Pinned
    // section (§3b: pinning is a shortcut, not a move).
    const unpinBtns = screen.getAllByRole("button", { name: "Unpin Level" });
    expect(unpinBtns).toHaveLength(2);
    await fireEvent.click(unpinBtns[0]);
    expect(setPinnedParams).toHaveBeenLastCalledWith(UID, []);
  });

  it("refuses a ninth pin: toasts and does not call setPinnedParams", async () => {
    const setPinnedParams = vi.spyOn(plugins, "setPinnedParams");
    const toastError = vi.spyOn(toasts, "error");
    plugins.params = [param(9, "Filter / Drive", 0.5)];
    plugins.catalog = { ...emptyCatalog(), pinnedParams: { [UID]: [1, 2, 3, 4, 5, 6, 7, 8] } };
    render(PluginParamPanel);

    await fireEvent.click(screen.getByRole("button", { name: "Pin Drive" }));

    expect(toastError).toHaveBeenCalledWith(
      "PINNED FULL",
      "Unpin a parameter first — the strip holds 8.",
    );
    expect(setPinnedParams).not.toHaveBeenCalled();
  });
});
