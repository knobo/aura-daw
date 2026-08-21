/**
 * Shared fold controller: the thing every BrowserShell caller plugs into
 * so collapse-all, persistence and the search layer are one implementation
 * instead of four copies of a Set.
 */
import { describe, expect, it } from "vitest";
import { FoldController, type FoldStorage } from "./fold-controller.svelte";

function memory(seed: Record<string, string[]> = {}): FoldStorage & { dump: (id: string) => string[] } {
  const map = new Map<string, Set<string>>(
    Object.entries(seed).map(([id, keys]) => [id, new Set(keys)]),
  );
  return {
    load: (id) => new Set(map.get(id) ?? []),
    save: (id, collapsed) => {
      map.set(id, new Set(collapsed));
    },
    dump: (id) => [...(map.get(id) ?? [])].sort(),
  };
}

const keys = ["g1", "g2", "g3"];

describe("FoldController", () => {
  it("loads the persisted own folds for its browser id", () => {
    const folds = new FoldController("instruments", memory({ instruments: ["g2"] }));
    expect([...folds.collapsed]).toEqual(["g2"]);
  });

  it("toggle(key, false) collapses a group and persists the own layer", () => {
    const store = memory();
    const folds = new FoldController("samples", store);
    folds.toggle("g1", false);
    expect(folds.collapsed.has("g1")).toBe(true);
    expect(store.dump("samples")).toEqual(["g1"]);
  });

  it("toggle(key, true) is a no-op when the group is already expanded", () => {
    const store = memory();
    const folds = new FoldController("samples", store);
    folds.toggle("g1", true);
    expect(folds.collapsed.has("g1")).toBe(false);
    expect(store.dump("samples")).toEqual([]);
  });

  it("setAll folds only the keys currently on screen and persists them", () => {
    const store = memory({ plugins: ["gone"] });
    const folds = new FoldController("plugins", store);
    folds.setAll(keys, true);
    expect([...folds.collapsed].sort()).toEqual(["g1", "g2", "g3", "gone"].sort());
    expect(store.dump("plugins").sort()).toEqual(["g1", "g2", "g3", "gone"].sort());
  });

  it("setAll(false) unfolds the visible keys and leaves off-screen folds alone", () => {
    const store = memory({ plugins: ["g1", "gone"] });
    const folds = new FoldController("plugins", store);
    folds.setAll(["g1", "g2"], false);
    expect([...folds.collapsed]).toEqual(["gone"]);
  });

  it("syncQuery expands everything while a query is active, then restores the own folds", () => {
    const folds = new FoldController("instruments", memory({ instruments: ["g1"] }));
    folds.syncQuery("surge");
    expect(folds.collapsed.size).toBe(0);
    folds.syncQuery("");
    expect([...folds.collapsed]).toEqual(["g1"]);
  });

  it("a fold made during search does not overwrite the user's own folds", () => {
    const store = memory({ instruments: ["g1"] });
    const folds = new FoldController("instruments", store);
    folds.syncQuery("surge");
    folds.toggle("g2", false);
    expect(folds.collapsed.has("g2")).toBe(true);
    expect(store.dump("instruments")).toEqual(["g1"]);
    folds.syncQuery("");
    expect([...folds.collapsed]).toEqual(["g1"]);
  });

  it("retarget swaps to another browser's persisted folds", () => {
    const store = memory({ "presets-instruments": ["banks"], "presets-patches": ["Factory"] });
    const folds = new FoldController("presets-instruments", store);
    expect([...folds.collapsed]).toEqual(["banks"]);
    folds.retarget("presets-patches");
    expect([...folds.collapsed]).toEqual(["Factory"]);
  });

  it("anyCollapsed is true once any currently visible group is folded", () => {
    const folds = new FoldController("instruments", memory({ instruments: ["g1"] }));
    expect(folds.anyCollapsed(keys)).toBe(true);
    expect(folds.anyCollapsed(["g2", "g3"])).toBe(false);
  });
});
