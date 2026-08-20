/**
 * insertEffect (the "add an effect to a track" action) must surface the new
 * slot on the track AND re-pull the instance registry, so the FX-chain popover
 * renders the plugin by NAME — not the raw `slot.instanceId.slice(0, 8)`
 * fallback the user was seeing — and so its params can be opened.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const insertAdd = vi.fn();
const pluginList = vi.fn();
const patchTrackLocal = vi.fn();
const trackById = vi.fn();

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    insertAdd: (...a: unknown[]) => insertAdd(...a),
    pluginList: (...a: unknown[]) => pluginList(...a),
  },
}));

vi.mock("./project.svelte", () => ({
  project: {
    trackById: (...a: unknown[]) => trackById(...a),
    patchTrackLocal: (...a: unknown[]) => patchTrackLocal(...a),
    tracks: [],
  },
}));

const { plugins } = await import("./plugins.svelte");

const UID = "clap:/usr/lib/clap/ZamTube.clap#com.zamaudio.ZamTube";
const INSTANCE = {
  id: "inst-1",
  uid: UID,
  name: "ZamTube",
  format: "clap" as const,
  status: "active" as const,
  trackId: "t1",
};
const SLOT = { id: "slot-1", instanceId: "inst-1", bypassed: false };
const TRACK = { id: "t1", name: "Audio 1", kind: "audio", inserts: [] };

beforeEach(() => {
  vi.clearAllMocks();
  plugins.instances = [];
  insertAdd.mockResolvedValue({ ...SLOT });
  pluginList.mockResolvedValue({ plugins: [], scanned: true, instances: [{ ...INSTANCE }] });
  trackById.mockReturnValue({ ...TRACK });
});

describe("insertEffect", () => {
  it("surfaces the slot on the track and resolves the new instance by name", async () => {
    await plugins.insertEffect("t1", UID);

    expect(insertAdd).toHaveBeenCalledWith("t1", UID);
    // The slot is pushed onto the track so the FX chain shows a real row,
    // not the raw-instance-id fallback.
    expect(patchTrackLocal).toHaveBeenCalledWith("t1", { inserts: [SLOT] });
    // The registry is re-pulled so byId() resolves the instance (name, params).
    expect(pluginList).toHaveBeenCalled();
    expect(plugins.byId("inst-1")?.name).toBe("ZamTube");
  });

  it("does not append a duplicate slot when the snapshot already carries it", async () => {
    trackById.mockReturnValue({ ...TRACK, inserts: [{ ...SLOT }] });
    await plugins.insertEffect("t1", UID);
    expect(patchTrackLocal).not.toHaveBeenCalled();
    expect(plugins.byId("inst-1")?.name).toBe("ZamTube");
  });
});
