/**
 * Plugin catalog mirror (spec 4.1.E): `plugins.svelte.ts`'s catalog state
 * against a mocked backend, following the `plugins-insert.test.ts` mocking
 * pattern. Covers the optimistic-local + coalesced-write contract: every
 * action updates `plugins.catalog` synchronously (favorites/recents/tags/
 * pinned params read back immediately, no round trip needed) while the
 * actual `plugin_catalog_update` invoke is debounced so a burst of actions
 * does not become a write storm.
 *
 * Fake timers throughout: the coalescing debounce is a real `setTimeout`,
 * and leaving it on the real clock would let one test's pending flush fire
 * during a LATER test. `afterEach` clears anything still pending.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PluginCatalog } from "../types/ipc";

const pluginCatalogGet = vi.fn();
const pluginCatalogUpdate = vi.fn();
const pluginScanStatus = vi.fn();
const pluginInstantiate = vi.fn();

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    pluginCatalogGet: (...a: unknown[]) => pluginCatalogGet(...a),
    pluginCatalogUpdate: (...a: unknown[]) => pluginCatalogUpdate(...a),
    pluginScanStatus: (...a: unknown[]) => pluginScanStatus(...a),
    pluginInstantiate: (...a: unknown[]) => pluginInstantiate(...a),
  },
}));

vi.mock("./project.svelte", () => ({
  project: {
    trackById: () => undefined,
    patchTrackLocal: () => {},
    tracks: [],
  },
}));

const { plugins } = await import("./plugins.svelte");

const EMPTY_CATALOG: PluginCatalog = {
  version: 1,
  scan: { entries: [] },
  favorites: [],
  recents: [],
  tags: {},
  pinnedParams: {},
};

const UID_A = "clap:/usr/lib/clap/A.clap#a";
const UID_B = "clap:/usr/lib/clap/B.clap#b";

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  plugins.catalog = { ...EMPTY_CATALOG, scan: { entries: [] }, tags: {}, pinnedParams: {} };
  plugins.scanStatus = null;
  plugins.instances = [];
  pluginCatalogGet.mockResolvedValue({ ...EMPTY_CATALOG });
  pluginCatalogUpdate.mockImplementation((patch) => Promise.resolve({ ...EMPTY_CATALOG, ...patch }));
  pluginScanStatus.mockResolvedValue({ scannedAt: 1_755_648_000, cached: 3, stale: 1, missing: 0 });
});

afterEach(() => {
  // Run (not just clear) anything still pending: the coalescing debounce
  // resets the store's OWN `patchTimer`/`pendingPatch` fields synchronously
  // at the top of its callback (before the backend await) — clearing the
  // scheduled timer without running it would leave those fields non-null/
  // non-empty, so the NEXT test's first queuePatch() sees a "flush already
  // scheduled" and never reschedules a real one.
  vi.runOnlyPendingTimers();
  vi.useRealTimers();
});

describe("loadCatalog", () => {
  it("pulls the catalog and scan status from the backend", async () => {
    pluginCatalogGet.mockResolvedValue({
      ...EMPTY_CATALOG,
      favorites: ["u1"],
    });
    await plugins.loadCatalog();
    expect(plugins.catalog.favorites).toEqual(["u1"]);
    expect(plugins.scanStatus).toEqual({ scannedAt: 1_755_648_000, cached: 3, stale: 1, missing: 0 });
  });

  it("a rejected pluginCatalogGet/pluginScanStatus is swallowed (transient, not fatal)", async () => {
    pluginCatalogGet.mockRejectedValue(new Error("no ipc"));
    pluginScanStatus.mockRejectedValue(new Error("no ipc"));
    await expect(plugins.loadCatalog()).resolves.toBeUndefined();
    expect(plugins.catalog).toEqual(EMPTY_CATALOG); // untouched by the failure
  });
});

describe("favorites", () => {
  it("toggleFavorite flips isFavorite immediately (optimistic, no await needed)", () => {
    expect(plugins.isFavorite(UID_A)).toBe(false);
    plugins.toggleFavorite(UID_A);
    expect(plugins.isFavorite(UID_A)).toBe(true);
    plugins.toggleFavorite(UID_A);
    expect(plugins.isFavorite(UID_A)).toBe(false);
  });

  it("persists the final state via ONE coalesced plugin_catalog_update, not one per toggle", async () => {
    plugins.toggleFavorite(UID_A);
    plugins.toggleFavorite(UID_B);
    plugins.toggleFavorite(UID_A); // A: on, then off again — still just one flush
    expect(plugins.isFavorite(UID_A)).toBe(false);
    expect(plugins.isFavorite(UID_B)).toBe(true);
    expect(pluginCatalogUpdate).not.toHaveBeenCalled(); // debounced, hasn't fired yet

    await vi.advanceTimersByTimeAsync(250);
    expect(pluginCatalogUpdate).toHaveBeenCalledTimes(1);
  });
});

describe("recents (noteUsed)", () => {
  it("moves a re-used uid to the front, de-duped, capped at 40", () => {
    for (let i = 0; i < 3; i++) plugins.noteUsed(`u${i}`);
    plugins.noteUsed("u0"); // re-use the oldest
    const uids = plugins.catalog.recents.map((r) => r.uid);
    expect(uids).toEqual(["u0", "u2", "u1"]);

    for (let i = 0; i < 50; i++) plugins.noteUsed(`v${i}`);
    expect(plugins.catalog.recents.length).toBe(40);
  });

  it("instantiate() calls noteUsed so a real use lands in recents", async () => {
    pluginInstantiate.mockResolvedValue({
      id: "inst-1",
      uid: UID_A,
      name: "A",
      format: "clap",
      status: "stub",
      trackId: null,
    });
    await plugins.instantiate(UID_A);
    expect(plugins.catalog.recents[0]?.uid).toBe(UID_A);
  });

  it("forgetRecent removes one entry; clearRecents empties the list", () => {
    plugins.noteUsed(UID_A);
    plugins.noteUsed(UID_B);
    plugins.forgetRecent(UID_A);
    expect(plugins.catalog.recents.map((r) => r.uid)).toEqual([UID_B]);
    plugins.clearRecents();
    expect(plugins.catalog.recents).toEqual([]);
  });
});

describe("tags", () => {
  it("setTags stores tags; an empty list clears the entry", () => {
    plugins.setTags(UID_A, ["Bass", "Live rig"]);
    expect(plugins.catalog.tags[UID_A]).toEqual(["Bass", "Live rig"]);
    plugins.setTags(UID_A, []);
    expect(plugins.catalog.tags[UID_A]).toBeUndefined();
  });
});

describe("pinned params", () => {
  it("caps at 8 per uid and de-dupes", () => {
    plugins.setPinnedParams(UID_A, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0, 1]);
    expect(plugins.pinnedParamsFor(UID_A)).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);
  });

  it("an empty list clears the entry", () => {
    plugins.setPinnedParams(UID_A, [1, 2]);
    expect(plugins.pinnedParamsFor(UID_A)).toEqual([1, 2]);
    plugins.setPinnedParams(UID_A, []);
    expect(plugins.pinnedParamsFor(UID_A)).toEqual([]);
    expect(plugins.catalog.pinnedParams[UID_A]).toBeUndefined();
  });
});
