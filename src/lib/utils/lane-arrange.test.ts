import { describe, expect, it } from "vitest";
import type { TrackState } from "../types/ipc";
import {
  arrangementForAssign,
  arrangementForGroupDissolve,
  arrangementForGroupRename,
  arrangementForMove,
  dropTargetAtY,
  groupNames,
  nextGroupName,
  normalizeGroupRuns,
} from "./lane-arrange";
import { buildLaneLayout, GROUP_HEADER_PX, LANE_HEIGHT_PX } from "./lane-layout";

function track(id: string, group: string | null = null): TrackState {
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
    color: "#112233",
    group,
  };
}

function layout(tracks: TrackState[], collapsedGroups: string[] = []) {
  return buildLaneLayout({
    tracks,
    collapsedTracks: new Set(),
    collapsedGroups: new Set(collapsedGroups),
  });
}

/** Ids only — the assertion most tests actually care about. */
const ids = (ps: { trackId: string }[]) => ps.map((p) => p.trackId);

describe("dropTargetAtY", () => {
  // t1 | [Drums] t2 t3 | t4
  const tracks = [track("t1"), track("t2", "Drums"), track("t3", "Drums"), track("t4")];
  const L = layout(tracks);
  const H = LANE_HEIGHT_PX;
  const G = GROUP_HEADER_PX;
  // rows: t1 @0..88 | header @88..110 | t2 @110..198 | t3 @198..286 | t4 @286..374

  it("drops above the first lane in its top half", () => {
    expect(dropTargetAtY(L, tracks, 5)).toEqual({ index: 0, group: null, y: 0 });
  });

  it("drops below a lane in its bottom half", () => {
    expect(dropTargetAtY(L, tracks, H - 5)).toEqual({ index: 1, group: null, y: H });
  });

  it("stays outside the group in a group header's top half", () => {
    expect(dropTargetAtY(L, tracks, H + 5)).toEqual({ index: 1, group: null, y: H });
  });

  it("joins the group at its start in a group header's bottom half", () => {
    expect(dropTargetAtY(L, tracks, H + G - 4)).toEqual({
      index: 1,
      group: "Drums",
      y: H + G,
    });
  });

  it("inherits the group when hovering a member — the whole rule in one case", () => {
    // Top half of t3 (a member): insert before it, IN the group.
    expect(dropTargetAtY(L, tracks, H + G + H + 5)).toEqual({
      index: 2,
      group: "Drums",
      y: H + G + H,
    });
  });

  it("stays in the group under its LAST member, so appending to a group works", () => {
    const bottomOfT3 = H + G + H * 2;
    expect(dropTargetAtY(L, tracks, bottomOfT3 - 5)).toEqual({
      index: 3,
      group: "Drums",
      y: bottomOfT3,
    });
  });

  it("leaves the group as soon as the pointer is over a lane outside it", () => {
    // Top half of t4 — the gesture for "out of the group, above t4".
    const topOfT4 = H + G + H * 2;
    expect(dropTargetAtY(L, tracks, topOfT4 + 5)).toEqual({
      index: 3,
      group: null,
      y: topOfT4,
    });
  });

  it("treats a folded group as one atomic row — never a drop INTO it", () => {
    const folded = layout(tracks, ["Drums"]);
    // rows: t1 @0..88 | folded header @88..110 | t4 @110..198
    expect(dropTargetAtY(folded, tracks, H + 4)).toEqual({ index: 1, group: null, y: H });
    // Bottom half skips PAST both members rather than landing inside.
    expect(dropTargetAtY(folded, tracks, H + G - 4)).toEqual({
      index: 3,
      group: null,
      y: H + G,
    });
  });

  it("lands at the end, ungrouped, past every row", () => {
    expect(dropTargetAtY(L, tracks, L.totalHeight + 200)).toEqual({
      index: 4,
      group: null,
      y: L.totalHeight,
    });
  });

  it("is a no-op target on an empty arrangement", () => {
    expect(dropTargetAtY(layout([]), [], 40)).toEqual({ index: 0, group: null, y: 0 });
  });
});

describe("arrangementForMove", () => {
  const tracks = [track("t1"), track("t2"), track("t3"), track("t4")];

  it("moves a lane up", () => {
    expect(ids(arrangementForMove(tracks, "t4", 1, null))).toEqual(["t1", "t4", "t2", "t3"]);
  });

  it("adjusts for the dragged lane's own removal when moving DOWN", () => {
    // Index 3 was measured against the list that still contained t1, so a
    // naive splice would land t1 one row too high.
    expect(ids(arrangementForMove(tracks, "t1", 3, null))).toEqual(["t2", "t3", "t1", "t4"]);
  });

  it("moving to the end puts the lane last", () => {
    expect(ids(arrangementForMove(tracks, "t2", 4, null))).toEqual(["t1", "t3", "t4", "t2"]);
  });

  it("dropping where it started changes nothing", () => {
    expect(ids(arrangementForMove(tracks, "t2", 1, null))).toEqual(["t1", "t2", "t3", "t4"]);
  });

  it("carries the target group onto the dragged lane", () => {
    const grouped = [track("t1"), track("t2", "Drums"), track("t3")];
    const out = arrangementForMove(grouped, "t3", 2, "Drums");
    expect(out).toEqual([
      { trackId: "t1", group: null },
      { trackId: "t2", group: "Drums" },
      { trackId: "t3", group: "Drums" },
    ]);
  });

  it("pulls a group back together rather than letting a drop split it", () => {
    // Dropping t4 (ungrouped) between the two Drums lanes would split the
    // group; normalization keeps the run contiguous, so the layout can
    // never paint a group in two pieces as a result of its own gesture.
    const grouped = [track("t1", "Drums"), track("t2", "Drums"), track("t3"), track("t4")];
    const out = arrangementForMove(grouped, "t4", 1, null);
    expect(ids(out)).toEqual(["t1", "t2", "t4", "t3"]);
    expect(out.map((p) => p.group)).toEqual(["Drums", "Drums", null, null]);
  });

  it("returns the arrangement unchanged for an unknown track id", () => {
    const out = arrangementForMove(tracks, "nope", 0, null);
    expect(ids(out)).toEqual(["t1", "t2", "t3", "t4"]);
  });
});

describe("normalizeGroupRuns", () => {
  it("gathers a split group at its first appearance", () => {
    const out = normalizeGroupRuns([
      { trackId: "a", group: "G" },
      { trackId: "b", group: null },
      { trackId: "c", group: "G" },
    ]);
    expect(ids(out)).toEqual(["a", "c", "b"]);
  });

  it("preserves group order by first appearance and member order within", () => {
    const out = normalizeGroupRuns([
      { trackId: "a", group: "B" },
      { trackId: "b", group: "A" },
      { trackId: "c", group: "B" },
      { trackId: "d", group: "A" },
    ]);
    expect(ids(out)).toEqual(["a", "c", "b", "d"]);
  });

  it("is idempotent — which is what lets arrange_lanes skip no-op group writes", () => {
    const once = normalizeGroupRuns([
      { trackId: "a", group: "G" },
      { trackId: "b", group: null },
      { trackId: "c", group: "G" },
    ]);
    expect(normalizeGroupRuns(once)).toEqual(once);
  });

  it("normalizes blank group names to null", () => {
    const out = normalizeGroupRuns([
      { trackId: "a", group: "  " },
      { trackId: "b", group: " G " },
    ]);
    expect(out).toEqual([
      { trackId: "a", group: null },
      { trackId: "b", group: "G" },
    ]);
  });
});

describe("groupNames / nextGroupName", () => {
  it("lists distinct groups in display order", () => {
    expect(groupNames([track("a", "B"), track("b"), track("c", "A"), track("d", "B")])).toEqual([
      "B",
      "A",
    ]);
  });

  it("picks a name that is free, so 'new group' never merges into an existing one", () => {
    // The name IS the identity, so a duplicate would silently join.
    expect(nextGroupName([])).toBe("Group 1");
    expect(nextGroupName([track("a", "Group 1"), track("b", "Group 2")])).toBe("Group 3");
    expect(nextGroupName([track("a", "Group 2")])).toBe("Group 1");
  });
});

describe("group rename / dissolve / assign", () => {
  const tracks = [track("t1", "Drums"), track("t2"), track("t3", "Drums")];

  it("rename moves every member to the new name", () => {
    const out = arrangementForGroupRename(tracks, "Drums", "Kit")!;
    expect(out).toEqual([
      { trackId: "t1", group: "Kit" },
      { trackId: "t3", group: "Kit" },
      { trackId: "t2", group: null },
    ]);
  });

  it("renaming to blank dissolves the group rather than creating a nameless one", () => {
    const out = arrangementForGroupRename(tracks, "Drums", "   ")!;
    expect(out.map((p) => p.group)).toEqual([null, null, null]);
  });

  it("renaming onto a DIFFERENT existing group's name is refused, not merged", () => {
    const withTwoGroups = [...tracks, track("t4", "Keys"), track("t5", "Keys")];
    expect(arrangementForGroupRename(withTwoGroups, "Drums", "Keys")).toBeNull();
    // The no-op case (renaming a group to its own current name) is fine.
    expect(arrangementForGroupRename(withTwoGroups, "Drums", "Drums")).not.toBeNull();
  });

  it("dissolve clears the group but leaves the order alone", () => {
    const out = arrangementForGroupDissolve(tracks, "Drums");
    expect(ids(out)).toEqual(["t1", "t2", "t3"]);
    expect(out.map((p) => p.group)).toEqual([null, null, null]);
  });

  it("assign MOVES the lane to the group's run, because membership implies contiguity", () => {
    const flat = [track("t1", "Drums"), track("t2"), track("t3")];
    const out = arrangementForAssign(flat, "t3", "Drums");
    expect(ids(out)).toEqual(["t1", "t3", "t2"]);
    expect(out.map((p) => p.group)).toEqual(["Drums", "Drums", null]);
  });

  it("assigning null takes the lane out of its group, leaving the rest in place", () => {
    // t1 leaves, so "Drums" is just t3 — a one-member run has nothing to
    // be gathered to, and normalization must not invent a move.
    const out = arrangementForAssign(tracks, "t1", null);
    expect(out).toEqual([
      { trackId: "t1", group: null },
      { trackId: "t2", group: null },
      { trackId: "t3", group: "Drums" },
    ]);
  });
});
