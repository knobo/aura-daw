/**
 * Native GUI affordance on the generic param panel: GUI sits next to the
 * status badge and calls plugin_show_gui for the open instance.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import type { PluginInstanceInfo } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

const pluginShowGui = vi.fn(async (_id: string) => {});

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

const { default: PluginParamPanel } = await import("./PluginParamPanel.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { modulation } = await import("../../state/modulation.svelte");

function inst(): PluginInstanceInfo {
  return {
    id: "i1",
    uid: "clap:/usr/lib/clap/glBars.clap#studio.kx.distrho.glBars",
    name: "glBars",
    format: "clap",
    status: "active",
  };
}

beforeEach(() => {
  pluginShowGui.mockClear();
  plugins.instances = [inst()];
  plugins.openInstanceId = "i1";
  plugins.params = [];
  plugins.paramsLoading = false;
  plugins.paramError = null;
  plugins.guiById = {};
  modulation.bindings = [];
});

afterEach(() => {
  cleanup();
  plugins.openInstanceId = "";
  plugins.instances = [];
  plugins.guiById = {};
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
