import { describe, expect, it } from "vitest";
import type { PluginDescriptor } from "../types/ipc";
import {
  buildBrowseSections,
  filterByFacets,
  filterByKind,
  filterSections,
  listCategoryFacets,
  pruneUnavailableFacets,
  rankQuickPick,
  visibleSections,
  type BrowseSection,
} from "./plugin-browse";

const desc = (
  uid: string,
  name: string,
  extra: Partial<PluginDescriptor> = {},
): PluginDescriptor => ({
  uid,
  format: "clap",
  name,
  isInstrument: true,
  audioInputs: 0,
  audioOutputs: 2,
  hasNoteInput: true,
  ...extra,
});

const fx = (uid: string, name: string, categories?: string[]) =>
  desc(uid, name, { isInstrument: false, categories });

const build = (over: Partial<Parameters<typeof buildBrowseSections>[0]> = {}) =>
  buildBrowseSections({ descriptors: [], favorites: [], recents: [], query: "", ...over });

const keys = (sections: BrowseSection[]) => sections.map((s) => s.key);
const names = (sections: BrowseSection[], key: string) =>
  sections.find((s) => s.key === key)?.items.map((d) => d.name) ?? [];

describe("buildBrowseSections", () => {
  it("orders the top-level sections favourites, recent, instruments, effects, uncategorised", () => {
    const sections = build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        fx("u2", "Reverb", ["Reverb"]),
        desc("u3", "Nameless"),
      ],
      favorites: ["u1"],
      recents: [{ uid: "u2", usedAt: 10 }],
    });

    expect(keys(sections).filter((k) => !k.includes(":"))).toEqual([
      "fav",
      "recent",
      "inst",
      "fx",
      "uncat",
    ]);
  });

  it("omits a section with nothing in it", () => {
    const sections = build({ descriptors: [desc("u1", "Surge", { categories: ["Synth"] })] });
    expect(keys(sections)).not.toContain("fav");
    expect(keys(sections)).not.toContain("recent");
    expect(keys(sections)).not.toContain("uncat");
  });

  it("puts favourited plugins in the favourites section", () => {
    const sections = build({
      descriptors: [desc("u1", "Surge", { categories: ["Synth"] }), desc("u2", "Vital", { categories: ["Synth"] })],
      favorites: ["u2"],
    });
    expect(names(sections, "fav")).toEqual(["Vital"]);
  });

  it("lists recents newest first, and skips a uid the scan no longer knows", () => {
    const sections = build({
      descriptors: [desc("u1", "Surge", { categories: ["Synth"] }), desc("u2", "Vital", { categories: ["Synth"] })],
      recents: [
        { uid: "u1", usedAt: 5 },
        { uid: "gone", usedAt: 9 },
        { uid: "u2", usedAt: 20 },
      ],
    });
    expect(names(sections, "recent")).toEqual(["Vital", "Surge"]);
  });

  it("sub-groups instruments under their descriptor categories", () => {
    const sections = build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        desc("u2", "TX16", { categories: ["Sampler"] }),
      ],
    });
    expect(keys(sections)).toEqual(["inst", "inst:Sampler", "inst:Synth"]);
    expect(names(sections, "inst:Synth")).toEqual(["Surge"]);
  });

  it("leaves the instruments parent empty — its plugins live in its category children", () => {
    const sections = build({ descriptors: [desc("u1", "Surge", { categories: ["Synth"] })] });
    expect(names(sections, "inst")).toEqual([]);
    expect(sections.find((s) => s.key === "inst")?.count).toBe(1);
  });

  it("repeats a plugin under each of its categories", () => {
    const sections = build({
      descriptors: [desc("u1", "Surge", { categories: ["Synth", "Sampler"] })],
    });
    expect(names(sections, "inst:Synth")).toEqual(["Surge"]);
    expect(names(sections, "inst:Sampler")).toEqual(["Surge"]);
  });

  it("sub-groups effects separately from instruments", () => {
    const sections = build({
      descriptors: [desc("u1", "Surge", { categories: ["Synth"] }), fx("u2", "Calf", ["Reverb"])],
    });
    expect(keys(sections)).toEqual(["inst", "inst:Synth", "fx", "fx:Reverb"]);
  });

  it("sends a plugin with no categories to uncategorised rather than inventing one", () => {
    const sections = build({ descriptors: [desc("u1", "Mystery"), fx("u2", "Odd", [])] });
    expect(names(sections, "uncat")).toEqual(["Mystery", "Odd"]);
    expect(keys(sections)).not.toContain("inst");
  });

  it("records each section's parent so a fold can hide its children", () => {
    const sections = build({ descriptors: [desc("u1", "Surge", { categories: ["Synth"] })] });
    expect(sections.find((s) => s.key === "inst:Synth")?.parentKey).toBe("inst");
    expect(sections.find((s) => s.key === "inst")?.parentKey).toBeUndefined();
  });

  it("narrows every section to the query and drops the ones left empty", () => {
    const sections = build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        desc("u2", "Vital", { categories: ["Synth"] }),
        fx("u3", "Calf Reverb", ["Reverb"]),
      ],
      favorites: ["u3"],
      query: "sur",
    });
    expect(names(sections, "inst:Synth")).toEqual(["Surge"]);
    expect(keys(sections)).not.toContain("fx");
    expect(keys(sections)).not.toContain("fav");
  });

  it("drops a parent whose every child was emptied by the query", () => {
    const sections = build({
      descriptors: [desc("u1", "Surge", { categories: ["Synth"] })],
      query: "nothing-matches-this",
    });
    expect(sections).toEqual([]);
  });

  it("matches on vendor as well as name", () => {
    const sections = build({
      descriptors: [desc("u1", "Surge", { categories: ["Synth"], vendor: "Vember" })],
      query: "vember",
    });
    expect(names(sections, "inst:Synth")).toEqual(["Surge"]);
  });

  it("matches on category — that is what 'type' searches", () => {
    const sections = build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        desc("u2", "TX16", { categories: ["Sampler"] }),
      ],
      query: "sampler",
    });
    expect(names(sections, "inst:Sampler")).toEqual(["TX16"]);
    expect(keys(sections)).not.toContain("inst:Synth");
  });

  it("matches on the catalog's user tags", () => {
    const sections = build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        desc("u2", "Vital", { categories: ["Synth"] }),
      ],
      tags: { u1: ["Live rig"] },
      query: "live rig",
    });
    expect(names(sections, "inst:Synth")).toEqual(["Surge"]);
  });

  it("a format: prefix keeps only that format, then ranks the rest of the query", () => {
    const sections = build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        desc("u2", "Calf Reverb", { isInstrument: false, format: "lv2", categories: ["Reverb"] }),
        desc("u3", "Airwindows Reverb", { isInstrument: false, format: "clap", categories: ["Reverb"] }),
      ],
      query: "format:lv2 reverb",
    });
    expect(names(sections, "fx:Reverb")).toEqual(["Calf Reverb"]);
    expect(keys(sections)).not.toContain("inst");
  });
});

describe("visibleSections", () => {
  const sections = () =>
    build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        fx("u2", "Calf", ["Reverb"]),
      ],
    });

  it("shows everything when nothing is folded", () => {
    expect(keys(visibleSections(sections(), new Set()))).toEqual([
      "inst",
      "inst:Synth",
      "fx",
      "fx:Reverb",
    ]);
  });

  it("hides a folded parent's children while keeping the parent itself", () => {
    expect(keys(visibleSections(sections(), new Set(["inst"])))).toEqual(["inst", "fx", "fx:Reverb"]);
  });

  it("leaves a folded leaf in place — the shell hides its rows, not the header", () => {
    expect(keys(visibleSections(sections(), new Set(["inst:Synth"])))).toEqual([
      "inst",
      "inst:Synth",
      "fx",
      "fx:Reverb",
    ]);
  });
});

describe("rankQuickPick", () => {
  const pick = (over: Partial<Parameters<typeof rankQuickPick>[0]> = {}) =>
    rankQuickPick({ descriptors: [], favorites: [], recents: [], query: "", ...over }).map(
      (d) => d.name,
    );

  it("puts favourites ahead of everything else when nothing is typed", () => {
    expect(
      pick({
        descriptors: [desc("u1", "Alpha"), desc("u2", "Beta"), desc("u3", "Gamma")],
        favorites: ["u3"],
      }),
    ).toEqual(["Gamma", "Alpha", "Beta"]);
  });

  it("puts recents after favourites, most recently used first", () => {
    expect(
      pick({
        descriptors: [desc("u1", "Alpha"), desc("u2", "Beta"), desc("u3", "Gamma")],
        favorites: ["u1"],
        recents: [
          { uid: "u2", usedAt: 5 },
          { uid: "u3", usedAt: 50 },
        ],
      }),
    ).toEqual(["Alpha", "Gamma", "Beta"]);
  });

  it("does not list a favourite twice for also being recent", () => {
    expect(
      pick({
        descriptors: [desc("u1", "Alpha"), desc("u2", "Beta")],
        favorites: ["u1"],
        recents: [{ uid: "u1", usedAt: 9 }],
      }),
    ).toEqual(["Alpha", "Beta"]);
  });

  it("keeps favourites first within a query, and drops non-matches", () => {
    expect(
      pick({
        descriptors: [desc("u1", "Super Synth"), desc("u2", "Sub Synth"), desc("u3", "Reverb")],
        favorites: ["u2"],
        query: "synth",
      }),
    ).toEqual(["Sub Synth", "Super Synth"]);
  });

  it("ranks the rest by fuzzy score, not by scan order", () => {
    expect(
      pick({
        descriptors: [desc("u1", "Mega Surge"), desc("u2", "Surge XT")],
        query: "surge",
      }),
    ).toEqual(["Surge XT", "Mega Surge"]);
  });

  it("returns nothing when the query matches nothing", () => {
    expect(pick({ descriptors: [desc("u1", "Alpha")], favorites: ["u1"], query: "zzz" })).toEqual([]);
  });

  it("honours a format: prefix", () => {
    expect(
      pick({
        descriptors: [desc("u1", "Surge"), desc("u2", "Calf", { format: "lv2", isInstrument: false })],
        query: "format:lv2",
      }),
    ).toEqual(["Calf"]);
  });

  it("lets a frequently used unstarred plugin climb past a starred one used once", () => {
    const now = 1_700_000_000_000;
    expect(
      pick({
        descriptors: [desc("u1", "Starred"), desc("u2", "Daily")],
        favorites: ["u1"],
        now,
        frecency: {
          u1: { count: 1, lastUsedAt: now - 40 * 86_400_000 },
          u2: { count: 5, lastUsedAt: now - 3_600_000 },
        },
      }),
    ).toEqual(["Daily", "Starred"]);
  });
});

describe("filterByFacets (winner spec §3.2)", () => {
  const pool = [
    desc("u1", "Surge", { categories: ["Synth"] }),
    fx("u2", "Calf Reverb", ["Reverb"]),
    desc("u3", "TX16", { categories: ["Sampler"] }),
    desc("u4", "Calf Filter", { isInstrument: false, format: "lv2", categories: ["Filter", "EQ"] }),
  ];

  it("AND across groups: FX + LV2 keeps only the LV2 effect", () => {
    expect(
      filterByFacets(pool, { kind: "fx", formats: ["lv2"] }).map((d) => d.name),
    ).toEqual(["Calf Filter"]);
  });

  it("OR inside the type group: Reverb or Synth keeps both", () => {
    expect(
      filterByFacets(pool, { kind: "all", categories: ["Reverb", "Synth"] }).map((d) => d.name),
    ).toEqual(["Surge", "Calf Reverb"]);
  });

  it("an empty format/type group is no constraint", () => {
    expect(filterByFacets(pool, { kind: "all" }).map((d) => d.name)).toEqual(pool.map((d) => d.name));
  });

  it("lists unique categories from the scan, sorted", () => {
    expect(listCategoryFacets(pool)).toEqual(["EQ", "Filter", "Reverb", "Sampler", "Synth"]);
  });

  it("lists only categories that still have a hit after the kind filter", () => {
    expect(listCategoryFacets(filterByKind(pool, "inst"))).toEqual(["Sampler", "Synth"]);
    expect(listCategoryFacets(filterByKind(pool, "fx"))).toEqual(["EQ", "Filter", "Reverb"]);
  });
});

describe("pruneUnavailableFacets", () => {
  it("drops a selected type that the current kind can no longer produce", () => {
    expect(pruneUnavailableFacets(["EQ", "Synth"], ["Sampler", "Synth"])).toEqual(["Synth"]);
  });

  it("keeps the selection when every chip is still on the row", () => {
    expect(pruneUnavailableFacets(["Synth"], ["Sampler", "Synth"])).toEqual(["Synth"]);
  });
});

describe("filterByKind (design §9.2)", () => {
  const pool = [
    desc("u1", "Surge"),
    fx("u2", "Reverb", ["Reverb"]),
    desc("u3", "Vital"),
  ];

  it("ALL keeps instruments and effects", () => {
    expect(filterByKind(pool, "all").map((d) => d.name)).toEqual(["Surge", "Reverb", "Vital"]);
  });

  it("INST keeps only instruments", () => {
    expect(filterByKind(pool, "inst").map((d) => d.name)).toEqual(["Surge", "Vital"]);
  });

  it("FX keeps only effects — the state today's checkbox cannot reach", () => {
    expect(filterByKind(pool, "fx").map((d) => d.name)).toEqual(["Reverb"]);
  });
});

describe("filterSections (design §9.2)", () => {
  const tree = () =>
    build({
      descriptors: [
        desc("u1", "Surge", { categories: ["Synth"] }),
        fx("u2", "Calf", ["Reverb"]),
      ],
      favorites: ["u1"],
      recents: [{ uid: "u2", usedAt: 1 }],
    });

  it("keeps the whole tree when neither shortlist toggle is on", () => {
    expect(keys(filterSections(tree(), {}))).toEqual(keys(tree()));
  });

  it("favourites-only drops every section that is not Favourites", () => {
    expect(keys(filterSections(tree(), { favoritesOnly: true }))).toEqual(["fav"]);
  });

  it("recents-only drops every section that is not Recent", () => {
    expect(keys(filterSections(tree(), { recentsOnly: true }))).toEqual(["recent"]);
  });

  it("both shortlist toggles keep both those sections", () => {
    expect(keys(filterSections(tree(), { favoritesOnly: true, recentsOnly: true }))).toEqual([
      "fav",
      "recent",
    ]);
  });
});
