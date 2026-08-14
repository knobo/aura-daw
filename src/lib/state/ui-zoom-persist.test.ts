/**
 * Interface zoom persistence: the zoom factor survives an app restart.
 * setUiZoom writes the (already clamped/snapped) value through the
 * preferences store, and prefs.init() restores it at boot — ignoring junk,
 * since anything on disk goes through the schema's own validation.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PREFS_KEY } from "../utils/prefs";
import { prefs } from "../prefs/prefs.svelte";
import { resetUiZoom, setUiZoom } from "./ui.svelte";

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

function storedZoom(storage: ReturnType<typeof fakeStorage>): unknown {
  return JSON.parse(storage.map.get(PREFS_KEY) ?? "{}").uiZoom;
}

let storage: ReturnType<typeof fakeStorage>;

beforeEach(() => {
  storage = fakeStorage();
  vi.stubGlobal("localStorage", storage);
  prefs.values.uiZoom = 1; // reset the singleton without touching prefs
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("interface zoom persistence", () => {
  it("setUiZoom persists the snapped value", () => {
    setUiZoom(1.2345);
    expect(storedZoom(storage)).toBe(1.2);
  });

  it("resetUiZoom persists 1.0", () => {
    setUiZoom(1.5);
    resetUiZoom();
    expect(storedZoom(storage)).toBe(1);
  });

  it("rejected input persists nothing", () => {
    setUiZoom(NaN);
    expect(storage.map.size).toBe(0);
  });

  it("prefs.init() restores a stored zoom", () => {
    storage.map.set(PREFS_KEY, JSON.stringify({ uiZoom: 1.5 }));
    prefs.init();
    expect(prefs.values.uiZoom).toBe(1.5);
  });

  it("prefs.init() keeps the default when nothing is stored", () => {
    prefs.init();
    expect(prefs.values.uiZoom).toBe(1);
  });

  it("prefs.init() clamps an out-of-range stored value", () => {
    storage.map.set(PREFS_KEY, JSON.stringify({ uiZoom: 99 }));
    prefs.init();
    expect(prefs.values.uiZoom).toBe(2);
  });

  it("prefs.init() ignores a non-numeric stored value", () => {
    storage.map.set(PREFS_KEY, JSON.stringify({ uiZoom: "big" }));
    prefs.init();
    expect(prefs.values.uiZoom).toBe(1);
  });

  it("a restored zoom survives the round trip exactly", () => {
    setUiZoom(1.7);
    prefs.values.uiZoom = 1; // simulate restart
    prefs.init();
    expect(prefs.values.uiZoom).toBe(1.7);
  });
});
