<script lang="ts">
  /**
   * Plugin browser: scan trigger (plugin_scan), discovered CLAP/LV2 list
   * with format badges, instantiation onto midi tracks (plugin_instantiate +
   * set_track_instrument "plugin:<id>" refs), insert-FX onto audio/midi
   * tracks, fuzzy search, and the live-instance rack with stub/active/crashed
   * status badges, rebinding, params and removal.
   */
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { openPluginParams } from "../../state/plugin-panel";
  import { toasts } from "../../state/toasts.svelte";
  import { zyn, isZynInstance } from "../../state/zynpatches.svelte";
  import { backend } from "../../tauri";
  import type { PluginDescriptor, PluginInstanceInfo } from "../../types/ipc";
  import { tracksBoundToInstance } from "../../utils/plugin-binding";
  import PluginConnectionBadge from "./PluginConnectionBadge.svelte";

  let instrumentsOnly = $state(true);
  let searchQuery = $state("");
  let busy = $state<Record<string, boolean>>({});

  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  /** Tracks that accept effect inserts: audio and MIDI only. */
  const insertTracks = $derived(
    project.tracks.filter((t) => t.kind === "audio" || t.kind === "midi"),
  );

  /** All plugins matching the instrument/effect filter. */
  const kindFiltered = $derived(
    instrumentsOnly
      ? plugins.descriptors.filter((d) => d.isInstrument)
      : plugins.descriptors,
  );

  /** Fuzzy match: does the query appear in name, vendor, uid or any category? */
  function fuzzy(d: PluginDescriptor, q: string): boolean {
    if (!q) return true;
    const lower = q.toLowerCase();
    return (
      d.name.toLowerCase().includes(lower) ||
      (d.vendor ?? "").toLowerCase().includes(lower) ||
      d.uid.toLowerCase().includes(lower) ||
      (d.categories ?? []).some((c) => c.toLowerCase().includes(lower))
    );
  }

  const shown = $derived(kindFiltered.filter((d) => fuzzy(d, searchQuery)));
  const hiddenFx = $derived(plugins.descriptors.filter((d) => !d.isInstrument).length);

  function onInstantiate(desc: PluginDescriptor, e: Event) {
    const sel = e.currentTarget as HTMLSelectElement;
    const trackId = sel.value === "-" ? null : sel.value;
    sel.value = "";
    void plugins.instantiate(desc.uid, trackId);
  }

  /** Add an effect as an insert-FX slot on the selected track. */
  async function onInsertEffect(uid: string, trackId: string) {
    const key = `${uid}:${trackId}`;
    busy[key] = true;
    try {
      await backend.insertAdd?.(trackId, uid);
    } catch (err) {
      console.error("[aura] insert_add failed:", err);
      toasts.error("INSERT FAILED", String(err));
    } finally {
      busy[key] = false;
    }
  }

  function onBind(inst: PluginInstanceInfo, e: Event) {
    const sel = e.currentTarget as HTMLSelectElement;
    if (sel.value) void plugins.bind(inst.id, sel.value);
    sel.value = "";
  }

  function boundTo(inst: PluginInstanceInfo, trackId: string): boolean {
    return tracksBoundToInstance(project.tracks, inst.id).some((t) => t.id === trackId);
  }

  function isBound(inst: PluginInstanceInfo): boolean {
    return tracksBoundToInstance(project.tracks, inst.id).length > 0;
  }
</script>

<div class="browser">
  <div class="scanrow">
    <button class="scan mono" disabled={plugins.scanning} onclick={() => plugins.scan()}>
      {plugins.scanning ? "◌ SCANNING…" : plugins.scanned ? "↻ RESCAN" : "⌕ SCAN PLUGINS"}
    </button>
  </div>

  <div class="toolbar">
    <input
      class="search mono"
      type="text"
      placeholder="search plugins…"
      bind:value={searchQuery}
      aria-label="Search plugins by name, vendor or category"
    />
    <label class="filter silk">
      <input type="checkbox" bind:checked={instrumentsOnly} />
      instruments
    </label>
  </div>

  {#if plugins.scanError}
    <div class="err silk" role="alert">scan failed — {plugins.scanError}</div>
  {/if}
  {#if plugins.error}
    <div class="err silk" role="alert">{plugins.error}</div>
  {/if}

  {#if !plugins.scanned && !plugins.scanning}
    <div class="empty silk">no scan yet — scan to discover installed clap / lv2 plugins</div>
  {:else if plugins.scanned && shown.length === 0}
    <div class="empty silk">
      {kindFiltered.length === 0
        ? "scan found no plugins on this machine"
        : `no match for "${searchQuery}" — try a different search or untick the filter`}
    </div>
  {/if}

  <div class="list">
    {#each shown as desc (desc.uid)}
      <div class="plug" class:fx={!desc.isInstrument}>
        <div class="row top">
          <span class="badge mono {desc.format}">{desc.format}</span>
          <span class="pname" title={desc.path ?? desc.uid}>{desc.name}</span>
          {#if plugins.busyUid === desc.uid}
            <span class="busy mono">◌</span>
          {/if}
        </div>
        <div class="row meta silk">
          <span class="vendor">{desc.vendor ?? "unknown vendor"}</span>
          {#if desc.version}<span>v{desc.version}</span>{/if}
          {#if !desc.isInstrument}
            <span class="fxchip mono">effect</span>
          {:else}
            <span class="instchip-small mono">instrument</span>
          {/if}
        </div>
        {#if desc.isInstrument}
          <div class="row">
            <select
              title="Instantiate {desc.name}"
              disabled={plugins.busyUid === desc.uid}
              onchange={(e) => onInstantiate(desc, e)}
            >
              <option value="">instantiate…</option>
              <option value="-">instance only (bind later)</option>
              {#each midiTracks as t (t.id)}
                <option value={t.id}>→ {t.name}</option>
              {/each}
            </select>
          </div>
        {:else}
          <div class="row">
            <select
              title="Add {desc.name} as an insert effect"
              disabled={!!busy[`${desc.uid}:sel`]}
              onchange={(e) => {
                const sel = e.currentTarget as HTMLSelectElement;
                const trackId = sel.value;
                sel.value = "";
                if (trackId) void onInsertEffect(desc.uid, trackId);
              }}
            >
              <option value="">add to track…</option>
              {#each insertTracks as t (t.id)}
                <option value={t.id}>→ {t.name}</option>
              {/each}
            </select>
          </div>
        {/if}
      </div>
    {/each}
  </div>

  {#if hiddenFx > 0 && instrumentsOnly}
    <div class="hint silk">{hiddenFx} effect{hiddenFx === 1 ? "" : "s"} hidden by the filter</div>
  {/if}

  {#if plugins.instances.length > 0}
    <div class="rackhead silk">instances</div>
    <div class="list">
      {#each plugins.instances as inst (inst.id)}
        <div class="inst" class:crashed={inst.status === "crashed"}>
          <div class="row top">
            <span class="badge mono {inst.format}">{inst.format}</span>
            <span class="pname" title={inst.uid}>{inst.name}</span>
            <span class="status mono {inst.status}" title="Instance status">{inst.status}</span>
            <button
              class="del mono"
              title="Remove instance"
              aria-label="Remove {inst.name} instance"
              onclick={() => plugins.remove(inst.id)}>×</button
            >
          </div>
          <div class="row">
            <button class="params mono" onclick={() => void openPluginParams(inst.id)}>
              ⚙ PARAMS
            </button>
            {#if isZynInstance(inst)}
              <button
                class="patches mono"
                title="Browse the ZynAddSubFX bank patches"
                onclick={() => zyn.openFor(inst.id)}
              >
                ▤ PATCHES
              </button>
            {/if}
            <select title="Bind to a MIDI track" onchange={(e) => onBind(inst, e)}>
              <option value="">{isBound(inst) ? "rebind…" : "bind to track…"}</option>
              {#each midiTracks as t (t.id)}
                <option value={t.id}>{t.name}{boundTo(inst, t.id) ? " ✓" : ""}</option>
              {/each}
            </select>
          </div>
          <div class="row">
            <PluginConnectionBadge instanceId={inst.id} />
          </div>
        </div>
      {/each}
    </div>
  {/if}

  {#if midiTracks.length === 0}
    <div class="hint silk">no midi tracks yet — add one with "+ midi" in the track rail</div>
  {/if}
  {#if insertTracks.length === 0}
    <div class="hint silk">no audio or midi tracks — add one to insert effects</div>
  {/if}
</div>

<style>
  .browser {
    display: flex;
    flex-direction: column;
    gap: 10px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-right: 2px;
  }

  .scanrow {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .scan {
    flex: 1;
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

  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .search {
    flex: 1;
    min-width: 0;
    padding: 6px 8px;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text);
    font-size: 11px;
    outline: none;
  }
  .search:focus {
    border-color: var(--cyan-dim);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.15);
  }
  .search::placeholder {
    color: var(--text-faint);
  }
  .filter {
    display: flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
    flex: none;
  }
  .filter input {
    accent-color: var(--violet);
  }

  .err {
    color: var(--red);
  }
  .empty,
  .hint {
    padding: 8px 0;
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .plug,
  .inst {
    border: var(--border-width) solid var(--glass-border);
    border-radius: 6px;
    background: rgb(var(--bg-1-rgb) / 0.6);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .plug.fx {
    opacity: 0.85;
  }
  .inst {
    border-color: rgb(var(--violet-rgb) / 0.22);
  }
  .inst.crashed {
    border-color: rgb(var(--red-rgb) / 0.4);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pname {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .meta {
    justify-content: flex-start;
    gap: 10px;
    letter-spacing: 0.08em;
  }
  .vendor {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .fxchip {
    color: var(--cyan);
    font-size: 7px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 1px 5px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--cyan-rgb) / 0.3);
    background: rgb(var(--cyan-rgb) / 0.06);
  }
  .instchip-small {
    color: var(--violet);
    font-size: 7px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 1px 5px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--violet-rgb) / 0.3);
    background: rgb(var(--violet-rgb) / 0.06);
  }
  .busy {
    color: var(--cyan);
    animation: spin-pulse 0.9s linear infinite;
  }
  @keyframes spin-pulse {
    50% {
      opacity: 0.35;
    }
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

  .status {
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: 3px;
  }
  .status.stub {
    color: var(--amber);
    background: rgb(var(--amber-rgb) / 0.1);
    animation: stub-pulse 1.2s ease-in-out infinite;
  }
  @keyframes stub-pulse {
    50% {
      opacity: 0.5;
    }
  }
  .status.active {
    color: var(--green);
    background: rgb(var(--green-rgb) / 0.1);
  }
  .status.crashed {
    color: var(--red);
    background: rgb(var(--red-rgb) / 0.12);
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

  .params {
    padding: 4px 8px;
    font-size: 9px;
    letter-spacing: 0.12em;
    border-radius: 4px;
    border: var(--border-width) solid rgb(var(--cyan-rgb) / 0.3);
    background: rgb(var(--cyan-rgb) / 0.06);
    color: var(--cyan);
    cursor: pointer;
    white-space: nowrap;
  }
  .params:hover {
    background: rgb(var(--cyan-rgb) / 0.16);
  }

  .patches {
    padding: 4px 8px;
    font-size: 9px;
    letter-spacing: 0.12em;
    border-radius: 4px;
    border: var(--border-width) solid rgb(var(--violet-rgb) / 0.35);
    background: rgb(var(--violet-rgb) / 0.07);
    color: var(--violet);
    cursor: pointer;
    white-space: nowrap;
  }
  .patches:hover {
    background: rgb(var(--violet-rgb) / 0.18);
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

  .rackhead {
    margin-top: 2px;
    padding-top: 8px;
    border-top: 1px solid var(--glass-border);
  }
</style>