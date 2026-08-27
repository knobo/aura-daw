/**
 * Control-surface layout algebra: widget kinds, targets, add-all recipes,
 * and hardware-homage racks. Pure TypeScript (no runes, no DOM, no
 * stores) so the panel, the tests and a later "give me an LPD8" command
 * share one rule set.
 *
 * The layout is chrome (ADR 0006). Bindings point at existing document
 * ids; they do not own mix or launch state.
 */

export const SURFACE_LAYOUT_VERSION = 2 as const;

/** v1 kept a device as a page mode (`page.templateId`); v2 makes it a group. */
const LEGACY_LAYOUT_VERSION = 1;

/** Widgets stamped by one device share a `rack:<id>` group. */
export const RACK_GROUP_PREFIX = "rack:";

export type SurfaceWidgetKind =
  | "knob"
  | "fader"
  | "gauge"
  | "pad"
  | "lamp"
  | "clipList"
  | "padGrid";

export type LampRole = "mute" | "solo" | "arm";

export type PadMode = "momentary" | "toggle";

export type SurfaceDeviceId = "lpd8" | "launchpad" | "mcu" | "nanokontrol";

/**
 * A hardware homage, described as data. Everything the faceplate needs to
 * lay itself out is here, so adding a device is a row in `DEVICE_RACKS`
 * rather than a branch in a component. This is the shape Plan V's V8
 * hardware map binds to.
 */
export interface SurfaceDevice {
  id: SurfaceDeviceId;
  /** Silkscreen on the faceplate; also the row in the `+` menu. */
  name: string;
  hint: string;
  /** Mode buttons down the left edge. Silkscreen only — no behaviour yet. */
  modes: readonly string[];
  knobs: number;
  /** Knobs are laid out in this many columns (the LPD8's eight sit 4×2). */
  knobCols: number;
  /** What a knob drives. A device with faders puts level on the fader. */
  knobParam: "gain" | "pan";
  faders: number;
  padCols: number;
  padRows: number;
}

export const DEVICE_RACKS: readonly SurfaceDevice[] = [
  {
    id: "lpd8",
    name: "AKAI LPD8",
    hint: "8 knobs, 4×2 pads, homage faceplate",
    modes: ["PROG", "PAD", "CC", "NOTE"],
    knobs: 8,
    knobCols: 4,
    knobParam: "gain",
    faders: 0,
    padCols: 4,
    padRows: 2,
  },
  {
    id: "launchpad",
    name: "Launchpad 8×8",
    hint: "64 clip pads, no pots",
    modes: ["SESSION", "USER", "MIXER"],
    knobs: 0,
    knobCols: 0,
    knobParam: "gain",
    faders: 0,
    padCols: 8,
    padRows: 8,
  },
  {
    id: "mcu",
    name: "MCU 8-strip",
    hint: "8 faders on level, 8 pots on pan",
    modes: ["TRACK", "PAN", "SEND"],
    knobs: 8,
    knobCols: 8,
    knobParam: "pan",
    faders: 8,
    padCols: 0,
    padRows: 0,
  },
  {
    id: "nanokontrol",
    name: "nanoKONTROL",
    hint: "8 compact faders and pots",
    modes: ["CYCLE", "SET"],
    knobs: 8,
    knobCols: 8,
    knobParam: "pan",
    faders: 8,
    padCols: 0,
    padRows: 0,
  },
];

export function deviceById(id: string | null | undefined): SurfaceDevice | undefined {
  return DEVICE_RACKS.find((d) => d.id === id);
}

export type AddRecipe = "all" | "tracks" | "clips" | "automations";

export type SurfaceTarget =
  | { kind: "trackGain"; trackId: string }
  | { kind: "trackPan"; trackId: string }
  | { kind: "trackMute"; trackId: string }
  | { kind: "trackSolo"; trackId: string }
  | { kind: "trackArm"; trackId: string }
  | { kind: "meter"; trackId: string }
  | { kind: "clipLaunch"; clipId: string }
  | { kind: "launchBinding"; bindingId: string }
  | { kind: "pluginParam"; instanceId: string; paramId: number };

export interface SurfaceWidget {
  id: string;
  kind: SurfaceWidgetKind;
  label: string;
  target: SurfaceTarget | null;
  /** Shared by the widgets a channel strip or a rack inserts. */
  groupId?: string;
  /** Set on every widget of a rack: which faceplate draws it. */
  deviceId?: SurfaceDeviceId;
  padMode?: PadMode;
  lampRole?: LampRole;
  cols?: number;
  rows?: number;
  /** Pad-grid cells: clip ids (or null = empty). Length = cols * rows. */
  cells?: Array<string | null>;
}

export interface SurfacePage {
  id: string;
  name: string;
  widgets: SurfaceWidget[];
}

export interface SurfaceLayout {
  version: typeof SURFACE_LAYOUT_VERSION;
  pages: SurfacePage[];
  activePageId: string;
}

export interface SurfaceTrack {
  id: string;
  name: string;
  kind: string;
  color: string;
}

export interface SurfaceClip {
  id: string;
  name: string;
  trackId: string;
}

export interface SurfaceAutomation {
  id: string;
  trackId: string;
  trackName: string;
  paramLabel: string;
  target: SurfaceTarget;
}

/** One bindable plugin parameter, from the params the panel has cached. */
export interface SurfacePluginParam {
  instanceId: string;
  instanceName: string;
  paramId: number;
  paramName: string;
}

export interface SurfaceContext {
  tracks: readonly SurfaceTrack[];
  midiClips: readonly SurfaceClip[];
  automations: readonly SurfaceAutomation[];
  /** Optional: only instances whose params have been read are bindable. */
  pluginParams?: readonly SurfacePluginParam[];
}

export type AddMenuId =
  | SurfaceWidgetKind
  | "strip"
  | "clear"
  | `recipe:${AddRecipe}`
  | `rack:${SurfaceDeviceId}`;

export interface AddMenuItem {
  id: AddMenuId;
  label: string;
  hint: string;
  group: "widget" | "recipe" | "rack" | "page";
}

/** The `+` dropdown, in display order. */
export const ADD_MENU: readonly AddMenuItem[] = [
  { id: "strip", label: "Channel strip", hint: "Mute, solo, arm, gauge, gain, pan", group: "widget" },
  { id: "knob", label: "Knob", hint: "Rotary for gain, pan or a plugin param", group: "widget" },
  { id: "fader", label: "Fader", hint: "Vertical level", group: "widget" },
  { id: "gauge", label: "Gauge", hint: "Analog VU for a track", group: "widget" },
  { id: "pad", label: "Pad", hint: "Rubber pad — fire a clip or toggle", group: "widget" },
  { id: "lamp", label: "Lamp", hint: "Mute, solo or arm", group: "widget" },
  { id: "clipList", label: "Clip list", hint: "Named MIDI clips, one-press play", group: "widget" },
  { id: "padGrid", label: "Pad grid", hint: "N×M pads, LPD8-style", group: "widget" },
  { id: "recipe:all", label: "Add all", hint: "Tracks, clips and automations", group: "recipe" },
  { id: "recipe:tracks", label: "Add all tracks", hint: "A strip per track", group: "recipe" },
  { id: "recipe:clips", label: "Add all clips", hint: "Clip list + pad grid", group: "recipe" },
  { id: "recipe:automations", label: "Add all automations", hint: "A knob per moving param", group: "recipe" },
  ...DEVICE_RACKS.map(
    (d): AddMenuItem => ({ id: `rack:${d.id}`, label: d.name, hint: d.hint, group: "rack" }),
  ),
  { id: "clear", label: "Clear page", hint: "Remove every widget on this deck", group: "page" },
];

export function newId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `surf-${Math.random().toString(36).slice(2, 10)}-${Date.now().toString(36)}`;
}

export function emptyLayout(): SurfaceLayout {
  const page = emptyPage("Deck");
  return { version: SURFACE_LAYOUT_VERSION, pages: [page], activePageId: page.id };
}

export function emptyPage(name = "Deck"): SurfacePage {
  return { id: newId(), name, widgets: [] };
}

export function activePage(layout: SurfaceLayout): SurfacePage {
  return layout.pages.find((p) => p.id === layout.activePageId) ?? layout.pages[0];
}

/** Stable key so add-all can skip targets already on the page. */
export function targetKey(target: SurfaceTarget | null | undefined): string | null {
  if (!target) return null;
  switch (target.kind) {
    case "trackGain":
    case "trackPan":
    case "trackMute":
    case "trackSolo":
    case "trackArm":
    case "meter":
      return `${target.kind}:${target.trackId}`;
    case "clipLaunch":
      return `clipLaunch:${target.clipId}`;
    case "launchBinding":
      return `launchBinding:${target.bindingId}`;
    case "pluginParam":
      return `pluginParam:${target.instanceId}:${target.paramId}`;
  }
}

export function pageHasTarget(page: SurfacePage, target: SurfaceTarget): boolean {
  const key = targetKey(target);
  if (!key) return false;
  for (const w of page.widgets) {
    if (targetKey(w.target) === key) return true;
    if (w.kind === "padGrid" && target.kind === "clipLaunch") {
      if (w.cells?.includes(target.clipId)) return true;
    }
  }
  return false;
}

export function mixableTracks(tracks: readonly SurfaceTrack[]): SurfaceTrack[] {
  return tracks.filter((t) => t.kind !== "automation");
}

/**
 * LPD8 pad numbering: bottom row is 1–4 left to right, top row is 5–8.
 * A 4×4 grid extends the same origin (bottom-left, row-major upward).
 */
export function padIndex(cols: number, rows: number, col: number, rowFromTop: number): number {
  const rowFromBottom = rows - 1 - rowFromTop;
  return rowFromBottom * cols + col;
}

export function gridSizeForClips(n: number): { cols: number; rows: number } {
  if (n <= 8) return { cols: 4, rows: 2 };
  if (n <= 16) return { cols: 4, rows: 4 };
  return { cols: 8, rows: 8 };
}

export function stripWidgets(track: SurfaceTrack): SurfaceWidget[] {
  const groupId = `strip:${track.id}`;
  return [
    lampWidget(track, "mute", groupId),
    lampWidget(track, "solo", groupId),
    lampWidget(track, "arm", groupId),
    {
      id: newId(),
      kind: "gauge",
      label: track.name,
      target: { kind: "meter", trackId: track.id },
      groupId,
    },
    {
      id: newId(),
      kind: "fader",
      label: "LEVEL",
      target: { kind: "trackGain", trackId: track.id },
      groupId,
    },
    {
      id: newId(),
      kind: "knob",
      label: "PAN",
      target: { kind: "trackPan", trackId: track.id },
      groupId,
    },
  ];
}

function lampWidget(track: SurfaceTrack, role: LampRole, groupId: string): SurfaceWidget {
  const kind =
    role === "mute" ? "trackMute" : role === "solo" ? "trackSolo" : "trackArm";
  return {
    id: newId(),
    kind: "lamp",
    label: role.toUpperCase(),
    target: { kind, trackId: track.id },
    groupId,
    lampRole: role,
    padMode: "toggle",
  };
}

export function unboundWidget(kind: SurfaceWidgetKind, extra: Partial<SurfaceWidget> = {}): SurfaceWidget {
  const defaults: Pick<SurfaceWidget, "label" | "padMode" | "lampRole" | "cols" | "rows" | "cells"> =
    kind === "pad"
      ? { label: "PAD", padMode: "momentary" }
      : kind === "lamp"
        ? { label: "MUTE", lampRole: "mute", padMode: "toggle" }
        : kind === "padGrid"
          ? { label: "PADS", cols: 4, rows: 2, cells: Array(8).fill(null) }
          : kind === "clipList"
            ? { label: "CLIPS" }
            : kind === "gauge"
              ? { label: "VU" }
              : kind === "fader"
                ? { label: "LEVEL" }
                : { label: "KNOB" };
  return { id: newId(), kind, target: extra.target ?? null, ...defaults, ...extra };
}

export function padGridForClips(clips: readonly SurfaceClip[]): SurfaceWidget {
  const { cols, rows } = gridSizeForClips(clips.length);
  return padGrid(cols, rows, clips);
}

/** A grid of a fixed size, filled from the bottom-left pad upward. */
function padGrid(cols: number, rows: number, clips: readonly SurfaceClip[]): SurfaceWidget {
  const cells: Array<string | null> = Array(cols * rows).fill(null);
  for (let i = 0; i < clips.length && i < cells.length; i++) {
    const col = i % cols;
    const rowFromBottom = Math.floor(i / cols);
    const rowFromTop = rows - 1 - rowFromBottom;
    cells[padIndex(cols, rows, col, rowFromTop)] = clips[i].id;
  }
  return { id: newId(), kind: "padGrid", label: "PADS", target: null, cols, rows, cells };
}

function replaceActive(layout: SurfaceLayout, widgets: SurfaceWidget[]): SurfaceLayout {
  const page = activePage(layout);
  return {
    ...layout,
    pages: layout.pages.map((p) => (p.id === page.id ? { ...page, widgets } : p)),
  };
}

function appendActive(layout: SurfaceLayout, extra: SurfaceWidget[]): SurfaceLayout {
  if (extra.length === 0) return layout;
  return replaceActive(layout, [...activePage(layout).widgets, ...extra]);
}

export function addWidget(layout: SurfaceLayout, widget: SurfaceWidget): SurfaceLayout {
  return appendActive(layout, [widget]);
}

/** Is this track's channel strip already on the page? A rack knob pointed
 * at the same gain is not a strip — the two coexist happily. */
export function pageHasStrip(page: SurfacePage, trackId: string): boolean {
  return page.widgets.some((w) => w.groupId === `strip:${trackId}`);
}

export function addStrip(layout: SurfaceLayout, track: SurfaceTrack): SurfaceLayout {
  const page = activePage(layout);
  if (pageHasStrip(page, track.id)) return layout;
  return appendActive(layout, stripWidgets(track));
}

export function addRecipe(layout: SurfaceLayout, recipe: AddRecipe, ctx: SurfaceContext): SurfaceLayout {
  if (recipe === "all") {
    return addRecipe(addRecipe(addRecipe(layout, "tracks", ctx), "clips", ctx), "automations", ctx);
  }
  if (recipe === "tracks") {
    let next = layout;
    for (const t of mixableTracks(ctx.tracks)) next = addStrip(next, t);
    return next;
  }
  if (recipe === "clips") {
    const page = activePage(layout);
    const extra: SurfaceWidget[] = [];
    if (!page.widgets.some((w) => w.kind === "clipList")) {
      extra.push(unboundWidget("clipList", { label: "CLIPS" }));
    }
    const unbound = ctx.midiClips.filter((c) => !pageHasTarget(page, { kind: "clipLaunch", clipId: c.id }));
    if (unbound.length > 0 && !page.widgets.some((w) => w.kind === "padGrid")) {
      extra.push(padGridForClips(unbound));
    } else if (unbound.length > 0) {
      extra.push(...unbound.map((c) => unboundWidget("pad", {
        label: c.name,
        target: { kind: "clipLaunch", clipId: c.id },
        padMode: "momentary",
      })));
    }
    return appendActive(layout, extra);
  }
  // automations
  const page = activePage(layout);
  const extra: SurfaceWidget[] = [];
  for (const a of ctx.automations) {
    if (pageHasTarget(page, a.target)) continue;
    extra.push({
      id: newId(),
      kind: "knob",
      label: `${a.trackName} · ${a.paramLabel}`,
      target: a.target,
    });
  }
  return appendActive(layout, extra);
}

/**
 * The widgets one device stamps. A rack is an object ON the page, not a
 * page mode: it carries its own group so a second one can sit beside the
 * first and either can be removed on its own. Targets already driven from
 * this page are skipped — a second LPD8 is there to reach the next eight
 * tracks and clips, not to shadow the first one's.
 */
export function rackWidgets(
  device: SurfaceDevice,
  ctx: SurfaceContext,
  page: SurfacePage,
): SurfaceWidget[] {
  const groupId = `${RACK_GROUP_PREFIX}${newId()}`;
  const base = { groupId, deviceId: device.id };
  const freeTracks = mixableTracks(ctx.tracks).filter(
    (t) => !pageHasTarget(page, { kind: "trackGain", trackId: t.id }),
  );
  const out: SurfaceWidget[] = [];

  for (let i = 0; i < device.faders; i++) {
    const t = freeTracks[i];
    out.push({
      ...base,
      id: newId(),
      kind: "fader",
      label: t ? t.name : `F${i + 1}`,
      target: t ? { kind: "trackGain", trackId: t.id } : null,
    });
  }
  for (let i = 0; i < device.knobs; i++) {
    const t = freeTracks[i];
    // Bound knobs wear the track they drive; the silkscreen K1..K8 stays on
    // the ones that are still empty (the faceplate prints it as a ghost).
    const label = !t ? `K${i + 1}` : device.knobParam === "pan" ? "PAN" : t.name;
    out.push({
      ...base,
      id: newId(),
      kind: "knob",
      label,
      target: t
        ? device.knobParam === "pan"
          ? { kind: "trackPan", trackId: t.id }
          : { kind: "trackGain", trackId: t.id }
        : null,
    });
  }
  if (device.padCols > 0 && device.padRows > 0) {
    const freeClips = ctx.midiClips.filter(
      (c) => !pageHasTarget(page, { kind: "clipLaunch", clipId: c.id }),
    );
    out.push({ ...padGrid(device.padCols, device.padRows, freeClips), ...base });
  }
  return out;
}

export function addRack(layout: SurfaceLayout, device: SurfaceDevice, ctx: SurfaceContext): SurfaceLayout {
  return appendActive(layout, rackWidgets(device, ctx, activePage(layout)));
}

/** Emptying the deck is its own menu action, never a side effect of adding. */
export function clearPage(layout: SurfaceLayout): SurfaceLayout {
  return replaceActive(layout, []);
}

export function removeWidget(layout: SurfaceLayout, widgetId: string): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.filter((w) => w.id !== widgetId),
  );
}

/** Remove every widget that shares this group — one strip, or one rack. */
export function removeGroup(layout: SurfaceLayout, groupId: string): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.filter((w) => w.groupId !== groupId),
  );
}

export function bindWidget(
  layout: SurfaceLayout,
  widgetId: string,
  target: SurfaceTarget | null,
  label?: string,
  extra: Partial<SurfaceWidget> = {},
): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.map((w) =>
      w.id === widgetId ? { ...w, ...extra, target, label: label ?? w.label } : w,
    ),
  );
}

export function setPadMode(layout: SurfaceLayout, widgetId: string, padMode: PadMode): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.map((w) => (w.id === widgetId ? { ...w, padMode } : w)),
  );
}

export function setGridCell(layout: SurfaceLayout, widgetId: string, index: number, clipId: string | null): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.map((w) => {
      if (w.id !== widgetId || w.kind !== "padGrid" || !w.cells) return w;
      if (index < 0 || index >= w.cells.length) return w;
      const cells = w.cells.slice();
      cells[index] = clipId;
      return { ...w, cells };
    }),
  );
}

/**
 * What one widget can be pointed at, in menu order. The `+` menu inserts
 * unbound knobs, faders, gauges, pads and lamps; this is the list the bind
 * picker shows for them, so an inserted widget has a way to become useful
 * other than being deleted again.
 */
export interface BindOption {
  /** Stable per-option key — the target key plus the lamp role. */
  key: string;
  /** Heading the option sits under; consecutive equal groups share one. */
  group: string;
  /** Menu row. */
  label: string;
  /** Silkscreen the widget wears once bound. */
  widgetLabel: string;
  target: SurfaceTarget;
  lampRole?: LampRole;
}

/** Kinds a single target can be picked for. Clip lists and pad grids read
 * the whole project instead, so they have nothing to bind. */
export function bindable(kind: SurfaceWidgetKind): boolean {
  return kind === "knob" || kind === "fader" || kind === "gauge" || kind === "pad" || kind === "lamp";
}

export function bindOptions(kind: SurfaceWidgetKind, ctx: SurfaceContext): BindOption[] {
  const tracks = mixableTracks(ctx.tracks);
  const out: BindOption[] = [];
  const add = (group: string, label: string, widgetLabel: string, target: SurfaceTarget, lampRole?: LampRole) => {
    out.push({ key: `${targetKey(target)}${lampRole ? `:${lampRole}` : ""}`, group, label, widgetLabel, target, lampRole });
  };

  if (kind === "knob" || kind === "fader") {
    for (const t of tracks) add("LEVEL", `${t.name} — level`, t.name, { kind: "trackGain", trackId: t.id });
    for (const t of tracks) add("PAN", `${t.name} — pan`, `${t.name} PAN`, { kind: "trackPan", trackId: t.id });
    for (const p of pluginParamOptions(ctx)) out.push(p);
  } else if (kind === "gauge") {
    for (const t of tracks) add("METER", t.name, t.name, { kind: "meter", trackId: t.id });
  } else if (kind === "pad") {
    for (const c of ctx.midiClips) add("CLIP", c.name, c.name, { kind: "clipLaunch", clipId: c.id });
    for (const t of tracks) add("MUTE", `${t.name} — mute`, `${t.name} MUTE`, { kind: "trackMute", trackId: t.id });
  } else if (kind === "lamp") {
    for (const t of tracks) add("MUTE", t.name, "MUTE", { kind: "trackMute", trackId: t.id }, "mute");
    for (const t of tracks) add("SOLO", t.name, "SOLO", { kind: "trackSolo", trackId: t.id }, "solo");
    for (const t of tracks) add("ARM", t.name, "ARM", { kind: "trackArm", trackId: t.id }, "arm");
  }

  const seen = new Set<string>();
  return out.filter((o) => (seen.has(o.key) ? false : (seen.add(o.key), true)));
}

/** Clips a pad-grid cell can hold. Same rows the pad picker shows, without
 * the track-mute alternatives — a grid cell fires a clip or nothing. */
export function cellOptions(ctx: SurfaceContext): BindOption[] {
  return ctx.midiClips.map((c) => ({
    key: `clipLaunch:${c.id}`,
    group: "CLIP",
    label: c.name,
    widgetLabel: c.name,
    target: { kind: "clipLaunch", clipId: c.id } as SurfaceTarget,
  }));
}

/** Plugin params worth offering: the ones already automated (they have a
 * readable label) plus whatever params the panel has cached. */
function pluginParamOptions(ctx: SurfaceContext): BindOption[] {
  const out: BindOption[] = [];
  for (const a of ctx.automations) {
    if (a.target.kind !== "pluginParam") continue;
    out.push({
      key: targetKey(a.target) as string,
      group: "PLUGIN PARAM",
      label: `${a.trackName} — ${a.paramLabel}`,
      widgetLabel: a.paramLabel,
      target: a.target,
    });
  }
  for (const p of ctx.pluginParams ?? []) {
    const target: SurfaceTarget = { kind: "pluginParam", instanceId: p.instanceId, paramId: p.paramId };
    out.push({
      key: targetKey(target) as string,
      group: "PLUGIN PARAM",
      label: `${p.instanceName} — ${p.paramName}`,
      widgetLabel: p.paramName,
      target,
    });
  }
  return out;
}

interface StoredPage {
  id?: unknown;
  name?: unknown;
  /** v1 only: the device this page WAS, back when a device was a page mode. */
  templateId?: unknown;
  widgets?: unknown;
}

export function parseLayout(raw: unknown): SurfaceLayout | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as { version?: unknown; pages?: unknown; activePageId?: unknown };
  const legacy = o.version === LEGACY_LAYOUT_VERSION;
  if (o.version !== SURFACE_LAYOUT_VERSION && !legacy) return null;
  if (!Array.isArray(o.pages) || o.pages.length === 0) return null;
  if (typeof o.activePageId !== "string") return null;
  const pages: SurfacePage[] = [];
  for (const raw of o.pages as StoredPage[]) {
    if (!raw || typeof raw.id !== "string" || typeof raw.name !== "string") return null;
    if (!Array.isArray(raw.widgets)) return null;
    const widgets = raw.widgets.filter(isWidget);
    pages.push({ id: raw.id, name: raw.name, widgets: legacy ? migrateV1Page(raw, widgets) : widgets });
  }
  const activePageId = pages.some((p) => p.id === o.activePageId) ? o.activePageId : pages[0].id;
  return { version: SURFACE_LAYOUT_VERSION, pages, activePageId };
}

/**
 * A v1 page whose `templateId` names a device WAS that device — the mode is
 * why `+` had to replace the deck to stamp one. Wrap its widgets in a rack
 * group and the same saved deck opens as an object on the page. Any other v1
 * page (`custom`, `mixer`, `blank`) was already a plain widget list.
 */
function migrateV1Page(page: StoredPage, widgets: SurfaceWidget[]): SurfaceWidget[] {
  const device = typeof page.templateId === "string" ? deviceById(page.templateId) : undefined;
  if (!device || widgets.length === 0) return widgets;
  const groupId = `${RACK_GROUP_PREFIX}${newId()}`;
  return widgets.map((w) => ({ ...w, groupId, deviceId: device.id }));
}

function isWidget(w: unknown): w is SurfaceWidget {
  if (!w || typeof w !== "object") return false;
  const o = w as SurfaceWidget;
  return typeof o.id === "string" && typeof o.kind === "string" && typeof o.label === "string";
}

/** Track id a widget's LED / gauge should follow, if any. */
export function meterTrackId(widget: SurfaceWidget, clips: readonly SurfaceClip[]): string | null {
  const t = widget.target;
  if (!t) return null;
  if ("trackId" in t) return t.trackId;
  if (t.kind === "clipLaunch") return clips.find((c) => c.id === t.clipId)?.trackId ?? null;
  return null;
}

export function storageKey(projectDir: string | null | undefined): string {
  return `aura.surface.v1:${projectDir && projectDir.length > 0 ? projectDir : "session"}`;
}

/** One device faceplate on the page, with the widgets it stamped. */
export interface SurfaceRack {
  groupId: string;
  device: SurfaceDevice;
  widgets: SurfaceWidget[];
}

export interface GroupedWidgets {
  racks: SurfaceRack[];
  strips: SurfaceWidget[][];
  loose: SurfaceWidget[];
}

/** Racks first, then channel-strip bundles (stable groupId order), then
 * free widgets. */
export function groupWidgets(page: SurfacePage): GroupedWidgets {
  const rackGroups = new Map<string, SurfaceWidget[]>();
  const strips = new Map<string, SurfaceWidget[]>();
  const loose: SurfaceWidget[] = [];
  for (const w of page.widgets) {
    if (!w.groupId) {
      loose.push(w);
      continue;
    }
    const bucket = w.groupId.startsWith(RACK_GROUP_PREFIX) ? rackGroups : strips;
    const g = bucket.get(w.groupId);
    if (g) g.push(w);
    else bucket.set(w.groupId, [w]);
  }
  const racks: SurfaceRack[] = [];
  for (const [groupId, widgets] of rackGroups) {
    const device = deviceById(widgets[0].deviceId);
    // A deck saved against a device this build no longer knows still shows
    // its controls — as loose widgets, never as a hole where a rack was.
    if (device) racks.push({ groupId, device, widgets });
    else loose.push(...widgets);
  }
  return { racks, strips: [...strips.values()], loose };
}
