import { describe, expect, it } from "vitest";
import {
  type BrowserGroup,
  type BrowserRowRef,
  flattenRows,
  fuzzyScore,
  groupItems,
  nextIndex,
  parseSearchQuery,
  rankItems,
  rowId,
} from "./browser-model";

describe("fuzzyScore", () => {
  it("returns 0 for no match", () => {
    expect(fuzzyScore("Surge XT", "zzz")).toBe(0);
  });

  it("returns 0 for an empty query", () => {
    expect(fuzzyScore("Surge XT", "")).toBe(0);
    expect(fuzzyScore("Surge XT", "   ")).toBe(0);
  });

  it("scores an exact match highest", () => {
    const exact = fuzzyScore("surge", "surge");
    const prefix = fuzzyScore("surge xt", "surge");
    expect(exact).toBeGreaterThan(prefix);
  });

  it("ranks a prefix hit above a mid-word hit", () => {
    const prefix = fuzzyScore("Cutoff Filter", "cut");
    const midWord = fuzzyScore("Bit Crusher", "cru");
    // Both match; the prefix must outrank the mid-word hit regardless of
    // position, not just happen to sort higher in this one example.
    expect(prefix).toBeGreaterThan(0);
    expect(midWord).toBeGreaterThan(0);
    expect(prefix).toBeGreaterThan(midWord);
  });

  it("ranks a word-boundary hit above a mid-word hit at the same query", () => {
    const boundary = fuzzyScore("Surge XT", "xt");
    const midWord = fuzzyScore("Text Synth", "xt");
    expect(boundary).toBeGreaterThan(midWord);
  });

  it("is case-insensitive", () => {
    expect(fuzzyScore("Surge XT", "SURGE")).toBe(fuzzyScore("surge xt", "surge"));
  });

  it("prefers a tighter/earlier match within the same tier", () => {
    // Both are mid-word hits (no word boundary before "ab"); the earlier
    // one in "cabbage" must outrank the later one in "crawlab".
    const early = fuzzyScore("cabbage", "ab");
    const later = fuzzyScore("crawlab", "ab");
    expect(early).toBeGreaterThan(later);
  });
});

interface Thing {
  name: string;
  vendor: string;
}

function thing(name: string, vendor: string): Thing {
  return { name, vendor };
}

describe("rankItems", () => {
  const keys = [
    { value: (t: Thing) => t.name, weight: 3 },
    { value: (t: Thing) => t.vendor, weight: 1 },
  ];

  it("returns items unchanged, in original order, for an empty query", () => {
    const items = [thing("Zebra", "A"), thing("Alpha", "B")];
    expect(rankItems(items, "", keys)).toEqual(items);
  });

  it("drops items that match in no field", () => {
    const items = [thing("Surge XT", "Surge Synth Team"), thing("Vital", "Matt Tytel")];
    const result = rankItems(items, "zzz", keys);
    expect(result).toEqual([]);
  });

  it("a hit in the name (weighted higher) outranks a hit in the vendor", () => {
    // "vital" matches item A's NAME; item B's name doesn't match at all but
    // its VENDOR does ("Vitalic Labs"). The name-field weight must win.
    const nameHit = thing("Vital", "Matt Tytel");
    const vendorHit = thing("Warmth", "Vitalic Labs");
    const result = rankItems([vendorHit, nameHit], "vital", keys);
    expect(result[0]).toBe(nameHit);
    expect(result[1]).toBe(vendorHit);
  });

  it("stable-sorts by score then by the first key's label ascending", () => {
    // All three match "syn" only via vendor (equal score); label (name)
    // breaks the tie alphabetically.
    const c = thing("Charlie", "Synthetics");
    const a = thing("Alpha", "Synthetics");
    const b = thing("Bravo", "Synthetics");
    const result = rankItems([c, a, b], "syn", keys);
    expect(result.map((t) => t.name)).toEqual(["Alpha", "Bravo", "Charlie"]);
  });
});

describe("groupItems", () => {
  it("groups items and preserves first-seen group order", () => {
    const items = [
      { name: "Vital", cat: "Synth" },
      { name: "Calf Reverb", cat: "Effect" },
      { name: "Surge XT", cat: "Synth" },
    ];
    const groups = groupItems(items, (i) => i.cat);
    expect(groups.map((g) => g.key)).toEqual(["Synth", "Effect"]);
    expect(groups[0].items.map((i) => i.name)).toEqual(["Vital", "Surge XT"]);
    expect(groups[1].items.map((i) => i.name)).toEqual(["Calf Reverb"]);
  });

  it("counts items per group via items.length", () => {
    const items = ["a", "a", "b"];
    const groups = groupItems(items, (x) => x);
    expect(groups.find((g) => g.key === "a")?.items.length).toBe(2);
    expect(groups.find((g) => g.key === "b")?.items.length).toBe(1);
  });

  it("returns an empty array for no items", () => {
    expect(groupItems([], (x: string) => x)).toEqual([]);
  });
});

describe("flattenRows", () => {
  const groups: BrowserGroup<string>[] = [
    { key: "g1", label: "Group 1", items: ["a", "b"] },
    { key: "g2", label: "Group 2", items: ["c"] },
  ];

  it("emits a group row followed by each item row, per group, when nothing is folded", () => {
    const rows = flattenRows(groups, new Set());
    expect(rows).toEqual([
      { kind: "group", groupKey: "g1", itemIndex: -1 },
      { kind: "item", groupKey: "g1", itemIndex: 0 },
      { kind: "item", groupKey: "g1", itemIndex: 1 },
      { kind: "group", groupKey: "g2", itemIndex: -1 },
      { kind: "item", groupKey: "g2", itemIndex: 0 },
    ]);
  });

  it("skips a folded group's items but keeps its header row", () => {
    const rows = flattenRows(groups, new Set(["g1"]));
    expect(rows).toEqual([
      { kind: "group", groupKey: "g1", itemIndex: -1 },
      { kind: "group", groupKey: "g2", itemIndex: -1 },
      { kind: "item", groupKey: "g2", itemIndex: 0 },
    ]);
  });

  it("accepts a predicate instead of a Set", () => {
    const rows = flattenRows(groups, (key) => key === "g2");
    expect(rows.filter((r) => r.groupKey === "g2")).toEqual([
      { kind: "group", groupKey: "g2", itemIndex: -1 },
    ]);
  });

  it("folding every group leaves only header rows", () => {
    const rows = flattenRows(groups, new Set(["g1", "g2"]));
    expect(rows.every((r) => r.kind === "group")).toBe(true);
    expect(rows).toHaveLength(2);
  });
});

describe("nextIndex", () => {
  const rows: BrowserRowRef[] = [
    { kind: "group", groupKey: "g1", itemIndex: -1 },
    { kind: "item", groupKey: "g1", itemIndex: 0 },
    { kind: "group", groupKey: "g2", itemIndex: -1 },
  ];

  it("moves forward and backward within bounds", () => {
    expect(nextIndex(rows, 0, 1)).toBe(1);
    expect(nextIndex(rows, 1, -1)).toBe(0);
  });

  it("clamps at the top and bottom instead of wrapping", () => {
    expect(nextIndex(rows, 0, -1)).toBe(0);
    expect(nextIndex(rows, rows.length - 1, 1)).toBe(rows.length - 1);
  });

  it("returns -1 for an empty row list", () => {
    expect(nextIndex([], 0, 1)).toBe(-1);
  });

  it("moving across a folded group's boundary just steps to the next visible row (folding already removed the rest)", () => {
    // Emulate: g1 is folded, so its item row never made it into `rows` in
    // the first place — moving from g1's header by +1 must land on g2's
    // header, not on an item that folding was supposed to hide.
    const folded: BrowserRowRef[] = [
      { kind: "group", groupKey: "g1", itemIndex: -1 },
      { kind: "group", groupKey: "g2", itemIndex: -1 },
      { kind: "item", groupKey: "g2", itemIndex: 0 },
    ];
    expect(nextIndex(folded, 0, 1)).toBe(1);
    expect(folded[nextIndex(folded, 0, 1)]).toEqual({ kind: "group", groupKey: "g2", itemIndex: -1 });
  });
});

describe("rowId", () => {
  it("gives distinct, stable ids for group and item rows", () => {
    const g: BrowserRowRef = { kind: "group", groupKey: "Synth", itemIndex: -1 };
    const i: BrowserRowRef = { kind: "item", groupKey: "Synth", itemIndex: 2 };
    expect(rowId(g)).not.toBe(rowId(i));
    expect(rowId(g)).toBe(rowId({ ...g }));
    expect(rowId(i)).toBe(rowId({ ...i }));
  });
});

describe("parseSearchQuery (design §9.3)", () => {
  it("leaves a plain query as text with no format facet", () => {
    expect(parseSearchQuery("reverb")).toEqual({ text: "reverb" });
  });

  it("strips a format: prefix and lowercases the value", () => {
    expect(parseSearchQuery("format:LV2 reverb")).toEqual({ text: "reverb", format: "lv2" });
  });

  it("treats a format-only query as a facet with empty text", () => {
    expect(parseSearchQuery("format:clap")).toEqual({ text: "", format: "clap" });
  });

  it("ignores a format: with no value", () => {
    expect(parseSearchQuery("format: reverb")).toEqual({ text: "format: reverb" });
  });
});
