/**
 * Priority+ toolbar overflow: given measured item widths and the space
 * available, decide which items stay in the row and which collapse into
 * an overflow menu. Chips with the lowest priority collapse first
 * (ties: rightmost first); dividers never enter the menu and are hidden
 * when they would lead, trail, or touch another divider.
 */

export type OverflowItem = {
  id: string;
  /** Measured on-screen width in px (border-box). */
  width: number;
  /** Higher stays in the row longer. Ignored for dividers. */
  priority: number;
  divider?: boolean;
};

export type OverflowResult = {
  /** Ids (chips and dividers) to render in the row. */
  visible: Set<string>;
  /** Non-divider ids for the menu, in display order. Empty = no menu. */
  overflow: string[];
};

/** Row width of `items` laid out with `gap` between them. */
function rowWidth(items: OverflowItem[], gap: number): number {
  if (items.length === 0) return 0;
  return items.reduce((w, it) => w + it.width, 0) + gap * (items.length - 1);
}

/** Drop dividers that would lead, trail, or sit next to another divider. */
function normalizeDividers(items: OverflowItem[]): OverflowItem[] {
  const out: OverflowItem[] = [];
  for (const it of items) {
    if (it.divider && (out.length === 0 || out[out.length - 1].divider)) continue;
    out.push(it);
  }
  while (out.length > 0 && out[out.length - 1].divider) out.pop();
  return out;
}

export function computeVisible(
  items: OverflowItem[],
  available: number,
  moreWidth: number,
  gap: number,
): OverflowResult {
  const all = normalizeDividers(items);
  if (rowWidth(all, gap) <= available) {
    return { visible: new Set(all.map((it) => it.id)), overflow: [] };
  }

  // Overflowing: the ⋯ button joins the row, so reserve room for it.
  // Drop the lowest-priority chip (ties: rightmost) until the rest fits.
  const dropOrder = items
    .map((it, index) => ({ it, index }))
    .filter(({ it }) => !it.divider)
    .sort((a, b) => a.it.priority - b.it.priority || b.index - a.index)
    .map(({ it }) => it.id);

  const dropped = new Set<string>();
  for (const id of dropOrder) {
    dropped.add(id);
    const kept = normalizeDividers(items.filter((it) => !dropped.has(it.id)));
    const width = rowWidth(kept, gap) + (kept.length > 0 ? gap : 0) + moreWidth;
    if (width <= available) {
      return {
        visible: new Set(kept.map((it) => it.id)),
        overflow: items.filter((it) => !it.divider && dropped.has(it.id)).map((it) => it.id),
      };
    }
  }

  // Not even the ⋯ button alone fits; show it anyway, everything collapses.
  return {
    visible: new Set(),
    overflow: items.filter((it) => !it.divider).map((it) => it.id),
  };
}
