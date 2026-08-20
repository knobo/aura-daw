<script lang="ts">
  /**
   * SAMPLES root: the configured folders (default library dir first) and one
   * directory level at a time. Click an audio row to audition it — the
   * audition never touches the project (no op, no dirty flag).
   *
   * The top-level folder picker is a handful of shortcuts — not enough
   * content to earn the full search/tree shell. A folder's contents can be
   * hundreds of files, so that level gets the shared browser layer: search,
   * a Folders/Audio-files grouping (true about a filesystem listing), and
   * per-folder fold state.
   *
   * Drag-out is wired in Task 6; this file only renders and auditions.
   */
  import { onMount } from "svelte";
  import { library } from "../../state/library.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import { encodeLibraryDrag } from "../../utils/library";
  import { loadFolds, saveFolds } from "../browser/browser-folds";
  import { flattenRows, groupItems, rankItems, rowId } from "../browser/browser-model";
  import BrowserShell from "../browser/BrowserShell.svelte";
  import BrowserSection from "../browser/BrowserSection.svelte";
  import BrowserRow from "../browser/BrowserRow.svelte";
  import EmptyState from "../browser/EmptyState.svelte";
  import type { LibraryEntry } from "../../types/ipc";

  onMount(() => void library.init());

  function sizeLabel(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} K`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} M`;
  }

  function folderName(path: string): string {
    return path.split(/[/\\]/).filter(Boolean).pop() ?? path;
  }

  const FOLDS_KEY = "samples";
  let query = $state("");
  let collapsed = $state(loadFolds(FOLDS_KEY));

  const ranked = $derived(rankItems(library.entries, query, [{ value: (e: LibraryEntry) => e.name }]));
  const groups = $derived(
    groupItems(ranked, (e) => (e.kind === "dir" ? "Folders" : "Audio files")),
  );
  const rows = $derived(flattenRows(groups, collapsed));
  const status = $derived(`${library.entries.length} item${library.entries.length === 1 ? "" : "s"}`);

  function toggleGroup(key: string, expand: boolean) {
    const next = new Set(collapsed);
    if (expand) next.delete(key);
    else next.add(key);
    collapsed = next;
    saveFolds(FOLDS_KEY, next);
  }

  function openEntry(e: LibraryEntry) {
    if (e.kind === "dir") void library.open(e.path);
    else void library.audition(e.path);
  }
</script>

{#if !library.available}
  <EmptyState message="The sample library is available in the desktop app." />
{:else if library.cwd === null}
  <div class="list">
    {#each library.folders as folder (folder)}
      <BrowserRow
        id={`samples-root-${folder}`}
        label={folderName(folder)}
        meta={folder === library.defaultRoot ? "DEFAULT" : ""}
        onclick={() => void library.open(folder)}
      />
    {/each}
  </div>
  <button class="cfg mono" onclick={() => (prefs.dialogOpen = true)}>+ configure folders</button>
{:else}
  <div class="crumbs">
    <button class="up mono" onclick={() => void library.up()}>↑ UP</button>
    <span class="here silk" title={library.cwd}>{library.cwd}</span>
    <button class="up mono" onclick={() => void library.refresh()} title="Rescan">⟳</button>
  </div>
  {#if library.error}
    <EmptyState variant="error" message={library.error} />
  {:else}
    {#if library.auditionError}
      <div class="note err silk" role="alert">{library.auditionError}</div>
    {/if}
    <BrowserShell
      bind:query
      placeholder="search this folder…"
      {status}
      {rows}
      onToggleGroup={toggleGroup}
      onActivate={(row) => {
        if (row.kind === "group") toggleGroup(row.groupKey, collapsed.has(row.groupKey));
      }}
    >
      {#snippet children({ activeId, setActive })}
        {#if library.loading}
          <div class="note silk">scanning…</div>
        {:else if rows.length === 0}
          <EmptyState
            message={library.entries.length === 0
              ? "Nothing here."
              : `No match for "${query}" — try a different search.`}
          />
        {/if}
        {#each groups as g (g.key)}
          {@const headerId = rowId({ kind: "group", groupKey: g.key, itemIndex: -1 })}
          <BrowserSection
            id={headerId}
            label={g.label}
            count={g.items.length}
            expanded={!collapsed.has(g.key)}
            active={activeId === headerId}
            onToggle={() => {
              setActive({ kind: "group", groupKey: g.key, itemIndex: -1 });
              toggleGroup(g.key, collapsed.has(g.key));
            }}
          >
            {#each g.items as e, i (e.path)}
              {@const rowIdStr = rowId({ kind: "item", groupKey: g.key, itemIndex: i })}
              <BrowserRow
                id={rowIdStr}
                label={e.name}
                meta={e.kind === "dir" ? "" : sizeLabel(e.sizeBytes)}
                active={activeId === rowIdStr}
                selected={library.auditioning === e.path}
                draggable={e.kind === "audio"}
                ondragstart={(ev) => {
                  if (e.kind === "audio" && ev.dataTransfer)
                    encodeLibraryDrag(ev.dataTransfer, { kind: "sampleFile", path: e.path, name: e.name });
                }}
                onclick={() => {
                  setActive({ kind: "item", groupKey: g.key, itemIndex: i });
                  openEntry(e);
                }}
              />
            {/each}
          </BrowserSection>
        {/each}
      {/snippet}
    </BrowserShell>
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
