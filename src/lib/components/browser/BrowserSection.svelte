<script lang="ts">
  import type { Snippet } from "svelte";

  /**
   * A collapsible group: disclosure triangle, label, count badge. Folding
   * is the caller's to own (a prop + a callback) — `BrowserShell` doesn't
   * know which localStorage key a given browser uses, and a browser may
   * want folding to interact with something else (e.g. clearing a search).
   */
  let {
    id,
    label,
    count,
    expanded,
    active = false,
    onToggle,
    children,
  }: {
    id: string;
    label: string;
    count: number;
    expanded: boolean;
    active?: boolean;
    onToggle: () => void;
    children: Snippet;
  } = $props();
</script>

<div class="section">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    {id}
    class="head"
    class:active
    role="treeitem"
    aria-expanded={expanded}
    aria-selected="false"
    tabindex="-1"
    onclick={onToggle}
    onkeydown={(e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        onToggle();
      }
    }}
  >
    <span class="disclosure mono" aria-hidden="true">{expanded ? "▾" : "▸"}</span>
    <span class="label silk">{label}</span>
    <span class="count mono">{count}</span>
  </div>
  {#if expanded}
    <div class="body" role="group" aria-labelledby={id}>
      {@render children()}
    </div>
  {/if}
</div>

<style>
  .head {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 6px 8px;
    border-radius: 5px;
    cursor: pointer;
  }
  .head:hover {
    background: rgb(var(--edge-rgb) / 0.06);
  }
  .head.active {
    outline: var(--focus-width) solid var(--cyan-dim);
    outline-offset: -1px;
  }
  .disclosure {
    flex: none;
    width: 10px;
    color: var(--text-faint);
    font-size: 9px;
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    flex: none;
    font-size: 9px;
    color: var(--text-faint);
  }
  .body {
    display: flex;
    flex-direction: column;
    padding-left: 14px;
  }
</style>
