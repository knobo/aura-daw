/**
 * Lane selection (4.5) — VIEW state on `lanes`, mirroring clip-selection's
 * conventions: a plain Set of ids, replaced (never mutated) so `$state`
 * reactivity fires, never persisted, and pruned/cleared by `sync()` the
 * same way the fold state is.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TrackState } from "../types/ipc";

vi.stubGlobal("localStorage", {
  getItem: () => null,
  setItem: () => {},
});

vi.mock("../tauri", () => ({
  backend: { mode: "demo", on: () => () => {} },
}));

const { lanes } = await import("./lanes.svelte");
const { project } = await import("./project.svelte");

function track(id: string): TrackState {
  return {
    id,
    name: id,
    kind: "audio",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#888",
  };
}

const ORDER = ["a", "b", "c", "d", "e"];

beforeEach(() => {
  project.tracks = ORDER.map(track);
  project.projectDir = "/proj";
  // Prime `#loadedFor` so a `sync()` called mid-test (the pruning tests
  // below) reads as "same project" rather than a switch that wipes the
  // selection the test just set up.
  lanes.sync();
  lanes.clearSelection(); // also resets the shift-extend anchor between tests
});

describe("selectOnly / isSelected", () => {
  it("replaces the whole selection with one id", () => {
    lanes.selection = new Set(["a", "b"]);
    lanes.selectOnly("c");
    expect(lanes.isSelected("c")).toBe(true);
    expect(lanes.isSelected("a")).toBe(false);
    expect([...lanes.selection]).toEqual(["c"]);
  });
});

describe("toggleSelected", () => {
  it("adds an unselected id", () => {
    lanes.toggleSelected("b");
    expect(lanes.isSelected("b")).toBe(true);
  });
  it("removes an already-selected id", () => {
    lanes.selection = new Set(["b"]);
    lanes.toggleSelected("b");
    expect(lanes.isSelected("b")).toBe(false);
  });
});

describe("extendTo", () => {
  it("selects the contiguous run between the last selected id and the target", () => {
    lanes.selectOnly("b");
    lanes.extendTo("d", ORDER);
    expect([...lanes.selection].sort()).toEqual(["b", "c", "d"]);
  });

  it("extends backwards just as well", () => {
    lanes.selectOnly("d");
    lanes.extendTo("b", ORDER);
    expect([...lanes.selection].sort()).toEqual(["b", "c", "d"]);
  });

  it("with nothing selected yet, behaves like selectOnly", () => {
    lanes.extendTo("c", ORDER);
    expect([...lanes.selection]).toEqual(["c"]);
  });

  it("an id outside the given order still selects on its own", () => {
    lanes.selectOnly("b");
    lanes.extendTo("ghost", ORDER);
    expect([...lanes.selection]).toEqual(["ghost"]);
  });
});

describe("clearSelection", () => {
  it("empties it", () => {
    lanes.selection = new Set(["a", "b"]);
    lanes.clearSelection();
    expect(lanes.selection.size).toBe(0);
  });
});

describe("selectGroup", () => {
  it("selects every member of the named group", () => {
    project.tracks = [
      { ...track("a"), group: "drums" },
      { ...track("b"), group: "drums" },
      track("c"),
    ];
    lanes.selectGroup("drums");
    expect([...lanes.selection].sort()).toEqual(["a", "b"]);
  });
});

describe("selectAll", () => {
  it("selects every track in the project", () => {
    lanes.selectAll();
    expect([...lanes.selection].sort()).toEqual([...ORDER].sort());
  });
});

describe("sync", () => {
  it("prunes ids for tracks that no longer exist", () => {
    lanes.selection = new Set(["a", "b", "ghost"]);
    project.tracks = [track("a")];
    lanes.sync();
    expect([...lanes.selection]).toEqual(["a"]);
  });

  it("clears the selection on a project switch", () => {
    lanes.selection = new Set(["a"]);
    project.projectDir = "/other";
    project.tracks = ORDER.map(track);
    lanes.sync();
    expect(lanes.selection.size).toBe(0);
  });
});
