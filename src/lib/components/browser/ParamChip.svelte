<script module lang="ts">
  export type ParamChipState = "plain" | "automated" | "mapped" | "recording";
</script>

<script lang="ts">
  /**
   * §3.1 — the signature element. One small mono capsule: parameter name,
   * current value, and a 2px left rail whose colour encodes state. The
   * same component renders in the rack row, the parameter panel, the
   * automation matrix and the lane-info indicator, so "what does the
   * cyan strip mean" only has to be learned once.
   *
   * The rail is decoration, not the only carrier of meaning — the state
   * is spelled out in `aria-label` too.
   */
  const STATE_WORD: Record<ParamChipState, string> = {
    plain: "",
    automated: "automated",
    mapped: "MIDI-mapped",
    recording: "recording",
  };

  let {
    label,
    value,
    format = (v: number) => String(v),
    state = "plain",
    title,
    onclick,
  }: {
    label: string;
    value: number;
    /** The chip doesn't guess at units — the caller says "62%" vs "1.2 kHz". */
    format?: (value: number) => string;
    state?: ParamChipState;
    /** Hover text — the accessible name (aria-label) already says the state,
     * so this is free to say what a click *does* instead ("Jump to…"). */
    title?: string;
    onclick?: () => void;
  } = $props();

  const formatted = $derived(format(value));
  const stateWord = $derived(STATE_WORD[state]);
  const spokenLabel = $derived(
    stateWord ? `${label}, ${formatted}, ${stateWord}` : `${label}, ${formatted}`,
  );
</script>

{#if onclick}
  <button type="button" class="chip mono state-{state}" aria-label={spokenLabel} {title} onclick={() => onclick?.()}>
    <span class="rail" aria-hidden="true"></span>
    <span class="label">{label}</span>
    <span class="value">{formatted}</span>
  </button>
{:else}
  <span class="chip mono state-{state}" aria-label={spokenLabel} {title}>
    <span class="rail" aria-hidden="true"></span>
    <span class="label">{label}</span>
    <span class="value">{formatted}</span>
  </span>
{/if}

<style>
  .chip {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 3px 8px 3px 11px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-1-rgb) / 0.6);
    font-size: 10px;
    color: var(--text-dim);
    white-space: nowrap;
    max-width: 100%;
    overflow: hidden;
  }
  button.chip {
    cursor: pointer;
    transition: background 120ms;
  }
  button.chip:hover {
    background: rgb(var(--bg-2-rgb) / 0.75);
  }

  .rail {
    position: absolute;
    left: 0;
    top: 2px;
    bottom: 2px;
    width: 2px;
    border-radius: 1px;
    background: var(--text-faint);
  }
  .state-automated .rail {
    background: var(--cyan);
  }
  .state-mapped .rail {
    background: var(--magenta);
  }
  .state-recording .rail {
    background: var(--amber);
  }

  .label {
    color: var(--text-mid);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .value {
    color: var(--text);
    font-weight: 500;
  }
</style>
