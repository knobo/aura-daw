<script lang="ts">
  /**
   * Sampler instrument browser: bank listing (sampler_list_instruments),
   * mini keyboard audition per instrument (sampler_preview_note), assignment
   * to midi tracks (set_track_instrument), and the "generate instrument"
   * bridge into AI Studio (stableAudioSfz).
   *
   * Built on the shared browser layer (`components/browser/`): grouped by
   * the bank folder each instrument's .sfz lives in — the one grouping
   * that's actually true about a flat sampler bank.
   */
  import { instruments } from "../../state/instruments.svelte";
  import { project } from "../../state/project.svelte";
  import { openStudio } from "../../state/ui.svelte";
  import type { InstrumentInfo } from "../../types/ipc";
  import { loadFolds, saveFolds } from "../browser/browser-folds";
  import { type BrowserRowRef, flattenRows, groupItems, rankItems, rowId } from "../browser/browser-model";
  import BrowserShell from "../browser/BrowserShell.svelte";
  import BrowserSection from "../browser/BrowserSection.svelte";
  import BrowserRow from "../browser/BrowserRow.svelte";
  import EmptyState from "../browser/EmptyState.svelte";

  const FOLDS_KEY = "instruments";

  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));

  const AUDITION_KEYS = [48, 52, 55, 60, 64, 67, 72];
  const KEY_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
  const keyName = (k: number) => `${KEY_NAMES[k % 12]}${Math.floor(k / 12) - 1}`;

  /** The immediate folder an instrument's .sfz lives in — the closest
   * thing a flat sampler bank has to a category. */
  function bankFolder(inst: InstrumentInfo): string {
    const parts = inst.sfzPath.split(/[/\\]/).filter(Boolean);
    parts.pop();
    return parts.pop() ?? "instruments";
  }

  function assignedTracks(inst: InstrumentInfo): string {
    return midiTracks
      .filter((t) => t.instrumentId === inst.id)
      .map((t) => t.name)
      .join(" · ");
  }

  function onAssign(inst: InstrumentInfo, e: Event) {
    const trackId = (e.currentTarget as HTMLSelectElement).value;
    if (trackId) void instruments.assign(trackId, inst.id);
    (e.currentTarget as HTMLSelectElement).value = "";
  }

  let query = $state("");
  let collapsed = $state(loadFolds(FOLDS_KEY));

  const ranked = $derived(
    rankItems(instruments.list, query, [
      { value: (i: InstrumentInfo) => i.name, weight: 2 },
      { value: (i: InstrumentInfo) => i.sfzPath, weight: 1 },
    ]),
  );
  const groups = $derived(groupItems(ranked, bankFolder));
  const rows = $derived(flattenRows(groups, collapsed));

  function toggleGroup(key: string, expand: boolean) {
    const next = new Set(collapsed);
    if (expand) next.delete(key);
    else next.add(key);
    collapsed = next;
    saveFolds(FOLDS_KEY, next);
  }

  function onActivate(row: BrowserRowRef) {
    if (row.kind === "group") {
      toggleGroup(row.groupKey, collapsed.has(row.groupKey));
    }
  }

  const status = $derived(
    `${instruments.list.length} instrument${instruments.list.length === 1 ? "" : "s"}`,
  );
</script>

<div class="browser">
  <button class="gen mono" title="Generate a new instrument from a prompt" onclick={() => openStudio("stableAudioSfz")}>
    <span class="spark">✦</span> GENERATE INSTRUMENT
  </button>

  {#if instruments.error}
    <div class="err silk" role="alert">{instruments.error}</div>
  {/if}

  {#if instruments.list.length === 0}
    <EmptyState
      message={instruments.loading
        ? "Loading bank…"
        : "No instruments yet. Generate one above, or load an .sfz via MCP."}
    />
  {:else}
    <BrowserShell
      bind:query
      placeholder="search instruments…"
      {status}
      {rows}
      onToggleGroup={toggleGroup}
      {onActivate}
    >
      {#snippet children({ activeId, setActive })}
        {#if rows.length === 0}
          <EmptyState message={`No match for "${query}" — try a different search.`} />
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
            {#each g.items as inst, i (inst.id)}
              {@const rowIdStr = rowId({ kind: "item", groupKey: g.key, itemIndex: i })}
              <BrowserRow
                id={rowIdStr}
                label={inst.name}
                meta={inst.keyLow != null && inst.keyHigh != null
                  ? `${keyName(inst.keyLow)}–${keyName(inst.keyHigh)}`
                  : ""}
                active={activeId === rowIdStr}
                selected={instruments.previewingId === inst.id}
                onclick={() => {
                  setActive({ kind: "item", groupKey: g.key, itemIndex: i });
                  instruments.preview(inst.id, 60);
                }}
              >
                {#snippet badge()}
                  <span class="regions silk">{inst.regionCount} rgn</span>
                {/snippet}
                {#snippet actions()}
                  <div class="audition" role="group" aria-label="Audition {inst.name}">
                    {#each AUDITION_KEYS as k (k)}
                      <button
                        class="pkey mono"
                        class:black={[1, 3, 6, 8, 10].includes(k % 12)}
                        title="Preview {keyName(k)}"
                        onclick={(e) => {
                          e.stopPropagation();
                          instruments.preview(inst.id, k);
                        }}
                      >
                        {keyName(k)}
                      </button>
                    {/each}
                  </div>
                  <select
                    title="Bind to a MIDI track"
                    onclick={(e) => e.stopPropagation()}
                    onchange={(e) => onAssign(inst, e)}
                  >
                    <option value="">assign to track…</option>
                    {#each midiTracks as t (t.id)}
                      <option value={t.id}>{t.name}{t.instrumentId === inst.id ? " ✓" : ""}</option>
                    {/each}
                  </select>
                  {#if assignedTracks(inst)}
                    <span class="bound silk" title="Bound tracks">⌁ {assignedTracks(inst)}</span>
                  {/if}
                {/snippet}
              </BrowserRow>
            {/each}
          </BrowserSection>
        {/each}
      {/snippet}
    </BrowserShell>
  {/if}

  {#if midiTracks.length === 0}
    <div class="hint silk">no midi tracks yet — add one with “+ midi” in the track rail</div>
  {/if}
</div>

<style>
  .browser {
    display: flex;
    flex-direction: column;
    gap: 10px;
    flex: 1;
    min-height: 0;
  }

  .gen {
    flex: none;
    padding: 8px 10px;
    font-size: 10px;
    letter-spacing: 0.16em;
    border: none;
    border-radius: 5px;
    cursor: pointer;
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--cyan), var(--violet));
    box-shadow: 0 0 calc(14px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.25);
    transition: filter 120ms;
  }
  .gen:hover {
    filter: brightness(1.15);
  }
  .spark {
    margin-right: 4px;
  }

  .err {
    flex: none;
    color: var(--red);
  }
  .hint {
    flex: none;
    padding: 4px 0;
  }

  .regions {
    color: var(--text-faint);
  }

  .audition {
    display: flex;
    gap: 3px;
    width: 100%;
  }
  .pkey {
    flex: 1;
    padding: 5px 0;
    font-size: 8px;
    letter-spacing: 0.05em;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.18);
    background: rgb(var(--bg-3-rgb) / 0.6);
    color: var(--text-dim);
    cursor: pointer;
    transition: background 80ms, color 80ms;
  }
  .pkey.black {
    background: rgb(var(--bg-0-rgb) / 0.8);
  }
  .pkey:hover {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
  }
  .pkey:active {
    filter: brightness(1.3);
  }

  select {
    flex: 1;
    min-width: 0;
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 10px;
    padding: 4px 6px;
  }
  .bound {
    color: var(--green);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 45%;
  }
</style>
