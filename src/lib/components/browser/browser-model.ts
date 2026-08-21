/**
 * Pure browsing primitives shared by every browser surface in the app
 * (plugins, instruments, Zyn patches, samples, presets). No Svelte, no DOM —
 * `BrowserShell.svelte` and friends are a thin rendering skin over this.
 *
 * The point of pulling this out is that ranking, grouping, flattening and
 * keyboard movement are all easy to get subtly wrong (off-by-one on a
 * folded group, a tie-break that isn't actually stable) and none of them
 * need a browser to test.
 */

/** A domain item normalized for the browser layer. `data` carries the
 * original record (a `PluginDescriptor`, an `InstrumentInfo`, …) so a row
 * can still reach whatever its actions need. */
export interface BrowserItem<T = unknown> {
  id: string;
  label: string;
  sublabel?: string;
  favorite?: boolean;
  data: T;
}

export interface BrowserGroup<T> {
  key: string;
  label: string;
  items: T[];
}

/** One position in the flattened, keyboard-navigable row order. `itemIndex`
 * is -1 for a group's own header row, otherwise the index into that
 * group's `items`. */
export interface BrowserRowRef {
  kind: "group" | "item";
  groupKey: string;
  itemIndex: number;
}

/** Stable DOM id for a row, so a component can render its own markup and
 * still line up with `BrowserShell`'s idea of "the active row". */
export function rowId(row: BrowserRowRef): string {
  return row.kind === "group"
    ? `browser-row-group-${row.groupKey}`
    : `browser-row-item-${row.groupKey}-${row.itemIndex}`;
}

/**
 * Rank text against a query. Not a boolean match: a prefix hit beats a
 * word-boundary hit beats a mid-word hit, and within a tier an earlier /
 * tighter match beats a later / looser one. 0 means "does not match".
 */
export function fuzzyScore(text: string, query: string): number {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const t = text.toLowerCase();
  const idx = t.indexOf(q);
  if (idx === -1) return 0;
  if (t === q) return 100;
  if (idx === 0) return 80 - Math.min(20, t.length - q.length);
  const before = t[idx - 1] ?? " ";
  const atWordBoundary = /[^a-z0-9]/.test(before);
  const base = atWordBoundary ? 60 : 30;
  return base - Math.min(20, idx);
}

export interface RankField<T> {
  /** Pull the searchable text for this field out of an item. */
  value: (item: T) => string;
  /** Higher weight = this field's hits matter more (e.g. name over vendor). */
  weight?: number;
}

/**
 * Score every item across `keys`, drop non-matches, sort by score
 * descending, then by the first key's value ascending (a deterministic
 * tie-break — "then label" in the spec). An empty query is "show
 * everything, unranked": the original order is returned unchanged.
 */
/**
 * One prefix, not a query language (design §9.3). `format:lv2 reverb`
 * means "LV2 plugins matching reverb"; a bare `format:clap` is the facet
 * with no further text. Anything that isn't `format:<token>` is left as
 * the ranked query so the other browsers can keep calling `rankItems`
 * unchanged.
 */
export interface ParsedQuery {
  text: string;
  format?: string;
}

export function parseSearchQuery(query: string): ParsedQuery {
  const match = query.match(/\bformat:(\S+)/i);
  if (!match) return { text: query.trim() };
  const format = match[1].toLowerCase();
  const text = query.replace(match[0], " ").replace(/\s+/g, " ").trim();
  return { text, format };
}

export function rankItems<T>(items: T[], query: string, keys: RankField<T>[]): T[] {
  if (!query.trim()) return items.slice();
  const primary = keys[0]?.value ?? (() => "");
  const scored = items.map((item) => {
    let score = 0;
    for (const k of keys) score += fuzzyScore(k.value(item), query) * (k.weight ?? 1);
    return { item, score, label: primary(item) };
  });
  return scored
    .filter((s) => s.score > 0)
    .sort((a, b) => b.score - a.score || a.label.localeCompare(b.label))
    .map((s) => s.item);
}

/** Group items by `keyOf`, preserving first-seen group order. The label
 * defaults to the key; callers with a nicer display name can remap it. */
export function groupItems<T>(items: T[], keyOf: (item: T) => string): BrowserGroup<T>[] {
  const order: string[] = [];
  const buckets = new Map<string, T[]>();
  for (const item of items) {
    const key = keyOf(item);
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = [];
      buckets.set(key, bucket);
      order.push(key);
    }
    bucket.push(item);
  }
  return order.map((key) => ({ key, label: key, items: buckets.get(key) ?? [] }));
}

/**
 * The flat, keyboard-navigable row order: every group's header, followed by
 * its items when the group isn't folded. This is the single source of
 * truth for "what does ↑/↓ skip" — a folded group's rows simply never
 * appear here.
 */
export function flattenRows<T>(
  groups: BrowserGroup<T>[],
  collapsed: ReadonlySet<string> | ((key: string) => boolean),
): BrowserRowRef[] {
  const isCollapsed = typeof collapsed === "function" ? collapsed : (k: string) => collapsed.has(k);
  const rows: BrowserRowRef[] = [];
  for (const g of groups) {
    rows.push({ kind: "group", groupKey: g.key, itemIndex: -1 });
    if (isCollapsed(g.key)) continue;
    for (let i = 0; i < g.items.length; i++) {
      rows.push({ kind: "item", groupKey: g.key, itemIndex: i });
    }
  }
  return rows;
}

/** Move `from` by `delta` rows, clamped to the flattened list's bounds.
 * Callers never need to reason about folded content themselves — that's
 * already baked into `rows` by `flattenRows`. */
export function nextIndex(rows: BrowserRowRef[], from: number, delta: number): number {
  if (rows.length === 0) return -1;
  return Math.max(0, Math.min(rows.length - 1, from + delta));
}
