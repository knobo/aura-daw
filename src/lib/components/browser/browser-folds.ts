/**
 * Group-fold persistence for the browser layer, in the shape
 * `state/lanes.svelte.ts` established for lane folds: which groups are
 * collapsed is VIEW state, not the document, so it lives in `localStorage`
 * keyed per browser rather than the catalog or the op log. Storage is
 * hand-editable and sometimes unavailable (private mode, quota, a garbage
 * value from an older build) — validate on read, and never let a bad value
 * throw during render.
 */

function storageKey(browserId: string): string {
  return `aura.browser.folds:${browserId}`;
}

/** Load the collapsed-group set for `browserId`. Falls back to "nothing
 * folded" for missing, unreadable or malformed storage. */
export function loadFolds(browserId: string): Set<string> {
  try {
    const raw = localStorage.getItem(storageKey(browserId));
    if (!raw) return new Set();
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((x): x is string => typeof x === "string"));
  } catch {
    return new Set();
  }
}

/** Persist the collapsed-group set for `browserId`. Best-effort: a full or
 * unavailable store must not break folding for the rest of the session. */
export function saveFolds(browserId: string, collapsed: ReadonlySet<string>): void {
  try {
    localStorage.setItem(storageKey(browserId), JSON.stringify([...collapsed]));
  } catch {
    /* storage full or unavailable — folding still works this session */
  }
}
