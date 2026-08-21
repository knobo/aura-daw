import { describe, expect, it } from "vitest";
import {
  allCollapsed,
  anyCollapsed,
  beginSearch,
  effectiveFolds,
  emptyFoldState,
  endSearch,
  setAllCollapsed,
  toggleFold,
  type FoldState,
} from "./browser-folds";

const state = (own: string[], search: string[] | null = null): FoldState => ({
  own: new Set(own),
  search: search === null ? null : new Set(search),
});

const keys = ["favourites", "instruments", "effects"];

describe("effectiveFolds", () => {
  it("uses the user's own folds when no search is running", () => {
    expect([...effectiveFolds(state(["effects"]))]).toEqual(["effects"]);
  });

  it("uses the search layer while a search is running, ignoring the own folds", () => {
    expect([...effectiveFolds(state(["effects"], []))]).toEqual([]);
  });
});

describe("beginSearch", () => {
  it("expands every group so a result is never hidden behind a fold", () => {
    expect(effectiveFolds(beginSearch(state(["effects", "instruments"]))).size).toBe(0);
  });

  it("keeps the user's own folds untouched underneath", () => {
    expect([...beginSearch(state(["effects"])).own]).toEqual(["effects"]);
  });

  it("does not discard folds made during a search that is already running", () => {
    const searching = state(["effects"], ["instruments"]);
    expect([...effectiveFolds(beginSearch(searching))]).toEqual(["instruments"]);
  });
});

describe("endSearch", () => {
  it("restores the folds the user had before searching", () => {
    const searching = state(["effects"], ["instruments"]);
    expect([...effectiveFolds(endSearch(searching))]).toEqual(["effects"]);
  });

  it("leaves a never-searched state alone", () => {
    expect([...effectiveFolds(endSearch(state(["effects"])))]).toEqual(["effects"]);
  });
});

describe("toggleFold", () => {
  it("folds and unfolds a group when no search is running", () => {
    const folded = toggleFold(emptyFoldState(), "effects");
    expect([...effectiveFolds(folded)]).toEqual(["effects"]);
    expect([...effectiveFolds(toggleFold(folded, "effects"))]).toEqual([]);
  });

  it("writes to the search layer during a search, leaving the own folds intact", () => {
    const next = toggleFold(state(["effects"], []), "instruments");
    expect([...next.search!]).toEqual(["instruments"]);
    expect([...next.own]).toEqual(["effects"]);
  });
});

describe("setAllCollapsed", () => {
  it("folds every known group", () => {
    expect([...effectiveFolds(setAllCollapsed(emptyFoldState(), keys, true))]).toEqual(keys);
  });

  it("unfolds every known group", () => {
    expect(effectiveFolds(setAllCollapsed(state(keys), keys, false)).size).toBe(0);
  });

  it("acts on the search layer during a search without disturbing the own folds", () => {
    const next = setAllCollapsed(state(["effects"], []), keys, true);
    expect([...next.search!]).toEqual(keys);
    expect([...next.own]).toEqual(["effects"]);
  });

  it("leaves folds for groups outside the given keys alone", () => {
    const next = setAllCollapsed(state(["gone"]), keys, true);
    expect(next.own.has("gone")).toBe(true);
  });
});

describe("anyCollapsed / allCollapsed", () => {
  it("reports any collapsed once a single known group is folded", () => {
    expect(anyCollapsed(state(["effects"]), keys)).toBe(true);
    expect(anyCollapsed(emptyFoldState(), keys)).toBe(false);
  });

  it("ignores folds for groups that are not currently on screen", () => {
    expect(anyCollapsed(state(["a-deleted-group"]), keys)).toBe(false);
  });

  it("reports all collapsed only when every known group is folded", () => {
    expect(allCollapsed(state(keys), keys)).toBe(true);
    expect(allCollapsed(state(["effects"]), keys)).toBe(false);
  });

  it("does not call an empty browser fully collapsed", () => {
    expect(allCollapsed(emptyFoldState(), [])).toBe(false);
  });
});
