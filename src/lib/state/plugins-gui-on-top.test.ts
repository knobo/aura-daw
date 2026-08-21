/**
 * pluginGuiOnTop must reach the host as soon as the preference changes.
 * Restart-only apply was the bug: the pref persisted, so a reboot looked
 * like it worked, but already-open editors never saw the toggle.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const pluginList = vi.fn();
const pluginSetGuiOnTop = vi.fn((_enabled: boolean) => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    pluginList: (...a: unknown[]) => pluginList(...a),
    pluginSetGuiOnTop: (enabled: boolean) => pluginSetGuiOnTop(enabled),
  },
}));

const { prefs } = await import("../prefs/prefs.svelte");
const { plugins } = await import("./plugins.svelte");

beforeEach(async () => {
  pluginList.mockResolvedValue({ plugins: [], scanned: true, instances: [], gui: {} });
  prefs.restoreDefaults();
  await plugins.refresh();
  vi.clearAllMocks();
  pluginList.mockResolvedValue({ plugins: [], scanned: true, instances: [], gui: {} });
});

describe("pluginGuiOnTop live apply", () => {
  it("pushes the current preference when the plugin mirror boots", async () => {
    prefs.set("pluginGuiOnTop", false);
    await plugins.refresh();
    expect(pluginSetGuiOnTop).toHaveBeenCalledWith(false);
  });

  it("pushes immediately when the preference is toggled", async () => {
    prefs.set("pluginGuiOnTop", false);
    expect(pluginSetGuiOnTop).toHaveBeenCalledWith(false);
    pluginSetGuiOnTop.mockClear();
    prefs.set("pluginGuiOnTop", true);
    expect(pluginSetGuiOnTop).toHaveBeenCalledWith(true);
  });
});
