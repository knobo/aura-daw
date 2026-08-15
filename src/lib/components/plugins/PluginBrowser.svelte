<script lang="ts">
  /**
   * Plugin browser: scan trigger (plugin_scan), discovered CLAP/LV2 list
   * with format badges, instantiation onto midi tracks (plugin_instantiate +
   * set_track_instrument "plugin:<id>" refs), and the live-instance rack with
   * stub/active/crashed status badges, rebinding, params and removal.
   */
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { zyn, isZynInstance } from "../../state/zynpatches.svelte";
  import type { PluginDescriptor, PluginInstanceInfo } from "../../types/ipc";
  import { tracksBoundToInstance } from "../../utils/plugin-binding";
  import PluginConnectionBadge from "./PluginConnectionBadge.svelte";

  let instrumentsOnly = $state(true);

  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  const shown = $derived(
    instrumentsOnly ? plugins.descriptors.filter((d) => d.isInstrument) : plugins.descriptors,
  );
  const hiddenFx = $derived(plugins.descriptors.length - shown.length);

  function onInstantiate(desc: PluginDescriptor, e: Event) {
    const sel = e.currentTarget as HTMLSelectElement;
    const trackId = sel.value === "-" ? null : sel.value;
    sel.value = "";
    void plugins.instantiate(desc.uid, trackId);
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
      {plugins.descriptors.length === 0
        ? "scan found no plugins on this machine"
        : "no instruments found — untick the filter to see effects"}
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
          {#if !desc.isInstrument}<span class="fxnote">fx — v1 hosts instruments only</span>{/if}
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
            <button class="params mono" onclick={() => plugins.openParams(inst.id)}>
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
    box-shadow: 0 0 14px rgba(157, 123, 255, 0.25);
    transition: filter 120ms;
  }
  .scan:hover:not(:disabled) {
    filter: brightness(1.15);
  }
  .scan:disabled {
    opacity: 0.6;
    cursor: wait;
  }
  .filter {
    display: flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
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
    border: 1px solid var(--glass-border);
    border-radius: 6px;
    background: rgba(10, 13, 23, 0.6);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .plug.fx {
    opacity: 0.55;
  }
  .inst {
    border-color: rgba(157, 123, 255, 0.22);
  }
  .inst.crashed {
    border-color: rgba(255, 65, 82, 0.4);
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
  .fxnote {
    color: var(--amber);
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
    border: 1px solid;
  }
  .badge.clap {
    color: var(--cyan);
    border-color: rgba(82, 229, 255, 0.4);
    background: rgba(82, 229, 255, 0.08);
  }
  .badge.lv2 {
    color: var(--violet);
    border-color: rgba(157, 123, 255, 0.4);
    background: rgba(157, 123, 255, 0.08);
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
    background: rgba(255, 200, 87, 0.1);
    animation: stub-pulse 1.2s ease-in-out infinite;
  }
  @keyframes stub-pulse {
    50% {
      opacity: 0.5;
    }
  }
  .status.active {
    color: #5cf2b8;
    background: rgba(92, 242, 184, 0.1);
  }
  .status.crashed {
    color: var(--red);
    background: rgba(255, 65, 82, 0.12);
  }

  select {
    flex: 1;
    min-width: 0;
    background: rgba(5, 7, 13, 0.7);
    color: var(--text-dim);
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    font-size: 10px;
    padding: 4px 6px;
  }

  .params {
    padding: 4px 8px;
    font-size: 9px;
    letter-spacing: 0.12em;
    border-radius: 4px;
    border: 1px solid rgba(82, 229, 255, 0.3);
    background: rgba(82, 229, 255, 0.06);
    color: var(--cyan);
    cursor: pointer;
    white-space: nowrap;
  }
  .params:hover {
    background: rgba(82, 229, 255, 0.16);
  }

  .patches {
    padding: 4px 8px;
    font-size: 9px;
    letter-spacing: 0.12em;
    border-radius: 4px;
    border: 1px solid rgba(157, 123, 255, 0.35);
    background: rgba(157, 123, 255, 0.07);
    color: var(--violet);
    cursor: pointer;
    white-space: nowrap;
  }
  .patches:hover {
    background: rgba(157, 123, 255, 0.18);
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
