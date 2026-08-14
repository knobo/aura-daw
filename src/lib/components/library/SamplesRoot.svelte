<script lang="ts">
  /**
   * SAMPLES root: the configured folders (default library dir first) and one
   * directory level at a time. Click an audio row to audition it — the
   * audition never touches the project (no op, no dirty flag).
   *
   * Drag-out is wired in Task 6; this file only renders and auditions.
   */
  import { onMount } from "svelte";
  import { library } from "../../state/library.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import { encodeLibraryDrag } from "../../utils/library";
  import LibraryRow from "./LibraryRow.svelte";

  onMount(() => void library.init());

  function sizeLabel(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} K`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} M`;
  }

  function folderName(path: string): string {
    return path.split(/[/\\]/).filter(Boolean).pop() ?? path;
  }
</script>

{#if !library.available}
  <div class="note silk">the sample library is available in the desktop app</div>
{:else if library.cwd === null}
  <div class="list">
    {#each library.folders as folder (folder)}
      <LibraryRow
        icon="▸"
        label={folderName(folder)}
        meta={folder === library.defaultRoot ? "DEFAULT" : ""}
        onclick={() => void library.open(folder)}
      />
    {/each}
  </div>
  <button class="cfg mono" onclick={() => (prefs.dialogOpen = true)}>+ CONFIGURE FOLDERS</button>
{:else}
  <div class="crumbs">
    <button class="up mono" onclick={() => void library.up()}>↑ UP</button>
    <span class="here silk" title={library.cwd}>{library.cwd}</span>
    <button class="up mono" onclick={() => void library.refresh()} title="Rescan">⟳</button>
  </div>
  {#if library.loading}
    <div class="note silk">scanning…</div>
  {:else if library.error}
    <div class="note err silk">{library.error}</div>
  {:else}
    <div class="list">
      {#each library.entries as e (e.path)}
        <LibraryRow
          icon={e.kind === "dir" ? "▸" : "♪"}
          label={e.name}
          meta={e.kind === "dir" ? "" : sizeLabel(e.sizeBytes)}
          active={library.auditioning === e.path}
          draggable={e.kind === "audio"}
          ondragstart={(ev) => {
            if (e.kind === "audio" && ev.dataTransfer)
              encodeLibraryDrag(ev.dataTransfer, {
                kind: "sampleFile",
                path: e.path,
                name: e.name,
              });
          }}
          onclick={() =>
            e.kind === "dir" ? void library.open(e.path) : void library.audition(e.path)}
        />
      {:else}
        <div class="note silk">nothing here</div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--glass-border);
    margin-bottom: 6px;
  }
  .here {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .up,
  .cfg {
    flex: none;
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 3px 6px;
    cursor: pointer;
  }
  .up:hover,
  .cfg:hover {
    color: var(--cyan);
    border-color: var(--cyan);
  }
  .cfg {
    margin-top: 8px;
    align-self: flex-start;
  }
  .note {
    padding: 10px 4px;
    letter-spacing: 0.1em;
  }
  .note.err {
    color: var(--red);
  }
</style>
