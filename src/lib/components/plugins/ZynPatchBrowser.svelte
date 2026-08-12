<script lang="ts">
  /**
   * ZynAddSubFX patch browser: the full .xiz bank list (zyn_list_patches,
   * ~1318 entries) grouped by bank, searchable, and windowed — only the
   * visible rows render DOM (constraint §10.12), so the list stays light.
   * Clicking a patch loads it (zyn_load_patch) and the store re-binds the
   * owning track so the change is audible without a restart.
   */
  import { zyn } from "../../state/zynpatches.svelte";
  import type { ZynPatch } from "../../types/ipc";

  let search = $state("");
  let scroller: HTMLDivElement | undefined = $state();
  let scrollTop = $state(0);
  let viewH = $state(400);

  const ROW_H = 24;
  const OVERSCAN = 8;

  type Row = { kind: "bank"; bank: string; count: number } | { kind: "patch"; patch: ZynPatch };

  const rows = $derived.by((): Row[] => {
    const q = search.trim().toLowerCase();
    const out: Row[] = [];
    let currentBank = "";
    let headerAt = -1;
    for (const p of zyn.patches) {
      if (q && !p.name.toLowerCase().includes(q) && !p.bank.toLowerCase().includes(q)) continue;
      if (p.bank !== currentBank) {
        currentBank = p.bank;
        headerAt = out.length;
        out.push({ kind: "bank", bank: p.bank, count: 0 });
      }
      out.push({ kind: "patch", patch: p });
      (out[headerAt] as { count: number }).count++;
    }
    return out;
  });

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
  const loadedPath = $derived(inst ? zyn.loaded[inst.id] : undefined);

  async function pick(patch: ZynPatch) {
    if (!inst || zyn.busyPath) return;
    await zyn.load(inst.id, patch);
  }
</script>

<div class="panel">
  <div class="head">
    <button class="back mono" title="Back to the plugin browser" onclick={() => zyn.close()}>‹</button>
    <span class="title" title={inst?.uid}>{inst?.name ?? "Zyn"} · patches</span>
    <span class="count silk">{zyn.patches.length}</span>
  </div>

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
    <div class="note silk">{zyn.patches.length === 0 ? "no zyn banks found on this machine" : "no match — clear the search"}</div>
  {/if}

  <div class="list" bind:this={scroller} onscroll={() => (scrollTop = scroller?.scrollTop ?? 0)}>
    <div class="spacer" style:height="{rows.length * ROW_H}px">
      <div class="window" style:transform="translateY({first * ROW_H}px)">
        {#each slice as row, i (first + i)}
          {#if row.kind === "bank"}
            <div class="bank mono" style:height="{ROW_H}px">
              <span class="bname">▤ {row.bank.replace(/_/g, " ")}</span>
              <span class="bcount silk">{row.count}</span>
            </div>
          {:else}
            <button
              class="patch mono"
              class:loaded={loadedPath === row.patch.path}
              class:busy={zyn.busyPath === row.patch.path}
              style:height="{ROW_H}px"
              title="{row.patch.bank} / {row.patch.name} — load into {inst?.name ?? 'Zyn'}"
              onclick={() => pick(row.patch)}
            >
              <span class="pnum silk">{String(row.patch.program).padStart(3, "0")}</span>
              <span class="pname">{row.patch.name}</span>
              {#if zyn.busyPath === row.patch.path}
                <span class="spin">◌</span>
              {:else if loadedPath === row.patch.path}
                <span class="check">⌁ live</span>
              {/if}
            </button>
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
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.6);
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
  .count {
    letter-spacing: 0.1em;
  }

  .search {
    flex: none;
    background: rgba(5, 7, 13, 0.7);
    color: var(--text);
    border: 1px solid var(--glass-border);
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
    border: 1px solid var(--glass-border);
    border-radius: 6px;
    background: rgba(5, 7, 13, 0.5);
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
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 0 9px;
    font-size: 9px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--violet);
    background: rgba(157, 123, 255, 0.06);
    border-bottom: 1px solid rgba(157, 123, 255, 0.12);
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
    background: rgba(82, 229, 255, 0.06);
    color: var(--text);
  }
  .patch.loaded {
    color: var(--cyan);
    background: rgba(82, 229, 255, 0.07);
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
    color: #5cf2b8;
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
