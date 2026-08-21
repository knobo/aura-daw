/**
 * Pure bulk-M/S/A semantics (4.5): what value a bulk press should apply,
 * and what a group of lanes should be shown as before it's pressed.
 */
import { describe, expect, it } from "vitest";
import { bulkTriState, bulkableTracks, fieldValues, nextBulkValue } from "./lane-bulk";
import type { TrackState } from "../types/ipc";

function track(patch: Partial<TrackState> = {}): TrackState {
  return {
    id: "t",
    name: "T",
    kind: "audio",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    automationMode: "read",
    color: "#888",
    ...patch,
  };
}

describe("nextBulkValue", () => {
  it("mutes all when any of the selection is unmuted", () => {
    expect(nextBulkValue([false, false, false])).toBe(true);
    expect(nextBulkValue([true, false, true])).toBe(true);
  });

  it("unmutes all only when every one is already muted", () => {
    expect(nextBulkValue([true, true, true])).toBe(false);
  });

  it("an empty selection has nothing to turn on", () => {
    expect(nextBulkValue([])).toBe(false);
  });
});

describe("bulkTriState", () => {
  it("is on when every value is true", () => {
    expect(bulkTriState([true, true])).toBe("on");
  });
  it("is off when every value is false", () => {
    expect(bulkTriState([false, false])).toBe("off");
  });
  it("is mixed when they disagree", () => {
    expect(bulkTriState([true, false])).toBe("mixed");
    expect(bulkTriState([false, true, true])).toBe("mixed");
  });
  it("an empty group reads as off, not mixed", () => {
    expect(bulkTriState([])).toBe("off");
  });
});

describe("bulkableTracks", () => {
  const tracks = [
    track({ id: "a", muted: true }),
    track({ id: "b", kind: "midi" }),
    track({ id: "auto", kind: "automation" }),
    track({ id: "bus", kind: "bus" }),
  ];

  it("filters to the given ids, dropping automation lanes even if named", () => {
    const got = bulkableTracks(tracks, ["a", "b", "auto"]);
    expect(got.map((t) => t.id)).toEqual(["a", "b"]);
  });

  it("accepts a Set of ids too", () => {
    const got = bulkableTracks(tracks, new Set(["b", "bus"]));
    expect(got.map((t) => t.id)).toEqual(["b", "bus"]);
  });

  it("with no ids given, every non-automation track is eligible", () => {
    const got = bulkableTracks(tracks);
    expect(got.map((t) => t.id)).toEqual(["a", "b", "bus"]);
  });
});

describe("fieldValues", () => {
  it("reads one boolean field across a track list", () => {
    const tracks = [track({ muted: true }), track({ muted: false })];
    expect(fieldValues(tracks, "muted")).toEqual([true, false]);
  });
});
