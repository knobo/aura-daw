<script lang="ts">
  /**
   * PRESETS root: loaded sampler instruments (sampler_list_instruments) and
   * ZynAddSubFX bank patches (zyn_list_patches). Drag onto a MIDI track to
   * apply — instruments bind through set_track_instrument, patches load into
   * the Zyn instance the track already hosts. Click an instrument to audition
   * middle C.
   */
  import { onMount } from "svelte";
  import { instruments } from "../../state/instruments.svelte";
  import { zyn } from "../../state/zynpatches.svelte";
  import { backend } from "../../tauri";
  import { encodeLibraryDrag } from "../../utils/library";
  import LibraryRow from "./LibraryRow.svelte";

  let section = $state<"instruments" | "patches">("instruments");
  let filter = $state("");

  onMount(() => {
    void instruments.refresh();
    if (zyn.patches.length === 0) void zyn.refresh();
  });

  const shownPatches = $derived(
    filter
      ? zyn.patches.filter((p) =>
          `${p.bank} ${p.name}`.toLowerCase().includes(filter.toLowerCase()),
        )
      : zyn.patches,
  );
</script>

<div class="switch">
  <button class="s mono" class:on={section === "instruments"} onclick={() => (section = "instruments")}>
    SAMPLER
  </button>
  <button class="s mono" class:on={section === "patches"} onclick={() => (section = "patches")}>
    ZYN
  </button>
</div>

{#if section === "instruments"}
  <div class="list">
    {#each instruments.list as i (i.id)}
      <LibraryRow
        icon="◈"
        label={i.name}
        meta={`${i.regionCount}`}
        draggable
        ondragstart={(e) =>
          e.dataTransfer &&
          encodeLibraryDrag(e.dataTransfer, {
            kind: "samplerInstrument",
            instrumentId: i.id,
            name: i.name,
          })}
        onclick={() => void backend.samplerPreviewNote(i.id, 60, 100)}
      />
    {:else}
      <div class="note silk">no instruments loaded</div>
    {/each}
  </div>
{:else}
  <input class="filter" placeholder="filter patches…" bind:value={filter} />
  <div class="list">
    {#each shownPatches as p (p.path)}
      <LibraryRow
        icon="◆"
        label={p.name}
        meta={p.bank}
        draggable
        ondragstart={(e) =>
          e.dataTransfer && encodeLibraryDrag(e.dataTransfer, { kind: "zynPatch", patch: p })}
      />
    {:else}
      <div class="note silk">no Zyn patches found</div>
    {/each}
  </div>
{/if}

<style>
  .switch {
    flex: none;
    display: flex;
    gap: 4px;
    margin-bottom: 6px;
  }
  .s {
    flex: 1;
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 3px 2px;
    cursor: pointer;
  }
  .s.on {
    color: var(--cyan);
    border-color: var(--cyan);
  }
  .filter {
    flex: none;
    margin-bottom: 6px;
    padding: 4px 6px;
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    background: rgba(0, 0, 0, 0.25);
    color: var(--text);
    font: inherit;
    font-size: 11px;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .note {
    padding: 10px 4px;
    letter-spacing: 0.1em;
  }
</style>
