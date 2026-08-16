/**
 * Last-opened project path: a single localStorage string that must never
 * crash the app. Same failure modes as prefs — missing storage, throwing
 * storage, junk on disk — all degrade to "no last project".
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LAST_PROJECT_KEY, readLastProjectDir, writeLastProjectDir } from "./last-project";

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

describe("last project path with storage available", () => {
  let storage: ReturnType<typeof fakeStorage>;

  beforeEach(() => {
    storage = fakeStorage();
    vi.stubGlobal("localStorage", storage);
  });

  it("readLastProjectDir returns undefined when nothing was written", () => {
    expect(readLastProjectDir()).toBeUndefined();
  });

  it("round-trips a written path", () => {
    writeLastProjectDir("/home/u/Music/AURA/Song.aura");
    expect(readLastProjectDir()).toBe("/home/u/Music/AURA/Song.aura");
  });

  it("stores the path under a dedicated key", () => {
    writeLastProjectDir("/p/Song.aura");
    expect([...storage.map.keys()]).toEqual([LAST_PROJECT_KEY]);
  });

  it("ignores an empty path so junk cannot overwrite a real one", () => {
    writeLastProjectDir("/p/Song.aura");
    writeLastProjectDir("");
    writeLastProjectDir("   ");
    expect(readLastProjectDir()).toBe("/p/Song.aura");
  });

  it("returns undefined when the stored value is empty", () => {
    storage.map.set(LAST_PROJECT_KEY, "");
    expect(readLastProjectDir()).toBeUndefined();
  });
});

describe("last project path without storage", () => {
  it("read returns undefined and write does not throw", () => {
    expect(readLastProjectDir()).toBeUndefined();
    expect(() => writeLastProjectDir("/p/Song.aura")).not.toThrow();
  });
});

describe("last project path with throwing storage", () => {
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

  it("read swallows the error and returns undefined", () => {
    expect(readLastProjectDir()).toBeUndefined();
  });

  it("write swallows the error", () => {
    expect(() => writeLastProjectDir("/p/Song.aura")).not.toThrow();
  });
});
