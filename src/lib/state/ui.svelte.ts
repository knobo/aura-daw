/** Small shared UI facts (renderer badge, right-dock tab). */

import { readPref, writePref } from "../utils/prefs";

export type DockTab = "" | "generate" | "hum" | "instruments" | "plugins" | "mcp";

export const ui = $state({
  /** "WEBGPU" | "CANVAS2D" | "" until the first painter reports in */
  rendererKind: "",
  /** Which right-dock panel is open ("" = closed). */
  dock: "" as DockTab,
  /** AI Studio: preselected job kind when opened from elsewhere. */
  studioKind: "aceStepGenerate",
  /** Flash notes (piano roll, keys, clip bodies) as the playhead hits them. */
  noteFlash: true,
  /** Piano roll panel height, CSS px. User-dragged (top edge); session only. */
  rollHeight: 340,
  /** Right dock width, CSS px. User-dragged (left edge); session only. */
  dockWidth: 340,
  /** Interface zoom factor (CSS `zoom` on the shell) — persisted as a preference. */
  zoom: 1,
});

export const UI_ZOOM_MIN = 0.8;
export const UI_ZOOM_MAX = 2.0;
export const UI_ZOOM_STEP = 0.1;

/** Clamp to [0.8, 2.0] and snap to the 0.1 grid so steps never drift. */
export function setUiZoom(factor: number) {
  if (!Number.isFinite(factor)) return;
  const clamped = Math.min(UI_ZOOM_MAX, Math.max(UI_ZOOM_MIN, factor));
  ui.zoom = Math.round(clamped * 10) / 10;
  writePref("uiZoom", ui.zoom);
}

/** Restore the persisted zoom at boot; junk on disk falls through setUiZoom's guards. */
export function initUiZoom() {
  const stored = readPref("uiZoom");
  if (typeof stored === "number") setUiZoom(stored);
}

export function zoomUiIn() {
  setUiZoom(ui.zoom + UI_ZOOM_STEP);
}

export function zoomUiOut() {
  setUiZoom(ui.zoom - UI_ZOOM_STEP);
}

export function resetUiZoom() {
  setUiZoom(1);
}

export function toggleDock(tab: Exclude<DockTab, "">) {
  ui.dock = ui.dock === tab ? "" : tab;
}

export function openStudio(kind?: string) {
  if (kind) ui.studioKind = kind;
  ui.dock = "generate";
}
