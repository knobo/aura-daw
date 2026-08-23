/**
 * Zyn patch browser: single-click loads, double-click plays C3 so a patch
 * can be heard without reaching for the piano roll.
 *
 * See Global Constraints in
 * docs/superpowers/plans/2026-08-18-dom-test-environment.md.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/svelte";
import type { PluginInstanceInfo, ZynPatch } from "../../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

vi.stubGlobal(
  "ResizeObserver",
  class {
    observe() {}
    unobserve() {}
    disconnect() {}
  },
);

const zynLoadPatch = vi.fn(async (_instanceId: string, _path: string) => {});
const pluginPreviewNote = vi.fn(async (_instanceId: string, _key: number, _velocity: number) => {});
const pluginPreviewNoteOn = vi.fn(async (_instanceId: string, _key: number, _velocity: number) => {});
const pluginPreviewNoteOff = vi.fn(async () => {});

vi.mock("../../tauri", () => ({
  backend: {
    mode: "demo",
    on: () => () => {},
    zynLoadPatch: (...args: [string, string]) => zynLoadPatch(...args),
    pluginPreviewNote: (...args: [string, number, number]) => pluginPreviewNote(...args),
    pluginPreviewNoteOn: (...args: [string, number, number]) => pluginPreviewNoteOn(...args),
    pluginPreviewNoteOff: () => pluginPreviewNoteOff(),
  },
}));

const { default: ZynPatchBrowser } = await import("./ZynPatchBrowser.svelte");
const { zyn } = await import("../../state/zynpatches.svelte");
const { plugins } = await import("../../state/plugins.svelte");
const { audition } = await import("../../state/audition.svelte");

const patch: ZynPatch = { bank: "Pads", name: "Warm Pad", program: 1, path: "/p.xiz" };

function inst(): PluginInstanceInfo {
  return {
    id: "zyn-1",
    uid: "lv2:http://zynaddsubfx.sourceforge.net",
    name: "ZynAddSubFX",
    format: "lv2",
    status: "active",
  };
}

beforeEach(() => {
  zynLoadPatch.mockClear();
  pluginPreviewNote.mockClear();
  pluginPreviewNoteOn.mockClear();
  pluginPreviewNoteOff.mockClear();
  zyn.patches = [patch];
  zyn.loading = false;
  zyn.error = null;
  zyn.busyPath = null;
  zyn.loaded = {};
  zyn.openInstanceId = "zyn-1";
  plugins.instances = [inst()];
  audition.enabled = false;
});

afterEach(() => {
  cleanup();
  zyn.patches = [];
  zyn.openInstanceId = "";
  plugins.instances = [];
  audition.enabled = false;
});

function loadButton() {
  return screen.getByRole("button", { name: /warm pad/i });
}

describe("ZynPatchBrowser patch audition", () => {
  it("single-click loads the patch without playing a note", async () => {
    render(ZynPatchBrowser);
    await fireEvent.click(loadButton());
    await waitFor(() => expect(zynLoadPatch).toHaveBeenCalledWith("zyn-1", "/p.xiz"));
    expect(pluginPreviewNoteOn).not.toHaveBeenCalled();
  });

  it("holding a loaded patch plays C3 until the pointer is released", async () => {
    zyn.loaded = { "zyn-1": patch };
    render(ZynPatchBrowser);
    const btn = loadButton();
    await fireEvent.pointerDown(btn);
    await waitFor(() => expect(pluginPreviewNoteOn).toHaveBeenCalledWith("zyn-1", 60, 100), {
      timeout: 500,
    });
    await fireEvent.pointerUp(btn);
    await waitFor(() => expect(pluginPreviewNoteOff).toHaveBeenCalled());
  });

  it("double-click does nothing while the audition preference is off — an audition must never load a patch", async () => {
    audition.enabled = false;
    render(ZynPatchBrowser);
    await fireEvent.dblClick(loadButton());
    // Give any (wrongly) in-flight async work a chance to reach the
    // backend before asserting the negative.
    await new Promise((r) => setTimeout(r, 50));
    expect(zynLoadPatch).not.toHaveBeenCalled();
    expect(pluginPreviewNoteOn).not.toHaveBeenCalled();
  });

  it("double-click plays the patch through the bound instance when the preference is on", async () => {
    audition.enabled = true;
    render(ZynPatchBrowser);
    await fireEvent.dblClick(loadButton());
    await waitFor(() => expect(zynLoadPatch).toHaveBeenCalledWith("zyn-1", "/p.xiz"), {
      timeout: 500,
    });
    await waitFor(() => expect(pluginPreviewNoteOn).toHaveBeenCalledWith("zyn-1", 60, 100), {
      timeout: 500,
    });
  });
});
