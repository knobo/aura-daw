<script lang="ts">
  import { onMount } from "svelte";
  import { historyBrowser } from "../../state/history.svelte";
  import { projectops } from "../../state/projectops.svelte";

  onMount(() => void historyBrowser.load());

  const bytes = (n: number) =>
    n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / 1024 / 1024).toFixed(1)} MiB`;

  async function step(direction: "undo" | "redo") {
    if (direction === "undo") await projectops.undo();
    else await projectops.redo();
    await historyBrowser.load();
  }
</script>

<section class="history" aria-label="Version history">
  <header>
    <div>
      <span class="silk">VERSION GRAPH</span>
      <h2>History</h2>
    </div>
    <button class="refresh mono" disabled={historyBrowser.loading} onclick={() => void historyBrowser.load()}>
      {historyBrowser.loading ? "…" : "↻"}
    </button>
  </header>

  {#if !historyBrowser.available}
    <p class="empty mono">History is available in the desktop app.</p>
  {:else if historyBrowser.overview}
    <div class="actions">
      <button disabled={!historyBrowser.overview.undoDepth} onclick={() => void step("undo")}>
        UNDO <span class="mono">{historyBrowser.overview.undoDepth}</span>
      </button>
      <button disabled={!historyBrowser.overview.redoDepth} onclick={() => void step("redo")}>
        REDO <span class="mono">{historyBrowser.overview.redoDepth}</span>
      </button>
    </div>

    <div class="stats mono">
      <span>{historyBrowser.overview.versions.length} revisions</span>
      <span>{bytes(historyBrowser.overview.retainedBytes)}</span>
      <span>{historyBrowser.overview.materialized} snapshots · {historyBrowser.overview.replayOnly} replay</span>
    </div>

    {#if historyBrowser.error}<p class="error mono">{historyBrowser.error}</p>{/if}

    <div class="body">
      <div class="versions" role="listbox" aria-label="Retained revisions">
        {#each historyBrowser.overview.versions as version (version.rev)}
          <button
            class:selected={historyBrowser.selectedRev === version.rev}
            role="option"
            aria-selected={historyBrowser.selectedRev === version.rev}
            onclick={() => void historyBrowser.select(version.rev)}
          >
            <span class="rev mono">r{version.rev}</span>
            <span class="kind">{version.materialized ? "SNAPSHOT" : "REPLAY"}</span>
            <span class="size mono">{bytes(version.chargedBytes)}</span>
          </button>
        {:else}
          <p class="empty mono">Make an edit to start version history.</p>
        {/each}
      </div>

      {#if historyBrowser.detail}
        <dl class="detail mono">
          <div><dt>REVISION</dt><dd>{historyBrowser.detail.rev}</dd></div>
          <div><dt>TRACKS</dt><dd>{historyBrowser.detail.trackCount}</dd></div>
          <div><dt>AUDIO CLIPS</dt><dd>{historyBrowser.detail.audioClipCount}</dd></div>
          <div><dt>MIDI CLIPS</dt><dd>{historyBrowser.detail.midiClipCount}</dd></div>
          <div><dt>AUTOMATION</dt><dd>{historyBrowser.detail.automationLaneCount}</dd></div>
        </dl>
      {/if}
    </div>
    <p class="note">Read-only revisions. Use Undo or Redo to change the project.</p>
  {/if}
</section>

<style>
  .history { display: flex; min-height: 0; height: 100%; flex-direction: column; gap: 12px; }
  header, .actions, .stats, .versions button, .detail div { display: flex; align-items: center; }
  header { justify-content: space-between; }
  h2 { margin: 2px 0 0; font-size: 18px; color: var(--text); }
  button { border: 1px solid var(--glass-border); background: var(--bg-raised); color: var(--text-dim); cursor: pointer; }
  button:hover:not(:disabled), button.selected { color: var(--cyan); border-color: var(--cyan-dim); }
  button:disabled { opacity: .4; cursor: default; }
  .refresh { width: 30px; height: 30px; border-radius: 4px; }
  .actions { gap: 8px; }
  .actions button { flex: 1; display: flex; justify-content: space-between; padding: 8px 10px; border-radius: 4px; }
  .stats { flex-wrap: wrap; gap: 5px 12px; color: var(--text-faint); font-size: 10px; }
  .body { min-height: 0; display: grid; grid-template-columns: minmax(150px, 1fr) minmax(115px, .75fr); gap: 10px; }
  .versions { min-height: 0; overflow: auto; display: flex; flex-direction: column; gap: 4px; }
  .versions button { width: 100%; gap: 7px; padding: 7px; border-radius: 4px; text-align: left; }
  .rev { color: var(--text); }
  .kind { font-size: 9px; letter-spacing: .1em; }
  .size { margin-left: auto; color: var(--text-faint); font-size: 9px; }
  .detail { margin: 0; padding: 8px; align-self: start; border: 1px solid var(--glass-border); border-radius: 5px; }
  .detail div { justify-content: space-between; gap: 12px; padding: 5px 0; }
  dt { color: var(--text-faint); font-size: 9px; }
  dd { margin: 0; color: var(--text); }
  .empty, .error, .note { color: var(--text-faint); font-size: 10px; }
  .error { color: var(--red); }
  .note { margin-top: auto; }
</style>
