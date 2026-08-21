/**
 * Zyn patch store: load coalescing and audition (load-then-C3).
 * Double-click in the browser is "hear this patch"; the note is not a
 * project edit, but loading the .xiz into the live instance still is.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { PluginInstanceInfo, ZynPatch } from "../types/ipc";

const invokes = {
  zynLoadPatch: vi.fn(async (_instanceId: string, _path: string) => {}),
  pluginPreviewNote: vi.fn(async (_instanceId: string, _key: number, _velocity: number) => {}),
  pluginPreviewNoteOn: vi.fn(async (_instanceId: string, _key: number, _velocity: number) => {}),
  pluginPreviewNoteOff: vi.fn(async () => {}),
};

vi.mock("../tauri", () => ({
  backend: { mode: "tauri", on: () => () => {}, ...invokes },
}));

const { zyn } = await import("./zynpatches.svelte");
const { plugins } = await import("./plugins.svelte");

const patch: ZynPatch = { bank: "Pads", name: "Warm Pad", program: 1, path: "/p.xiz" };
const other: ZynPatch = { bank: "Bass", name: "Sub", program: 2, path: "/b.xiz" };

function zynInst(): PluginInstanceInfo {
  return {
    id: "zyn-1",
    uid: "lv2:http://zynaddsubfx.sourceforge.net",
    name: "ZynAddSubFX",
    format: "lv2",
    status: "active",
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  zyn.loaded = {};
  zyn.busyPath = null;
  zyn.error = null;
  zyn.openInstanceId = "zyn-1";
  plugins.instances = [zynInst()];
});

describe("zyn.audition", () => {
  it("loads the patch then plays C3 on the instance", async () => {
    await zyn.audition("zyn-1", patch);
    expect(invokes.zynLoadPatch).toHaveBeenCalledWith("zyn-1", "/p.xiz");
    expect(invokes.pluginPreviewNoteOn).toHaveBeenCalledWith("zyn-1", 60, 100);
  });

  it("skips load when that patch is already live, still plays C3", async () => {
    zyn.loaded = { "zyn-1": patch };
    await zyn.audition("zyn-1", patch);
    expect(invokes.zynLoadPatch).not.toHaveBeenCalled();
    expect(invokes.pluginPreviewNoteOn).toHaveBeenCalledWith("zyn-1", 60, 100);
  });

  it("does not play a note when load fails", async () => {
    invokes.zynLoadPatch.mockRejectedValueOnce(new Error("no banks"));
    await zyn.audition("zyn-1", patch);
    expect(invokes.pluginPreviewNoteOn).not.toHaveBeenCalled();
  });

  it("awaits an in-flight load of the same patch instead of starting a second", async () => {
    let finish: () => void = () => {};
    invokes.zynLoadPatch.mockImplementationOnce(
      () => new Promise<void>((resolve) => { finish = resolve; }),
    );
    const loading = zyn.load("zyn-1", patch);
    const hearing = zyn.audition("zyn-1", patch);
    expect(invokes.zynLoadPatch).toHaveBeenCalledTimes(1);
    finish();
    await loading;
    await hearing;
    expect(invokes.zynLoadPatch).toHaveBeenCalledTimes(1);
    expect(invokes.pluginPreviewNoteOn).toHaveBeenCalledTimes(1);
  });

  it("loads a different patch even if another load is finishing", async () => {
    zyn.loaded = { "zyn-1": patch };
    await zyn.audition("zyn-1", other);
    expect(invokes.zynLoadPatch).toHaveBeenCalledWith("zyn-1", "/b.xiz");
    expect(invokes.pluginPreviewNoteOn).toHaveBeenCalledWith("zyn-1", 60, 100);
  });

  it("previewUp releases the held note", async () => {
    await zyn.previewDown("zyn-1");
    await zyn.previewUp();
    expect(invokes.pluginPreviewNoteOff).toHaveBeenCalled();
  });
});
