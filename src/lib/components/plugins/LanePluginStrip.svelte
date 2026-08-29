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
  import { paramFollow } from "../../state/param-follow.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { modulation } from "../../state/modulation.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import { backend } from "../../tauri";
  import { openPluginParams } from "../../state/plugin-panel";
  import { revealParamLane } from "../../utils/lane-reveal";
  import { paramNormalized } from "../../utils/plugin-params";
  import { buildLaneStrip, fitLaneStrip, type StripChip, type StripDevice } from "../../utils/lane-strip";
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
      // Painted values follow automation while it is driving them, so this
      // chip cannot read 0.80 while the param panel's chip for the same
      // param reads 0.30 (Track D ruling 2 — the document is not what the
      // plugin has during playback).
      paramInfo: (instanceId, paramId) => {
        const info = plugins.paramInfo(instanceId, paramId);
        return info && paramFollow.overlay(instanceId, info);
      },
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

  // Alt+click is the bypass modifier. On the instrument device (no bypass
  // concept) it must do NOTHING, not silently fall through to opening
  // params — a modifier that means something else on one device in the
  // same row is worse than a modifier that's a no-op there.
  function onNameClick(d: StripDevice, e: MouseEvent) {
    if (e.altKey) {
      if (d.kind === "insert") void toggleBypass(d);
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

  // A "plain" chip (pinned but not yet automated) MINTS the lane on click;
  // an "automated" one jumps to what's already there — the same branch
  // `PluginParamPanel`'s `chipTitle` makes on `pluginBound`, just read off
  // `chip.state` instead since the strip never opens the panel.
  function chipTitle(chip: StripChip): string {
    return chip.state === "automated"
      ? `Jump to ${chip.label}'s lane`
      : `Create an automation lane for ${chip.label}`;
  }
</script>

{#if fit.shown.length > 0}
  <div class="strip" class:folded class:hasmore={fit.overflow > 0 && !!onoverflow} role="group" aria-label="Plugin chain for {track.name}">
    <!-- The devices clip, the `+N` does not. `overflow: hidden` on the
         strip is the belt (Ruling P-6), but now that `.metadata-row` is
         `nowrap` the belt actually tightens — and it would have cut off
         the one control that says there is more chain to see. -->
    <div class="devices">
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
          <!-- The name, not just the status. On a crowded lane the strip is
               clipped down to its dots (`overflow: hidden`, Ruling P-6), and
               the name button that carried this title is the first thing
               gone — so the dot has to answer "which plugin is that". -->
          <span class="dot {d.status}" title="{d.name} — {d.status}" aria-hidden="true"></span>
          <button type="button" class="name mono" title={nameTitle(d)} onclick={(e) => onNameClick(d, e)}>
            {d.name}
          </button>
          {#each d.chips as chip (chip.paramId)}
            <ParamChip
              label={chip.label}
              value={0}
              format={() => chip.valueText}
              state={chip.state}
              title={chipTitle(chip)}
              onclick={() => onChipClick(d.instanceId, chip.paramId)}
            />
          {/each}
        </span>
      {/if}
    {/each}
    <!-- Gated on `onoverflow` too: the folded strip (dots only, per the
         winner spec) is deliberately given no `onoverflow` handler, so a
         folded lane with more than `maxEntries` devices just caps at the
         dots it can show — intentional, not a missed affordance. -->
  </div>
    <!-- Gated on `onoverflow` too: the folded strip (dots only, per the
         winner spec) is deliberately given no `onoverflow` handler, so a
         folded lane with more than `maxEntries` devices just caps at the
         dots it can show — intentional, not a missed affordance. -->
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
    /* `auto`, not 0. `auto` resolves to min-content, and because
       `.devices` below is a scroll container its min-content is just its
       own floor — so this floors the strip at "the dots, plus `+N` if
       there is one" and no more. At 0 the row squeezed the whole strip
       away and a lane with a plugin chain looked like a lane without one. */
    /* An explicit floor, because `auto` (min-content) would be the full
       chain and defeat the point. 22px is the two dots an unfolded strip
       shows (`maxEntries` 2, a 7px dot, a 4px gap — Ruling P-6's constant,
       spent here); `.hasmore` adds the `+N` button, which cannot shrink and
       would otherwise be the thing the clip cut off. */
    overflow: hidden;
    min-width: 22px;
    gap: 4px;
  }
  .strip.hasmore {
    min-width: 48px;
  }
  .overflow {
    flex: none;
  }
  /* First to give way in the lane header's routing row. That row cannot
     wrap and cannot grow (`.header` is a fixed `--track-height` and has to
     match the lane column row for row), so when it is over-subscribed the
     order matters: plugin NAMES go before the group name, because a dot
     still shows status, the FX chip beside it still shows the count, and
     both carry the name in a tooltip — while the group name has no other
     home on screen once the group header has scrolled out of view. */
  .strip:not(.folded) {
    flex-shrink: 100;
  }

  .devices {
    display: flex;
    align-items: center;
    flex-wrap: nowrap;
    overflow: hidden;
    /* The dots and nothing else: `maxEntries` is 2 unfolded (Ruling P-6),
       a `.dot` is 7px and the gap is 4. The names inside carry
       `min-width: 0` with an ellipsis, so they are what gives way — and
       the status dots, which are the strip's whole point, survive any
       width. Clipping these away left a lane reading "+1" with nothing
       beside it to be one more THAN. */
    min-width: 22px;
    flex: 0 1 auto;
    gap: inherit;
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
