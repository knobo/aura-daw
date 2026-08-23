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
   * Clicking a patch loads it (zyn_load_patch). Double-click also plays C3
   * through the bound track so the new sound can be heard without the
   * piano roll — load is still a project edit; the note is not.
   */
  import { zyn } from "../../state/zynpatches.svelte";
  // Aliased: this file already declares a local `audition(patch)` helper
  // (the hold-to-preview function below) — importing the shared store
  // under its usual name would collide with that declaration.
  import { audition as auditionStore } from "../../state/audition.svelte";
  import type { ZynPatch } from "../../types/ipc";
  import PluginConnectionBadge from "./PluginConnectionBadge.svelte";
  import { FoldController } from "../browser/fold-controller.svelte";
  import { type BrowserGroup, flattenRows, groupItems, rankItems } from "../browser/browser-model";
  import EmptyState from "../browser/EmptyState.svelte";

  const FAV_KEY = "aura.zyn.patch-favorites";

  function loadPatchFavs(): Set<string> {
    try {
      const raw = localStorage.getItem(FAV_KEY);
      if (!raw) return new Set();
      const parsed: unknown = JSON.parse(raw);
      if (!Array.isArray(parsed)) return new Set();
      return new Set(parsed.filter((x): x is string => typeof x === "string"));
    } catch {
      return new Set();
    }
  }

  function savePatchFavs(favs: ReadonlySet<string>) {
    try {
      localStorage.setItem(FAV_KEY, JSON.stringify([...favs]));
    } catch {
      /* quota — starring still works this session */
    }
  }

  let search = $state("");
  let favOnly = $state(false);
  let favorites = $state(loadPatchFavs());
  let scroller: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewH = $state(400);
  const folds = new FoldController("zyn-patches");
  $effect(() => folds.syncQuery(search));
  const collapsed = $derived(folds.collapsed);

  const ROW_H = 24;
  const OVERSCAN = 8;

  const ranked = $derived(
    rankItems(zyn.patches, search, [
      { value: (p: ZynPatch) => p.name, weight: 2 },
      { value: (p: ZynPatch) => p.bank, weight: 1 },
    ]).filter((p) => !favOnly || favorites.has(p.path)),
  );
  const groups = $derived(groupItems(ranked, (p) => p.bank));
  const groupKeys = $derived(groups.map((g) => g.key));
  const groupByKey = $derived(new Map(groups.map((g) => [g.key, g] as const)));
  const rows = $derived(flattenRows(groups, collapsed));

  function toggleBank(key: string) {
    folds.toggle(key, collapsed.has(key));
  }

  function toggleFav(path: string) {
    const next = new Set(favorites);
    if (!next.delete(path)) next.add(path);
    favorites = next;
    savePatchFavs(next);
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

  async function audition(patch: ZynPatch) {
    if (!inst || inst.status !== "active") return;
    await zyn.audition(inst.id, patch);
  }

  let holdTimer: ReturnType<typeof setTimeout> | null = null;
  let holding = false;

  function onPatchPointerDown(patch: ZynPatch, e: PointerEvent) {
    if (e.button !== 0 || !inst || inst.status !== "active") return;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    holdTimer = setTimeout(() => {
      holdTimer = null;
      holding = true;
      void audition(patch);
    }, 160);
  }

  function onPatchPointerUp() {
    if (holdTimer) {
      clearTimeout(holdTimer);
      holdTimer = null;
    }
    if (holding) {
      holding = false;
      void zyn.previewUp();
    }
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

  <div class="tools">
    <input
      type="text"
      class="search mono"
      placeholder="⌕ search {zyn.patches.length} patches…"
      bind:value={search}
      spellcheck="false"
    />
    <button
      type="button"
      class="foldall mono"
      aria-label={folds.anyCollapsed(groupKeys) ? "Expand all groups" : "Collapse all groups"}
      title={folds.anyCollapsed(groupKeys) ? "Expand all groups" : "Collapse all groups"}
      onclick={() => folds.setAll(groupKeys, !folds.anyCollapsed(groupKeys))}
    >
      <span aria-hidden="true">{folds.anyCollapsed(groupKeys) ? "⌄" : "⌃"}</span>
    </button>
    <button
      type="button"
      class="chip mono"
      class:on={favOnly}
      aria-pressed={favOnly}
      title="Favourites only"
      onclick={() => (favOnly = !favOnly)}
    >
      ★
    </button>
  </div>

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
              <div
                class="patch mono"
                class:loaded={loadedPath === patch.path}
                class:busy={zyn.busyPath === patch.path}
                style:height="{ROW_H}px"
              >
                <button
                  type="button"
                  class="pload"
                  title="{patch.bank} / {patch.name} — click to load, hold to hear C3"
                  onclick={() => pick(patch)}
                  onpointerdown={(e) => onPatchPointerDown(patch, e)}
                  onpointerup={onPatchPointerUp}
                  onpointercancel={onPatchPointerUp}
                  ondblclick={() => {
                    if (!auditionStore.enabled) return;
                    if (!inst || inst.status !== "active") {
                      void auditionStore.play({
                        kind: "silent",
                        reason: "no live Zyn instance to audition through",
                      });
                      return;
                    }
                    void zyn.audition(inst.id, patch);
                    setTimeout(() => void zyn.previewUp(), 700);
                  }}
                >
                  <span class="pnum silk">{String(patch.program).padStart(3, "0")}</span>
                  <span class="pname">{patch.name}</span>
                  {#if zyn.busyPath === patch.path}
                    <span class="spin">◌</span>
                  {:else if loadedPath === patch.path}
                    <span class="check">⌁ live</span>
                  {/if}
                </button>
                <button
                  type="button"
                  class="star"
                  class:on={favorites.has(patch.path)}
                  aria-pressed={favorites.has(patch.path)}
                  title={favorites.has(patch.path) ? "Remove from favourites" : "Add to favourites"}
                  onclick={() => toggleFav(patch.path)}
                >
                  {favorites.has(patch.path) ? "★" : "☆"}
                </button>
              </div>
            {/if}
          {/if}
        {/each}
      </div>
    </div>
  </div>

  <div class="foot silk">
    click loads · hold to hear C3 on the bound track
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

  .tools {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
  }
  .search {
    flex: 1;
    min-width: 0;
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
  .foldall {
    flex: none;
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    font-size: 11px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    cursor: pointer;
  }
  .foldall:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .chip {
    flex: none;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    padding: 3px 8px;
    cursor: pointer;
  }
  .chip.on {
    color: var(--amber);
    border-color: var(--amber);
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
    padding: 0 4px 0 16px;
    background: transparent;
    color: var(--text-dim);
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
  .pload {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 100%;
    padding: 0;
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    cursor: pointer;
    text-align: left;
  }
  .star {
    flex: none;
    width: 18px;
    height: 18px;
    padding: 0;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 11px;
  }
  .star.on,
  .star:hover {
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
