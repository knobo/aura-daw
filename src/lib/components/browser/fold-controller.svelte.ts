/**
 * The fold controller every BrowserShell caller shares (design §8.1).
 *
 * Persistence, the two-layer FoldState, collapse-all and "search reveals,
 * clearing restores" used to be copied per browser as a raw Set. The
 * shell's fold-all button then had nothing to bind to. One controller
 * owns that state; the browsers just construct it and pass `onFoldAll`.
 */
import {
  anyCollapsed as anyFolded,
  beginSearch,
  effectiveFolds,
  endSearch,
  loadFolds,
  saveFolds,
  setAllCollapsed,
  toggleFold,
  type FoldState,
} from "./browser-folds";

export interface FoldStorage {
  load(browserId: string): Set<string>;
  save(browserId: string, collapsed: ReadonlySet<string>): void;
}

const localStorageFolds: FoldStorage = {
  load: loadFolds,
  save: saveFolds,
};

export class FoldController {
  folds = $state<FoldState>({ own: new Set(), search: null });
  #id: string;
  #storage: FoldStorage;

  constructor(browserId: string, storage: FoldStorage = localStorageFolds) {
    this.#id = browserId;
    this.#storage = storage;
    this.folds = { own: storage.load(browserId), search: null };
  }

  get collapsed(): ReadonlySet<string> {
    return effectiveFolds(this.folds);
  }

  /** Switch which browser id we persist against (PresetsRoot swaps
   * instruments/patches). Reloads that id's own folds. */
  retarget(browserId: string) {
    if (browserId === this.#id) return;
    this.#id = browserId;
    this.folds = { own: this.#storage.load(browserId), search: null };
  }

  /** Open or close the search layer. Idempotent: a running search keeps
   * folds made during it; a cleared query puts the own folds back. */
  syncQuery(query: string) {
    const next = query.trim() ? beginSearch(this.folds) : endSearch(this.folds);
    if (next !== this.folds) this.folds = next;
  }

  /** Expand (`true`) or collapse (`false`) one group. No-op when the
   * group is already in that state — matching BrowserShell's
   * `onToggleGroup(key, expand)` contract. */
  toggle(key: string, expand: boolean) {
    if (this.collapsed.has(key) === expand) {
      this.folds = toggleFold(this.folds, key);
      this.#persist();
    }
  }

  setAll(keys: readonly string[], collapse: boolean) {
    this.folds = setAllCollapsed(this.folds, keys, collapse);
    this.#persist();
  }

  anyCollapsed(keys: readonly string[]): boolean {
    return anyFolded(this.folds, keys);
  }

  #persist() {
    this.#storage.save(this.#id, this.folds.own);
  }
}
