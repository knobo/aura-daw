/**
 * Last-opened project directory. A single localStorage string, not a
 * preference: it is session continuity, not something the user edits.
 * Failures (no storage, quota, junk) degrade to "no last project".
 */

export const LAST_PROJECT_KEY = "aura.lastProjectDir";

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

/** Absolute .aura dir of the last successfully opened project, or undefined. */
export function readLastProjectDir(): string | undefined {
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

/** Remember a project dir. Empty / whitespace paths are ignored. */
export function writeLastProjectDir(path: string): void {
  const trimmed = path.trim();
  if (!trimmed) return;
  const store = storage();
  if (!store) return;
  try {
    store.setItem(LAST_PROJECT_KEY, trimmed);
  } catch {
    // quota / private mode — the path just doesn't stick
  }
}
