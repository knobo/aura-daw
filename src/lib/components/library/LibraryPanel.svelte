<script lang="ts">
  /**
   * Library & browser panel (Track E): three roots — SAMPLES (folders on
   * disk), CLIPS (the open project's clips), PRESETS (instruments and
   * patches). The panel is chrome: it lists, auditions and drags; every
   * mutation it can cause is an existing, channel-routed command, so a drop
   * is undoable for free.
   *
   * This shell is a pure delegator (each root owns its own list, search and
   * grouping) so it doesn't mount `components/browser/BrowserShell` itself
   * — SamplesRoot and PresetsRoot do that individually. The root switch
   * below is restyled onto the same filter-chip vocabulary `BrowserShell`
   * uses elsewhere, so it reads as one family even though it isn't one
   * of its instances.
   */
  import { library, type LibraryRoot } from "../../state/library.svelte";
  import SamplesRoot from "./SamplesRoot.svelte";
  import ClipsRoot from "./ClipsRoot.svelte";
  import PresetsRoot from "./PresetsRoot.svelte";

  const ROOTS: { id: LibraryRoot; label: string }[] = [
    { id: "samples", label: "SAMPLES" },
    { id: "clips", label: "CLIPS" },
    { id: "presets", label: "PRESETS" },
  ];
</script>

<div class="panel">
  <div class="roots" role="tablist">
    {#each ROOTS as r (r.id)}
      <button
        class="root mono"
        class:on={library.root === r.id}
        role="tab"
        aria-selected={library.root === r.id}
        onclick={() => (library.root = r.id)}
      >
        {r.label}
      </button>
    {/each}
  </div>

  <div class="body">
    {#if library.root === "samples"}
      <SamplesRoot />
    {:else if library.root === "clips"}
      <ClipsRoot />
    {:else if library.root === "presets"}
      <PresetsRoot />
    {/if}
  </div>
</div>

<style>
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .roots {
    flex: none;
    display: flex;
    gap: 4px;
  }
  .root {
    flex: 1;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 4px 2px;
    cursor: pointer;
  }
  .root.on {
    color: var(--cyan);
    border-color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.06);
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
</style>
