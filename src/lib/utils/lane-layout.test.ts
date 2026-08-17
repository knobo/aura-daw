import { describe, expect, it } from "vitest";
import type { TrackState } from "../types/ipc";
import {
  bandForTrackRange,
  buildLaneLayout,
  GROUP_HEADER_PX,
  groupOf,
  LANE_COLLAPSED_PX,
  LANE_HEIGHT_PX,
  nearestTrackIndexAtY,
  trackBand,
  trackIndexAtY,
} from "./lane-layout";

function track(id: string, group: string | null = null): TrackState {
  return {
    id,
    name: id.toUpperCase(),
    kind: "audio",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    color: `#00000${id.slice(-1)}`,
    group,
  };
}

function layout(
  tracks: TrackState[],
  collapsedTracks: string[] = [],
  collapsedGroups: string[] = [],
) {
  return buildLaneLayout({
    tracks,
    collapsedTracks: new Set(collapsedTracks),
    collapsedGroups: new Set(collapsedGroups),
  });
}

describe("groupOf", () => {
  it("treats absent, null, empty and whitespace-only as ungrouped", () => {
    // The backend normalizes "" to null on write; the UI must agree, or a
    // stray Some("") from anywhere would render a nameless group header.
    expect(groupOf(track("t1"))).toBeNull();
    expect(groupOf({ ...track("t1"), group: undefined })).toBeNull();
    expect(groupOf({ ...track("t1"), group: "" })).toBeNull();
    expect(groupOf({ ...track("t1"), group: "   " })).toBeNull();
    expect(groupOf({ ...track("t1"), group: "  Drums " })).toBe("Drums");
  });
});

describe("buildLaneLayout", () => {
  it("stacks plain lanes at the full height", () => {
    const l = layout([track("t1"), track("t2"), track("t3")]);
    expect(l.rows.map((r) => [r.top, r.height])).toEqual([
      [0, LANE_HEIGHT_PX],
      [LANE_HEIGHT_PX, LANE_HEIGHT_PX],
      [LANE_HEIGHT_PX * 2, LANE_HEIGHT_PX],
    ]);
    expect(l.totalHeight).toBe(LANE_HEIGHT_PX * 3);
  });

  it("shrinks a folded lane to a strip and pulls everything below it up", () => {
    const l = layout([track("t1"), track("t2"), track("t3")], ["t2"]);
    expect(l.rows.map((r) => r.height)).toEqual([
      LANE_HEIGHT_PX,
      LANE_COLLAPSED_PX,
      LANE_HEIGHT_PX,
    ]);
    expect(l.rows[2].top).toBe(LANE_HEIGHT_PX + LANE_COLLAPSED_PX);
    expect(l.totalHeight).toBe(LANE_HEIGHT_PX * 2 + LANE_COLLAPSED_PX);
  });

  it("emits a header row for a group and keeps its members below it", () => {
    const l = layout([track("t1"), track("t2", "Drums"), track("t3", "Drums")]);
    expect(l.rows.map((r) => r.kind)).toEqual(["track", "group", "track", "track"]);
    const header = l.rows[1];
    expect(header.kind === "group" && header.trackIds).toEqual(["t2", "t3"]);
    expect(header.height).toBe(GROUP_HEADER_PX);
    // The group's colour is its first member's — a folded group still has
    // to be recognisable, and it has no colour of its own.
    expect(header.kind === "group" && header.color).toBe(track("t2").color);
  });

  it("paints no member rows for a folded group", () => {
    const l = layout([track("t1"), track("t2", "Drums"), track("t3", "Drums")], [], ["Drums"]);
    expect(l.rows.map((r) => r.kind)).toEqual(["track", "group"]);
    expect(l.totalHeight).toBe(LANE_HEIGHT_PX + GROUP_HEADER_PX);
    // Crucially, hidden lanes get NO entry in the id map: nothing may try
    // to position a clip or an overlay against a lane that is not on
    // screen.
    expect(l.byTrackId.has("t2")).toBe(false);
    expect(l.byTrackId.has("t1")).toBe(true);
  });

  it("carries the track's index in project.tracks, not its row position", () => {
    // Rows and track indices stop being the same sequence as soon as a
    // group header is inserted. Everything sample-space is keyed by the
    // TRACK index, so this is the field that must stay honest.
    const l = layout([track("t1", "Drums"), track("t2", "Drums"), track("t3")]);
    const rows = l.rows.filter((r) => r.kind === "track");
    expect(rows.map((r) => r.kind === "track" && r.trackIndex)).toEqual([0, 1, 2]);
    expect(l.rows[0].kind).toBe("group"); // the group header shifted them
  });

  it("renders a split group as two runs rather than one broken group", () => {
    // A group is the MAXIMAL CONTIGUOUS RUN. `arrangeLanes` keeps runs
    // contiguous for every gesture the UI offers, so this only happens to
    // a project that arrives split from elsewhere — and showing two
    // headers is better than drawing one group in two places (and far
    // better than silently reordering the user's arrangement).
    const l = layout([track("t1", "Drums"), track("t2"), track("t3", "Drums")]);
    const headers = l.rows.filter((r) => r.kind === "group");
    expect(headers).toHaveLength(2);
    expect(headers.every((h) => h.kind === "group" && h.group === "Drums")).toBe(true);
  });

  it("is empty, not broken, with no tracks", () => {
    const l = layout([]);
    expect(l.rows).toEqual([]);
    expect(l.totalHeight).toBe(0);
  });
});

describe("trackIndexAtY", () => {
  const tracks = [track("t1"), track("t2", "Drums"), track("t3", "Drums")];

  it("maps y to the lane it falls in", () => {
    const l = layout(tracks);
    expect(trackIndexAtY(l, 0)).toBe(0);
    expect(trackIndexAtY(l, LANE_HEIGHT_PX - 1)).toBe(0);
    expect(trackIndexAtY(l, LANE_HEIGHT_PX + GROUP_HEADER_PX)).toBe(1);
  });

  it("returns null over a group header instead of the lane above it", () => {
    // The old clamping version could not say "no lane here", so a marquee
    // dragged across a header silently selected the lane above.
    const l = layout(tracks);
    expect(trackIndexAtY(l, LANE_HEIGHT_PX + 1)).toBeNull();
  });

  it("returns null past the last row and above the first", () => {
    const l = layout(tracks);
    expect(trackIndexAtY(l, l.totalHeight + 50)).toBeNull();
    expect(trackIndexAtY(l, -10)).toBeNull();
  });
});

describe("nearestTrackIndexAtY", () => {
  const tracks = [track("t1"), track("t2", "Drums"), track("t3", "Drums")];

  it("clamps to the first lane above a group header", () => {
    const l = layout(tracks);
    expect(nearestTrackIndexAtY(l, LANE_HEIGHT_PX + 1)).toBe(0);
  });

  it("clamps past the end to the last visible lane", () => {
    const l = layout(tracks);
    expect(nearestTrackIndexAtY(l, l.totalHeight + 500)).toBe(2);
  });

  it("clamps above the first row to the first lane", () => {
    const l = layout(tracks);
    expect(nearestTrackIndexAtY(l, -500)).toBe(0);
  });

  it("is null only when nothing is visible at all", () => {
    const l = layout([track("t1", "Drums")], [], ["Drums"]);
    expect(nearestTrackIndexAtY(l, 10)).toBeNull();
  });
});

describe("trackBand / bandForTrackRange", () => {
  it("gives one lane's band, and nothing for a hidden one", () => {
    const l = layout([track("t1"), track("t2", "Drums")], [], ["Drums"]);
    expect(trackBand(l, "t1")).toEqual({ top: 0, height: LANE_HEIGHT_PX });
    expect(trackBand(l, "t2")).toBeNull();
  });

  it("spans a range of lanes", () => {
    const tracks = [track("t1"), track("t2"), track("t3")];
    const l = layout(tracks);
    expect(bandForTrackRange(l, tracks, 0, 2)).toEqual({
      top: 0,
      height: LANE_HEIGHT_PX * 3,
    });
  });

  it("shrinks onto the visible lanes when part of the range is folded away", () => {
    // A launch region spanning a folded group must not draw a block over
    // the fold — it collapses onto what is actually on screen.
    const tracks = [track("t1"), track("t2", "Drums"), track("t3")];
    const l = layout(tracks, [], ["Drums"]);
    expect(bandForTrackRange(l, tracks, 0, 2)).toEqual({
      top: 0,
      height: LANE_HEIGHT_PX + GROUP_HEADER_PX + LANE_HEIGHT_PX,
    });
  });

  it("is null when every lane in the range is hidden", () => {
    const tracks = [track("t1", "Drums"), track("t2", "Drums")];
    const l = layout(tracks, [], ["Drums"]);
    expect(bandForTrackRange(l, tracks, 0, 1)).toBeNull();
  });
});
