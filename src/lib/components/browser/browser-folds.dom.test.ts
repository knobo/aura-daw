import { beforeEach, describe, expect, it } from "vitest";
import { loadFolds, saveFolds } from "./browser-folds";

// Storage-backed, so jsdom rather than the node `unit` project: the node
// env has no `localStorage`, and these helpers swallow that absence by
// design — a node test would pass vacuously against a no-op.
describe("loadFolds / saveFolds", () => {
  beforeEach(() => localStorage.clear());

  it("round-trips a fold set through storage", () => {
    saveFolds("plugins", new Set(["effects"]));
    expect([...loadFolds("plugins")]).toEqual(["effects"]);
  });

  it("returns nothing folded for a browser that never saved", () => {
    expect(loadFolds("never-seen").size).toBe(0);
  });

  it("ignores a malformed stored value rather than throwing", () => {
    localStorage.setItem("aura.browser.folds:plugins", "{oops");
    expect(loadFolds("plugins").size).toBe(0);
  });
});
