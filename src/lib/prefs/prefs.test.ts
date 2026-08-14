/**
 * Preferences store: reactive schema-backed values persisted through the
 * aura.prefs blob. Pins the lifecycle — defaults before init, validated
 * restore at init, write-through + change notification on set — and that
 * junk on disk or junk callers can never corrupt the live values.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PREFS_KEY } from "../utils/prefs";
import { PREF_SCHEMA } from "./schema";
import { prefs } from "./prefs.svelte";

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

function stored(storage: ReturnType<typeof fakeStorage>): Record<string, unknown> {
  return JSON.parse(storage.map.get(PREFS_KEY) ?? "{}");
}

let storage: ReturnType<typeof fakeStorage>;

beforeEach(() => {
  storage = fakeStorage();
  vi.stubGlobal("localStorage", storage);
  prefs.restoreDefaults(); // reset the singleton between tests
  storage.map.clear();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("prefs store", () => {
  it("starts from schema defaults", () => {
    expect(prefs.values.clipOpenAutoplay).toBe(PREF_SCHEMA.clipOpenAutoplay.default);
    expect(prefs.values.uiZoom).toBe(PREF_SCHEMA.uiZoom.default);
  });

  it("init() restores valid persisted values and ignores junk", () => {
    storage.map.set(
      PREFS_KEY,
      JSON.stringify({
        clipOpenAutoplay: true,
        uiZoom: 97, // out of range → clamped, not rejected
        mcpDefaultMode: "yolo", // junk → default stands
        unknownKey: "kept for future versions",
      }),
    );
    prefs.init();
    expect(prefs.values.clipOpenAutoplay).toBe(true);
    expect(prefs.values.uiZoom).toBe(2.0);
    expect(prefs.values.mcpDefaultMode).toBe("confirmDestructive");
  });

  it("set() updates the live value and writes through", () => {
    prefs.set("clipOpenAutoplay", true);
    expect(prefs.values.clipOpenAutoplay).toBe(true);
    expect(stored(storage).clipOpenAutoplay).toBe(true);
  });

  it("set() coerces numbers before storing", () => {
    prefs.set("uiZoom", 1.4499999);
    expect(prefs.values.uiZoom).toBe(1.4);
    expect(stored(storage).uiZoom).toBe(1.4);
  });

  it("set() ignores invalid values entirely", () => {
    prefs.set("mcpDefaultMode", "root-access");
    expect(prefs.values.mcpDefaultMode).toBe("confirmDestructive");
    expect(stored(storage).mcpDefaultMode).toBeUndefined();
  });

  it("set() preserves foreign keys already in the blob", () => {
    storage.map.set(PREFS_KEY, JSON.stringify({ someOtherTool: 42 }));
    prefs.set("noteFlash", false);
    expect(stored(storage).someOtherTool).toBe(42);
    expect(stored(storage).noteFlash).toBe(false);
  });

  it("onChange fires on real changes, not on no-ops or invalid sets", () => {
    const seen: unknown[] = [];
    const off = prefs.onChange("mcpDefaultMode", (v) => seen.push(v));
    prefs.set("mcpDefaultMode", "full");
    prefs.set("mcpDefaultMode", "full"); // unchanged → silent
    prefs.set("mcpDefaultMode", "junk"); // invalid → silent
    expect(seen).toEqual(["full"]);
    off();
    prefs.set("mcpDefaultMode", "readOnly");
    expect(seen).toEqual(["full"]);
  });

  it("restoreDefaults() puts every value back and persists that", () => {
    prefs.set("clipOpenAutoplay", true);
    prefs.set("uiZoom", 1.6);
    prefs.restoreDefaults();
    expect(prefs.values.clipOpenAutoplay).toBe(false);
    expect(prefs.values.uiZoom).toBe(1);
    expect(stored(storage).uiZoom).toBe(1);
  });

  it("survives a storage-less environment", () => {
    vi.unstubAllGlobals();
    vi.stubGlobal("localStorage", undefined);
    prefs.init();
    prefs.set("noteFlash", false);
    expect(prefs.values.noteFlash).toBe(false); // live value still works
  });
});
