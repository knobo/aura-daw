import { describe, expect, it } from "vitest";
import {
  ADD_MENU,
  activePage,
  addRecipe,
  addStrip,
  addWidget,
  applyTemplate,
  bindWidget,
  emptyLayout,
  gridSizeForClips,
  groupWidgets,
  meterTrackId,
  mixableTracks,
  padGridForClips,
  padIndex,
  pageHasTarget,
  parseLayout,
  removeGroup,
  removeWidget,
  setGridCell,
  setPadMode,
  storageKey,
  stripWidgets,
  targetKey,
  unboundWidget,
  type SurfaceContext,
  type SurfaceLayout,
} from "./control-surface";

const ctx: SurfaceContext = {
  tracks: [
    { id: "t1", name: "Drums", kind: "midi", color: "#52e5ff" },
    { id: "t2", name: "Bass", kind: "audio", color: "#ff4fd8" },
    { id: "t3", name: "Gain auto", kind: "automation", color: "#ffc857" },
  ],
  midiClips: [
    { id: "c1", name: "kick", trackId: "t1" },
    { id: "c2", name: "snare", trackId: "t1" },
    { id: "c3", name: "hat", trackId: "t1" },
  ],
  automations: [
    { id: "a1", trackId: "t1", trackName: "Drums", paramLabel: "gain", target: { kind: "trackGain", trackId: "t1" } },
    { id: "a2", trackId: "t2", trackName: "Bass", paramLabel: "pan", target: { kind: "trackPan", trackId: "t2" } },
    {
      id: "a3",
      trackId: "t2",
      trackName: "Bass",
      paramLabel: "Cutoff",
      target: { kind: "pluginParam", instanceId: "p1", paramId: 7 },
    },
  ],
};

describe("empty layout", () => {
  it("starts with one blank page and no widgets", () => {
    const layout = emptyLayout();
    expect(layout.version).toBe(1);
    expect(layout.pages).toHaveLength(1);
    expect(layout.activePageId).toBe(layout.pages[0].id);
    expect(activePage(layout).widgets).toEqual([]);
    expect(activePage(layout).templateId).toBe("blank");
  });
});

describe("pad numbering (LPD8 origin)", () => {
  it("puts pad 1 at bottom-left of a 4×2 grid", () => {
    // LPD8: top row P5–P8 (rowFromTop 0), bottom row P1–P4 (rowFromTop 1).
    expect(padIndex(4, 2, 0, 1)).toBe(0); // P1
    expect(padIndex(4, 2, 3, 1)).toBe(3); // P4
    expect(padIndex(4, 2, 0, 0)).toBe(4); // P5
    expect(padIndex(4, 2, 3, 0)).toBe(7); // P8
  });

  it("sizes the grid by clip count", () => {
    expect(gridSizeForClips(0)).toEqual({ cols: 4, rows: 2 });
    expect(gridSizeForClips(8)).toEqual({ cols: 4, rows: 2 });
    expect(gridSizeForClips(9)).toEqual({ cols: 4, rows: 4 });
    expect(gridSizeForClips(17)).toEqual({ cols: 8, rows: 8 });
  });

  it("fills a pad grid bottom-left origin", () => {
    const grid = padGridForClips(ctx.midiClips);
    expect(grid.cols).toBe(4);
    expect(grid.rows).toBe(2);
    // c1 → P1 (bottom-left), c2 → P2, c3 → P3
    expect(grid.cells?.[padIndex(4, 2, 0, 1)]).toBe("c1");
    expect(grid.cells?.[padIndex(4, 2, 1, 1)]).toBe("c2");
    expect(grid.cells?.[padIndex(4, 2, 2, 1)]).toBe("c3");
    expect(grid.cells?.[padIndex(4, 2, 0, 0)]).toBeNull();
  });
});

describe("channel strips", () => {
  it("skips automation tracks", () => {
    expect(mixableTracks(ctx.tracks).map((t) => t.id)).toEqual(["t1", "t2"]);
  });

  it("builds mute, solo, arm, gauge, fader, pan for a track", () => {
    const widgets = stripWidgets(ctx.tracks[0]);
    expect(widgets.map((w) => w.kind)).toEqual(["lamp", "lamp", "lamp", "gauge", "fader", "knob"]);
    expect(new Set(widgets.map((w) => w.groupId)).size).toBe(1);
    expect(widgets.find((w) => w.lampRole === "mute")?.target).toEqual({ kind: "trackMute", trackId: "t1" });
    expect(widgets.find((w) => w.kind === "fader")?.target).toEqual({ kind: "trackGain", trackId: "t1" });
    expect(widgets.find((w) => w.label === "PAN")?.target).toEqual({ kind: "trackPan", trackId: "t1" });
  });

  it("refuses a second strip for the same track", () => {
    const once = addStrip(emptyLayout(), ctx.tracks[0]);
    const twice = addStrip(once, ctx.tracks[0]);
    expect(activePage(twice).widgets).toHaveLength(activePage(once).widgets.length);
  });
});

describe("recipes", () => {
  it("add all tracks inserts one strip per mixable track", () => {
    const layout = addRecipe(emptyLayout(), "tracks", ctx);
    const page = activePage(layout);
    expect(page.widgets.filter((w) => w.kind === "fader")).toHaveLength(2);
    expect(pageHasTarget(page, { kind: "trackGain", trackId: "t1" })).toBe(true);
    expect(pageHasTarget(page, { kind: "trackGain", trackId: "t3" })).toBe(false);
  });

  it("add all clips inserts a list and a pad grid, not duplicates", () => {
    const once = addRecipe(emptyLayout(), "clips", ctx);
    const twice = addRecipe(once, "clips", ctx);
    const page = activePage(twice);
    expect(page.widgets.filter((w) => w.kind === "clipList")).toHaveLength(1);
    expect(page.widgets.filter((w) => w.kind === "padGrid")).toHaveLength(1);
    expect(pageHasTarget(page, { kind: "clipLaunch", clipId: "c1" })).toBe(true);
  });

  it("add all automations skips targets the strips already own", () => {
    const withTracks = addRecipe(emptyLayout(), "tracks", ctx);
    const withAuto = addRecipe(withTracks, "automations", ctx);
    const knobs = activePage(withAuto).widgets.filter((w) => w.kind === "knob");
    // pan for t2 is already on the strip; gain for t1 is the fader; leftover is the plugin param.
    const plugin = knobs.filter((w) => w.target?.kind === "pluginParam");
    expect(plugin).toHaveLength(1);
    expect(plugin[0].label).toContain("Cutoff");
  });

  it("add all is tracks then clips then automations", () => {
    const layout = addRecipe(emptyLayout(), "all", ctx);
    const page = activePage(layout);
    expect(page.widgets.some((w) => w.kind === "fader")).toBe(true);
    expect(page.widgets.some((w) => w.kind === "padGrid")).toBe(true);
    expect(page.widgets.some((w) => w.kind === "clipList")).toBe(true);
    expect(page.widgets.some((w) => w.target?.kind === "pluginParam")).toBe(true);
  });
});

describe("templates", () => {
  it("LPD8 stamps 8 knobs and a 2×4 pad grid", () => {
    const layout = applyTemplate(emptyLayout(), "lpd8", ctx);
    const page = activePage(layout);
    expect(page.templateId).toBe("lpd8");
    expect(page.widgets.filter((w) => w.kind === "knob")).toHaveLength(8);
    const grid = page.widgets.find((w) => w.kind === "padGrid");
    expect(grid?.cols).toBe(4);
    expect(grid?.rows).toBe(2);
    expect(grid?.cells?.[padIndex(4, 2, 0, 1)]).toBe("c1");
  });

  it("mixer is the tracks recipe on a fresh page", () => {
    const dirty = addWidget(emptyLayout(), unboundWidget("pad"));
    const layout = applyTemplate(dirty, "mixer", ctx);
    const page = activePage(layout);
    expect(page.templateId).toBe("mixer");
    expect(page.widgets.some((w) => w.kind === "pad")).toBe(false);
    expect(page.widgets.filter((w) => w.kind === "fader")).toHaveLength(2);
  });

  it("blank clears the page", () => {
    const dirty = addRecipe(emptyLayout(), "all", ctx);
    const layout = applyTemplate(dirty, "blank", ctx);
    expect(activePage(layout).widgets).toEqual([]);
    expect(activePage(layout).templateId).toBe("blank");
  });
});

describe("remove and bind", () => {
  it("removeWidget drops one control", () => {
    const layout = addWidget(emptyLayout(), unboundWidget("knob", { label: "X" }));
    const id = activePage(layout).widgets[0].id;
    expect(activePage(removeWidget(layout, id)).widgets).toEqual([]);
  });

  it("removeGroup drops a whole strip", () => {
    const layout = addStrip(emptyLayout(), ctx.tracks[0]);
    const groupId = activePage(layout).widgets[0].groupId!;
    expect(activePage(removeGroup(layout, groupId)).widgets).toEqual([]);
  });

  it("bindWidget rewires a target", () => {
    const layout = addWidget(emptyLayout(), unboundWidget("knob"));
    const id = activePage(layout).widgets[0].id;
    const next = bindWidget(layout, id, { kind: "trackGain", trackId: "t1" }, "DRUMS");
    expect(activePage(next).widgets[0].target).toEqual({ kind: "trackGain", trackId: "t1" });
    expect(activePage(next).widgets[0].label).toBe("DRUMS");
  });

  it("setPadMode flips momentary/toggle", () => {
    const layout = addWidget(emptyLayout(), unboundWidget("pad"));
    const id = activePage(layout).widgets[0].id;
    const next = setPadMode(layout, id, "toggle");
    expect(activePage(next).widgets[0].padMode).toBe("toggle");
  });

  it("setGridCell writes a clip into a pad", () => {
    const layout = addWidget(emptyLayout(), padGridForClips([]));
    const id = activePage(layout).widgets[0].id;
    const next = setGridCell(layout, id, padIndex(4, 2, 0, 1), "c1");
    expect(activePage(next).widgets[0].cells?.[0]).toBe("c1");
  });
});

describe("parse and storage", () => {
  it("round-trips a populated layout", () => {
    const layout = addRecipe(emptyLayout(), "all", ctx);
    const parsed = parseLayout(JSON.parse(JSON.stringify(layout)));
    expect(parsed).not.toBeNull();
    expect(parsed!.pages[0].widgets.length).toBe(layout.pages[0].widgets.length);
  });

  it("rejects a wrong version or an empty page list", () => {
    expect(parseLayout({ version: 2, pages: [], activePageId: "x" })).toBeNull();
    expect(parseLayout({ version: 1, pages: [], activePageId: "x" })).toBeNull();
    expect(parseLayout(null)).toBeNull();
  });

  it("falls back to the first page when activePageId is stale", () => {
    const layout = emptyLayout();
    const raw: SurfaceLayout = { ...layout, activePageId: "gone" };
    const parsed = parseLayout(raw);
    expect(parsed?.activePageId).toBe(layout.pages[0].id);
  });

  it("keys storage by project dir so two songs do not share a deck", () => {
    expect(storageKey("/tmp/song.aura")).toBe("aura.surface.v1:/tmp/song.aura");
    expect(storageKey(null)).toBe("aura.surface.v1:session");
  });
});

describe("meter track resolution", () => {
  it("follows a clip pad to its track", () => {
    const pad = unboundWidget("pad", { target: { kind: "clipLaunch", clipId: "c1" } });
    expect(meterTrackId(pad, ctx.midiClips)).toBe("t1");
  });

  it("returns the track id on mix targets", () => {
    const fader = unboundWidget("fader", { target: { kind: "trackGain", trackId: "t2" } });
    expect(meterTrackId(fader, ctx.midiClips)).toBe("t2");
  });
});

describe("add menu", () => {
  it("lists widgets, recipes and templates without overlapping ids", () => {
    const ids = ADD_MENU.map((i) => i.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ADD_MENU.filter((i) => i.group === "recipe").map((i) => i.id)).toEqual([
      "recipe:all",
      "recipe:tracks",
      "recipe:clips",
      "recipe:automations",
    ]);
    expect(ids).toContain("template:lpd8");
  });
});

describe("groupWidgets", () => {
  it("bundles a strip and leaves free widgets loose", () => {
    let layout = addStrip(emptyLayout(), ctx.tracks[0]);
    layout = addWidget(layout, unboundWidget("pad"));
    const grouped = groupWidgets(activePage(layout));
    expect(grouped.strips).toHaveLength(1);
    expect(grouped.strips[0]).toHaveLength(6);
    expect(grouped.loose).toHaveLength(1);
    expect(grouped.loose[0].kind).toBe("pad");
  });
});

describe("target keys", () => {
  it("distinguishes two params on the same plugin", () => {
    expect(targetKey({ kind: "pluginParam", instanceId: "p1", paramId: 1 })).not.toBe(
      targetKey({ kind: "pluginParam", instanceId: "p1", paramId: 2 }),
    );
  });
});
