import { describe, expect, it } from "vitest";
import {
  ADD_MENU,
  activePage,
  addRecipe,
  addRack,
  addStrip,
  addWidget,
  bindOptions,
  bindWidget,
  bindable,
  cellOptions,
  clearPage,
  DEVICE_RACKS,
  deviceById,
  emptyLayout,
  gridSizeForClips,
  groupWidgets,
  meterTrackId,
  mixableTracks,
  padGridForClips,
  padIndex,
  pageHasStrip,
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
  type SurfacePage,
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
  launchBindings: [
    { id: "lb1", name: "Scene 1" },
    { id: "lb2", name: "Scene 2" },
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
    expect(layout.version).toBe(3);
    expect(layout.pages).toHaveLength(1);
    expect(layout.activePageId).toBe(layout.pages[0].id);
    expect(activePage(layout).widgets).toEqual([]);
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
    expect(grid.cells?.[padIndex(4, 2, 0, 1)]).toEqual({ kind: "clipLaunch", clipId: "c1" });
    expect(grid.cells?.[padIndex(4, 2, 1, 1)]).toEqual({ kind: "clipLaunch", clipId: "c2" });
    expect(grid.cells?.[padIndex(4, 2, 2, 1)]).toEqual({ kind: "clipLaunch", clipId: "c3" });
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

  it("still gives a track a strip when a rack knob already drives its gain", () => {
    const withRack = addRack(emptyLayout(), deviceById("lpd8")!, ctx);
    const layout = addStrip(withRack, ctx.tracks[0]);
    expect(pageHasStrip(activePage(layout), "t1")).toBe(true);
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

describe("racks", () => {
  const lpd8 = deviceById("lpd8")!;

  it("appends a faceplate instead of replacing the deck", () => {
    const withStrip = addStrip(emptyLayout(), ctx.tracks[0]);
    const before = activePage(withStrip).widgets.length;
    const layout = addRack(withStrip, lpd8, ctx);
    const page = activePage(layout);
    // The reported defect: `+` destroyed whatever was already on the page.
    expect(page.widgets.length).toBeGreaterThan(before);
    expect(pageHasTarget(page, { kind: "trackGain", trackId: "t1" })).toBe(true);
    expect(page.widgets.filter((w) => w.kind === "lamp")).toHaveLength(3);
  });

  it("stamps 8 knobs and a 4x2 pad block into one group", () => {
    const page = activePage(addRack(emptyLayout(), lpd8, ctx));
    const rack = page.widgets.filter((w) => w.deviceId === "lpd8");
    expect(rack).toHaveLength(9);
    expect(new Set(rack.map((w) => w.groupId)).size).toBe(1);
    expect(rack[0].groupId?.startsWith("rack:")).toBe(true);
    expect(rack.filter((w) => w.kind === "knob")).toHaveLength(8);
    const grid = rack.find((w) => w.kind === "padGrid");
    expect(grid?.cols).toBe(4);
    expect(grid?.rows).toBe(2);
    expect(grid?.cells?.[padIndex(4, 2, 0, 1)]).toEqual({ kind: "clipLaunch", clipId: "c1" });
  });

  it("gives the knobs the tracks and the pads the clips, in order", () => {
    const page = activePage(addRack(emptyLayout(), lpd8, ctx));
    const knobs = page.widgets.filter((w) => w.kind === "knob");
    expect(knobs[0].target).toEqual({ kind: "trackGain", trackId: "t1" });
    expect(knobs[0].label).toBe("Drums");
    // No third mixable track, so the silkscreen stays on the empty knobs.
    expect(knobs[2].target).toBeNull();
    expect(knobs[2].label).toBe("K3");
  });

  it("puts two racks side by side, each on its own group", () => {
    const layout = addRack(addRack(emptyLayout(), lpd8, ctx), lpd8, ctx);
    const groups = new Set(activePage(layout).widgets.map((w) => w.groupId));
    expect(groups.size).toBe(2);
  });

  it("removes one rack without touching the other", () => {
    const layout = addRack(addRack(emptyLayout(), lpd8, ctx), lpd8, ctx);
    const first = activePage(layout).widgets[0].groupId!;
    const page = activePage(removeGroup(layout, first));
    expect(page.widgets.some((w) => w.groupId === first)).toBe(false);
    expect(page.widgets).toHaveLength(9);
  });

  it("reaches for the next tracks and clips rather than shadowing the first rack", () => {
    const second = activePage(addRack(addRack(emptyLayout(), lpd8, ctx), lpd8, ctx))
      .widgets.slice(9);
    expect(second.filter((w) => w.kind === "knob").every((w) => w.target === null)).toBe(true);
    expect(second.find((w) => w.kind === "padGrid")?.cells?.every((c) => c === null)).toBe(true);
  });

  it("lays a fader device out as faders on level and knobs on pan", () => {
    const mcu = deviceById("mcu")!;
    const rack = activePage(addRack(emptyLayout(), mcu, ctx)).widgets;
    expect(rack.filter((w) => w.kind === "fader")).toHaveLength(8);
    expect(rack.find((w) => w.kind === "fader")?.target).toEqual({ kind: "trackGain", trackId: "t1" });
    expect(rack.find((w) => w.kind === "knob")?.target).toEqual({ kind: "trackPan", trackId: "t1" });
    expect(rack.some((w) => w.kind === "padGrid")).toBe(false);
  });

  it("describes every device as data, not as code", () => {
    const ids = DEVICE_RACKS.map((d) => d.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids).toContain("lpd8");
    for (const d of DEVICE_RACKS) {
      expect(d.name.length).toBeGreaterThan(0);
      expect(d.knobs + d.faders + d.padCols * d.padRows).toBeGreaterThan(0);
      if (d.knobs > 0) expect(d.knobCols).toBeGreaterThan(0);
    }
    expect(deviceById("nope")).toBeUndefined();
  });

  it("clears the page as an explicit action", () => {
    const dirty = addRecipe(emptyLayout(), "all", ctx);
    expect(activePage(clearPage(dirty)).widgets).toEqual([]);
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

  it("setGridCell writes a target into a pad", () => {
    const layout = addWidget(emptyLayout(), padGridForClips([]));
    const id = activePage(layout).widgets[0].id;
    const next = setGridCell(layout, id, padIndex(4, 2, 0, 1), { kind: "clipLaunch", clipId: "c1" });
    expect(activePage(next).widgets[0].cells?.[0]).toEqual({ kind: "clipLaunch", clipId: "c1" });
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
    expect(parseLayout({ version: 4, pages: [], activePageId: "x" })).toBeNull();
    expect(parseLayout({ version: 0, pages: [], activePageId: "x" })).toBeNull();
    expect(parseLayout(null)).toBeNull();
  });

  it("opens a saved v1 lpd8 deck as a rack, and migrates its bare-string cells too", () => {
    const knob = { id: "w1", kind: "knob", label: "Drums", target: { kind: "trackGain", trackId: "t1" } };
    const grid = {
      id: "w2",
      kind: "padGrid",
      label: "PADS",
      target: null,
      cols: 4,
      rows: 2,
      cells: ["c1", null, null, null, null, null, null, null],
    };
    const parsed = parseLayout({
      version: 1,
      activePageId: "p1",
      pages: [{ id: "p1", name: "Deck", templateId: "lpd8", widgets: [knob, grid] }],
    });
    expect(parsed?.version).toBe(3);
    const grouped = groupWidgets(parsed!.pages[0]);
    expect(grouped.racks).toHaveLength(1);
    expect(grouped.racks[0].device.id).toBe("lpd8");
    expect(grouped.racks[0].widgets.map((w) => w.id)).toEqual(["w1", "w2"]);
    expect(grouped.loose).toHaveLength(0);
    // migrateV1Page only wraps widgets in a rack group; it never touches
    // cells, so the v1→v3 cell migration must run on this path too.
    const migratedGrid = grouped.racks[0].widgets.find((w) => w.id === "w2");
    expect(migratedGrid?.cells?.[0]).toEqual({ kind: "clipLaunch", clipId: "c1" });
    expect(migratedGrid?.cells?.[1]).toBeNull();
  });

  it("opens a saved v2 deck and turns a bare-string cell into a clipLaunch target", () => {
    const grid = {
      id: "w1",
      kind: "padGrid",
      label: "PADS",
      target: null,
      cols: 4,
      rows: 2,
      cells: ["c1", null, null, null, null, null, null, null],
    };
    const parsed = parseLayout({
      version: 2,
      activePageId: "p1",
      pages: [{ id: "p1", name: "Deck", widgets: [grid] }],
    });
    expect(parsed?.version).toBe(3);
    const cells = parsed!.pages[0].widgets[0].cells;
    expect(cells?.[0]).toEqual({ kind: "clipLaunch", clipId: "c1" });
    expect(cells?.[1]).toBeNull();
  });

  it("leaves an already-migrated v3 cell alone", () => {
    const grid = {
      id: "w1",
      kind: "padGrid",
      label: "PADS",
      target: null,
      cols: 4,
      rows: 2,
      cells: [{ kind: "player", playerId: "p1" }, null],
    };
    const parsed = parseLayout({
      version: 3,
      activePageId: "p1",
      pages: [{ id: "p1", name: "Deck", widgets: [grid] }],
    });
    expect(parsed?.pages[0].widgets[0].cells?.[0]).toEqual({ kind: "player", playerId: "p1" });
  });

  it("leaves a v1 deck that was never a device alone", () => {
    const knob = { id: "w1", kind: "knob", label: "K1", target: null };
    const parsed = parseLayout({
      version: 1,
      activePageId: "p1",
      pages: [{ id: "p1", name: "Deck", templateId: "custom", widgets: [knob] }],
    });
    expect(groupWidgets(parsed!.pages[0]).loose).toHaveLength(1);
    expect(groupWidgets(parsed!.pages[0]).racks).toHaveLength(0);
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
    expect(ids).toContain("rack:lpd8");
    expect(ids).toContain("clear");
    // Every device is a menu row, so a new one is a row of data.
    for (const d of DEVICE_RACKS) expect(ids).toContain(`rack:${d.id}`);
    expect(ids.some((i) => i.startsWith("template:"))).toBe(false);
  });
});

describe("groupWidgets", () => {
  it("bundles a strip and leaves free widgets loose", () => {
    let layout = addStrip(emptyLayout(), ctx.tracks[0]);
    layout = addWidget(layout, unboundWidget("pad"));
    const grouped = groupWidgets(activePage(layout));
    expect(grouped.racks).toHaveLength(0);
    expect(grouped.strips).toHaveLength(1);
    expect(grouped.strips[0]).toHaveLength(6);
    expect(grouped.loose).toHaveLength(1);
    expect(grouped.loose[0].kind).toBe("pad");
  });

  it("keeps a rack out of the strip bucket and carries its device along", () => {
    let layout = addStrip(emptyLayout(), ctx.tracks[0]);
    layout = addRack(layout, deviceById("lpd8")!, ctx);
    const grouped = groupWidgets(activePage(layout));
    expect(grouped.racks).toHaveLength(1);
    expect(grouped.racks[0].device.id).toBe("lpd8");
    expect(grouped.racks[0].widgets).toHaveLength(9);
    expect(grouped.strips).toHaveLength(1);
    expect(grouped.loose).toHaveLength(0);
  });

  it("shows a rack whose device this build no longer knows as loose controls", () => {
    const page: SurfacePage = {
      id: "p",
      name: "Deck",
      widgets: [unboundWidget("knob", { groupId: "rack:x", deviceId: "gone" as never })],
    };
    const grouped = groupWidgets(page);
    expect(grouped.racks).toHaveLength(0);
    expect(grouped.loose).toHaveLength(1);
  });
});

describe("target keys", () => {
  it("distinguishes two params on the same plugin", () => {
    expect(targetKey({ kind: "pluginParam", instanceId: "p1", paramId: 1 })).not.toBe(
      targetKey({ kind: "pluginParam", instanceId: "p1", paramId: 2 }),
    );
  });
});

describe("player targets", () => {
  it("gives a player target a stable key distinct from a clip's", () => {
    expect(targetKey({ kind: "player", playerId: "p1" })).toBe("player:p1");
    expect(targetKey({ kind: "player", playerId: "p1" })).not.toBe(
      targetKey({ kind: "clipLaunch", clipId: "p1" }),
    );
  });

  it("accepts a player target on a pad", () => {
    // Brief's snippet calls `blankLayout()` / `addWidget(layout, "pad")`;
    // neither exists on this branch's API (`emptyLayout()` +
    // `addWidget(layout, widget)`, per the neighbouring tests in this
    // file). Using the real signatures — same assertion.
    const layout = addWidget(emptyLayout(), unboundWidget("pad"));
    const pad = layout.pages[0].widgets[0];
    const bound = bindWidget(layout, pad.id, { kind: "player", playerId: "p1" });
    expect(bound.pages[0].widgets[0].target).toEqual({ kind: "player", playerId: "p1" });
  });
});

describe("bind options", () => {
  it("offers levels, pans and automated plugin params to a knob", () => {
    const opts = bindOptions("knob", ctx);
    // automation tracks are not mixable, so t3 is absent
    expect(opts.filter((o) => o.group === "LEVEL").map((o) => o.target)).toEqual([
      { kind: "trackGain", trackId: "t1" },
      { kind: "trackGain", trackId: "t2" },
    ]);
    expect(opts.filter((o) => o.group === "PAN")).toHaveLength(2);
    const param = opts.find((o) => o.group === "PLUGIN PARAM");
    expect(param?.target).toEqual({ kind: "pluginParam", instanceId: "p1", paramId: 7 });
    expect(param?.widgetLabel).toBe("Cutoff");
  });

  it("does not offer the same target twice when it is both automated and cached", () => {
    const withCache: SurfaceContext = {
      ...ctx,
      pluginParams: [
        { instanceId: "p1", instanceName: "Filter", paramId: 7, paramName: "Cutoff" },
        { instanceId: "p1", instanceName: "Filter", paramId: 9, paramName: "Res" },
      ],
    };
    const params = bindOptions("fader", withCache).filter((o) => o.group === "PLUGIN PARAM");
    expect(params).toHaveLength(2);
    expect(params.map((o) => o.key)).toEqual(["pluginParam:p1:7", "pluginParam:p1:9"]);
  });

  it("gives a lamp the role that goes with its target", () => {
    const opts = bindOptions("lamp", ctx);
    const solo = opts.find((o) => o.target.kind === "trackSolo");
    expect(solo?.lampRole).toBe("solo");
    expect(solo?.widgetLabel).toBe("SOLO");
    expect(new Set(opts.map((o) => o.group))).toEqual(new Set(["MUTE", "SOLO", "ARM"]));
  });

  it("has nothing to offer the widgets that read the whole project", () => {
    expect(bindable("clipList")).toBe(false);
    expect(bindable("padGrid")).toBe(false);
    expect(bindOptions("clipList", ctx)).toEqual([]);
    expect(bindOptions("padGrid", ctx)).toEqual([]);
  });

  it("gives a pad-grid cell the same option set a loose pad gets — clip, launcher and mute", () => {
    // A cell holds a full SurfaceTarget now, the same union a loose pad's
    // `target` holds, so it is no longer clip-only.
    expect(cellOptions(ctx)).toEqual(bindOptions("pad", ctx));
    const kinds = new Set(cellOptions(ctx).map((o) => o.target.kind));
    expect(kinds).toEqual(new Set(["clipLaunch", "launchBinding", "trackMute"]));
  });
});

describe("bindWidget", () => {
  it("carries the rest of the patch, so a lamp gets its role", () => {
    const lamp = unboundWidget("lamp");
    const layout = bindWidget(addWidget(emptyLayout(), lamp), lamp.id, { kind: "trackArm", trackId: "t1" }, "ARM", {
      lampRole: "arm",
    });
    const w = activePage(layout).widgets[0];
    expect(w.target).toEqual({ kind: "trackArm", trackId: "t1" });
    expect(w.lampRole).toBe("arm");
    expect(w.label).toBe("ARM");
  });
});

describe("launcher bindings on a pad", () => {
  it("offers every launch binding, keyed apart from a clip of the same id", () => {
    const opts = bindOptions("pad", ctx);
    const launcher = opts.filter((o) => o.target.kind === "launchBinding");
    expect(launcher.map((o) => o.label)).toEqual(["Scene 1", "Scene 2"]);
    expect(launcher[0].key).toBe("launchBinding:lb1");
    expect(targetKey({ kind: "launchBinding", bindingId: "x" })).not.toBe(
      targetKey({ kind: "clipLaunch", clipId: "x" }),
    );
  });

  it("offers every launch binding to a pad-grid cell too", () => {
    const launcher = cellOptions(ctx).filter((o) => o.target.kind === "launchBinding");
    expect(launcher.map((o) => o.label)).toEqual(["Scene 1", "Scene 2"]);
  });
});
