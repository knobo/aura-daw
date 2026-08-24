<script lang="ts">
  /**
   * The automation matrix (design §6.1, plan §6.1): the fourth Plugin
   * Manager mode. Same rows as the rack, grouped by parameter instead of
   * by instance, plus track-param curves (gain, pan) — everything that
   * moves in this project, in one list. Clicking a row reveals that
   * parameter's lane.
   */
  import { untrack } from "svelte";
  import { paramFollow } from "../../state/param-follow.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { project } from "../../state/project.svelte";
  import { modulation } from "../../state/modulation.svelte";
  import { revealParamLane } from "../../utils/lane-reveal";
  import { buildMatrix, matrixByParam, type MatrixRow } from "../../utils/automation-matrix";
  import { FoldController } from "../browser/fold-controller.svelte";
  import { rowId } from "../browser/browser-model";
  import BrowserSection from "../browser/BrowserSection.svelte";
  import BrowserRow from "../browser/BrowserRow.svelte";
  import EmptyState from "../browser/EmptyState.svelte";
  import ParamChip from "../browser/ParamChip.svelte";

  /** `onCount` reports the row total out (the footer's "N params move" —
   * `PluginManager.svelte` needs the number but must not recompute
   * `buildMatrix` a second time just to get it). */
  let { onCount }: { onCount?: (n: number) => void } = $props();

  /** Inert stand-in for `MatrixInput.visible` — see `instanceIds` below. */
  const EMPTY_VISIBLE: ReadonlyMap<string, ReadonlySet<string>> = new Map();

  const rows = $derived(
    buildMatrix({
      bindings: modulation.bindings,
      instances: plugins.instances,
      tracks: project.tracks,
      visible: modulation.visible,
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
  const groups = $derived(matrixByParam(rows));

  const folds = new FoldController("plugins-matrix");
  const collapsed = $derived(folds.collapsed);

  // Which instance ids need `ensureParams`, computed SEPARATELY from
  // `rows`. `rows` also depends on `plugins.paramCache` (via `paramInfo`)
  // and `modulation.visible` — both change on nearly every row click
  // (`revealParamLane` → `modulation.show`) and on every knob-drag input
  // event while some param panel is open elsewhere. `buildMatrix` always
  // returns a fresh array, and Svelte 5 `$derived` invalidates whatever
  // reads it on ANY dependency touch, not on array-content equality — so
  // an effect that reads `rows` would re-run on those unrelated changes
  // too. Row MEMBERSHIP (which bindings survive, and their instanceId)
  // never depends on paramInfo/visible, only on bindings/instances/tracks,
  // so build with inert stand-ins for the two value-carrying inputs.
  const instanceIds = $derived(
    new Set(
      buildMatrix({
        bindings: modulation.bindings,
        instances: plugins.instances,
        tracks: project.tracks,
        visible: EMPTY_VISIBLE,
        paramInfo: () => undefined,
      })
        .map((r) => r.instanceId)
        .filter((id): id is string => id !== null),
    ),
  );

  // The membership split above is necessary but not sufficient:
  // `plugins.ensureParams`'s own dedup guard (`if (this.paramCache[id])
  // return;`) reads `paramCache` SYNCHRONOUSLY, and a reactive read that
  // happens inside an `$effect` body — even buried inside a function the
  // effect merely calls — makes that effect depend on it too. Calling
  // `ensureParams` un-`untrack`ed here would silently resubscribe this
  // effect to `paramCache` and reintroduce the exact bug the `instanceIds`
  // split above exists to avoid (verified by hand: removing the `untrack`
  // makes the DOM test "re-requests ensureParams only when..." fail with
  // an extra call after the very first `ensureParams` resolution, since
  // its resolve is what writes `paramCache`).
  $effect(() => {
    const ids = instanceIds;
    untrack(() => {
      for (const instanceId of ids) void plugins.ensureParams(instanceId);
    });
  });

  $effect(() => {
    onCount?.(rows.length);
  });

  function onRowClick(row: MatrixRow) {
    void revealParamLane(row.trackId, row.target);
  }
</script>

{#if rows.length === 0}
  <EmptyState message="Nothing is automated yet. Automate a parameter and it shows up here." />
{:else}
  <div class="matrix">
    {#each groups as g (g.label)}
      {@const headerId = rowId({ kind: "group", groupKey: g.label, itemIndex: -1 })}
      <BrowserSection
        id={headerId}
        label={g.label}
        count={g.rows.length}
        expanded={!collapsed.has(g.label)}
        onToggle={() => folds.toggle(g.label, collapsed.has(g.label))}
      >
        {#each g.rows as row, i (row.bindingId)}
          {@const rowIdStr = rowId({ kind: "item", groupKey: g.label, itemIndex: i })}
          <BrowserRow
            id={rowIdStr}
            label={row.trackName}
            meta={row.pluginName ?? "—"}
            onclick={() => onRowClick(row)}
          >
            {#snippet badge()}
              <span
                class="visible-dot"
                class:on={row.laneVisible}
                title={row.laneVisible ? "Lane visible" : "Lane hidden"}
                aria-hidden="true"
              ></span>
            {/snippet}
            {#snippet actions()}
              <ParamChip label={row.paramLabel} value={0} format={() => row.valueText} state="automated" />
              <span class="mode mono">{row.mode}</span>
            {/snippet}
          </BrowserRow>
        {/each}
      </BrowserSection>
    {/each}
  </div>
{/if}

<style>
  .matrix {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
  }

  .visible-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    flex: none;
    background: var(--text-faint);
  }
  .visible-dot.on {
    background: var(--cyan);
  }

  .mode {
    flex: none;
    padding: 3px 7px;
    font-size: 8px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    color: var(--text-dim);
  }
</style>
