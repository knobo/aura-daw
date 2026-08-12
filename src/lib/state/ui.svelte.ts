/** Small shared UI facts (renderer badge, right-dock tab). */

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
});

export function toggleDock(tab: Exclude<DockTab, "">) {
  ui.dock = ui.dock === tab ? "" : tab;
}

export function openStudio(kind?: string) {
  if (kind) ui.studioKind = kind;
  ui.dock = "generate";
}
