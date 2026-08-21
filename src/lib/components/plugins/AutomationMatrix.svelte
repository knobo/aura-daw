<script lang="ts">
  /**
   * The automation matrix (design §6.1, plan §6.1): the fourth Plugin
   * Manager mode. Same rows as the rack, grouped by parameter instead of
   * by instance, plus track-param curves (gain, pan) — everything that
   * moves in this project, in one list. Clicking a row reveals that
   * parameter's lane.
   */
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

  const rows = $derived(
    buildMatrix({
      bindings: modulation.bindings,
      instances: plugins.instances,
      tracks: project.tracks,
      visible: modulation.visible,
      paramInfo: (instanceId, paramId) => plugins.paramInfo(instanceId, paramId),
    }),
  );
  const groups = $derived(matrixByParam(rows));

  const folds = new FoldController("plugins-matrix");
  const collapsed = $derived(folds.collapsed);

  // Fill in names/values for whatever isn't cached yet. `ensureParams`
  // dedups in-flight requests itself, so calling it for the same instance
  // id every time `rows` recomputes is cheap — it only round-trips once.
  $effect(() => {
    for (const instanceId of new Set(rows.map((r) => r.instanceId))) {
      if (instanceId) void plugins.ensureParams(instanceId);
    }
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
