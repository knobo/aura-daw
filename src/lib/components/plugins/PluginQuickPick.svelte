<script lang="ts">
  /**
   * Ctrl+P (plan §5.3). Narrow overlay, one input, ranked list. Enter adds
   * to the selected track as its instrument; Shift+Enter adds as an insert.
   * Favourites stay first even when a query is running — you're reaching
   * for something you already use.
   */
  import { tick } from "svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { lanes } from "../../state/lanes.svelte";
  import { ui } from "../../state/ui.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import type { PluginDescriptor, TrackState } from "../../types/ipc";
  import { rankQuickPick } from "../../utils/plugin-browse";
  import { bumpFrecency, loadFrecency, saveFrecency } from "../../utils/plugin-frecency";
  import EmptyState from "../browser/EmptyState.svelte";

  let query = $state("");
  let cursor = $state(0);
  let inputEl: HTMLInputElement | undefined = $state();

  const frecency = $derived(loadFrecency());
  const ranked = $derived(
    rankQuickPick({
      descriptors: plugins.descriptors,
      favorites: plugins.catalog.favorites,
      recents: plugins.catalog.recents,
      query,
      tags: plugins.catalog.tags,
      frecency,
    }),
  );

  $effect(() => {
    ranked;
    if (cursor > ranked.length - 1) cursor = Math.max(0, ranked.length - 1);
  });

  const active = $derived(ranked[cursor] ?? null);

  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  const insertTracks = $derived(
    project.tracks.filter((t) => t.kind === "audio" || t.kind === "midi"),
  );

  function preferred(pool: TrackState[]): TrackState | undefined {
    const selected = pool.filter((t) => lanes.isSelected(t.id));
    return (selected.length ? selected : pool)[0];
  }

  $effect(() => {
    void tick().then(() => inputEl?.focus());
  });

  function close() {
    ui.pluginPickerOpen = false;
  }

  async function commit(asInsert: boolean) {
    const d = active;
    if (!d) return;
    try {
      if (asInsert) {
        const t = preferred(insertTracks);
        if (!t) {
          toasts.error("NO TRACK", "Add an audio or MIDI track first.");
          return;
        }
        await plugins.insertEffect(t.id, d.uid);
      } else {
        await plugins.instantiate(d.uid, preferred(midiTracks)?.id ?? null);
      }
      saveFrecency(bumpFrecency(loadFrecency(), d.uid, Date.now()));
      close();
    } catch (err) {
      toasts.error("PLUGIN FAILED", String(err));
    }
  }

  function onKeydown(e: KeyboardEvent) {
    switch (e.key) {
      case "Escape":
        e.preventDefault();
        close();
        break;
      case "ArrowDown":
        e.preventDefault();
        if (ranked.length) cursor = Math.min(ranked.length - 1, cursor + 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        if (ranked.length) cursor = Math.max(0, cursor - 1);
        break;
      case "Enter":
        e.preventDefault();
        void commit(e.shiftKey);
        break;
    }
  }

  function pick(d: PluginDescriptor, asInsert: boolean) {
    cursor = ranked.findIndex((x) => x.uid === d.uid);
    void commit(asInsert);
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions — the dialog is the
     keydown root so Enter/Esc work from the input too (they bubble). -->
<div
  class="scrim"
  role="presentation"
  onclick={close}
  onkeydown={(e) => {
    if (e.key === "Escape") close();
  }}
>
  <div
    class="panel glass"
    role="dialog"
    aria-modal="true"
    aria-label="Add plugin"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={onKeydown}
  >
    <input
      bind:this={inputEl}
      bind:value={query}
      class="search mono"
      type="text"
      placeholder="add plugin…  ↵ instrument   ⇧↵ insert"
      aria-label="Search plugins"
    />
    <div class="list" role="listbox" aria-label="Matching plugins">
      {#if ranked.length === 0}
        <EmptyState
          message={plugins.descriptors.length === 0
            ? "No plugins scanned yet."
            : `No match for "${query}".`}
        />
      {/if}
      {#each ranked as d, i (d.uid)}
        <button
          type="button"
          class="opt"
          class:on={i === cursor}
          role="option"
          aria-selected={i === cursor}
          onclick={() => pick(d, false)}
        >
          <span class="badge mono {d.format}">{d.format}</span>
          <span class="name">{d.name}</span>
          {#if plugins.isFavorite(d.uid)}<span class="star" aria-hidden="true">★</span>{/if}
          <span class="dest silk">
            {#if d.isInstrument}
              → {preferred(midiTracks)?.name ?? "instance"} (instrument)
            {:else}
              → {preferred(insertTracks)?.name ?? "—"} (insert)
            {/if}
          </span>
          <span class="vendor silk">{d.vendor ?? ""}</span>
        </button>
      {/each}
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 40;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 14vh;
    background: rgb(var(--bg-0-rgb) / 0.45);
  }
  .panel {
    width: min(520px, calc(100vw - 32px));
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    border-radius: 8px;
  }
  .search {
    width: 100%;
    padding: 8px 10px;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text);
    font-size: 13px;
    outline: none;
  }
  .search:focus-visible {
    border-color: var(--cyan-dim);
  }
  .list {
    max-height: 360px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 8px;
    border: none;
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .opt.on,
  .opt:hover {
    background: rgb(var(--cyan-rgb) / 0.08);
  }
  .name {
    flex: none;
    font-size: 12px;
  }
  .dest {
    flex: none;
    font-size: 9px;
    letter-spacing: 0.06em;
    color: var(--cyan);
  }
  .vendor {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-faint);
  }
  .star {
    color: var(--amber);
    font-size: 11px;
  }
  .badge {
    flex: none;
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    padding: 2px 5px;
    border-radius: 3px;
    border: var(--border-width) solid;
  }
  .badge.clap {
    color: var(--cyan);
    border-color: rgb(var(--cyan-rgb) / 0.4);
    background: rgb(var(--cyan-rgb) / 0.08);
  }
  .badge.lv2 {
    color: var(--violet);
    border-color: rgb(var(--violet-rgb) / 0.4);
    background: rgb(var(--violet-rgb) / 0.08);
  }
</style>
