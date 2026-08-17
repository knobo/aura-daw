/** Small shared UI facts (renderer badge, right-dock tab). */

import { PREF_SCHEMA } from "../prefs/schema";
import { prefs } from "../prefs/prefs.svelte";

export type DockTab =
  | ""
  | "composer"
  | "generate"
  | "hum"
  | "library"
  | "instruments"
  | "plugins"
  | "mcp";

/** The bottom region shows one of these at a time. */
export type BottomPanel = "roll" | "pitch";

export const ui = $state({
  /** "WEBGPU" | "CANVAS2D" | "" until the first painter reports in */
  rendererKind: "",
  /** Which right-dock panel is open ("" = closed). */
  dock: "" as DockTab,
  /** AI Studio: preselected job kind when opened from elsewhere. */
  studioKind: "aceStepGenerate",
  /** Which bottom panel is showing. Both share `rollHeight` and the same
   * top-edge resize handle, so switching tabs does not resize the app. */
  bottomPanel: "roll" as BottomPanel,
  /** Piano roll panel height, CSS px. User-dragged (top edge); session only. */
  rollHeight: 340,
  /** Right dock width, CSS px. User-dragged (left edge); session only. */
  dockWidth: 340,
});

// Interface zoom lives in the preferences store (prefs.values.uiZoom) —
// clamp/snap/persistence all come from its schema entry. These helpers are
// the ergonomic mutation API for shortcuts and the transport-bar chips.
export const UI_ZOOM_MIN = PREF_SCHEMA.uiZoom.min;
export const UI_ZOOM_MAX = PREF_SCHEMA.uiZoom.max;
export const UI_ZOOM_STEP = PREF_SCHEMA.uiZoom.step;

export function setUiZoom(factor: number) {
  prefs.set("uiZoom", factor);
}

export function zoomUiIn() {
  setUiZoom(prefs.values.uiZoom + UI_ZOOM_STEP);
}

export function zoomUiOut() {
  setUiZoom(prefs.values.uiZoom - UI_ZOOM_STEP);
}

export function resetUiZoom() {
  setUiZoom(PREF_SCHEMA.uiZoom.default);
}

export function toggleDock(tab: Exclude<DockTab, "">) {
  ui.dock = ui.dock === tab ? "" : tab;
}

/**
 * One bare letter per dock panel. Single source of truth: the window
 * shortcut in App.svelte and the hints on the tabs both read this map, so
 * a rebind cannot leave the UI advertising a key that no longer works.
 */
export const DOCK_SHORTCUT: Record<Exclude<DockTab, "">, string> = {
  // Not "c": bare C is already the library's CLIPS destination in
  // App.svelte, and `dockTabForKey` is tested BEFORE it — a composer bound
  // there would silently shadow a shipped shortcut. "o" as in cOmposer.
  composer: "o",
  generate: "g",
  hum: "h",
  library: "l",
  instruments: "i",
  plugins: "p",
  mcp: "m",
};

const BY_KEY = new Map(
  Object.entries(DOCK_SHORTCUT).map(([tab, key]) => [key, tab as Exclude<DockTab, "">]),
);

/** The panel a bare keypress opens, or undefined for an unbound key. */
export function dockTabForKey(key: string): Exclude<DockTab, ""> | undefined {
  return BY_KEY.get(key.toLowerCase());
}

export function openStudio(kind?: string) {
  if (kind) ui.studioKind = kind;
  ui.dock = "generate";
}
