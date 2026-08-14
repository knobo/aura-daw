/**
 * User preferences: a tiny JSON bag in localStorage. These tests pin the
 * contract: round-trip, tolerance for missing storage (vitest runs in a
 * plain node environment), corrupt JSON, and throwing storage (private
 * browsing / quota) — none of which may ever crash the app.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PREFS_KEY, readPref, writePref } from "./prefs";

function fakeStorage(initial: Record<string, string> = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem: (key: string) => (map.has(key) ? (map.get(key) as string) : null),
    setItem: (key: string, value: string) => {
      map.set(key, value);
    },
    map,
  };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("prefs with storage available", () => {
  let storage: ReturnType<typeof fakeStorage>;

  beforeEach(() => {
    storage = fakeStorage();
    vi.stubGlobal("localStorage", storage);
  });

  it("readPref returns undefined for a key never written", () => {
    expect(readPref("uiZoom")).toBeUndefined();
  });

  it("round-trips a written value", () => {
    writePref("uiZoom", 1.5);
    expect(readPref("uiZoom")).toBe(1.5);
  });

  it("keeps other keys intact when writing", () => {
    writePref("uiZoom", 1.5);
    writePref("dockWidth", 400);
    expect(readPref("uiZoom")).toBe(1.5);
    expect(readPref("dockWidth")).toBe(400);
  });

  it("stores everything under a single namespaced key", () => {
    writePref("uiZoom", 1.5);
    expect([...storage.map.keys()]).toEqual([PREFS_KEY]);
  });

  it("returns undefined when the stored JSON is corrupt", () => {
    storage.map.set(PREFS_KEY, "{not json");
    expect(readPref("uiZoom")).toBeUndefined();
  });

  it("recovers by overwriting corrupt JSON on the next write", () => {
    storage.map.set(PREFS_KEY, "{not json");
    writePref("uiZoom", 1.2);
    expect(readPref("uiZoom")).toBe(1.2);
  });

  it("returns undefined when the stored JSON is not an object", () => {
    storage.map.set(PREFS_KEY, '"just a string"');
    expect(readPref("uiZoom")).toBeUndefined();
  });
});

describe("prefs without storage (node, old webviews)", () => {
  it("readPref returns undefined and writePref is a no-op", () => {
    // No localStorage stub here: plain node has none.
    expect(readPref("uiZoom")).toBeUndefined();
    expect(() => writePref("uiZoom", 1.5)).not.toThrow();
  });
});

describe("prefs with throwing storage (private mode, quota)", () => {
  beforeEach(() => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("quota");
      },
    });
  });

  it("readPref swallows the error and returns undefined", () => {
    expect(readPref("uiZoom")).toBeUndefined();
  });

  it("writePref swallows the error", () => {
    expect(() => writePref("uiZoom", 1.5)).not.toThrow();
  });
});
