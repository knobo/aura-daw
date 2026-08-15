<script lang="ts">
  /**
   * Track-header target menu: gain, pan, then each plugin instance's
   * params grouped by instance. A checkmark marks a target that already
   * has a binding. Picking mints a curve+binding when none exists and
   * shows that overlay.
   */
  import type { PluginInstanceInfo, PluginParamInfo, TrackState } from "../types/ipc";
  import { modulation, panNormalizedOf } from "../state/modulation.svelte";
  import { plugins } from "../state/plugins.svelte";
  import { backend } from "../tauri";

  let { track, onclose }: { track: TrackState; onclose: () => void } = $props();

  let groups = $state<{ inst: PluginInstanceInfo; params: PluginParamInfo[] }[]>([]);

  $effect(() => {
    const insts = plugins.instances.filter(
      (i) => i.trackId === track.id || track.instrumentId === `plugin:${i.id}`,
    );
    let cancelled = false;
    void Promise.all(
      insts.map(async (inst) => ({
        inst,
        params: await backend.pluginGetParams(inst.id).catch(() => [] as PluginParamInfo[]),
      })),
    ).then((next) => {
      if (!cancelled) groups = next;
    });
    return () => {
      cancelled = true;
    };
  });

  function boundGain(): boolean {
    return (
      modulation.bindingsFor({ kind: "trackParam", trackId: track.id, param: "gain" }).length > 0
    );
  }
  function boundPan(): boolean {
    return (
      modulation.bindingsFor({ kind: "trackParam", trackId: track.id, param: "pan" }).length > 0
    );
  }
  function boundPlugin(instanceId: string, paramId: number): boolean {
    return (
      modulation.bindingsFor({ kind: "pluginParam", instanceId, paramId }).length > 0
    );
  }

  function pickGain() {
    void modulation.pickTarget(track.id, {
      kind: "trackParam",
      trackId: track.id,
      param: "gain",
    });
    onclose();
  }
  function pickPan() {
    void modulation.pickTarget(
      track.id,
      { kind: "trackParam", trackId: track.id, param: "pan" },
      panNormalizedOf(track.pan),
    );
    onclose();
  }
  function pickPlugin(inst: PluginInstanceInfo, p: PluginParamInfo) {
    const n = p.max === p.min ? 0 : (p.value - p.min) / (p.max - p.min);
    void modulation.pickTarget(
      track.id,
      { kind: "pluginParam", instanceId: inst.id, paramId: p.id },
      n,
    );
    onclose();
  }
</script>

<div class="backdrop" role="presentation" onpointerdown={onclose}></div>
<div class="menu glass mono" role="menu" aria-label="Automation lanes">
  <button role="menuitem" onclick={pickGain}>
    <span>Gain</span>
    {#if boundGain()}<span class="mark" aria-label="bound">✓</span>{/if}
  </button>
  <button role="menuitem" onclick={pickPan}>
    <span>Pan</span>
    {#if boundPan()}<span class="mark" aria-label="bound">✓</span>{/if}
  </button>
  {#each groups as g (g.inst.id)}
    <div class="group">{g.inst.name}</div>
    {#each g.params as p (p.id)}
      <button role="menuitem" onclick={() => pickPlugin(g.inst, p)}>
        <span title={p.name}>{p.name}</span>
        {#if boundPlugin(g.inst.id, p.id)}<span class="mark" aria-label="bound">✓</span>{/if}
      </button>
    {/each}
  {/each}
  {#if modulation.hasVisible(track.id)}
    <button
      class="hide"
      role="menuitem"
      onclick={() => {
        modulation.hideAll(track.id);
        onclose();
      }}
    >
      Hide lanes
    </button>
  {/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 90;
    min-width: 168px;
    max-height: 280px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding: 5px;
    border-radius: 7px;
    background: rgba(8, 10, 19, 0.96);
  }
  .menu button {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 14px;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 7px 9px;
    font-size: 10px;
    letter-spacing: 0.16em;
    color: var(--text-dim);
    cursor: pointer;
    text-align: left;
  }
  .menu button:hover {
    background: rgba(82, 229, 255, 0.09);
    color: var(--cyan);
  }
  .mark {
    color: var(--cyan);
    letter-spacing: 0;
  }
  .group {
    padding: 8px 9px 3px;
    font-size: 8px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--text-faint);
    border-top: 1px solid var(--glass-border);
    margin-top: 3px;
  }
  .hide {
    border-top: 1px solid var(--glass-border);
    margin-top: 4px;
    color: var(--text-faint);
  }
</style>
