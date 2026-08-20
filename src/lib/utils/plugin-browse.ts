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

import type { PluginDescriptor } from "../types/ipc";
import { rankItems } from "../components/browser/browser-model";

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
}

const RANK_KEYS = [
  { value: (d: PluginDescriptor) => d.name, weight: 2 },
  { value: (d: PluginDescriptor) => d.vendor ?? "" },
  { value: (d: PluginDescriptor) => (d.categories ?? []).join(" ") },
];

function narrow(items: PluginDescriptor[], query: string): PluginDescriptor[] {
  return query.trim() ? rankItems(items, query, RANK_KEYS) : items;
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
      items: narrow([...(byCategory.get(category) ?? [])].sort(byName), query),
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
): BrowseSection[] {
  const narrowed = narrow(items, query);
  if (narrowed.length === 0) return [];
  return [{ key, label, depth: 0, items: narrowed, count: narrowed.length }];
}

export function buildBrowseSections(input: BrowseInput): BrowseSection[] {
  const { descriptors, favorites, recents, query } = input;
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
    ...flat("fav", "★ Favourites", favorited, query),
    ...flat("recent", "⏱ Recent", recent, query),
    ...categorised("inst", "Instruments", instruments, query),
    ...categorised("fx", "Effects", effects, query),
    ...flat("uncat", "Uncategorised", uncategorised, query),
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
 * The quick picker's one ranked list (plan §5.3): favourites, then recents
 * by recency, then everything else by fuzzy score.
 *
 * The tiers survive the query rather than being replaced by it — with
 * `Ctrl+P` you are reaching for something you already use, so a favourite
 * that matches must beat a better-scoring stranger. Each plugin appears
 * once, in its best tier.
 */
export function rankQuickPick(input: BrowseInput): PluginDescriptor[] {
  const { descriptors, favorites, recents, query } = input;
  const matching = narrow([...descriptors], query);

  const favoriteSet = new Set(favorites);
  const recency = new Map(recents.map((r) => [r.uid, r.usedAt]));

  const seen = new Set<string>();
  const take = (items: PluginDescriptor[]) => {
    const out = items.filter((d) => !seen.has(d.uid));
    for (const d of out) seen.add(d.uid);
    return out;
  };

  const favorited = take(matching.filter((d) => favoriteSet.has(d.uid)).sort(byName));
  const recent = take(
    matching
      .filter((d) => recency.has(d.uid))
      .sort((a, b) => (recency.get(b.uid) ?? 0) - (recency.get(a.uid) ?? 0)),
  );
  // `narrow` already ordered the remainder by score for a real query; with
  // no query it is scan order, which is as good an answer as any.
  const rest = take(matching);

  return [...favorited, ...recent, ...rest];
}
