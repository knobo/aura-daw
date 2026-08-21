/**
 * `plugins.paramCache` / `ensureParams` — the lazy per-instance param cache
 * that lets a strip/chip render a plugin's params without that instance
 * being the OPEN one. Follows `plugins-insert.test.ts`'s mocking pattern.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const pluginGetParams = vi.fn();
const pluginRemove = vi.fn();

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    pluginGetParams: (...a: unknown[]) => pluginGetParams(...a),
    pluginRemove: (...a: unknown[]) => pluginRemove(...a),
    pluginList: vi.fn(() => Promise.resolve({ plugins: [], scanned: true, instances: [] })),
  },
}));

vi.mock("./project.svelte", () => ({
  project: {
    trackById: () => undefined,
    patchTrackLocal: () => {},
    tracks: [],
    beginGesture: () => Promise.resolve(undefined),
    endGesture: () => {},
  },
}));

const { plugins } = await import("./plugins.svelte");

const PARAMS = [{ id: 1, name: "Cutoff", min: 0, max: 1, default: 0.5, value: 0.5, steps: 0 }];

beforeEach(() => {
  vi.clearAllMocks();
  pluginRemove.mockResolvedValue(undefined);
  plugins.instances = [];
  plugins.paramCache = {};
  plugins.openInstanceId = "";
  plugins.params = [];
});

describe("ensureParams", () => {
  it("calls pluginGetParams once per instance and caches the result", async () => {
    pluginGetParams.mockResolvedValue(PARAMS);
    await plugins.ensureParams("inst-1");
    expect(pluginGetParams).toHaveBeenCalledTimes(1);
    expect(pluginGetParams).toHaveBeenCalledWith("inst-1");
    expect(plugins.paramCache["inst-1"]).toEqual(PARAMS);
    expect(plugins.paramInfo("inst-1", 1)).toEqual(PARAMS[0]);

    // Already cached: a second call is a no-op, no extra invoke.
    await plugins.ensureParams("inst-1");
    expect(pluginGetParams).toHaveBeenCalledTimes(1);
  });

  it("concurrent callers for the same instance share one in-flight request", async () => {
    let resolve!: (v: typeof PARAMS) => void;
    pluginGetParams.mockReturnValue(
      new Promise((res) => {
        resolve = res;
      }),
    );
    const p1 = plugins.ensureParams("inst-1");
    const p2 = plugins.ensureParams("inst-1");
    expect(pluginGetParams).toHaveBeenCalledTimes(1);
    resolve(PARAMS);
    await Promise.all([p1, p2]);
    expect(plugins.paramCache["inst-1"]).toEqual(PARAMS);
  });

  it("a rejected call leaves the cache empty and does not throw", async () => {
    pluginGetParams.mockRejectedValue(new Error("enumerate failed"));
    await expect(plugins.ensureParams("inst-1")).resolves.toBeUndefined();
    expect(plugins.paramCache["inst-1"]).toBeUndefined();
    expect(plugins.paramInfo("inst-1", 1)).toBeUndefined();

    // A later call retries — the failure was not cached "as empty".
    pluginGetParams.mockResolvedValue(PARAMS);
    await plugins.ensureParams("inst-1");
    expect(plugins.paramCache["inst-1"]).toEqual(PARAMS);
  });

  it("mirrors the open instance's params into the cache as they load", async () => {
    pluginGetParams.mockResolvedValue(PARAMS);
    await plugins.openParams("inst-1");
    expect(plugins.paramCache["inst-1"]).toEqual(plugins.params);
  });

  it("removing an instance evicts its cache entry", async () => {
    pluginGetParams.mockResolvedValue(PARAMS);
    plugins.instances = [
      { id: "inst-1", uid: "u1", name: "Reverb", format: "clap", status: "active" },
    ];
    await plugins.ensureParams("inst-1");
    expect(plugins.paramCache["inst-1"]).toEqual(PARAMS);

    await plugins.remove("inst-1");
    expect(plugins.paramCache["inst-1"]).toBeUndefined();
  });
});
