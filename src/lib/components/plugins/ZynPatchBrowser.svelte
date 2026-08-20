<script lang="ts">
  /**
   * ZynAddSubFX patch browser: the full .xiz bank list (zyn_list_patches,
   * ~1318 entries) grouped by bank, searchable, foldable per bank, and
   * windowed — only the visible rows render DOM (constraint §10.12) — so
   * the list stays light even with every bank expanded.
   *
   * Ranking/grouping/fold-flattening come from the shared
   * `components/browser/browser-model.ts`, but the row markup is hand-
   * rolled rather than `BrowserShell`/`BrowserRow`: those mount every row
   * unconditionally, which is exactly what this file exists to avoid at
   * this row count. Search box and status line intentionally mirror
   * `BrowserShell`'s vocabulary so it still *reads* as the same browser.
   *
   * Clicking a patch loads it (zyn_load_patch) and the store re-binds the
   * owning track so the change is audible without a restart.
   */
  import { zyn } from "../../state/zynpatches.svelte";
  import type { ZynPatch } from "../../types/ipc";
  import PluginConnectionBadge from "./PluginConnectionBadge.svelte";
  import { loadFolds, saveFolds } from "../browser/browser-folds";
  import { type BrowserGroup, flattenRows, groupItems, rankItems } from "../browser/browser-model";
  import EmptyState from "../browser/EmptyState.svelte";

  const FOLDS_KEY = "zyn-patches";

  let search = $state("");
  let scroller: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewH = $state(400);
  let collapsed = $state(loadFolds(FOLDS_KEY));

  const ROW_H = 24;
  const OVERSCAN = 8;

  const ranked = $derived(
    rankItems(zyn.patches, search, [
      { value: (p: ZynPatch) => p.name, weight: 2 },
      { value: (p: ZynPatch) => p.bank, weight: 1 },
    ]),
  );
  const groups = $derived(groupItems(ranked, (p) => p.bank));
  const groupByKey = $derived(new Map(groups.map((g) => [g.key, g] as const)));
  const rows = $derived(flattenRows(groups, collapsed));

  function toggleBank(key: string) {
    const next = new Set(collapsed);
    if (!next.delete(key)) next.add(key);
    collapsed = next;
    saveFolds(FOLDS_KEY, next);
  }

  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN));
  const visibleCount = $derived(Math.ceil(viewH / ROW_H) + OVERSCAN * 2);
  const slice = $derived(rows.slice(first, first + visibleCount));

  $effect(() => {
    if (!scroller) return;
    const el = scroller;
    const ro = new ResizeObserver(() => (viewH = el.clientHeight));
    ro.observe(el);
    viewH = el.clientHeight;
    return () => ro.disconnect();
  });

  const inst = $derived(zyn.openInstance);
  const loadedPath = $derived(inst ? zyn.loaded[inst.id]?.path : undefined);

  async function pick(patch: ZynPatch) {
    if (!inst || zyn.busyPath) return;
    await zyn.load(inst.id, patch);
  }

  function bankOf(key: string): BrowserGroup<ZynPatch> | undefined {
    return groupByKey.get(key);
  }
</script>

<div class="panel">
  <div class="head">
    <button class="back mono" title="Back to the plugin browser" onclick={() => zyn.close()}>‹</button>
    <span class="title" title={inst?.uid}>{inst?.name ?? "Zyn"} · patches</span>
    <span class="count silk">{zyn.patches.length}</span>
  </div>
  {#if inst}
    <div class="connrow">
      <PluginConnectionBadge instanceId={inst.id} />
    </div>
  {/if}

  <input
    type="text"
    class="search mono"
    placeholder="⌕ search {zyn.patches.length} patches…"
    bind:value={search}
    spellcheck="false"
  />

  {#if zyn.error}
    <div class="err silk" role="alert">{zyn.error}</div>
  {/if}
  {#if inst && inst.status !== "active"}
    <div class="note silk">instance is “{inst.status}” — patches apply once the dsp is live</div>
  {/if}

  {#if zyn.loading}
    <div class="note silk">reading banks…</div>
  {:else if rows.length === 0}
    <EmptyState
      message={zyn.patches.length === 0
        ? "No Zyn banks found on this machine."
        : `No match for "${search}" — clear the search.`}
    />
  {/if}

  <div class="list" bind:this={scroller} onscroll={() => (scrollTop = scroller?.scrollTop ?? 0)}>
    <div class="spacer" style:height="{rows.length * ROW_H}px">
      <div class="window" style:transform="translateY({first * ROW_H}px)">
        {#each slice as row, i (first + i)}
          {#if row.kind === "group"}
            {@const bank = bankOf(row.groupKey)}
            <button
              class="bank mono"
              style:height="{ROW_H}px"
              aria-expanded={!collapsed.has(row.groupKey)}
              title="{collapsed.has(row.groupKey) ? 'Expand' : 'Collapse'} {row.groupKey}"
              onclick={() => toggleBank(row.groupKey)}
            >
              <span class="disclosure" aria-hidden="true">{collapsed.has(row.groupKey) ? "▸" : "▾"}</span>
              <span class="bname">{row.groupKey.replace(/_/g, " ")}</span>
              <span class="bcount silk">{bank?.items.length ?? 0}</span>
            </button>
          {:else}
            {@const patch = bankOf(row.groupKey)?.items[row.itemIndex]}
            {#if patch}
              <button
                class="patch mono"
                class:loaded={loadedPath === patch.path}
                class:busy={zyn.busyPath === patch.path}
                style:height="{ROW_H}px"
                title="{patch.bank} / {patch.name} — load into {inst?.name ?? 'Zyn'}"
                onclick={() => pick(patch)}
              >
                <span class="pnum silk">{String(patch.program).padStart(3, "0")}</span>
                <span class="pname">{patch.name}</span>
                {#if zyn.busyPath === patch.path}
                  <span class="spin">◌</span>
                {:else if loadedPath === patch.path}
                  <span class="check">⌁ live</span>
                {/if}
              </button>
            {/if}
          {/if}
        {/each}
      </div>
    </div>
  </div>

  <div class="foot silk">
    loading a patch re-binds its track so the new sound reaches the engine
  </div>
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 1;
    min-height: 0;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  .back {
    width: 24px;
    height: 24px;
    line-height: 1;
    font-size: 15px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-2-rgb) / 0.6);
    color: var(--text-dim);
    cursor: pointer;
  }
  .back:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 500;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .connrow {
    flex: none;
  }
  .count {
    letter-spacing: 0.1em;
  }

  .search {
    flex: none;
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 10px;
    padding: 6px 8px;
  }
  .search:focus {
    border-color: var(--violet);
  }

  .note,
  .err {
    flex: none;
  }
  .err {
    color: var(--red);
  }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 6px;
    background: rgb(var(--bg-0-rgb) / 0.5);
  }
  .spacer {
    position: relative;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
    will-change: transform;
  }

  .bank {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 9px;
    font-size: 9px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--violet);
    background: rgb(var(--violet-rgb) / 0.06);
    border: none;
    border-bottom: var(--border-width) solid rgb(var(--violet-rgb) / 0.12);
    cursor: pointer;
    text-align: left;
  }
  .bank:hover {
    background: rgb(var(--violet-rgb) / 0.12);
  }
  .disclosure {
    flex: none;
    width: 8px;
    font-size: 8px;
  }
  .bname {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .patch {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 9px 0 16px;
    font-size: 10px;
    border: none;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    text-align: left;
  }
  .patch:hover {
    background: rgb(var(--cyan-rgb) / 0.06);
    color: var(--text);
  }
  .patch.loaded {
    color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.07);
  }
  .patch.busy {
    color: var(--amber);
  }
  .pnum {
    letter-spacing: 0.08em;
    flex: none;
  }
  .pname {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .check {
    font-size: 8px;
    letter-spacing: 0.12em;
    color: var(--green);
  }
  .spin {
    animation: spin-pulse 0.9s linear infinite;
  }
  @keyframes spin-pulse {
    50% {
      opacity: 0.35;
    }
  }

  .foot {
    flex: none;
    letter-spacing: 0.08em;
  }
</style>
