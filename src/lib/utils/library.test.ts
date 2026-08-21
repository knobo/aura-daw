/**
 * Pure helpers behind the library panel. No DOM, no stores — the vitest
 * environment is "node" (see vitest.config.ts).
 */
import { describe, expect, it } from "vitest";
import { parentDir } from "./library";
import {
  LIBRARY_DRAG_MIME,
  decodeLibraryDrag,
  encodeLibraryDrag,
  groupLiveUsages,
  hasLibraryDrag,
  nextUsage,
} from "./library";
import type { ClipUsageEntry } from "./library";

describe("parentDir", () => {
  it("walks up a POSIX path", () => {
    expect(parentDir("/home/u/Music/AURA/Library/drums")).toBe("/home/u/Music/AURA/Library");
    expect(parentDir("/home/u/Music/AURA/Library/drums/")).toBe("/home/u/Music/AURA/Library");
  });

  it("walks up a Windows path", () => {
    expect(parentDir("C:\\Users\\u\\Samples\\drums")).toBe("C:\\Users\\u\\Samples");
  });

  it("returns null at a filesystem root, so the UI can hide the up button", () => {
    expect(parentDir("/")).toBeNull();
    expect(parentDir("C:\\")).toBeNull();
    expect(parentDir("")).toBeNull();
  });
});

/** Minimal stand-in for DataTransfer (vitest runs in "node" — no DOM). */
function fakeDataTransfer() {
  const store = new Map<string, string>();
  return {
    types: [] as string[],
    setData(type: string, data: string) {
      store.set(type, data);
      if (!this.types.includes(type)) this.types.push(type);
    },
    getData(type: string) {
      return store.get(type) ?? "";
    },
    effectAllowed: "" as string,
  };
}

describe("library drag payloads", () => {
  it("round-trips a sample-file payload", () => {
    const dt = fakeDataTransfer();
    encodeLibraryDrag(dt, { kind: "sampleFile", path: "/lib/kick.wav", name: "kick.wav" });
    expect(dt.types).toContain(LIBRARY_DRAG_MIME);
    expect(hasLibraryDrag(dt)).toBe(true);
    expect(decodeLibraryDrag(dt)).toEqual({
      kind: "sampleFile",
      path: "/lib/kick.wav",
      name: "kick.wav",
    });
  });

  it("round-trips every payload kind", () => {
    const payloads = [
      { kind: "sampleFile", path: "/a.wav", name: "a.wav" },
      { kind: "projectAudioClip", clipId: "c1" },
      { kind: "projectMidiClip", clipId: "m1" },
      { kind: "samplerInstrument", instrumentId: "i1", name: "Piano" },
      { kind: "zynPatch", patch: { bank: "Arpeggios", name: "Arp 1", program: 1, path: "/a.xiz" } },
      { kind: "pluginInstrument", uid: "clap:surge", name: "Surge XT" },
      { kind: "pluginEffect", uid: "clap:verb", name: "Calf Reverb" },
      { kind: "pluginInstance", instanceId: "i1", name: "Surge XT" },
    ] as const;
    for (const p of payloads) {
      const dt = fakeDataTransfer();
      encodeLibraryDrag(dt, p);
      expect(decodeLibraryDrag(dt)).toEqual(p);
    }
  });

  it("ignores foreign drags — an OS file drop is never mistaken for ours", () => {
    const dt = fakeDataTransfer();
    dt.types.push("Files");
    expect(hasLibraryDrag(dt)).toBe(false);
    expect(decodeLibraryDrag(dt)).toBeNull();
    expect(hasLibraryDrag(null)).toBe(false);
    expect(decodeLibraryDrag(null)).toBeNull();
  });

  it("refuses malformed or unknown payloads instead of throwing", () => {
    const bad = fakeDataTransfer();
    bad.setData(LIBRARY_DRAG_MIME, "{not json");
    expect(decodeLibraryDrag(bad)).toBeNull();

    const unknown = fakeDataTransfer();
    unknown.setData(LIBRARY_DRAG_MIME, JSON.stringify({ kind: "somethingElse" }));
    expect(decodeLibraryDrag(unknown)).toBeNull();
  });
});

function entry(id: string, name: string, trackId: string, startTicks = 0): ClipUsageEntry {
  return { id, name, trackId, timelineStartTicks: startTicks };
}

describe("groupLiveUsages", () => {
  it("groups every placement by name, on tracks that still exist", () => {
    const clips = [
      entry("a", "Groove", "t1", 0),
      entry("b", "Groove", "t2", 960),
      entry("c", "Other", "t1", 0),
    ];
    const live = new Set(["t1", "t2"]);
    const groups = groupLiveUsages(clips, live);
    expect(groups.get("Groove")).toEqual([clips[0], clips[1]]);
    expect(groups.get("Other")).toEqual([clips[2]]);
  });

  it("drops placements whose track was deleted", () => {
    const clips = [entry("a", "Groove", "t1"), entry("b", "Groove", "t-gone")];
    const live = new Set(["t1"]);
    expect(groupLiveUsages(clips, live).get("Groove")).toEqual([clips[0]]);
  });

  it("omits a name with no live placement", () => {
    const clips = [entry("a", "Groove", "t-gone")];
    expect(groupLiveUsages(clips, new Set()).get("Groove")).toBeUndefined();
  });
});

describe("nextUsage", () => {
  it("returns the first location when nothing is current", () => {
    const locs = [entry("a", "Groove", "t1"), entry("b", "Groove", "t2")];
    expect(nextUsage(locs, null)).toBe(locs[0]);
  });

  it("advances to the following location", () => {
    const locs = [entry("a", "Groove", "t1"), entry("b", "Groove", "t2")];
    expect(nextUsage(locs, "a")).toBe(locs[1]);
  });

  it("wraps around after the last location", () => {
    const locs = [entry("a", "Groove", "t1"), entry("b", "Groove", "t2")];
    expect(nextUsage(locs, "b")).toBe(locs[0]);
  });

  it("restarts from the first location when the current id is stale", () => {
    const locs = [entry("a", "Groove", "t1"), entry("b", "Groove", "t2")];
    expect(nextUsage(locs, "gone")).toBe(locs[0]);
  });

  it("is null for an empty group", () => {
    expect(nextUsage([], null)).toBeNull();
  });
});
