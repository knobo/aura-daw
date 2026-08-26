/**
 * Control-surface layout algebra: widget kinds, targets, add-all recipes,
 * and hardware-homage templates. Pure TypeScript (no runes, no DOM, no
 * stores) so the panel, the tests and a later "give me an LPD8" command
 * share one rule set.
 *
 * The layout is chrome (ADR 0006). Bindings point at existing document
 * ids; they do not own mix or launch state.
 */

export const SURFACE_LAYOUT_VERSION = 1 as const;

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

export type SurfaceTemplateId = "blank" | "lpd8" | "mixer";

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
  /** Shared by the widgets a channel-strip recipe inserts. */
  groupId?: string;
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
  templateId: SurfaceTemplateId | "custom";
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
  | `recipe:${AddRecipe}`
  | `template:${SurfaceTemplateId}`;

export interface AddMenuItem {
  id: AddMenuId;
  label: string;
  hint: string;
  group: "widget" | "recipe" | "template";
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
  { id: "template:lpd8", label: "AKAI LPD8", hint: "8 knobs, 2×4 pads, homage faceplate", group: "template" },
  { id: "template:mixer", label: "Mixer", hint: "Channel strips across the page", group: "template" },
  { id: "template:blank", label: "Blank page", hint: "Empty faceplate", group: "template" },
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
  return { id: newId(), name, templateId: "blank", widgets: [] };
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
  const cells: Array<string | null> = Array(cols * rows).fill(null);
  for (let i = 0; i < clips.length && i < cells.length; i++) {
    const col = i % cols;
    const rowFromBottom = Math.floor(i / cols);
    const rowFromTop = rows - 1 - rowFromBottom;
    cells[padIndex(cols, rows, col, rowFromTop)] = clips[i].id;
  }
  return {
    id: newId(),
    kind: "padGrid",
    label: "PADS",
    target: null,
    cols,
    rows,
    cells,
  };
}

function replaceActive(
  layout: SurfaceLayout,
  widgets: SurfaceWidget[],
  templateId?: SurfacePage["templateId"],
): SurfaceLayout {
  const page = activePage(layout);
  const nextPage: SurfacePage = {
    ...page,
    widgets,
    templateId: templateId ?? page.templateId,
  };
  return {
    ...layout,
    pages: layout.pages.map((p) => (p.id === page.id ? nextPage : p)),
  };
}

function appendActive(layout: SurfaceLayout, extra: SurfaceWidget[]): SurfaceLayout {
  if (extra.length === 0) return layout;
  const page = activePage(layout);
  return replaceActive(layout, [...page.widgets, ...extra], "custom");
}

export function addWidget(layout: SurfaceLayout, widget: SurfaceWidget): SurfaceLayout {
  return appendActive(layout, [widget]);
}

export function addStrip(layout: SurfaceLayout, track: SurfaceTrack): SurfaceLayout {
  const page = activePage(layout);
  if (pageHasTarget(page, { kind: "trackGain", trackId: track.id })) return layout;
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

export function applyTemplate(layout: SurfaceLayout, template: SurfaceTemplateId, ctx: SurfaceContext): SurfaceLayout {
  if (template === "blank") {
    return replaceActive(layout, [], "blank");
  }
  if (template === "mixer") {
    const filled = addRecipe(replaceActive(layout, [], "mixer"), "tracks", ctx);
    return replaceActive(filled, activePage(filled).widgets, "mixer");
  }
  // lpd8
  const tracks = mixableTracks(ctx.tracks);
  const knobs: SurfaceWidget[] = [];
  for (let i = 0; i < 8; i++) {
    const t = tracks[i];
    knobs.push({
      id: newId(),
      kind: "knob",
      // Bound knobs wear the track they drive; the silkscreen K1..K8 stays
      // on the ones that are still empty (the faceplate prints it as a ghost).
      label: t ? t.name : `K${i + 1}`,
      target: t ? { kind: "trackGain", trackId: t.id } : null,
    });
  }
  const grid = padGridForClips(ctx.midiClips.slice(0, 8));
  return replaceActive(layout, [...knobs, grid], "lpd8");
}

export function removeWidget(layout: SurfaceLayout, widgetId: string): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.filter((w) => w.id !== widgetId),
    page.widgets.some((w) => w.id === widgetId) ? "custom" : page.templateId,
  );
}

/** Remove every widget that shares this strip group. */
export function removeGroup(layout: SurfaceLayout, groupId: string): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.filter((w) => w.groupId !== groupId),
    "custom",
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
    "custom",
  );
}

export function setPadMode(layout: SurfaceLayout, widgetId: string, padMode: PadMode): SurfaceLayout {
  const page = activePage(layout);
  return replaceActive(
    layout,
    page.widgets.map((w) => (w.id === widgetId ? { ...w, padMode } : w)),
    "custom",
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
    "custom",
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

export function parseLayout(raw: unknown): SurfaceLayout | null {
  if (!raw || typeof raw !== "object") return null;
  const o = raw as Partial<SurfaceLayout>;
  if (o.version !== SURFACE_LAYOUT_VERSION) return null;
  if (!Array.isArray(o.pages) || o.pages.length === 0) return null;
  if (typeof o.activePageId !== "string") return null;
  const pages: SurfacePage[] = [];
  for (const p of o.pages) {
    if (!p || typeof p.id !== "string" || typeof p.name !== "string") return null;
    if (!Array.isArray(p.widgets)) return null;
    pages.push({
      id: p.id,
      name: p.name,
      templateId: p.templateId ?? "custom",
      widgets: p.widgets.filter(isWidget),
    });
  }
  const activePageId = pages.some((p) => p.id === o.activePageId) ? o.activePageId : pages[0].id;
  return { version: SURFACE_LAYOUT_VERSION, pages, activePageId };
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

export interface GroupedWidgets {
  strips: SurfaceWidget[][];
  loose: SurfaceWidget[];
}

/** Channel-strip bundles first (stable groupId order), then free widgets. */
export function groupWidgets(page: SurfacePage): GroupedWidgets {
  const strips = new Map<string, SurfaceWidget[]>();
  const loose: SurfaceWidget[] = [];
  for (const w of page.widgets) {
    if (w.groupId) {
      const g = strips.get(w.groupId);
      if (g) g.push(w);
      else strips.set(w.groupId, [w]);
    } else {
      loose.push(w);
    }
  }
  return { strips: [...strips.values()], loose };
}
