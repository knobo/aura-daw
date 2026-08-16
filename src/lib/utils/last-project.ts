/**
 * Recent-project history. A localStorage JSON list (newest first), not a
 * preference: it is session continuity, not something the user edits.
 * Failures (no storage, quota, junk) degrade to "no recent projects".
 *
 * The older single-string `aura.lastProjectDir` key is still read so a
 * machine that only has the pre-list persist does not forget its last
 * project. New writes go only to the list key.
 */

export const LAST_PROJECT_KEY = "aura.lastProjectDir";
export const RECENT_PROJECTS_KEY = "aura.recentProjectDirs";
/** Hard cap on stored history. The preferences slider may show fewer. */
export const RECENT_PROJECTS_CAP = 20;

type PrefsStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

function storage(): PrefsStorage | undefined {
  try {
    return (globalThis as { localStorage?: PrefsStorage }).localStorage;
  } catch {
    return undefined;
  }
}

function sanitize(paths: unknown): string[] {
  if (!Array.isArray(paths)) return [];
  const out: string[] = [];
  for (const raw of paths) {
    if (typeof raw !== "string") continue;
    const path = raw.trim();
    if (!path || out.includes(path)) continue;
    out.push(path);
    if (out.length >= RECENT_PROJECTS_CAP) break;
  }
  return out;
}

function readLegacy(): string | undefined {
  const store = storage();
  if (!store) return undefined;
  try {
    const value = store.getItem(LAST_PROJECT_KEY);
    if (typeof value !== "string") return undefined;
    const path = value.trim();
    return path.length > 0 ? path : undefined;
  } catch {
    return undefined;
  }
}

/** Newest-first history. Falls back to the legacy single-path key. */
export function readRecentProjectDirs(): string[] {
  const store = storage();
  if (!store) return [];
  try {
    const raw = store.getItem(RECENT_PROJECTS_KEY);
    if (raw != null) {
      try {
        return sanitize(JSON.parse(raw));
      } catch {
        return [];
      }
    }
  } catch {
    return [];
  }
  const legacy = readLegacy();
  return legacy ? [legacy] : [];
}

/** Absolute .aura dir of the last successfully opened project, or undefined. */
export function readLastProjectDir(): string | undefined {
  return readRecentProjectDirs()[0];
}

/** Remember a project dir as the newest history entry. Empty paths ignored. */
export function writeLastProjectDir(path: string): void {
  const trimmed = path.trim();
  if (!trimmed) return;
  const next = sanitize([trimmed, ...readRecentProjectDirs()]);
  const store = storage();
  if (!store) return;
  try {
    store.setItem(RECENT_PROJECTS_KEY, JSON.stringify(next));
  } catch {
    // quota / private mode — the path just doesn't stick
  }
}

/** Folder name of an .aura dir, for the recent-projects menu. */
export function recentProjectLabel(path: string): string {
  const trimmed = path.replace(/[/\\]+$/, "");
  const slash = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return slash >= 0 ? trimmed.slice(slash + 1) : trimmed;
}
