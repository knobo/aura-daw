<script lang="ts">
  /**
   * The progression: one chip per chord region, with its Roman numeral and its
   * function. Click a chip to read WHY it is there; × removes it and closes the
   * gap. Both are one `harmony_set` — one op, one undo.
   */
  import type { HarmonySpanView } from "../../types/ipc";

  let {
    spans,
    selectedTick,
    ticksPerBar,
    onselect,
    onremove,
  }: {
    spans: HarmonySpanView[];
    selectedTick: number | null;
    ticksPerBar: number;
    onselect: (tick: number | null) => void;
    onremove: (tick: number) => void;
  } = $props();

  const bars = (span: HarmonySpanView) =>
    Math.max(1, Math.round(span.lengthTicks / Math.max(1, ticksPerBar)));
</script>

<div class="strip" role="list" aria-label="Chord progression">
  {#if spans.length === 0}
    <p class="empty mono">no chords yet — click the circle, or generate a progression</p>
  {:else}
    {#each spans as span (span.tick)}
      <div class="chip" class:on={selectedTick === span.tick} data-function={span.function} role="listitem">
        <button
          class="pick"
          title={span.why}
          onclick={() => onselect(selectedTick === span.tick ? null : span.tick)}
        >
          <span class="symbol">{span.pretty}</span>
          <span class="roman mono">{span.roman}</span>
          {#if bars(span) > 1}<span class="bars mono">×{bars(span)}</span>{/if}
          {#if span.borrowed}<span class="borrowed mono" title="borrowed from another key">↯</span>{/if}
        </button>
        <button class="kill mono" title="Remove this chord" onclick={() => onremove(span.tick)}>×</button>
      </div>
    {/each}
  {/if}
</div>

<style>
  .strip {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    min-height: 30px;
  }

  .empty {
    margin: 0;
    font-size: 9px;
    color: var(--text-dim);
    letter-spacing: 0.06em;
  }

  .chip {
    display: flex;
    align-items: stretch;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 3px;
    background: rgb(var(--bg-2-rgb) / 0.7);
    overflow: hidden;
  }
  .chip.on {
    border-color: var(--cyan);
    box-shadow: 0 0 calc(6px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.35);
  }

  .pick {
    display: flex;
    align-items: baseline;
    gap: 4px;
    padding: 4px 6px;
    border: none;
    background: transparent;
    color: var(--text);
    cursor: pointer;
    font-family: var(--font-ui);
  }
  .pick:hover {
    background: rgb(var(--cyan-rgb) / 0.1);
  }

  /* A chip is coloured by what the chord DOES, not by where it sits. */
  .chip[data-function="tonic"] .roman {
    color: var(--green);
  }
  .chip[data-function="predominant"] .roman {
    color: var(--cyan);
  }
  .chip[data-function="dominant"] .roman {
    color: var(--amber);
  }
  .chip[data-function="chromatic"] .roman {
    color: var(--violet);
  }

  .symbol {
    font-size: 12px;
    font-weight: 600;
  }
  .roman {
    font-size: 9px;
    letter-spacing: 0.04em;
  }
  .bars,
  .borrowed {
    font-size: 8px;
    color: var(--text-dim);
  }

  .kill {
    border: none;
    border-left: var(--border-width) solid var(--glass-border);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    padding: 0 5px;
    font-size: 11px;
  }
  .kill:hover {
    color: var(--red);
    background: rgb(var(--red-rgb) / 0.12);
  }
</style>
