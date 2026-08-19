<script lang="ts">
  /**
   * Inline effect-chain rack: renders insert-FX slots as clickable chips
   * inside the track header. Each chip shows the plugin name, format badge,
   * bypass toggle, and a shortcuts popover.
   *
   * Pedagogic design: the chain reads left-to-right = signal flow, exactly
   * like a guitar pedalboard. Empty state gently suggests what to do:
   * "No effects — + to add reverb, EQ, compressor…"
   */
  import type { InsertSlot, PluginInstanceInfo, TrackState } from "../../types/ipc";
  import { plugins } from "../../state/plugins.svelte";
  import { openPluginParams } from "../../state/plugin-panel";
  import { backend } from "../../tauri";

  let {
    track,
  }: {
    track: TrackState;
  } = $props();

  const insertSlots = $derived(track.inserts ?? []);
  const busy = $state<Record<string, boolean>>({});

  /** Resolve each slot to its plugin instance info. */
  function slotInstance(slot: InsertSlot): PluginInstanceInfo | undefined {
    return plugins.byId(slot.instanceId);
  }

  /** Pick an effect from the browser and add it as a new insert slot. */
  async function addEffect(uid: string) {
    const key = `add:${uid}`;
    busy[key] = true;
    try {
      await backend.insertAdd?.(track.id, uid);
    } catch (err) {
      console.error("[aura] insert_add failed:", err);
    } finally {
      busy[key] = false;
    }
  }

  /** Toggle bypass on a slot. */
  async function toggleBypass(slot: InsertSlot) {
    busy[slot.id] = true;
    try {
      await backend.insertSetBypass?.(track.id, slot.id, !slot.bypassed);
    } catch (err) {
      console.error("[aura] insert_set_bypass failed:", err);
    } finally {
      busy[slot.id] = false;
    }
  }

  /** Remove a slot and its plugin instance. */
  async function removeSlot(slot: InsertSlot) {
    busy[slot.id] = true;
    try {
      await backend.insertRemove?.(track.id, slot.id);
    } catch (err) {
      console.error("[aura] insert_remove failed:", err);
    } finally {
      busy[slot.id] = false;
    }
  }

  /** Open the param panel for a slot's plugin instance. */
  function openParams(instanceId: string) {
    void openPluginParams(instanceId);
  }

  /** Move a slot to a new index (drag-to-reorder). */
  async function moveSlot(slotId: string, toIndex: number) {
    try {
      await backend.insertReorder?.(track.id, slotId, toIndex);
    } catch (err) {
      console.error("[aura] insert_reorder failed:", err);
    }
  }

  // ── effect picker popup ──
  let pickerOpen = $state(false);

  const fxPlugins = $derived(
    plugins.descriptors.filter((d) => !d.isInstrument && plugins.scanned),
  );

  const hasEffectsScanned = $derived(fxPlugins.length > 0);
  const scanNeeded = $derived(!plugins.scanned && !plugins.scanning);
</script>

<div class="chain">
  <div class="chainhead">
    <span class="label mono">Effects</span>
    <span class="picker">
      <button
        class="addbtn mono"
        title="Add an effect (reverb, EQ, compressor…)"
        aria-label="Add effect to {track.name}"
        onclick={() => (pickerOpen = !pickerOpen)}
        disabled={plugins.scanning}
      >
        +{plugins.scanning ? " scan…" : ""}
      </button>
      {#if pickerOpen}
        <div class="backdrop" role="presentation" onpointerdown={() => (pickerOpen = false)}></div>
        <div class="fxpicker mono" role="menu" aria-label="Select an effect plugin">
          {#if scanNeeded}
            <button
              class="scanbtn"
              role="menuitem"
              onclick={() => {
                void plugins.scan();
                pickerOpen = false;
              }}
            >
              Scan for plugins…
            </button>
          {:else if !hasEffectsScanned}
            <div class="empty-state silk">No effects found — scan discovered no effect plugins</div>
          {:else}
            {#each fxPlugins as desc (desc.uid)}
              <button
                class="fxitem"
                role="menuitem"
                disabled={!!busy[`add:${desc.uid}`]}
                onclick={() => {
                  void addEffect(desc.uid);
                  pickerOpen = false;
                }}
              >
                <span class="fbadge mono {desc.format}">{desc.format}</span>
                <span class="fname">{desc.name}</span>
                {#if desc.vendor}
                  <span class="fvendor silk">{desc.vendor}</span>
                {/if}
              </button>
            {/each}
          {/if}
        </div>
      {/if}
    </span>
  </div>

  <div class="slots" role="list" aria-label="Effect chain">
    {#if insertSlots.length === 0}
      <span class="empty silk">+ add effect</span>
    {:else}
      {#each insertSlots as slot (slot.id)}
        {@const inst = slotInstance(slot)}
        <div
          class="slot"
          class:bypassed={slot.bypassed}
          role="listitem"
          aria-label={inst?.name ?? "Unknown effect"}
        >
          {#if inst}
            <button
              class="byp mono"
              class:on={!slot.bypassed}
              title={slot.bypassed ? "Bypassed — click to enable" : "Active — click to bypass"}
              aria-pressed={!slot.bypassed}
              onclick={() => toggleBypass(slot)}
              disabled={!!busy[slot.id]}
            >
              {slot.bypassed ? "B" : "A"}
            </button>
            <span class="fbadge mono {inst.format}">{inst.format}</span>
            <button
              class="pname"
              title="Open {inst.name} parameters"
              onclick={() => openParams(inst.id)}
            >
              {inst.name}
            </button>
            {#if inst.status === "stub"}
              <span class="status mono stub">stub</span>
            {:else if inst.status === "crashed"}
              <span class="status mono crashed">crash</span>
            {/if}
            <button
              class="del mono"
              title="Remove {inst.name} from the chain"
              aria-label="Remove {inst.name}"
              onclick={() => removeSlot(slot)}
              disabled={!!busy[slot.id]}
            >
              ×
            </button>
          {:else}
            <span class="unknown silk">unknown instance {slot.instanceId.slice(0, 8)}…</span>
            <button
              class="del mono"
              title="Remove this stale slot"
              onclick={() => removeSlot(slot)}
            >
              ×
            </button>
          {/if}
        </div>
      {/each}
    {/if}
  </div>

  {#if insertSlots.length > 1}
    <div class="reorder-row mono">
      {#each insertSlots as slot, i (slot.id)}
        {#if i > 0}
          <button
            class="reorderbtn"
            title="Move {slotInstance(slot)?.name ?? 'effect'} earlier in chain"
            onclick={() => moveSlot(slot.id, i - 1)}
          >↑</button>
        {/if}
        {#if i < insertSlots.length - 1}
          <button
            class="reorderbtn"
            title="Move {slotInstance(slot)?.name ?? 'effect'} later in chain"
            onclick={() => moveSlot(slot.id, i + 1)}
          >↓</button>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .chain {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 1px 0;
  }
  .chainhead {
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 14px;
  }
  .label {
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-faint);
    width: 62px;
    flex: none;
  }
  .picker {
    position: relative;
    flex: none;
  }
  .addbtn {
    padding: 2px 8px;
    border-radius: 3px;
    border: var(--border-width) dashed rgb(var(--edge-rgb) / 0.25);
    background: transparent;
    color: var(--text-faint);
    font-size: 8px;
    letter-spacing: 0.12em;
    cursor: pointer;
    white-space: nowrap;
  }
  .addbtn:hover:not(:disabled) {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .addbtn:disabled {
    cursor: wait;
  }
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .fxpicker {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 90;
    min-width: 200px;
    max-height: 200px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding: 5px;
    border-radius: 7px;
    background: rgb(var(--bg-sunken-rgb) / 0.96);
    border: var(--border-width) solid var(--glass-border);
  }
  .fxitem {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 6px 8px;
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
  .fname {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-dim);
  }
  .fvendor {
    font-size: 8px;
    color: var(--text-faint);
  }
  .scanbtn {
    font-size: 10px;
    padding: 6px 8px;
    color: var(--cyan);
    background: transparent;
    border: none;
    cursor: pointer;
    text-align: left;
  }
  .scanbtn:hover {
    background: rgb(var(--cyan-rgb) / 0.09);
  }
  .empty-state {
    padding: 6px 8px;
    font-size: 9px;
  }

  .slots {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .empty {
    font-size: 8px;
    color: var(--text-faint);
    padding: 1px 0;
    cursor: default;
  }

  .slot {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 5px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--violet-rgb) / 0.18);
    background: rgb(var(--bg-1-rgb) / 0.5);
    min-height: 18px;
  }
  .slot.bypassed {
    opacity: 0.5;
    border-color: var(--glass-border);
  }

  .byp {
    flex: none;
    width: 15px;
    height: 14px;
    line-height: 1;
    font-size: 8px;
    letter-spacing: 0;
    border-radius: 3px;
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
    box-shadow: 0 0 calc(5px * var(--glow-scale)) rgb(var(--green-rgb) / 0.3);
  }
  .byp:hover:not(:disabled) {
    border-color: rgb(var(--green-rgb) / 0.4);
  }
  .byp:disabled {
    cursor: wait;
  }

  .fbadge {
    flex: none;
    font-size: 7px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 1px 4px;
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

  .status {
    flex: none;
    font-size: 7px;
    letter-spacing: 0.1em;
    padding: 1px 4px;
    border-radius: 2px;
  }
  .status.stub {
    color: var(--amber);
    background: rgb(var(--amber-rgb) / 0.1);
  }
  .status.crashed {
    color: var(--red);
    background: rgb(var(--red-rgb) / 0.1);
  }

  .unknown {
    flex: 1;
    font-size: 8px;
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

  .reorder-row {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
    padding-left: 62px;
  }
  .reorderbtn {
    background: transparent;
    border: var(--border-width) solid transparent;
    color: var(--text-faint);
    font-size: 9px;
    cursor: pointer;
    padding: 0 3px;
  }
  .reorderbtn:hover {
    color: var(--cyan);
    border-color: rgb(var(--cyan-rgb) / 0.3);
    border-radius: 2px;
  }
</style>