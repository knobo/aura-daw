<script lang="ts">
  /**
   * Track-header target menu: gain, pan, then each plugin instance's
   * params grouped by instance. A checkmark marks a target that already
   * has a binding. Picking mints a curve+binding when none exists and
   * shows that overlay.
   */
  import type { PluginInstanceInfo, PluginParamInfo, TargetRef, TrackState } from "../types/ipc";
  import { modulation, panNormalizedOf, targetsEqual } from "../state/modulation.svelte";
  import { plugins } from "../state/plugins.svelte";
  import { project } from "../state/project.svelte";
  import { backend } from "../tauri";

  let {
    track,
    onclose,
    sourceTrackId,
    contentId,
  }: {
    track?: TrackState;
    onclose: () => void;
    /** When set, picks bind this automation track (project-wide targets). */
    sourceTrackId?: string;
    /** Piano-roll clip envelope: self track gain/pan + instrument params. */
    contentId?: string;
  } = $props();

  const projectScope = $derived(!!sourceTrackId);
  const clipScope = $derived(!!contentId);
  const mixerTracks = $derived(
    projectScope ? project.tracks.filter((t) => t.kind !== "automation") : track ? [track] : [],
  );

  let groups = $state<{ inst: PluginInstanceInfo; params: PluginParamInfo[] }[]>([]);

  $effect(() => {
    const insts = projectScope
      ? plugins.instances.slice()
      : clipScope
        ? [plugins.instanceForRef(track?.instrumentId)].filter(
            (i): i is PluginInstanceInfo => i != null,
          )
        : plugins.instances.filter(
            (i) =>
              !!track && (i.trackId === track.id || track.instrumentId === `plugin:${i.id}`),
          ).concat(
            // Also include any plugin instances that are in the track's
            // insert-FX slots so their params appear in the automation picker.
            (track?.inserts ?? [])
              .map((s) => plugins.byId(s.instanceId))
              .filter((i): i is PluginInstanceInfo => i != null),
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

  function bound(target: TargetRef): boolean {
    if (contentId) {
      return modulation.clipEnvelopeBindings(contentId).some((b) => targetsEqual(b.target, target));
    }
    if (sourceTrackId) {
      return modulation.bindingsFrom(sourceTrackId).some((b) => {
        const t = b.target;
        if (t.kind !== target.kind) return false;
        if (t.kind === "trackParam" && target.kind === "trackParam") {
          return t.trackId === target.trackId && t.param === target.param;
        }
        if (t.kind === "pluginParam" && target.kind === "pluginParam") {
          return t.instanceId === target.instanceId && t.paramId === target.paramId;
        }
        return false;
      });
    }
    return modulation.bindingsFor(target).length > 0;
  }

  function pick(target: TargetRef, initial?: number) {
    if (contentId) {
      void modulation.pickClipEnvelope(contentId, target, initial);
    } else if (sourceTrackId) {
      void modulation.addTarget(sourceTrackId, target);
    } else if (track) {
      void modulation.pickTarget(track.id, target, initial);
    }
    onclose();
  }
  function pickGain(t: TrackState) {
    if (contentId) pick({ kind: "selfTrackParam", param: "gain" });
    else pick({ kind: "trackParam", trackId: t.id, param: "gain" });
  }
  function pickPan(t: TrackState) {
    if (contentId) pick({ kind: "selfTrackParam", param: "pan" }, panNormalizedOf(t.pan));
    else pick({ kind: "trackParam", trackId: t.id, param: "pan" }, panNormalizedOf(t.pan));
  }
  function pickPlugin(inst: PluginInstanceInfo, p: PluginParamInfo) {
    const n = p.max === p.min ? 0 : (p.value - p.min) / (p.max - p.min);
    if (contentId) pick({ kind: "selfInstrumentParam", paramId: p.id }, n);
    else pick({ kind: "pluginParam", instanceId: inst.id, paramId: p.id }, n);
  }

  function gainBound(t: TrackState): boolean {
    return contentId
      ? bound({ kind: "selfTrackParam", param: "gain" })
      : bound({ kind: "trackParam", trackId: t.id, param: "gain" });
  }
  function panBound(t: TrackState): boolean {
    return contentId
      ? bound({ kind: "selfTrackParam", param: "pan" })
      : bound({ kind: "trackParam", trackId: t.id, param: "pan" });
  }
  function pluginBound(inst: PluginInstanceInfo, p: PluginParamInfo): boolean {
    return contentId
      ? bound({ kind: "selfInstrumentParam", paramId: p.id })
      : bound({ kind: "pluginParam", instanceId: inst.id, paramId: p.id });
  }
</script>

<div class="backdrop" role="presentation" onpointerdown={onclose}></div>
<div
  class="menu glass mono"
  role="menu"
  aria-label={clipScope ? "Clip envelope target" : projectScope ? "Add automation target" : "Automation lanes"}
>
  {#each mixerTracks as t (t.id)}
    {#if projectScope}
      <div class="group">{t.name}</div>
    {/if}
    <button role="menuitem" onclick={() => pickGain(t)}>
      <span>Gain</span>
      {#if gainBound(t)}<span class="mark" aria-label="bound">✓</span>{/if}
    </button>
    <button role="menuitem" onclick={() => pickPan(t)}>
      <span>Pan</span>
      {#if panBound(t)}<span class="mark" aria-label="bound">✓</span>{/if}
    </button>
  {/each}
  {#each groups as g (g.inst.id)}
    <div class="group">{g.inst.name}</div>
    {#each g.params as p (p.id)}
      <button role="menuitem" onclick={() => pickPlugin(g.inst, p)}>
        <span title={p.name}>{p.name}</span>
        {#if pluginBound(g.inst, p)}<span class="mark" aria-label="bound">✓</span>{/if}
      </button>
    {/each}
  {/each}
  {#if track && !projectScope && !clipScope && modulation.hasVisible(track.id)}
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
  {#if clipScope && contentId && modulation.hasVisible(contentId)}
    <button
      class="hide"
      role="menuitem"
      onclick={() => {
        modulation.hideAll(contentId);
        onclose();
      }}
    >
      Hide envelope
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
    background: rgb(var(--bg-sunken-rgb) / 0.96);
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
    background: rgb(var(--cyan-rgb) / 0.09);
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
