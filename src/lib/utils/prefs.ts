/**
 * User preferences: a single JSON object under one localStorage key.
 * localStorage (not the Tauri fs plugin) on purpose: it works identically
 * in the real app and the plain-browser demo backend, and needs no IPC.
 * Every failure mode — no storage (node tests, exotic webviews), corrupt
 * JSON, private-mode/quota throws — degrades to "no preference", never
 * to a crash: callers must validate values themselves anyway, since
 * anything read here is user-editable disk state.
 */

export const PREFS_KEY = "aura.prefs";

/** Structural slice of Storage so tests can stub it without a DOM. */
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

function readAll(store: PrefsStorage): Record<string, unknown> {
  try {
    const parsed: unknown = JSON.parse(store.getItem(PREFS_KEY) ?? "");
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      return parsed as Record<string, unknown>;
    }
  } catch {
    // corrupt or missing — treated as empty
  }
  return {};
}

/** Read one preference; undefined when unset, unreadable, or storage-less. */
export function readPref(key: string): unknown {
  const store = storage();
  return store ? readAll(store)[key] : undefined;
}

/** Write one preference, keeping the others. Failures are silent. */
export function writePref(key: string, value: unknown): void {
  const store = storage();
  if (!store) return;
  try {
    store.setItem(PREFS_KEY, JSON.stringify({ ...readAll(store), [key]: value }));
  } catch {
    // quota / private mode — the preference just doesn't stick
  }
}
