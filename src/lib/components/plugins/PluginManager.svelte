<script lang="ts">
  /**
   * Plugin Manager (winner spec): three surfaces, no single home.
   * SPLIT is the default when the project has instances — rack on top
   * (what exists), browse below (what could). Ctrl+P is the add-in-the-flow
   * surface; the lane strip is the Bitwig job (next pass).
   */
  import { onMount } from "svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { lanes } from "../../state/lanes.svelte";
  import { modulation } from "../../state/modulation.svelte";
  import { openPluginParams } from "../../state/plugin-panel";
  import { toasts } from "../../state/toasts.svelte";
  import { zyn, isZynInstance } from "../../state/zynpatches.svelte";
  import { audition } from "../../state/audition.svelte";
  import { backend } from "../../tauri";
  import type { PluginDescriptor, PluginFormat, TrackState } from "../../types/ipc";
  import { FoldController } from "../browser/fold-controller.svelte";
  import { flattenRows, rowId, type BrowserRowRef } from "../browser/browser-model";
  import BrowserShell from "../browser/BrowserShell.svelte";
  import BrowserSection from "../browser/BrowserSection.svelte";
  import BrowserRow from "../browser/BrowserRow.svelte";
  import EmptyState from "../browser/EmptyState.svelte";
  import ParamChip from "../browser/ParamChip.svelte";
  import AuditionChip from "../browser/AuditionChip.svelte";
  import PluginConnectionBadge from "./PluginConnectionBadge.svelte";
  import AutomationMatrix from "./AutomationMatrix.svelte";
  import { resolveDescriptorTarget, resolvePluginInstanceTarget } from "../../utils/audition-target";
  import {
    buildBrowseSections,
    filterByFacets,
    filterSections,
    listCategoryFacets,
    pruneUnavailableFacets,
    visibleSections,
    type KindFilter,
  } from "../../utils/plugin-browse";
  import { buildRack, rackByTrack, rackCounts, type RackEntry } from "../../utils/plugin-rack";
  import {
    DEFAULT_SPLIT_PX,
    initialManagerMode,
    loadManagerView,
    saveManagerView,
    type ManagerMode,
  } from "../../utils/plugin-manager-view";
  import { bumpFrecency, loadFrecency, saveFrecency } from "../../utils/plugin-frecency";
  import { encodeLibraryDrag } from "../../utils/library";

  let query = $state("");
  let userMode = $state<ManagerMode | null>(null);
  let kind = $state<KindFilter>("all");
  let favoritesOnly = $state(false);
  let recentsOnly = $state(false);
  let formats = $state<PluginFormat[]>([]);
  let categories = $state<string[]>([]);
  let splitPx = $state(DEFAULT_SPLIT_PX);
  /** Which projectDir the chips were loaded for — a snapshot that writes
   * the same dir must not clobber a chip the user just pressed. */
  let loadedFor: string | null | undefined;

  // `lastSilentReason` is a global singleton with no decay (unlike
  // `sounding`) — it's cleared only by the next successful `play()`
  // anywhere in the app. This panel remounts fresh on every dock-tab
  // switch, so without this it can open already showing a stale reason
  // left behind by a double-click in a *different* browser (e.g. Presets).
  // Clearing on mount means the notice only ever reports something the
  // user did in this panel.
  onMount(() => {
    audition.lastSilentReason = null;
  });

  $effect(() => {
    const dir = project.projectDir;
    if (dir === loadedFor) return;
    loadedFor = dir;
    const stored = loadManagerView(dir);
    userMode = stored.mode ?? null;
    kind = stored.kind ?? "all";
    favoritesOnly = stored.favoritesOnly ?? false;
    recentsOnly = stored.recentsOnly ?? false;
    formats = stored.formats ?? [];
    categories = stored.categories ?? [];
    splitPx = stored.splitPx ?? DEFAULT_SPLIT_PX;
  });

  const mode = $derived(initialManagerMode(userMode ?? undefined, plugins.instances.length));
  const showRack = $derived(mode === "rack" || mode === "split");
  const showBrowse = $derived(mode === "browse" || mode === "split");

  function persist(next: {
    mode?: ManagerMode | null;
    kind?: KindFilter;
    favoritesOnly?: boolean;
    recentsOnly?: boolean;
    formats?: PluginFormat[];
    categories?: string[];
    splitPx?: number;
  }) {
    if (next.mode !== undefined) userMode = next.mode;
    if (next.kind !== undefined) kind = next.kind;
    if (next.favoritesOnly !== undefined) favoritesOnly = next.favoritesOnly;
    if (next.recentsOnly !== undefined) recentsOnly = next.recentsOnly;
    if (next.formats !== undefined) formats = next.formats;
    if (next.categories !== undefined) categories = next.categories;
    if (next.splitPx !== undefined) splitPx = next.splitPx;
    saveManagerView(project.projectDir, {
      mode: next.mode ?? userMode ?? mode,
      kind: next.kind ?? kind,
      favoritesOnly: next.favoritesOnly ?? favoritesOnly,
      recentsOnly: next.recentsOnly ?? recentsOnly,
      formats: next.formats ?? formats,
      categories: next.categories ?? categories,
      splitPx: next.splitPx ?? splitPx,
    });
  }

  function toggleFormat(fmt: PluginFormat) {
    persist({ formats: formats.includes(fmt) ? formats.filter((f) => f !== fmt) : [...formats, fmt] });
  }

  function toggleCategory(cat: string) {
    persist({
      categories: categories.includes(cat) ? categories.filter((c) => c !== cat) : [...categories, cat],
    });
  }

  const browseFolds = new FoldController("plugins-browse");
  const rackFolds = new FoldController("plugins-rack");
  $effect(() => browseFolds.syncQuery(query));
  $effect(() => rackFolds.syncQuery(query));

  const browseCollapsed = $derived(browseFolds.collapsed);
  const rackCollapsed = $derived(rackFolds.collapsed);

  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  const insertTracks = $derived(
    project.tracks.filter((t) => t.kind === "audio" || t.kind === "midi"),
  );

  function preferredTrack(pool: TrackState[]): TrackState | undefined {
    const selected = pool.filter((t) => lanes.isSelected(t.id));
    return (selected.length ? selected : pool)[0];
  }

  const instrumentTarget = $derived(preferredTrack(midiTracks));
  const insertTarget = $derived(preferredTrack(insertTracks));

  const facetPool = $derived(filterByFacets(plugins.descriptors, { kind, formats }));
  const typeFacets = $derived(listCategoryFacets(facetPool));
  $effect(() => {
    const next = pruneUnavailableFacets(categories, typeFacets);
    if (next.length !== categories.length) persist({ categories: next });
  });
  const filteredDescriptors = $derived(
    filterByFacets(plugins.descriptors, { kind, formats, categories }),
  );

  const browseTree = $derived(
    filterSections(
      buildBrowseSections({
        descriptors: filteredDescriptors,
        favorites: plugins.catalog.favorites,
        recents: plugins.catalog.recents,
        query,
        tags: plugins.catalog.tags,
      }),
      { favoritesOnly, recentsOnly },
    ),
  );
  const browseShown = $derived(visibleSections(browseTree, browseCollapsed));
  const browseGroups = $derived(
    browseShown.map((s) => ({ key: s.key, label: s.label, items: s.items })),
  );

  const rack = $derived(
    buildRack({
      instances: plugins.instances,
      descriptors: plugins.descriptors,
      tracks: project.tracks,
      bindings: modulation.bindings,
    }).filter((e) => {
      const uid = e.descriptor?.uid ?? e.instance.uid;
      if (favoritesOnly || recentsOnly) {
        const fav = favoritesOnly && plugins.isFavorite(uid);
        const rec = recentsOnly && plugins.catalog.recents.some((r) => r.uid === uid);
        if (!fav && !rec) return false;
      }
      if (!e.descriptor) return kind === "all" && formats.length === 0 && categories.length === 0;
      return filterByFacets([e.descriptor], { kind, formats, categories }).length > 0;
    }),
  );
  const rackGroups = $derived(
    rackByTrack(rack).map((g) => ({
      key: g.trackId ?? "orphans",
      label: g.trackName,
      items: g.entries,
    })),
  );
  const counts = $derived(rackCounts(rack));

  const browseRows = $derived(flattenRows(browseGroups, browseCollapsed));
  const browseKeys = $derived(browseGroups.map((g) => g.key));
  const rackKeys = $derived(rackGroups.map((g) => g.key));
  const anyFolded = $derived(
    mode === "rack"
      ? rackFolds.anyCollapsed(rackKeys)
      : mode === "browse"
        ? browseFolds.anyCollapsed(browseKeys)
        : rackFolds.anyCollapsed(rackKeys) || browseFolds.anyCollapsed(browseKeys),
  );

  // `AutomationMatrix` reports its own row count out via `onCount` — the
  // footer needs the number but must not recompute `buildMatrix` a second
  // time (unlike `rack`, computed once below and shared by `rackGroups`
  // and `rackCounts(rack)`, a second independent build here could drift
  // from the matrix's own if the projection rules ever change).
  let matrixCount = $state(0);

  const status = $derived.by(() => {
    if (mode === "matrix") {
      return matrixCount === 1 ? "1 param moves" : `${matrixCount} params move`;
    }
    const n = filteredDescriptors.length;
    const catalog = `${n} plugin${n === 1 ? "" : "s"}`;
    if (mode === "browse") return catalog;
    const bits = [`${counts.total} live`];
    if (counts.crashed) bits.push(`${counts.crashed} crashed`);
    if (counts.orphans) bits.push(`${counts.orphans} unused`);
    if (counts.stub) bits.push(`${counts.stub} loading`);
    if (counts.automated) bits.push(`${counts.automated} automated`);
    return mode === "split" ? `${bits.join(" · ")} · ${catalog}` : bits.join(" · ");
  });

  function toggleGroup(key: string, expand: boolean) {
    if (rackKeys.includes(key)) rackFolds.toggle(key, expand);
    else browseFolds.toggle(key, expand);
  }

  function foldAll(collapse: boolean) {
    if (showRack) rackFolds.setAll(rackKeys, collapse);
    if (showBrowse) browseFolds.setAll(browseKeys, collapse);
  }

  function noteFrecency(uid: string) {
    saveFrecency(bumpFrecency(loadFrecency(), uid, Date.now()));
  }

  function onSplitDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const startY = e.clientY;
    const startH = splitPx;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture?.(e.pointerId);
    const onMove = (ev: PointerEvent) => {
      persist({ splitPx: Math.max(96, startH + ev.clientY - startY) });
    };
    const onUp = () => {
      el.removeEventListener("pointermove", onMove);
      el.removeEventListener("pointerup", onUp);
    };
    el.addEventListener("pointermove", onMove);
    el.addEventListener("pointerup", onUp);
  }

  async function addDescriptor(d: PluginDescriptor, asInsert: boolean) {
    const t = asInsert || !d.isInstrument ? insertTarget : instrumentTarget;
    if (asInsert || !d.isInstrument) {
      if (!t) {
        toasts.error("NO TRACK", "Add an audio or MIDI track first.");
        return;
      }
      await placeDescriptor(d, t.id);
      return;
    }
    await placeDescriptor(d, t?.id ?? null);
  }

  async function placeDescriptor(d: PluginDescriptor, trackId: string | null) {
    try {
      if (!d.isInstrument) {
        if (!trackId) {
          toasts.error("NO TRACK", "Add an audio or MIDI track first.");
          return;
        }
        await plugins.insertEffect(trackId, d.uid);
      } else {
        await plugins.instantiate(d.uid, trackId);
      }
      noteFrecency(d.uid);
    } catch (err) {
      toasts.error("PLUGIN FAILED", String(err));
    }
  }

  function onPlaceChange(d: PluginDescriptor, e: Event) {
    const sel = e.currentTarget as HTMLSelectElement;
    const trackId = sel.value;
    sel.value = "";
    if (!trackId) return;
    void placeDescriptor(d, trackId === "-" ? null : trackId);
  }

  function dragDescriptor(d: PluginDescriptor, e: DragEvent) {
    if (!e.dataTransfer) return;
    encodeLibraryDrag(
      e.dataTransfer,
      d.isInstrument
        ? { kind: "pluginInstrument", uid: d.uid, name: d.name }
        : { kind: "pluginEffect", uid: d.uid, name: d.name },
    );
  }

  function onActivate(row: BrowserRowRef) {
    if (row.kind === "group") {
      toggleGroup(row.groupKey, browseCollapsed.has(row.groupKey) || rackCollapsed.has(row.groupKey));
      return;
    }
    if (mode === "browse") {
      const section = browseShown.find((s) => s.key === row.groupKey);
      const d = section?.items[row.itemIndex];
      if (d) void addDescriptor(d, !d.isInstrument);
      return;
    }
    const group = rackGroups.find((g) => g.key === row.groupKey);
    const entry = group?.items[row.itemIndex];
    if (entry) void openPluginParams(entry.instance.id);
  }

  async function toggleBypass(entry: RackEntry) {
    const place = entry.placements.find((p) => p.kind === "insert");
    if (!place || place.kind !== "insert") return;
    const slot = project.trackById(place.trackId)?.inserts?.[place.slotIndex];
    if (!slot) return;
    try {
      await backend.insertSetBypass?.(place.trackId, slot.id, !place.bypassed);
    } catch (err) {
      toasts.error("BYPASS FAILED", String(err));
    }
  }

  function placementLabel(entry: RackEntry): string {
    if (entry.placements.length === 0) return "not on any track";
    return entry.placements
      .map((p) => (p.kind === "instrument" ? p.trackName : `${p.trackName} · fx ${p.slotIndex + 1}`))
      .join(" · ");
  }
</script>

<div class="manager">
  <div class="top">
    <button class="scan mono" disabled={plugins.scanning} onclick={() => plugins.scan()}>
      {plugins.scanning ? "◌ SCANNING…" : plugins.scanned ? "↻ RESCAN" : "⌕ SCAN PLUGINS"}
    </button>
    <div class="modes" role="group" aria-label="Plugin manager mode">
      <button
        type="button"
        class="chip mono"
        class:on={mode === "browse"}
        aria-pressed={mode === "browse"}
        onclick={() => persist({ mode: "browse" })}
      >
        BROWSE
      </button>
      {#if plugins.instances.length > 0}
        <button
          type="button"
          class="chip mono"
          class:on={mode === "split"}
          aria-pressed={mode === "split"}
          onclick={() => persist({ mode: "split" })}
        >
          SPLIT
        </button>
      {/if}
      <button
        type="button"
        class="chip mono"
        class:on={mode === "rack"}
        aria-pressed={mode === "rack"}
        onclick={() => persist({ mode: "rack" })}
      >
        RACK
      </button>
      <button
        type="button"
        class="chip mono"
        class:on={mode === "matrix"}
        aria-pressed={mode === "matrix"}
        onclick={() => persist({ mode: "matrix" })}
      >
        MATRIX
      </button>
    </div>
  </div>

  {#if plugins.scanError}
    <div class="err silk" role="alert">scan failed — {plugins.scanError}</div>
  {/if}
  {#if plugins.error}
    <div class="err silk" role="alert">{plugins.error}</div>
  {/if}
  {#if audition.lastSilentReason}
    <div class="note silk" role="status">{audition.lastSilentReason}</div>
  {/if}

  {#if mode === "matrix"}
    <!-- The search box and facet chips are catalog filters; the matrix
         isn't the catalog, so they'd be dead controls here. -->
    <div class="panes">
      <AutomationMatrix onCount={(n) => (matrixCount = n)} />
    </div>
    <div class="status silk">{status}</div>
  {:else if !plugins.scanned && !plugins.scanning && plugins.descriptors.length === 0}
    <EmptyState
      message="No plugins yet. Scan to find your CLAP and LV2 installs."
      action={{ label: "SCAN PLUGINS", onclick: () => plugins.scan() }}
    />
  {:else}
    <div class="toolbar">
      <input
        bind:value={query}
        class="search mono"
        type="text"
        placeholder="search plugins…"
        aria-label="search plugins…"
      />
      <button
        type="button"
        class="foldall mono"
        aria-label={anyFolded ? "Expand all groups" : "Collapse all groups"}
        title={anyFolded ? "Expand all groups" : "Collapse all groups"}
        onclick={() => foldAll(!anyFolded)}
      >
        <span aria-hidden="true">{anyFolded ? "⌄" : "⌃"}</span>
      </button>
      <div class="chips primary">
        <button type="button" class="chip mono" class:on={kind === "all"} aria-pressed={kind === "all"} onclick={() => persist({ kind: "all" })}>ALL</button>
        <button type="button" class="chip mono" class:on={kind === "inst"} aria-pressed={kind === "inst"} onclick={() => persist({ kind: "inst" })}>INST</button>
        <button type="button" class="chip mono" class:on={kind === "fx"} aria-pressed={kind === "fx"} onclick={() => persist({ kind: "fx" })}>FX</button>
        <button type="button" class="chip mono" class:on={formats.includes("clap")} aria-pressed={formats.includes("clap")} onclick={() => toggleFormat("clap")}>CLAP</button>
        <button type="button" class="chip mono" class:on={formats.includes("lv2")} aria-pressed={formats.includes("lv2")} onclick={() => toggleFormat("lv2")}>LV2</button>
        <button type="button" class="chip mono" class:on={favoritesOnly} aria-pressed={favoritesOnly} title="Favourites only" onclick={() => persist({ favoritesOnly: !favoritesOnly })}>★</button>
        <button type="button" class="chip mono" class:on={recentsOnly} aria-pressed={recentsOnly} title="Recent only" onclick={() => persist({ recentsOnly: !recentsOnly })}>⏱</button>
        <AuditionChip />
      </div>
    </div>
    {#if typeFacets.length > 0}
      <div class="chips types" role="group" aria-label="Plugin type">
        {#each typeFacets as cat (cat)}
          <button type="button" class="chip mono" class:on={categories.includes(cat)} aria-pressed={categories.includes(cat)} onclick={() => toggleCategory(cat)}>{cat}</button>
        {/each}
      </div>
    {/if}

    <div class="panes">
      {#if showRack}
        <div class="pane rack" class:fill={mode === "rack"} style:height={mode === "split" ? `${splitPx}px` : undefined}>
          {#if rackGroups.length === 0}
            <EmptyState message="Nothing live in this project. Switch to Browse to add a plugin." />
          {/if}
          {#each rackGroups as g (g.key)}
            {@const headerId = rowId({ kind: "group", groupKey: g.key, itemIndex: -1 })}
            <BrowserSection
              id={headerId}
              label={g.label}
              count={g.items.length}
              expanded={!rackCollapsed.has(g.key)}
              onToggle={() => toggleGroup(g.key, rackCollapsed.has(g.key))}
            >
              {#each g.items as entry, i (entry.instance.id)}
                {@const rowIdStr = rowId({ kind: "item", groupKey: g.key, itemIndex: i })}
                {@const inst = entry.instance}
                <BrowserRow
                  id={rowIdStr}
                  label={inst.name}
                  meta={placementLabel(entry)}
                  selected={plugins.openInstanceId === inst.id}
                  draggable={entry.placements.length === 0}
                  ondragstart={(e) => {
                    if (entry.placements.length === 0 && e.dataTransfer) {
                      encodeLibraryDrag(e.dataTransfer, {
                        kind: "pluginInstance",
                        instanceId: inst.id,
                        name: inst.name,
                      });
                    }
                  }}
                  onclick={() => void openPluginParams(inst.id)}
                  ondblclick={() =>
                    void audition.play(resolvePluginInstanceTarget(inst.id, project.tracks))}
                >
                  {#snippet badge()}
                    <span class="dot {inst.status}" title={inst.status} aria-hidden="true"></span>
                    <span class="badge mono {inst.format}">{inst.format}</span>
                  {/snippet}
                  {#snippet actions()}
                    {#each entry.automated as p (p.paramId)}
                      <ParamChip label={`#${p.paramId}`} value={0} format={() => "~"} state="automated" />
                    {/each}
                    {#if entry.placements.length === 0}
                      <PluginConnectionBadge instanceId={inst.id} />
                    {/if}
                    {#if entry.placements.some((p) => p.kind === "insert")}
                      <button type="button" class="act mono" class:on={entry.placements.some((p) => p.kind === "insert" && p.bypassed)} onclick={(e) => { e.stopPropagation(); void toggleBypass(entry); }}>BYP</button>
                    {/if}
                    {#if isZynInstance(inst)}
                      <button type="button" class="act mono" onclick={(e) => { e.stopPropagation(); zyn.openFor(inst.id); }}>PATCHES</button>
                    {/if}
                    {#if plugins.hasGui(inst.id)}
                      <button type="button" class="act mono" title="Open native plugin GUI" onclick={(e) => { e.stopPropagation(); void plugins.showGui(inst.id); }}>GUI</button>
                    {/if}
                    <button type="button" class="act mono" onclick={(e) => { e.stopPropagation(); void openPluginParams(inst.id); }}>PARAMS</button>
                    <button type="button" class="del" title="Remove {inst.name}" aria-label="Remove {inst.name} instance" onclick={(e) => { e.stopPropagation(); void plugins.remove(inst.id); }}>×</button>
                  {/snippet}
                </BrowserRow>
              {/each}
            </BrowserSection>
          {/each}
        </div>
      {/if}

      {#if mode === "split"}
        <div class="splitter" role="separator" aria-orientation="horizontal" aria-label="Resize rack" onpointerdown={onSplitDown}></div>
      {/if}

      {#if showBrowse}
        <div class="pane browse" class:fill={mode !== "rack"}>
          <BrowserShell
            bind:query
            chrome={false}
            {status}
            rows={browseRows}
            onToggleGroup={toggleGroup}
            {onActivate}
            anyCollapsed={browseFolds.anyCollapsed(browseKeys)}
            onFoldAll={(collapse) => browseFolds.setAll(browseKeys, collapse)}
          >
      {#snippet children({ activeId, setActive })}
        {#if browseRows.length === 0}
          <EmptyState
            message={plugins.descriptors.length === 0
              ? "Scan found no plugins on this machine."
              : `No match for "${query}".`}
          />
        {/if}

        {#if showBrowse}
          {#each browseShown as s (s.key)}
            {@const headerId = rowId({ kind: "group", groupKey: s.key, itemIndex: -1 })}
            <BrowserSection
              id={headerId}
              label={s.label}
              count={s.count}
              depth={s.depth}
              expanded={!browseCollapsed.has(s.key)}
              active={activeId === headerId}
              onToggle={() => {
                setActive({ kind: "group", groupKey: s.key, itemIndex: -1 });
                toggleGroup(s.key, browseCollapsed.has(s.key));
              }}
            >
              {#each s.items as d, i (d.uid)}
                {@const rowIdStr = rowId({ kind: "item", groupKey: s.key, itemIndex: i })}
                <BrowserRow
                  id={rowIdStr}
                  label={d.name}
                  meta={d.vendor ?? ""}
                  favorite={plugins.isFavorite(d.uid)}
                  onFavorite={() => plugins.toggleFavorite(d.uid)}
                  active={activeId === rowIdStr}
                  draggable
                  ondragstart={(e) => dragDescriptor(d, e)}
                  onclick={() => {
                    setActive({ kind: "item", groupKey: s.key, itemIndex: i });
                  }}
                  ondblclick={() =>
                    void audition.play(
                      resolveDescriptorTarget(d.uid, plugins.instances, project.tracks),
                    )}
                >
                  {#snippet badge()}
                    <span class="badge mono {d.format}">{d.format}</span>
                  {/snippet}
                  {#snippet actions()}
                    {#if d.isInstrument}
                      <select
                        class="place mono"
                        aria-label="Add {d.name} to a MIDI track"
                        title="Choose a MIDI track, or drag this row onto a lane"
                        disabled={midiTracks.length === 0}
                        onchange={(e) => onPlaceChange(d, e)}
                        onclick={(e) => e.stopPropagation()}
                        onpointerdown={(e) => e.stopPropagation()}
                      >
                        <option value="">{midiTracks.length === 0 ? "add a MIDI track first" : "add to track…"}</option>
                        {#each midiTracks as t (t.id)}
                          <option value={t.id}>{t.name}</option>
                        {/each}
                        <option value="-">not on a track</option>
                      </select>
                    {:else}
                      <select
                        class="place mono"
                        aria-label="Insert {d.name} on a track"
                        title="Choose a track, or drag this row onto a lane"
                        disabled={insertTracks.length === 0}
                        onchange={(e) => onPlaceChange(d, e)}
                        onclick={(e) => e.stopPropagation()}
                        onpointerdown={(e) => e.stopPropagation()}
                      >
                        <option value="">{insertTracks.length === 0 ? "add a track first" : "insert on track…"}</option>
                        {#each insertTracks as t (t.id)}
                          <option value={t.id}>{t.name}</option>
                        {/each}
                      </select>
                    {/if}
                  {/snippet}
                </BrowserRow>
              {/each}
            </BrowserSection>
          {/each}
        {/if}
      {/snippet}
          </BrowserShell>
        </div>
      {/if}
    </div>
    <div class="status silk">{status}</div>
  {/if}
</div>

<style>
  .manager {
    display: flex;
    flex-direction: column;
    gap: 10px;
    flex: 1;
    min-height: 0;
  }

  .top {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .toolbar {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: nowrap;
  }
  .search {
    flex: 1;
    min-width: 80px;
    padding: 6px 8px;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text);
    font-size: 11px;
    outline: none;
  }
  .search:focus-visible {
    border-color: var(--cyan-dim);
  }
  .foldall {
    flex: none;
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    font-size: 11px;
    line-height: 1;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    cursor: pointer;
  }
  .foldall:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .chips {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-wrap: nowrap;
    min-width: 0;
    overflow-x: auto;
  }
  .chips.primary {
    flex: 1;
  }
  .chips.types {
    flex: none;
    padding-bottom: 2px;
  }

  .panes {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .pane {
    min-height: 96px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }
  .pane.fill {
    flex: 1;
  }
  .splitter {
    flex: none;
    height: 6px;
    margin: 2px 0;
    cursor: ns-resize;
    border-radius: 3px;
    background: var(--glass-border);
  }
  .splitter:hover {
    background: var(--cyan-dim);
  }
  .status {
    flex: none;
    padding-top: 4px;
    border-top: var(--border-width) solid var(--glass-border);
  }
  .scan {
    flex: 1;
    min-width: 0;
    padding: 8px 10px;
    font-size: 10px;
    letter-spacing: 0.16em;
    border: none;
    border-radius: 5px;
    cursor: pointer;
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--violet), var(--magenta));
    box-shadow: 0 0 calc(14px * var(--glow-scale)) rgb(var(--violet-rgb) / 0.25);
    transition: filter 120ms;
  }
  .scan:hover:not(:disabled) {
    filter: brightness(1.15);
  }
  .scan:disabled {
    opacity: 0.6;
    cursor: wait;
  }

  .modes {
    display: flex;
    gap: 4px;
    flex: none;
  }

  .chip {
    flex: none;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    background: transparent;
    color: var(--text-faint);
    font-size: 8px;
    letter-spacing: 0.1em;
    padding: 2px 6px;
    white-space: nowrap;
    cursor: pointer;
  }
  .chip.on {
    color: var(--cyan);
    border-color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.06);
  }

  .err {
    color: var(--red);
  }

  .badge {
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

  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
  }
  .dot.active {
    background: var(--green);
  }
  .dot.stub {
    background: var(--amber);
  }
  .dot.crashed {
    background: var(--red);
  }

  .act {
    flex: none;
    padding: 3px 7px;
    font-size: 8px;
    letter-spacing: 0.12em;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .act:hover:not(:disabled) {
    color: var(--cyan);
    border-color: var(--cyan);
  }
  .act.on {
    color: var(--amber);
    border-color: var(--amber);
  }
  .act:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .place {
    flex: none;
    max-width: 9rem;
    padding: 3px 4px;
    font-size: 8px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    cursor: pointer;
  }
  .place:hover:not(:disabled) {
    color: var(--cyan);
    border-color: var(--cyan);
  }
  .place:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }

  .del {
    width: 16px;
    height: 16px;
    line-height: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 13px;
    cursor: pointer;
    border-radius: 3px;
  }
  .del:hover {
    color: var(--red);
  }
</style>
