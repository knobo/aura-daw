/**
 * Plugin Manager view state (plan §5.2 / design §9): mode and the chip
 * row, persisted per project in localStorage — the same ruling as lane
 * folds. Not the catalog, not the op log.
 */

import type { PluginFormat } from "../types/ipc";
import type { KindFilter } from "./plugin-browse";

export type { KindFilter };
export type ManagerMode = "browse" | "split" | "rack" | "matrix";

export interface ManagerView {
  mode: ManagerMode;
  kind: KindFilter;
  favoritesOnly: boolean;
  recentsOnly: boolean;
  formats: PluginFormat[];
  categories: string[];
  splitPx: number;
}

export const DEFAULT_SPLIT_PX = 220;

export const DEFAULT_VIEW: ManagerView = {
  mode: "browse",
  kind: "all",
  favoritesOnly: false,
  recentsOnly: false,
  formats: [],
  categories: [],
  splitPx: DEFAULT_SPLIT_PX,
};

function storageKey(projectDir: string | null): string {
  return `aura.plugin-manager.view:${projectDir ?? ""}`;
}

/** SPLIT is the default whenever the project has instances and the user
 * hasn't pinned a focus (winner spec §5). A stored choice wins. */
export function initialManagerMode(
  stored: ManagerMode | undefined,
  instanceCount: number,
): ManagerMode {
  if (stored === "browse" || stored === "split" || stored === "rack" || stored === "matrix") {
    return stored;
  }
  return instanceCount > 0 ? "split" : "browse";
}

export function loadManagerView(projectDir: string | null): Partial<ManagerView> {
  try {
    const raw = localStorage.getItem(storageKey(projectDir));
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return {};
    const o = parsed as Record<string, unknown>;
    const view: Partial<ManagerView> = {};
    if (o.mode === "browse" || o.mode === "split" || o.mode === "rack" || o.mode === "matrix") {
      view.mode = o.mode;
    }
    if (o.kind === "all" || o.kind === "inst" || o.kind === "fx") view.kind = o.kind;
    if (typeof o.favoritesOnly === "boolean") view.favoritesOnly = o.favoritesOnly;
    if (typeof o.recentsOnly === "boolean") view.recentsOnly = o.recentsOnly;
    if (Array.isArray(o.formats)) {
      view.formats = o.formats.filter((x): x is PluginFormat => x === "clap" || x === "lv2");
    }
    if (Array.isArray(o.categories)) {
      view.categories = o.categories.filter((x): x is string => typeof x === "string");
    }
    if (typeof o.splitPx === "number" && o.splitPx > 0) view.splitPx = o.splitPx;
    return view;
  } catch {
    return {};
  }
}

export function saveManagerView(projectDir: string | null, view: ManagerView): void {
  try {
    localStorage.setItem(storageKey(projectDir), JSON.stringify(view));
  } catch {
    /* storage full or unavailable — the session still works */
  }
}
