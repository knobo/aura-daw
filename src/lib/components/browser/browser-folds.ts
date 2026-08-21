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

/**
 * Fold state as two layers (design §8.1). `own` is what the user chose and
 * what gets persisted; `search` is a scratch layer that exists only while a
 * query is running.
 *
 * The two layers are the whole point. A search that auto-expands matching
 * groups must not be indistinguishable from the user expanding them: the
 * moment the query clears, their folds have to come back exactly as they
 * left them. Flattening this into one set loses that, and "my folds
 * evaporated because I typed something" is the bug it prevents.
 */
export interface FoldState {
  own: ReadonlySet<string>;
  /** null = no search running. */
  search: ReadonlySet<string> | null;
}

export function emptyFoldState(): FoldState {
  return { own: new Set(), search: null };
}

/** The folds actually in force right now. */
export function effectiveFolds(state: FoldState): ReadonlySet<string> {
  return state.search ?? state.own;
}

/** A query became active: everything expands so no result hides behind a
 * fold. Idempotent — a search already running keeps the folds the user has
 * made *during* it. */
export function beginSearch(state: FoldState): FoldState {
  return state.search ? state : { own: state.own, search: new Set() };
}

/** The query cleared: the user's own folds come back. */
export function endSearch(state: FoldState): FoldState {
  return state.search ? { own: state.own, search: null } : state;
}

function withLayer(state: FoldState, next: Set<string>): FoldState {
  return state.search ? { own: state.own, search: next } : { own: next, search: null };
}

export function toggleFold(state: FoldState, key: string): FoldState {
  const next = new Set(effectiveFolds(state));
  if (!next.delete(key)) next.add(key);
  return withLayer(state, next);
}

/** Collapse-all / expand-all over the groups currently on screen. Folds for
 * groups outside `keys` survive untouched — a group that scrolled out of a
 * filter should not lose its state to a button that never saw it. */
export function setAllCollapsed(
  state: FoldState,
  keys: readonly string[],
  collapsed: boolean,
): FoldState {
  const next = new Set(effectiveFolds(state));
  for (const key of keys) {
    if (collapsed) next.add(key);
    else next.delete(key);
  }
  return withLayer(state, next);
}

/** Any of the visible groups folded? Drives the single button's "the next
 * press unfolds" rule, the same way `lanes.anyCollapsed()` does. */
export function anyCollapsed(state: FoldState, keys: readonly string[]): boolean {
  const folds = effectiveFolds(state);
  return keys.some((key) => folds.has(key));
}

export function allCollapsed(state: FoldState, keys: readonly string[]): boolean {
  const folds = effectiveFolds(state);
  return keys.length > 0 && keys.every((key) => folds.has(key));
}
