<script lang="ts">
  import type { Snippet } from "svelte";
  import FavoriteToggle from "./FavoriteToggle.svelte";

  /**
   * One row: format badge, primary label, secondary meta, an optional
   * favourite star, a trailing action slot. `selected` (the user chose
   * this row — it's bound, it's loaded) and `active` (the keyboard cursor
   * is here right now) are independent and look different on purpose —
   * conflating them reads as "wait, did clicking this DO something?".
   *
   * A `<div>`, not a `<button>`: WebKitGTK (Tauri on Linux) refuses to
   * start a native drag from inside a form control even with
   * draggable="true" — see `LibraryRow.svelte`, the precedent this
   * follows.
   */
  let {
    id,
    label,
    meta = "",
    badge,
    favorite,
    onFavorite,
    itemLabel,
    selected = false,
    active = false,
    draggable = false,
    onclick,
    onpointerdown,
    onpointerup,
    onpointercancel,
    ondragstart,
    actions,
  }: {
    id: string;
    label: string;
    meta?: string;
    badge?: Snippet;
    /** Omit both to leave favouriting unwired — the row still renders fine
     * without a star when the caller has no favourites source of truth. */
    favorite?: boolean;
    onFavorite?: () => void;
    itemLabel?: string;
    selected?: boolean;
    active?: boolean;
    draggable?: boolean;
    onclick?: () => void;
    onpointerdown?: (e: PointerEvent) => void;
    onpointerup?: (e: PointerEvent) => void;
    onpointercancel?: (e: PointerEvent) => void;
    ondragstart?: (e: DragEvent) => void;
    actions?: Snippet;
  } = $props();
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  {id}
  class="row"
  class:selected
  class:active
  role="treeitem"
  aria-selected={selected}
  tabindex="-1"
  {draggable}
  title={label}
  onclick={() => onclick?.()}
  onpointerdown={(e) => onpointerdown?.(e)}
  onpointerup={(e) => onpointerup?.(e)}
  onpointercancel={(e) => onpointercancel?.(e)}
  onkeydown={(e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onclick?.();
    }
  }}
  ondragstart={(e) => ondragstart?.(e)}
>
  {#if badge}<span class="badge">{@render badge()}</span>{/if}
  <span class="label">{label}</span>
  {#if meta}<span class="meta silk">{meta}</span>{/if}
  {#if onFavorite}
    <FavoriteToggle favorite={!!favorite} itemLabel={itemLabel ?? label} onclick={onFavorite} />
  {/if}
  {#if actions}<span class="actions">{@render actions()}</span>{/if}
</div>

<style>
  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 5px 8px;
    width: 100%;
    padding: 6px 8px;
    border-radius: 5px;
    color: var(--text-dim);
    cursor: pointer;
  }
  .row:hover {
    background: rgb(var(--edge-rgb) / 0.08);
    color: var(--text);
  }
  .row.selected {
    color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.08);
  }
  .row.active {
    outline: var(--focus-width) solid var(--cyan-dim);
    outline-offset: -1px;
  }

  .badge {
    flex: none;
    display: flex;
    align-items: center;
  }
  .label {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    flex: none;
    letter-spacing: 0.06em;
    color: var(--text-faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 40%;
  }
  /* Own full-width line rather than trailing inline: rows range from a
     single icon-button to a mini keyboard + a track-assign select, and
     forcing all of that onto one line is how the original per-browser
     cards ended up cramped. */
  .actions {
    flex: 1 0 100%;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
  }
</style>
