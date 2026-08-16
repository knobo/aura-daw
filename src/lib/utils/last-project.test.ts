/**
 * Last-opened project path: a single localStorage string that must never
 * crash the app. Same failure modes as prefs — missing storage, throwing
 * storage, junk on disk — all degrade to "no last project".
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  LAST_PROJECT_KEY,
  RECENT_PROJECTS_CAP,
  RECENT_PROJECTS_KEY,
  readLastProjectDir,
  readRecentProjectDirs,
  recentProjectLabel,
  writeLastProjectDir,
} from "./last-project";

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

  it("stores the history under the recent-projects key", () => {
    writeLastProjectDir("/p/Song.aura");
    expect([...storage.map.keys()]).toEqual([RECENT_PROJECTS_KEY]);
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

describe("recent project history", () => {
  let storage: ReturnType<typeof fakeStorage>;

  beforeEach(() => {
    storage = fakeStorage();
    vi.stubGlobal("localStorage", storage);
  });

  it("keeps the newest path first and does not drop earlier ones", () => {
    writeLastProjectDir("/p/A.aura");
    writeLastProjectDir("/p/B.aura");
    expect(readRecentProjectDirs()).toEqual(["/p/B.aura", "/p/A.aura"]);
    expect(readLastProjectDir()).toBe("/p/B.aura");
  });

  it("moves a re-opened path to the front instead of duplicating it", () => {
    writeLastProjectDir("/p/A.aura");
    writeLastProjectDir("/p/B.aura");
    writeLastProjectDir("/p/A.aura");
    expect(readRecentProjectDirs()).toEqual(["/p/A.aura", "/p/B.aura"]);
  });

  it("caps the stored history so it cannot grow without bound", () => {
    for (let i = 0; i < RECENT_PROJECTS_CAP + 3; i++) {
      writeLastProjectDir(`/p/P${i}.aura`);
    }
    const recent = readRecentProjectDirs();
    expect(recent).toHaveLength(RECENT_PROJECTS_CAP);
    expect(recent[0]).toBe(`/p/P${RECENT_PROJECTS_CAP + 2}.aura`);
    expect(recent.at(-1)).toBe("/p/P3.aura");
  });

  it("migrates a legacy single last-project path into the list", () => {
    storage.map.set(LAST_PROJECT_KEY, "/p/Legacy.aura");
    expect(readRecentProjectDirs()).toEqual(["/p/Legacy.aura"]);
    expect(readLastProjectDir()).toBe("/p/Legacy.aura");
  });

  it("prefers the new list when both keys are present", () => {
    storage.map.set(LAST_PROJECT_KEY, "/p/Legacy.aura");
    storage.map.set(RECENT_PROJECTS_KEY, JSON.stringify(["/p/New.aura"]));
    expect(readRecentProjectDirs()).toEqual(["/p/New.aura"]);
  });

  it("labels a project by its folder name", () => {
    expect(recentProjectLabel("/home/u/Music/AURA/Song.aura")).toBe("Song.aura");
    expect(recentProjectLabel("/p/Song.aura/")).toBe("Song.aura");
  });
});
