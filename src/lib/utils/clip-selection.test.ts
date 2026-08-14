import { describe, expect, it } from "vitest";
import {
  applySelection,
  marqueeClipHits,
  parseKey,
  refKey,
  type ClipRef,
  type LaneBox,
} from "./clip-selection";

const A = (id: string): ClipRef => ({ kind: "audio", id });
const M = (id: string): ClipRef => ({ kind: "midi", id });

describe("refKey / parseKey", () => {
  it("round-trips both kinds", () => {
    expect(refKey(A("c-1"))).toBe("audio:c-1");
    expect(refKey(M("c-1"))).toBe("midi:c-1");
    expect(parseKey("midi:c-1")).toEqual(M("c-1"));
  });

  it("keeps an id containing a colon intact", () => {
    // ids are uuids today, but the encoding must not be the weak link.
    expect(parseKey(refKey(A("a:b:c")))).toEqual(A("a:b:c"));
  });

  it("distinguishes an audio and a midi clip that share an id", () => {
    expect(refKey(A("x"))).not.toBe(refKey(M("x")));
  });
});

describe("applySelection", () => {
  const cur = new Set(["audio:c-1", "midi:c-2"]);

  it("replace keeps only the hits", () => {
    expect(applySelection(cur, [M("c-3")], "replace")).toEqual(new Set(["midi:c-3"]));
  });

  it("add unions without dropping the existing selection", () => {
    expect(applySelection(cur, [M("c-3")], "add")).toEqual(
      new Set(["audio:c-1", "midi:c-2", "midi:c-3"]),
    );
  });

  it("subtract removes hits and ignores misses", () => {
    expect(applySelection(cur, [A("c-1"), A("nope")], "subtract")).toEqual(
      new Set(["midi:c-2"]),
    );
  });

  it("toggle flips each hit independently", () => {
    expect(applySelection(cur, [A("c-1"), M("c-3")], "toggle")).toEqual(
      new Set(["midi:c-2", "midi:c-3"]),
    );
  });

  it("never mutates the input set", () => {
    const before = new Set(cur);
    applySelection(cur, [M("c-9")], "add");
    expect(cur).toEqual(before);
  });
});

describe("marqueeClipHits", () => {
  const boxes: LaneBox[] = [
    { ref: A("a1"), laneIndex: 0, startSamples: 0, endSamples: 1000 },
    { ref: M("m1"), laneIndex: 1, startSamples: 500, endSamples: 1500 },
    { ref: A("a2"), laneIndex: 2, startSamples: 4000, endSamples: 5000 },
  ];

  it("hits clips whose span overlaps the band and whose lane is in range", () => {
    expect(marqueeClipHits(boxes, 400, 600, 0, 1)).toEqual([A("a1"), M("m1")]);
  });

  it("excludes clips outside the lane range even when the time overlaps", () => {
    expect(marqueeClipHits(boxes, 400, 600, 1, 1)).toEqual([M("m1")]);
  });

  it("uses half-open overlap: touching edges do not count", () => {
    // a1 spans [0,1000); a band starting exactly at 1000 must miss it.
    expect(marqueeClipHits(boxes, 1000, 1200, 0, 2)).toEqual([M("m1")]);
  });

  it("returns an empty array when nothing overlaps", () => {
    expect(marqueeClipHits(boxes, 2000, 3000, 0, 2)).toEqual([]);
  });
});
