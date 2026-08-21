<script lang="ts">
  import type { Snippet } from "svelte";
  import { type BrowserRowRef, nextIndex, rowId } from "./browser-model";

  /**
   * The frame every browser shares: search, filter chips, a scrollable
   * body, a footer status line, and keyboard navigation over the caller's
   * flattened row order. The shell owns "which row is the keyboard cursor
   * on" (`aria-activedescendant`, roving over `rows`); the caller owns the
   * actual markup for each row — `children` gets `{ activeId, setActive }`
   * so a row can render its own selected/active styling and a click can
   * still move the keyboard cursor to match.
   */
  let {
    query = $bindable(""),
    placeholder = "search…",
    filters,
    status,
    rows,
    onToggleGroup,
    onActivate,
    anyCollapsed = false,
    onFoldAll,
    chrome = true,
    children,
    role: listRole = "tree",
  }: {
    query?: string;
    placeholder?: string;
    filters?: Snippet;
    status: string;
    rows: BrowserRowRef[];
    onToggleGroup?: (groupKey: string, expand: boolean) => void;
    onActivate?: (row: BrowserRowRef) => void;
    /** Drives the fold-all button's label: any group folded means the next
     * press unfolds, matching `lanes.anyCollapsed()` so the two surfaces
     * never disagree about what one button does. */
    anyCollapsed?: boolean;
    /** Omit to leave the button out — a browser with no groups has nothing
     * to fold. */
    onFoldAll?: (collapse: boolean) => void;
    /** False = list only. Plugin Manager draws a shared toolbar above a
     * split rack/browse, so a second search field would be a lie. */
    chrome?: boolean;
    children: Snippet<[{ activeId: string | null; setActive: (row: BrowserRowRef) => void }]>;
    role?: "listbox" | "tree";
  } = $props();

  let searchEl: HTMLInputElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();
  let activeIndex = $state(0);

  // The row list reshapes under the cursor constantly (search narrows it,
  // a group folds) — keep the index in bounds rather than pointing past
  // the end or going negative.
  $effect(() => {
    if (activeIndex > rows.length - 1) activeIndex = Math.max(0, rows.length - 1);
  });

  const activeRow = $derived(rows[activeIndex] ?? null);
  const activeId = $derived(activeRow ? rowId(activeRow) : null);

  $effect(() => {
    if (!activeId) return;
    // jsdom (and some real embeds) don't implement scrollIntoView; the
    // optional call is a no-op there rather than a throw.
    document.getElementById(activeId)?.scrollIntoView?.({ block: "nearest" });
  });

  function move(delta: number) {
    activeIndex = nextIndex(rows, activeIndex, delta);
  }

  function setActive(row: BrowserRowRef) {
    const idx = rows.findIndex(
      (r) => r.kind === row.kind && r.groupKey === row.groupKey && r.itemIndex === row.itemIndex,
    );
    if (idx >= 0) activeIndex = idx;
  }

  function onListKeydown(e: KeyboardEvent) {
    if (e.key === "/") {
      e.preventDefault();
      searchEl?.focus();
      return;
    }
    if (rows.length === 0) return;
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        move(1);
        break;
      case "ArrowUp":
        e.preventDefault();
        move(-1);
        break;
      case "Home":
        e.preventDefault();
        activeIndex = 0;
        break;
      case "End":
        e.preventDefault();
        activeIndex = rows.length - 1;
        break;
      case "ArrowRight":
        if (activeRow?.kind === "group") {
          e.preventDefault();
          onToggleGroup?.(activeRow.groupKey, true);
        }
        break;
      case "ArrowLeft":
        if (activeRow) {
          e.preventDefault();
          if (activeRow.kind === "item") {
            const headerIdx = rows.findIndex(
              (r) => r.kind === "group" && r.groupKey === activeRow.groupKey,
            );
            if (headerIdx >= 0) activeIndex = headerIdx;
          }
          onToggleGroup?.(activeRow.groupKey, false);
        }
        break;
      case "Enter":
        if (activeRow) {
          e.preventDefault();
          onActivate?.(activeRow);
        }
        break;
    }
  }

  function onSearchKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      query = "";
      listEl?.focus();
    }
  }
</script>

<div class="shell">
  {#if chrome}
  <div class="toolbar">
    <input
      bind:this={searchEl}
      bind:value={query}
      class="search mono"
      type="text"
      {placeholder}
      aria-label={placeholder}
      onkeydown={onSearchKeydown}
    />
    {#if onFoldAll}
      <button
        type="button"
        class="foldall mono"
        aria-label={anyCollapsed ? "Expand all groups" : "Collapse all groups"}
        title={anyCollapsed ? "Expand all groups" : "Collapse all groups"}
        onclick={() => onFoldAll?.(!anyCollapsed)}
      >
        <span aria-hidden="true">{anyCollapsed ? "⌄" : "⌃"}</span>
      </button>
    {/if}
    {#if filters}
      <div class="chips">{@render filters()}</div>
    {/if}
  </div>
  {/if}

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -- `role` is dynamic
       (tree/listbox), so the linter can't see it's a widget role that
       needs to hold focus for the aria-activedescendant pattern. -->
  <div
    bind:this={listEl}
    class="list"
    role={listRole}
    tabindex="0"
    aria-activedescendant={activeId ?? undefined}
    onkeydown={onListKeydown}
  >
    {@render children({ activeId, setActive })}
  </div>

  {#if chrome}
  <div class="status silk">{status}</div>
  {/if}
</div>

<style>
  .shell {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 1;
    min-height: 0;
  }

  .toolbar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .search {
    flex: 1;
    min-width: 80px;
    padding: 6px 8px;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text);
    font-size: 11px;
    outline: none;
  }
  .search:focus-visible {
    border-color: var(--cyan-dim);
  }
  .search::placeholder {
    color: var(--text-faint);
  }
  .foldall {
    flex: none;
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    font-size: 11px;
    line-height: 1;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    cursor: pointer;
  }
  .foldall:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .chips {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  /* The list carries its own focus ring instead of the global button/input
     one — a bare tree container getting the same 1px outline as a control
     reads as broken, not focused. */
  .list:focus-visible {
    outline: var(--focus-width) solid var(--cyan);
    outline-offset: -1px;
    border-radius: 5px;
  }

  .status {
    flex: none;
    padding-top: 4px;
    border-top: var(--border-width) solid var(--glass-border);
  }
</style>
