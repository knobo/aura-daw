<script lang="ts">
  import { onMount } from "svelte";
  import { backend } from "../../tauri";
  import { historyBrowser } from "../../state/history.svelte";
  import { projectops } from "../../state/projectops.svelte";

  // The dock also requests a load when HISTORY becomes active. Concurrent
  // requests are coalesced; this fallback covers keyboard/direct mounting.
  onMount(() => {
    void historyBrowser.load();
    // Keep an open browser live. `project://changed` is the common commit
    // announcement for user edits, agent edits, Undo and Redo. `refresh`
    // queues a fresh read if an earlier overview request is still in flight.
    return backend.on("project://changed", () => void historyBrowser.refresh());
  });

  const bytes = (n: number) =>
    n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(1)} KiB` : `${(n / 1024 / 1024).toFixed(1)} MiB`;

  function title(label: string) {
    const clean = label.trim();
    if (!clean) return "Project edit";
    return clean.charAt(0).toUpperCase() + clean.slice(1);
  }

  async function step(direction: "undo" | "redo") {
    if (direction === "undo") await projectops.undo();
    else await projectops.redo();
    await historyBrowser.refresh();
  }
</script>

<section class="history" aria-label="Version history">
  <header>
    <div>
      <span class="silk">PROJECT HISTORY</span>
      <h2>Undo &amp; history</h2>
    </div>
    <button
      class="refresh mono"
      disabled={historyBrowser.loading}
      aria-label="Refresh history"
      onclick={() => void historyBrowser.refresh()}
    >
      {historyBrowser.loading ? "REFRESHING…" : "↻ REFRESH"}
    </button>
  </header>

  <p class="intro">Each row is an edit kept by the project. Select one to inspect it, then use Undo to here to walk the project back to it — or Undo and Redo to step one edit at a time.</p>

  {#if !historyBrowser.available}
    <p class="empty mono">History is available in the desktop app.</p>
  {:else}
    <div class="actions">
      <button disabled={!historyBrowser.overview?.undoDepth} onclick={() => void step("undo")}>
        <span>UNDO</span><span class="mono">{historyBrowser.overview?.undoDepth ?? 0}</span>
      </button>
      <button disabled={!historyBrowser.overview?.redoDepth} onclick={() => void step("redo")}>
        <span>REDO</span><span class="mono">{historyBrowser.overview?.redoDepth ?? 0}</span>
      </button>
    </div>

    {#if historyBrowser.error}<p class="error mono">{historyBrowser.error}</p>{/if}

    {#if historyBrowser.loading && !historyBrowser.overview}
      <p class="empty mono">Loading project history…</p>
    {:else if historyBrowser.overview}
      <div class="stats mono">
        <span>{historyBrowser.overview.versions.length} edits retained</span>
        <span>{bytes(historyBrowser.overview.retainedBytes)} charged</span>
      </div>

      <div class="versions" role="listbox" aria-label="Retained revisions">
        {#each historyBrowser.overview.versions as version (version.rev)}
          <button
            class:selected={historyBrowser.selectedRev === version.rev}
            role="option"
            aria-selected={historyBrowser.selectedRev === version.rev}
            onclick={() => void historyBrowser.select(version.rev)}
          >
            <span class="rowtext">
              <span class="label">{title(version.label)}</span>
              <span class="meta mono">r{version.rev} · {version.actor} · {bytes(version.chargedBytes)}</span>
            </span>
            <span class="kind">{version.materialized ? "SNAPSHOT" : "REPLAY"}</span>
          </button>
        {:else}
          <p class="empty mono">Make an edit to start project history.</p>
        {/each}
      </div>

      {#if historyBrowser.detail}
        <section class="detail" aria-label="Selected revision details">
          <h3>Revision {historyBrowser.detail.rev}</h3>
          <dl class="mono">
            <div><dt>TRACKS</dt><dd>{historyBrowser.detail.trackCount}</dd></div>
            <div><dt>AUDIO CLIPS</dt><dd>{historyBrowser.detail.audioClipCount}</dd></div>
            <div><dt>MIDI CLIPS</dt><dd>{historyBrowser.detail.midiClipCount}</dd></div>
            <div><dt>AUTOMATION</dt><dd>{historyBrowser.detail.automationLaneCount}</dd></div>
          </dl>
          <button
            class="undoto"
            disabled={!historyBrowser.canUndoToSelected || historyBrowser.loading}
            title={historyBrowser.canUndoToSelected
              ? "Undo every step after this revision"
              : "Only a revision still on the undo path can be walked back to"}
            onclick={() => void historyBrowser.undoToSelected()}
          >
            UNDO TO HERE
          </button>
        </section>
      {/if}
    {/if}
  {/if}
</section>

<style>
  .history { display: flex; min-height: 0; height: 100%; flex-direction: column; gap: 10px; }
  header, .actions, .stats, .versions button, .detail dl, .detail dl div { display: flex; align-items: center; }
  header { justify-content: space-between; gap: 12px; }
  h2 { margin: 2px 0 0; font-size: 18px; color: var(--text); }
  h3 { margin: 0 0 8px; color: var(--text); font-size: 12px; }
  .intro { margin: 0; color: var(--text-faint); font-size: 11px; line-height: 1.45; }
  button { border: 1px solid var(--glass-border); background: var(--bg-raised); color: var(--text-dim); cursor: pointer; }
  button:hover:not(:disabled), button.selected { color: var(--cyan); border-color: var(--cyan-dim); }
  button:disabled { opacity: .4; cursor: default; }
  .refresh { min-width: 86px; height: 30px; padding: 0 9px; border-radius: 4px; font-size: 9px; letter-spacing: .08em; }
  .actions { gap: 8px; }
  .actions button { flex: 1; display: flex; justify-content: space-between; padding: 8px 10px; border-radius: 4px; }
  .stats { justify-content: space-between; gap: 12px; color: var(--text-faint); font-size: 10px; }
  .versions { min-height: 100px; overflow: auto; display: flex; flex-direction: column; gap: 5px; }
  .versions button { flex: none; width: 100%; justify-content: space-between; gap: 10px; padding: 9px; border-radius: 5px; text-align: left; }
  .rowtext { display: flex; min-width: 0; flex-direction: column; gap: 3px; }
  .label { overflow: hidden; color: var(--text); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
  .meta { color: var(--text-faint); font-size: 9px; }
  .kind { flex: none; color: var(--text-faint); font-size: 8px; letter-spacing: .1em; }
  .detail { flex: none; padding: 10px; border: 1px solid var(--glass-border); border-radius: 5px; background: var(--bg-sunken); }
  .detail dl { margin: 0; flex-wrap: wrap; gap: 8px 16px; }
  .undoto { margin-top: 10px; width: 100%; height: 30px; border-radius: 4px; font-size: 9px; letter-spacing: .08em; }
  .detail dl div { gap: 6px; }
  dt { color: var(--text-faint); font-size: 9px; }
  dd { margin: 0; color: var(--text); }
  .empty, .error { color: var(--text-faint); font-size: 10px; }
  .error { color: var(--red); }
</style>
