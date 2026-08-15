/**
 * Opening params from the piano-roll / track chip must actually show the
 * param panel. Dock prefers the Zyn patch browser whenever
 * `zyn.openInstanceId` is set, so a leftover patch session (the usual
 * workflow after switching a sound) swallowed the chip click.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    pluginGetParams: vi.fn(() =>
      Promise.resolve([{ id: 1, name: "cutoff", min: 0, max: 1, default: 0, value: 0, steps: 0 }]),
    ),
    pluginList: vi.fn(() => Promise.resolve({ plugins: [], scanned: true, instances: [] })),
    zynListPatches: vi.fn(() => Promise.resolve([])),
  },
}));

const { plugins } = await import("./plugins.svelte");
const { zyn } = await import("./zynpatches.svelte");
const { ui } = await import("./ui.svelte");
const { openPluginParams } = await import("./plugin-panel");

beforeEach(() => {
  plugins.openInstanceId = "";
  plugins.params = [];
  zyn.openInstanceId = "";
  ui.dock = "";
});

describe("openPluginParams", () => {
  it("opens the plugins dock on the param panel, even if a patch browser is up", async () => {
    zyn.openInstanceId = "zyn-1";
    ui.dock = "library";

    await openPluginParams("zyn-1");

    expect(ui.dock).toBe("plugins");
    expect(zyn.openInstanceId).toBe("");
    expect(plugins.openInstanceId).toBe("zyn-1");
    expect(plugins.params).toHaveLength(1);
  });
});
