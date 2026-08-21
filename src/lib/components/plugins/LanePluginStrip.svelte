<script lang="ts">
  /**
   * The lane plugin strip (design §3.4, plan §6.3): the plugin chain, its
   * status dots and its pinned-or-automated param chips, right on the track
   * header — "easy to keep an overview and jump to the parameter you want
   * to automate" without opening the manager. Reads `lane-strip.ts`'s pure
   * projection; this component is display + gesture wiring only.
   *
   * Ruling P-5: the FX chip stays — this strip sits beside it, not instead
   * of it. Ruling P-6: the overflow budget (`maxEntries`/`chipsPerEntry`)
   * is a constant, not a measurement; the CSS (`flex-wrap: nowrap;
   * overflow: hidden`) is the belt that keeps a long name from pushing the
   * FX chip out of the row.
   */
  import { untrack } from "svelte";
  import type { TrackState } from "../../types/ipc";
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { modulation } from "../../state/modulation.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import { backend } from "../../tauri";
  import { openPluginParams } from "../../state/plugin-panel";
  import { revealParamLane } from "../../utils/lane-reveal";
  import { paramNormalized } from "../../utils/plugin-params";
  import { buildLaneStrip, fitLaneStrip, type StripDevice } from "../../utils/lane-strip";
  import ParamChip from "../browser/ParamChip.svelte";

  let {
    track,
    folded = false,
    onoverflow,
  }: {
    track: TrackState;
    folded?: boolean;
    /** Overflow's `+N` reaches up to the parent, which owns the
     * `InsertChain` popover (same popover the FX chip opens). */
    onoverflow?: () => void;
  } = $props();

  const devices = $derived(
    buildLaneStrip({
      track,
      instances: plugins.instances,
      bindings: modulation.bindings,
      pinnedFor: (uid) => plugins.pinnedParamsFor(uid),
      paramInfo: (instanceId, paramId) => plugins.paramInfo(instanceId, paramId),
    }),
  );

  const fit = $derived(
    fitLaneStrip(
      devices,
      folded ? { maxEntries: 4, chipsPerEntry: 0 } : { maxEntries: 2, chipsPerEntry: 2 },
    ),
  );

  // instance-id MEMBERSHIP only, deliberately re-derived from inert
  // stand-ins for bindings/pinnedFor/paramInfo rather than read off
  // `devices` — see AutomationMatrix.svelte's identical split. `devices`
  // also carries chip VALUES (via paramInfo/pinnedFor) and is new-by-
  // reference on every recompute, so an effect reading it would re-run on
  // every knob drag and every lane click. Which instances are on this
  // track's chain never depends on param values or bindings, only on
  // `track.instrumentId`/`track.inserts` and `plugins.instances`.
  const instanceIds = $derived(
    new Set(
      buildLaneStrip({
        track,
        instances: plugins.instances,
        bindings: [],
        pinnedFor: () => [],
        paramInfo: () => undefined,
      }).map((d) => d.instanceId),
    ),
  );

  // `ensureParams` reads `paramCache` synchronously for its dedup guard;
  // wrapped in `untrack` so this effect stays subscribed to `instanceIds`
  // only, not to the cache it just populated (see the comment above and
  // Task 2's dom test for what breaks without it).
  $effect(() => {
    const ids = instanceIds;
    untrack(() => {
      for (const id of ids) void plugins.ensureParams(id);
    });
  });

  function nameTitle(d: StripDevice): string {
    return d.kind === "insert"
      ? `Click: open ${d.name}'s params · Alt+click: ${d.bypassed ? "un-bypass" : "bypass"} ${d.name}`
      : `Click: open ${d.name}'s params`;
  }

  async function toggleBypass(d: StripDevice) {
    if (d.kind !== "insert" || d.slotIndex === undefined) return;
    const slot = project.trackById(track.id)?.inserts?.[d.slotIndex];
    if (!slot) return;
    try {
      await backend.insertSetBypass?.(track.id, slot.id, !d.bypassed);
    } catch (err) {
      toasts.error("BYPASS FAILED", String(err));
    }
  }

  function onNameClick(d: StripDevice, e: MouseEvent) {
    if (e.altKey && d.kind === "insert") {
      void toggleBypass(d);
      return;
    }
    void openPluginParams(d.instanceId);
  }

  function onChipClick(instanceId: string, paramId: number) {
    const info = plugins.paramInfo(instanceId, paramId);
    void revealParamLane(
      track.id,
      { kind: "pluginParam", instanceId, paramId },
      info ? paramNormalized(info) : undefined,
    );
  }
</script>

{#if fit.shown.length > 0}
  <div class="strip" class:folded role="group" aria-label="Plugin chain for {track.name}">
    {#each fit.shown as d (d.instanceId)}
      {#if folded}
        <!-- Winner spec §3.4: "Folded lane: dots only, no chips." The
             collapsed row is 22px tall — a name button doesn't fit, and
             a dot per device is the whole point of the folded strip. -->
        <span
          class="dot {d.status}"
          title="{d.name} — {d.status}"
          role="img"
          aria-label="{d.name}, {d.status}"
        ></span>
      {:else}
        <span class="device" class:bypassed={d.bypassed}>
          <span class="dot {d.status}" title={d.status} aria-hidden="true"></span>
          <button type="button" class="name mono" title={nameTitle(d)} onclick={(e) => onNameClick(d, e)}>
            {d.name}
          </button>
          {#each d.chips as chip (chip.paramId)}
            <ParamChip
              label={chip.label}
              value={0}
              format={() => chip.valueText}
              state={chip.state}
              title="Jump to {chip.label}'s lane"
              onclick={() => onChipClick(d.instanceId, chip.paramId)}
            />
          {/each}
        </span>
      {/if}
    {/each}
    {#if fit.overflow > 0 && onoverflow}
      <button type="button" class="overflow mono" title="Show the rest of the chain" onclick={() => onoverflow?.()}>
        +{fit.overflow}
      </button>
    {/if}
  </div>
{/if}

<style>
  .strip {
    display: flex;
    align-items: center;
    flex-wrap: nowrap;
    overflow: hidden;
    min-width: 0;
    gap: 4px;
  }
  .strip.folded {
    gap: 3px;
  }

  .device {
    display: flex;
    align-items: center;
    gap: 3px;
    min-width: 0;
    flex: 0 1 auto;
  }
  .device.bypassed .name {
    color: var(--text-faint);
    text-decoration: line-through;
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

  .name {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 96px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 2px 4px;
    border-radius: 3px;
    border: none;
    background: none;
    color: var(--text-mid);
    font-size: 10px;
    cursor: pointer;
  }
  .name:hover {
    color: var(--text);
    background: rgb(var(--bg-2-rgb) / 0.6);
  }

  .overflow {
    flex: none;
    padding: 2px 6px;
    border-radius: 3px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-1-rgb) / 0.6);
    color: var(--text-dim);
    font-size: 10px;
    cursor: pointer;
  }
  .overflow:hover {
    color: var(--text);
    background: rgb(var(--bg-2-rgb) / 0.75);
  }
</style>
