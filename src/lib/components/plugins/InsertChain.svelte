<script lang="ts">
  /**
   * Effect chain popover: shows the current insert-FX chain for a track
   * with bypass toggles, param access, reorder, and an add button.
   *
   * Positioned below the FX chip in the track header metadata row
   * (same popover pattern as LanePickerMenu).
   */
  import type { InsertSlot, PluginInstanceInfo, TrackState } from "../../types/ipc";
  import { plugins } from "../../state/plugins.svelte";
  import { openPluginParams } from "../../state/plugin-panel";
  import { toasts } from "../../state/toasts.svelte";
  import { backend } from "../../tauri";

  let {
    track,
    onclose,
  }: {
    track: TrackState;
    onclose: () => void;
  } = $props();

  const insertSlots = $derived(track.inserts ?? []);
  const busy = $state<Record<string, boolean>>({});

  function slotInstance(slot: InsertSlot): PluginInstanceInfo | undefined {
    return plugins.byId(slot.instanceId);
  }

  async function addEffect(uid: string) {
    const key = `add:${uid}`;
    busy[key] = true;
    try {
      await plugins.insertEffect(track.id, uid);
    } catch (err) {
      console.error("[aura] insert_add failed:", err);
      toasts.error("INSERT FAILED", String(err));
    } finally {
      busy[key] = false;
    }
  }

  async function toggleBypass(slot: InsertSlot) {
    busy[slot.id] = true;
    try {
      await backend.insertSetBypass?.(track.id, slot.id, !slot.bypassed);
    } catch (err) {
      console.error("[aura] insert_set_bypass failed:", err);
      toasts.error("BYPASS FAILED", String(err));
    } finally {
      busy[slot.id] = false;
    }
  }

  async function removeSlot(slot: InsertSlot) {
    busy[slot.id] = true;
    try {
      await backend.insertRemove?.(track.id, slot.id);
    } catch (err) {
      console.error("[aura] insert_remove failed:", err);
      toasts.error("REMOVE FAILED", String(err));
    } finally {
      busy[slot.id] = false;
    }
  }

  function onOpenParams(instanceId: string) {
    onclose();
    void openPluginParams(instanceId);
  }

  async function moveSlot(slotId: string, toIndex: number) {
    try {
      await backend.insertReorder?.(track.id, slotId, toIndex);
    } catch (err) {
      console.error("[aura] insert_reorder failed:", err);
    }
  }

  // ── add-effect sub-picker ──
  let showPicker = $state(false);
  let filter = $state("");

  // Pre-close if the track is gone or has no inserts
  const fxPlugins = $derived(
    plugins.descriptors.filter((d) => !d.isInstrument && plugins.scanned),
  );
  const filteredFx = $derived(
    fxPlugins.filter((d) => {
      const q = filter.trim().toLowerCase();
      if (!q) return true;
      return (
        d.name.toLowerCase().includes(q) ||
        (d.vendor ?? "").toLowerCase().includes(q) ||
        d.uid.toLowerCase().includes(q)
      );
    }),
  );
  const scanNeeded = $derived(!plugins.scanned && !plugins.scanning);
  const hasEffects = $derived(insertSlots.length > 0);
</script>

<div class="backdrop" role="presentation" onpointerdown={onclose}></div>
<div class="popover glass mono" role="menu" aria-label="Effect chain for {track.name}">
  <div class="pophead">
    <span class="ptitle">Effects</span>
    {#if !showPicker}
      <button
        class="addbtn mono"
        title="Add an effect"
        onclick={() => (showPicker = true)}
        disabled={plugins.scanning}
      >
        +{plugins.scanning ? " scan…" : ""}
      </button>
    {/if}
  </div>

  {#if showPicker}
    <div class="picker-panel">
      {#if scanNeeded}
        <button class="scanbtn" onclick={() => { void plugins.scan(); showPicker = false; }}>
          Scan for plugins…
        </button>
      {:else}
        <input
          class="filter-input"
          type="text"
          placeholder="filter effects…"
          bind:value={filter}
          aria-label="Filter effects"
        />
        {#if filteredFx.length === 0}
          <span class="empty-silk">{fxPlugins.length === 0 ? "No effects found" : "No effects match"}</span>
        {:else}
          {#each filteredFx as desc (desc.uid)}
            <button
              class="fxitem"
              disabled={!!busy[`add:${desc.uid}`]}
              onclick={() => { void addEffect(desc.uid); showPicker = false; }}
            >
              <span class="fbadge mono {desc.format}">{desc.format}</span>
              <span class="fname">{desc.name}</span>
            </button>
          {/each}
        {/if}
      {/if}
      <button class="backbtn mono" onclick={() => (showPicker = false)}>
        ← Back
      </button>
    </div>
  {:else}
    {#if hasEffects}
      <div class="slots">
        {#each insertSlots as slot (slot.id)}
          {@const inst = slotInstance(slot)}
          <div class="slot" class:bypassed={slot.bypassed}>
            {#if inst}
              <div class="slot-row">
                <button
                  class="byp mono"
                  class:on={!slot.bypassed}
                  title={slot.bypassed ? "Enable" : "Bypass"}
                  onclick={() => toggleBypass(slot)}
                  disabled={!!busy[slot.id]}
                >
                  {slot.bypassed ? "B" : "A"}
                </button>
                <span class="fbadge mono {inst.format}">{inst.format}</span>
                {#if inst.status !== "active"}
                  <span
                    class="slot-status mono {inst.status}"
                    title="This plugin is {inst.status} — it will not process audio"
                  >
                    {inst.status}
                  </span>
                {/if}
                <button class="pname" onclick={() => onOpenParams(inst.id)}>
                  {inst.name}
                </button>
                <button
                  class="del mono"
                  title="Remove"
                  onclick={() => removeSlot(slot)}
                  disabled={!!busy[slot.id]}
                >
                  ×
                </button>
              </div>
              {#if insertSlots.length > 1}
                <div class="slot-arrows">
                  {#if insertSlots.indexOf(slot) > 0}
                    <button class="arrowbtn" onclick={() => moveSlot(slot.id, insertSlots.indexOf(slot) - 1)}>↑</button>
                  {/if}
                  {#if insertSlots.indexOf(slot) < insertSlots.length - 1}
                    <button class="arrowbtn" onclick={() => moveSlot(slot.id, insertSlots.indexOf(slot) + 1)}>↓</button>
                  {/if}
                </div>
              {/if}
            {:else}
              <span class="unknown-silk">{slot.instanceId.slice(0, 8)}…</span>
              <button class="del mono" onclick={() => removeSlot(slot)}>×</button>
            {/if}
          </div>
        {/each}
      </div>
    {:else}
      <span class="empty-silk">No effects yet</span>
    {/if}
  {/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .popover {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 90;
    min-width: 180px;
    max-width: 260px;
    max-height: 280px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding: 6px;
    border-radius: 7px;
    background: rgb(var(--bg-sunken-rgb) / 0.96);
    border: var(--border-width) solid var(--glass-border);
  }
  .pophead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 4px;
  }
  .ptitle {
    font-size: 9px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--text-dim);
  }
  .addbtn {
    padding: 2px 7px;
    border-radius: 3px;
    border: var(--border-width) dashed rgb(var(--edge-rgb) / 0.3);
    background: transparent;
    color: var(--text-faint);
    font-size: 9px;
    letter-spacing: 0.1em;
    cursor: pointer;
  }
  .addbtn:hover:not(:disabled) {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .addbtn:disabled {
    cursor: wait;
  }

  .picker-panel {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .filter-input {
    /* Keep the filter box pinned while the effect list scrolls under it. */
    position: sticky;
    top: 0;
    z-index: 2;
    margin-bottom: 2px;
    padding: 4px 7px;
    font-size: 10px;
    color: var(--text);
    background: rgb(var(--bg-sunken-rgb) / 0.98);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    outline: none;
  }
  .filter-input:focus {
    border-color: var(--cyan-dim);
  }
  .filter-input::placeholder {
    color: var(--text-faint);
  }
  .fxitem {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 5px 7px;
    font-size: 10px;
    cursor: pointer;
    text-align: left;
  }
  .fxitem:hover:not(:disabled) {
    background: rgb(var(--cyan-rgb) / 0.09);
    color: var(--cyan);
  }
  .fxitem:disabled {
    cursor: wait;
    opacity: 0.5;
  }
  .scanbtn {
    font-size: 10px;
    padding: 5px 7px;
    color: var(--cyan);
    background: transparent;
    border: none;
    cursor: pointer;
    text-align: left;
    border-radius: 4px;
  }
  .scanbtn:hover {
    background: rgb(var(--cyan-rgb) / 0.09);
  }
  .backbtn {
    font-size: 9px;
    padding: 4px 7px;
    color: var(--text-faint);
    background: transparent;
    border: none;
    cursor: pointer;
    text-align: left;
    border-radius: 4px;
    margin-top: 2px;
  }
  .backbtn:hover {
    color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.07);
  }

  .slots {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .slot {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 3px 5px;
    border-radius: 4px;
    border: var(--border-width) solid rgb(var(--violet-rgb) / 0.2);
    background: rgb(var(--bg-1-rgb) / 0.5);
  }
  .slot.bypassed {
    opacity: 0.5;
    border-color: var(--glass-border);
  }
  .slot-row {
    display: flex;
    align-items: center;
    gap: 5px;
    min-height: 18px;
  }
  .slot-arrows {
    display: flex;
    gap: 2px;
    padding-left: 22px;
  }
  .arrowbtn {
    background: transparent;
    border: none;
    color: var(--text-faint);
    font-size: 8px;
    cursor: pointer;
    padding: 0 2px;
    line-height: 1;
  }
  .arrowbtn:hover {
    color: var(--cyan);
  }

  .byp {
    flex: none;
    width: 14px;
    height: 13px;
    line-height: 1;
    font-size: 7px;
    letter-spacing: 0;
    border-radius: 2px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.2);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    padding: 0;
  }
  .byp.on {
    color: var(--bg-0);
    background: var(--green);
    border-color: var(--green);
  }
  .byp:hover:not(:disabled) {
    border-color: rgb(var(--green-rgb) / 0.4);
  }

  .fbadge {
    flex: none;
    font-size: 7px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 1px 3px;
    border-radius: 2px;
    border: var(--border-width) solid;
  }
  .fbadge.clap {
    color: var(--cyan);
    border-color: rgb(var(--cyan-rgb) / 0.4);
    background: rgb(var(--cyan-rgb) / 0.08);
  }
  .fbadge.lv2 {
    color: var(--violet);
    border-color: rgb(var(--violet-rgb) / 0.4);
    background: rgb(var(--violet-rgb) / 0.08);
  }

  .slot-status {
    flex: none;
    font-size: 7px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    padding: 1px 4px;
    border-radius: 2px;
    border: var(--border-width) solid;
  }
  .slot-status.stub {
    color: var(--amber);
    border-color: rgb(var(--amber-rgb) / 0.4);
    background: rgb(var(--amber-rgb) / 0.1);
  }
  .slot-status.crashed {
    color: var(--red);
    border-color: rgb(var(--red-rgb) / 0.45);
    background: rgb(var(--red-rgb) / 0.12);
  }

  .pname {
    flex: 1;
    min-width: 0;
    text-align: left;
    font-size: 9px;
    color: var(--text);
    background: transparent;
    border: none;
    cursor: pointer;
    padding: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pname:hover {
    color: var(--cyan);
  }

  .del {
    flex: none;
    width: 12px;
    height: 12px;
    line-height: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 11px;
    cursor: pointer;
    padding: 0;
  }
  .del:hover:not(:disabled) {
    color: var(--red);
  }

  .empty-silk {
    font-size: 9px;
    color: var(--text-faint);
    padding: 4px 0;
  }
  .unknown-silk {
    flex: 1;
    font-size: 8px;
    color: var(--text-faint);
  }
</style>