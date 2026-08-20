<script lang="ts">
  /**
   * PRESETS root: loaded sampler instruments (sampler_list_instruments) and
   * ZynAddSubFX bank patches (zyn_list_patches). Drag onto a MIDI track to
   * apply — instruments bind through set_track_instrument, patches load into
   * the Zyn instance the track already hosts. Click an instrument to audition
   * middle C.
   *
   * Sampler vs Zyn used to be a tab switch; it's now the browser layer's
   * filter chips — same job, shared vocabulary. Instruments group by their
   * bank folder (the same grouping `InstrumentBrowser` uses); patches group
   * by bank (the same grouping `ZynPatchBrowser` uses) — one grouping idea,
   * reused everywhere it applies rather than invented per screen.
   */
  import { onMount } from "svelte";
  import { instruments } from "../../state/instruments.svelte";
  import { zyn } from "../../state/zynpatches.svelte";
  import { backend } from "../../tauri";
  import { encodeLibraryDrag } from "../../utils/library";
  import { loadFolds, saveFolds } from "../browser/browser-folds";
  import { flattenRows, groupItems, rankItems, rowId } from "../browser/browser-model";
  import BrowserShell from "../browser/BrowserShell.svelte";
  import BrowserSection from "../browser/BrowserSection.svelte";
  import BrowserRow from "../browser/BrowserRow.svelte";
  import EmptyState from "../browser/EmptyState.svelte";
  import type { InstrumentInfo, ZynPatch } from "../../types/ipc";

  /** Named, because the fold state's initial storage key is derived from it
   * too — and reading `section` there instead would read a rune out of a
   * reactive position for a value that is a constant at that point. */
  const INITIAL_SECTION = "instruments" as const;
  let section = $state<"instruments" | "patches">(INITIAL_SECTION);
  let query = $state("");

  onMount(() => {
    void instruments.refresh();
    if (zyn.patches.length === 0) void zyn.refresh();
  });

  /** The immediate folder an instrument's .sfz lives in — same grouping
   * `InstrumentBrowser` uses, so the two screens agree with each other. */
  function bankFolder(inst: InstrumentInfo): string {
    const parts = inst.sfzPath.split(/[/\\]/).filter(Boolean);
    parts.pop();
    return parts.pop() ?? "instruments";
  }

  const foldsKey = $derived(`presets-${section}`);
  let collapsed = $state(loadFolds(`presets-${INITIAL_SECTION}`));
  $effect(() => {
    collapsed = loadFolds(foldsKey);
  });

  function toggleGroup(key: string, expand: boolean) {
    const next = new Set(collapsed);
    if (expand) next.delete(key);
    else next.add(key);
    collapsed = next;
    saveFolds(foldsKey, next);
  }

  const instrumentGroups = $derived(
    groupItems(
      rankItems(instruments.list, query, [{ value: (i: InstrumentInfo) => i.name }]),
      bankFolder,
    ),
  );
  const patchGroups = $derived(
    groupItems(
      rankItems(zyn.patches, query, [
        { value: (p: ZynPatch) => p.name, weight: 2 },
        { value: (p: ZynPatch) => p.bank, weight: 1 },
      ]),
      (p) => p.bank,
    ),
  );

  const instrumentRows = $derived(flattenRows(instrumentGroups, collapsed));
  const patchRows = $derived(flattenRows(patchGroups, collapsed));
  const rows = $derived(section === "instruments" ? instrumentRows : patchRows);
  const status = $derived(
    section === "instruments"
      ? `${instruments.list.length} instrument${instruments.list.length === 1 ? "" : "s"}`
      : `${zyn.patches.length} patch${zyn.patches.length === 1 ? "" : "es"}`,
  );
</script>

<BrowserShell
  bind:query
  placeholder="search presets…"
  {status}
  {rows}
  onToggleGroup={toggleGroup}
  onActivate={(row) => {
    if (row.kind === "group") toggleGroup(row.groupKey, collapsed.has(row.groupKey));
  }}
>
  {#snippet filters()}
    <button
      type="button"
      class="chip mono"
      class:on={section === "instruments"}
      onclick={() => (section = "instruments")}
    >
      sampler
    </button>
    <button
      type="button"
      class="chip mono"
      class:on={section === "patches"}
      onclick={() => (section = "patches")}
    >
      zyn
    </button>
  {/snippet}

  {#snippet children({ activeId, setActive })}
    {#if rows.length === 0}
      <EmptyState
        message={section === "instruments"
          ? instruments.list.length === 0
            ? "No instruments loaded yet."
            : `No match for "${query}".`
          : zyn.patches.length === 0
            ? "No Zyn patches found."
            : `No match for "${query}".`}
      />
    {/if}
    {#if section === "instruments"}
      {#each instrumentGroups as g (g.key)}
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
          {#each g.items as i, idx (i.id)}
            {@const rowIdStr = rowId({ kind: "item", groupKey: g.key, itemIndex: idx })}
            <BrowserRow
              id={rowIdStr}
              label={i.name}
              meta={`${i.regionCount} rgn`}
              active={activeId === rowIdStr}
              draggable
              ondragstart={(e) =>
                e.dataTransfer &&
                encodeLibraryDrag(e.dataTransfer, {
                  kind: "samplerInstrument",
                  instrumentId: i.id,
                  name: i.name,
                })}
              onclick={() => {
                setActive({ kind: "item", groupKey: g.key, itemIndex: idx });
                void backend.samplerPreviewNote(i.id, 60, 100);
              }}
            />
          {/each}
        </BrowserSection>
      {/each}
    {:else}
      {#each patchGroups as g (g.key)}
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
          {#each g.items as p, idx (p.path)}
            {@const rowIdStr = rowId({ kind: "item", groupKey: g.key, itemIndex: idx })}
            <BrowserRow
              id={rowIdStr}
              label={p.name}
              active={activeId === rowIdStr}
              draggable
              ondragstart={(e) =>
                e.dataTransfer && encodeLibraryDrag(e.dataTransfer, { kind: "zynPatch", patch: p })}
              onclick={() => setActive({ kind: "item", groupKey: g.key, itemIndex: idx })}
            />
          {/each}
        </BrowserSection>
      {/each}
    {/if}
  {/snippet}
</BrowserShell>

<style>
  .chip {
    flex: none;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 3px 8px;
    cursor: pointer;
  }
  .chip.on {
    color: var(--cyan);
    border-color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.06);
  }
</style>
