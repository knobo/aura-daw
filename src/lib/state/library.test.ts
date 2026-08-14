/**
 * Library store: folder navigation, the per-folder cache, and audition.
 * Drag/drop behaviour is Task 6's; these pin the browsing half.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LibraryEntry } from "../types/ipc";

const dirEntry = (name: string, path: string): LibraryEntry => ({
  name,
  path,
  kind: "dir",
  ext: "",
  sizeBytes: 0,
  modifiedMs: 0,
});
const fileEntry = (name: string, path: string): LibraryEntry => ({
  name,
  path,
  kind: "audio",
  ext: "wav",
  sizeBytes: 100,
  modifiedMs: 0,
});

const invokes = {
  libraryScan: vi.fn(async (dir: string): Promise<LibraryEntry[]> =>
    dir === "/lib" ? [dirEntry("drums", "/lib/drums"), fileEntry("kick.wav", "/lib/kick.wav")] : [],
  ),
  libraryDefaultRoot: vi.fn(async () => "/lib"),
  libraryAudition: vi.fn(async (_p: string, _s?: number | null) => {}),
  libraryAuditionStop: vi.fn(async () => {}),
};

const mockBackend = { mode: "tauri" as const, on: () => () => {}, ...invokes };
vi.mock("../tauri", () => ({ backend: mockBackend }));

const { prefs } = await import("../prefs/prefs.svelte");
const { library } = await import("./library.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  prefs.set("librarySampleFolders", []);
  library.reset();
});

describe("library store", () => {
  it("resolves the default root once and lists it first among the folders", async () => {
    await library.init();
    expect(invokes.libraryDefaultRoot).toHaveBeenCalledTimes(1);
    prefs.set("librarySampleFolders", ["/extra", "/lib"]);
    expect(library.folders).toEqual(["/lib", "/extra"]);

    await library.init();
    expect(invokes.libraryDefaultRoot).toHaveBeenCalledTimes(1);
  });

  it("opens a folder, exposes its entries, and serves a re-open from cache", async () => {
    await library.init();
    await library.open("/lib");
    expect(library.cwd).toBe("/lib");
    expect(library.entries.map((e) => e.name)).toEqual(["drums", "kick.wav"]);
    expect(library.loading).toBe(false);
    expect(library.error).toBeNull();

    await library.open("/lib/drums");
    await library.open("/lib");
    expect(invokes.libraryScan).toHaveBeenCalledTimes(2);
  });

  it("refresh() busts the cache for the current folder", async () => {
    await library.init();
    await library.open("/lib");
    await library.refresh();
    expect(invokes.libraryScan).toHaveBeenCalledTimes(2);
  });

  it("surfaces a scan failure as an error and leaves the entries empty", async () => {
    invokes.libraryScan.mockRejectedValueOnce(new Error("cannot read /nope"));
    await library.open("/nope");
    expect(library.error).toContain("cannot read /nope");
    expect(library.entries).toEqual([]);
    expect(library.loading).toBe(false);
  });

  it("up() walks to the parent and clears at a configured root", async () => {
    await library.init();
    await library.open("/lib/drums");
    await library.up();
    expect(library.cwd).toBe("/lib");
    await library.up();
    expect(library.cwd).toBeNull();
  });

  it("audition() plays a file and records which one is sounding", async () => {
    await library.audition("/lib/kick.wav");
    expect(invokes.libraryAudition).toHaveBeenCalledWith("/lib/kick.wav", null);
    expect(library.auditioning).toBe("/lib/kick.wav");

    await library.stopAudition();
    expect(invokes.libraryAuditionStop).toHaveBeenCalledTimes(1);
    expect(library.auditioning).toBe("");
  });

  it("clears the auditioning marker when the backend rejects", async () => {
    invokes.libraryAudition.mockRejectedValueOnce(new Error("no output device"));
    await library.audition("/lib/kick.wav");
    expect(library.auditioning).toBe("");
    expect(library.error).toContain("no output device");
  });
});
