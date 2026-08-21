/**
 * Browse mode's section tree (plan §5.2): favourites, recents, instruments
 * and effects sub-grouped by their descriptor categories, and everything
 * else under Uncategorised.
 *
 * Pure, so the ordering rules that are easy to get wrong — a plugin in two
 * categories, a recent whose bundle has since been uninstalled, a parent
 * left standing after the query emptied all its children — are settled
 * here rather than inside a `{#each}`.
 *
 * The tree is returned FLAT, with a `parentKey`, because `BrowserShell`'s
 * keyboard model (`flattenRows`) is a flat row order. Nesting is a
 * rendering fact (`depth`) and a folding fact (`visibleSections`), not a
 * different data structure.
 */

import type { PluginDescriptor, PluginFormat } from "../types/ipc";
import { fuzzyScore, parseSearchQuery, rankItems } from "../components/browser/browser-model";
import { frecencyScore, type FrecencyTable } from "./plugin-frecency";

export interface BrowseSection {
  key: string;
  label: string;
  /** Set on a category child; absent on a top-level section. */
  parentKey?: string;
  depth: number;
  /** Plugins directly in this section. Empty on a parent that only holds
   * category children. */
  items: PluginDescriptor[];
  /** What the header badge shows: own items, or the children's total. */
  count: number;
}

export interface BrowseInput {
  descriptors: readonly PluginDescriptor[];
  favorites: readonly string[];
  recents: readonly { uid: string; usedAt: number }[];
  query: string;
  /** Catalog user tags, keyed by uid. Ranked the same way as name. */
  tags?: Readonly<Record<string, string[]>>;
  /** Optional frecency table for the picker (winner spec §3.1). */
  frecency?: FrecencyTable;
  now?: number;
}

export type KindFilter = "all" | "inst" | "fx";

function rankKeys(tags: Readonly<Record<string, string[]>> = {}) {
  return [
    { value: (d: PluginDescriptor) => d.name, weight: 2 },
    { value: (d: PluginDescriptor) => d.vendor ?? "" },
    { value: (d: PluginDescriptor) => (d.categories ?? []).join(" ") },
    { value: (d: PluginDescriptor) => (tags[d.uid] ?? []).join(" "), weight: 2 },
  ];
}

function narrow(
  items: PluginDescriptor[],
  query: string,
  tags: Readonly<Record<string, string[]>> = {},
): PluginDescriptor[] {
  const parsed = parseSearchQuery(query);
  const pool = parsed.format ? items.filter((d) => d.format === parsed.format) : items;
  return parsed.text ? rankItems(pool, parsed.text, rankKeys(tags)) : pool.slice();
}

/** Exclusive kind chip: ALL / INST / FX. */
export function filterByKind(
  descriptors: readonly PluginDescriptor[],
  kind: KindFilter,
): PluginDescriptor[] {
  if (kind === "inst") return descriptors.filter((d) => d.isInstrument);
  if (kind === "fx") return descriptors.filter((d) => !d.isInstrument);
  return descriptors.slice();
}

export interface FacetFilter {
  kind: KindFilter;
  /** Empty / omitted = every format. Multi-select is OR. */
  formats?: readonly PluginFormat[];
  /** Empty / omitted = every type. Multi-select is OR. */
  categories?: readonly string[];
}

/** AND across facet groups, OR inside a group (Ableton 12's rule). */
export function filterByFacets(
  descriptors: readonly PluginDescriptor[],
  facets: FacetFilter,
): PluginDescriptor[] {
  let pool = filterByKind(descriptors, facets.kind);
  if (facets.formats && facets.formats.length > 0) {
    const want = new Set(facets.formats);
    pool = pool.filter((d) => want.has(d.format));
  }
  if (facets.categories && facets.categories.length > 0) {
    const want = new Set(facets.categories);
    pool = pool.filter((d) => (d.categories ?? []).some((c) => want.has(c)));
  }
  return pool;
}

/** Unique `categories[]` from the scan, sorted, for the type-chip row. */
export function listCategoryFacets(descriptors: readonly PluginDescriptor[]): string[] {
  const seen = new Set<string>();
  for (const d of descriptors) {
    for (const c of d.categories ?? []) seen.add(c);
  }
  return [...seen].sort((a, b) => a.localeCompare(b));
}

/** Drop selected type chips that the current kind/format can no longer
 * produce — INST + "EQ plugin" is a query with no answers, and the chip
 * should leave with the empty result. */
export function pruneUnavailableFacets(
  selected: readonly string[],
  available: readonly string[],
): string[] {
  const live = new Set(available);
  return selected.filter((s) => live.has(s));
}

/** ★ / ⏱ shortlist toggles: keep those sections, drop the rest. Both off
 * is the full tree; both on is the union of the two shortlists. */
export function filterSections(
  sections: readonly BrowseSection[],
  opts: { favoritesOnly?: boolean; recentsOnly?: boolean } = {},
): BrowseSection[] {
  const { favoritesOnly, recentsOnly } = opts;
  if (!favoritesOnly && !recentsOnly) return sections.slice();
  return sections.filter((s) => {
    const top = s.parentKey ?? s.key;
    return (favoritesOnly && top === "fav") || (recentsOnly && top === "recent");
  });
}

function byName(a: PluginDescriptor, b: PluginDescriptor): number {
  return a.name.localeCompare(b.name);
}

/** One top-level section plus its category children, or nothing at all if
 * the query emptied every child. */
function categorised(
  key: string,
  label: string,
  descriptors: PluginDescriptor[],
  query: string,
  tags: Readonly<Record<string, string[]>> = {},
): BrowseSection[] {
  const byCategory = new Map<string, PluginDescriptor[]>();
  for (const d of descriptors) {
    for (const category of d.categories ?? []) {
      const bucket = byCategory.get(category);
      if (bucket) bucket.push(d);
      else byCategory.set(category, [d]);
    }
  }

  const children = [...byCategory.keys()]
    .sort((a, b) => a.localeCompare(b))
    .map((category) => ({
      key: `${key}:${category}`,
      label: category,
      parentKey: key,
      depth: 1,
      items: narrow([...(byCategory.get(category) ?? [])].sort(byName), query, tags),
    }))
    .filter((child) => child.items.length > 0)
    .map((child) => ({ ...child, count: child.items.length }));

  if (children.length === 0) return [];
  const total = children.reduce((n, c) => n + c.items.length, 0);
  return [{ key, label, depth: 0, items: [], count: total }, ...children];
}

function flat(
  key: string,
  label: string,
  items: PluginDescriptor[],
  query: string,
  tags: Readonly<Record<string, string[]>> = {},
): BrowseSection[] {
  const narrowed = narrow(items, query, tags);
  if (narrowed.length === 0) return [];
  return [{ key, label, depth: 0, items: narrowed, count: narrowed.length }];
}

export function buildBrowseSections(input: BrowseInput): BrowseSection[] {
  const { descriptors, favorites, recents, query, tags = {} } = input;
  const byUid = new Map(descriptors.map((d) => [d.uid, d]));

  const favorited = favorites
    .map((uid) => byUid.get(uid))
    .filter((d): d is PluginDescriptor => d !== undefined)
    .sort(byName);

  // Newest first. A recent whose bundle was uninstalled between sessions
  // simply isn't in the scan any more — drop it rather than render a row
  // that cannot be instantiated.
  const recent = [...recents]
    .sort((a, b) => b.usedAt - a.usedAt)
    .map((r) => byUid.get(r.uid))
    .filter((d): d is PluginDescriptor => d !== undefined);

  const hasCategory = (d: PluginDescriptor) => (d.categories ?? []).length > 0;
  const instruments = descriptors.filter((d) => d.isInstrument && hasCategory(d));
  const effects = descriptors.filter((d) => !d.isInstrument && hasCategory(d));
  // No categories means no categories — a guessed one would be a lie the
  // user then has to unlearn.
  const uncategorised = descriptors.filter((d) => !hasCategory(d)).sort(byName);

  return [
    ...flat("fav", "★ Favourites", favorited, query, tags),
    ...flat("recent", "⏱ Recent", recent, query, tags),
    ...categorised("inst", "Instruments", instruments, query, tags),
    ...categorised("fx", "Effects", effects, query, tags),
    ...flat("uncat", "Uncategorised", uncategorised, query, tags),
  ];
}

/** Drop the children of folded parents. A folded LEAF stays — hiding its
 * own rows is `flattenRows`' job, and removing the header would leave no
 * way to unfold it. */
export function visibleSections(
  sections: readonly BrowseSection[],
  collapsed: ReadonlySet<string>,
): BrowseSection[] {
  return sections.filter((s) => !s.parentKey || !collapsed.has(s.parentKey));
}

/**
 * The quick picker's one ranked list (winner spec §3.1): frecency, not
 * rigid tiers. Favourites get a nudge, not a lock; recents break remaining
 * ties so the old "most recently used first" still holds when nothing has
 * a use-count yet. Each plugin appears once.
 */
export function rankQuickPick(input: BrowseInput): PluginDescriptor[] {
  const { descriptors, favorites, recents, query, tags = {}, frecency = {}, now = Date.now() } = input;
  const matching = narrow([...descriptors], query, tags);
  const favoriteSet = new Set(favorites);
  const recency = new Map(recents.map((r) => [r.uid, r.usedAt]));
  const q = parseSearchQuery(query).text;

  return matching
    .map((d) => ({
      d,
      score: frecencyScore(frecency[d.uid], now, favoriteSet.has(d.uid)) + (q ? fuzzyScore(d.name, q) * 2 : 0),
      usedAt: recency.get(d.uid) ?? 0,
    }))
    .sort((a, b) => b.score - a.score || b.usedAt - a.usedAt || a.d.name.localeCompare(b.d.name))
    .map((x) => x.d);
}
