<script lang="ts">
  /**
   * Loop-jam "EVOLVE" cluster — sits in Timeline's `.jamband`, the strip
   * between the ruler and the lanes, horizontally tracking the loop region's
   * left edge. Evolve button (+ optional prompt/seed popover),
   * state chip driven by loopjam://state, cancel while in flight. The loop
   * band itself breathes/flashes via classes set in Timeline.svelte.
   */
  import { loopjam } from "../../state/loopjam.svelte";
  import { transport } from "../../state/transport.svelte";
  import { view } from "../../state/view.svelte";

  let prompt = $state("");
  let seed = $state("");
  let showOptions = $state(false);

  const hasRegion = $derived(
    transport.snap.loopEndSamples > transport.snap.loopStartSamples,
  );
  const x = $derived(Math.max(6, view.xOf(transport.snap.loopStartSamples)));
  const st = $derived(loopjam.status);

  const STATE_LABEL: Record<string, string> = {
    idle: "idle",
    generating: "generating",
    ready: "ready · swaps at wrap",
    applied: "applied",
  };

  async function evolve() {
    showOptions = false;
    await loopjam.evolve(prompt, seed);
  }
</script>

{#if hasRegion}
  <div class="jam" style:left="{x.toFixed(1)}px">
    <div class="cluster glass">
      <button
        class="evolve mono"
        disabled={loopjam.busy}
        title="Evolve the loop — ACE-Step repaints the region, the new audio swaps in at the next wrap"
        onclick={evolve}
      >
        ⟳ EVOLVE
      </button>
      <span
        class="state mono {st.state}"
        title={st.lastError ? `last error: ${st.lastError}` : `loop-jam ${st.state}`}
      >
        {STATE_LABEL[st.state] ?? st.state}{st.generation > 0 ? ` · g${st.generation}` : ""}
      </span>
      {#if loopjam.busy}
        <button class="cancel mono" title="Cancel the evolve cycle" onclick={() => loopjam.cancel()}>×</button>
      {/if}
      <button
        class="gear mono"
        class:on={showOptions}
        title="Prompt / seed"
        onclick={() => (showOptions = !showOptions)}>⚙</button
      >
    </div>

    {#if showOptions}
      <div class="options glass">
        <label class="field">
          <span class="silk">prompt · optional</span>
          <input type="text" placeholder="mutate the groove…" bind:value={prompt} spellcheck="false" />
        </label>
        <label class="field small">
          <span class="silk">seed</span>
          <input type="text" placeholder="auto" bind:value={seed} />
        </label>
      </div>
    {/if}

    {#if loopjam.error}
      <div class="err mono" role="alert" title={loopjam.error}>{loopjam.error}</div>
    {/if}
  </div>
{/if}

<style>
  .jam {
    /* relative, not absolute: the panel lives in Timeline's .jamband and
       gives that band its height, instead of floating over the first lane.
       `left` still tracks the loop region's left edge. */
    position: relative;
    margin: 4px 0;
    z-index: 12;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-width: 340px;
    pointer-events: none;
  }
  .cluster,
  .options,
  .err {
    pointer-events: auto;
  }
  .cluster {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 6px;
    border-radius: 5px;
    background: rgb(var(--bg-sunken-rgb) / 0.85);
    width: fit-content;
  }

  .evolve {
    padding: 3px 8px;
    font-size: 9px;
    letter-spacing: 0.16em;
    border: none;
    border-radius: 4px;
    cursor: pointer;
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--amber), var(--magenta));
    box-shadow: 0 0 calc(10px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.25);
    transition: filter 120ms;
    white-space: nowrap;
  }
  .evolve:hover:not(:disabled) {
    filter: brightness(1.15);
  }
  .evolve:disabled {
    filter: saturate(0.25) brightness(0.7);
    cursor: not-allowed;
  }

  .state {
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: 3px;
    border: var(--border-width) solid var(--glass-border);
    color: var(--text-faint);
    white-space: nowrap;
  }
  .state.generating {
    color: var(--magenta);
    border-color: var(--magenta-dim);
    animation: state-pulse 1s ease-in-out infinite;
  }
  .state.ready {
    color: var(--amber);
    border-color: rgb(var(--amber-rgb) / 0.5);
    animation: state-pulse 0.8s ease-in-out infinite;
  }
  .state.applied {
    color: var(--green);
    border-color: rgb(var(--green-rgb) / 0.4);
  }
  @keyframes state-pulse {
    50% {
      opacity: 0.45;
    }
  }

  .cancel,
  .gear {
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 11px;
    padding: 0 2px;
  }
  .cancel:hover {
    color: var(--red);
  }
  .gear:hover,
  .gear.on {
    color: var(--amber);
  }

  .options {
    display: flex;
    gap: 7px;
    padding: 7px 8px;
    border-radius: 5px;
    background: rgb(var(--bg-sunken-rgb) / 0.92);
    width: 300px;
    max-width: 90vw;
  }
  .field {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field.small {
    flex: 0 0 70px;
  }
  input[type="text"] {
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 10px;
    padding: 4px 7px;
  }
  input:focus {
    border-color: rgb(var(--amber-rgb) / 0.5);
  }

  .err {
    font-size: 9px;
    color: var(--red);
    background: rgb(var(--bg-sunken-rgb) / 0.85);
    border: var(--border-width) solid rgb(var(--red-rgb) / 0.35);
    border-radius: 4px;
    padding: 3px 7px;
    max-width: 320px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    width: fit-content;
  }
</style>
