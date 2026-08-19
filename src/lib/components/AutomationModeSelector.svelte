<script lang="ts">
  /**
   * Off/Read/Write/Touch/Latch picker for a track's automation mode. Dumb
   * mode-in/onchange-out component — TrackHeader owns the `project` call,
   * this only renders the five options and reports clicks.
   */
  import type { AutomationMode } from "../types/ipc";

  let {
    mode,
    onchange,
  }: { mode: AutomationMode; onchange: (mode: AutomationMode) => void } = $props();

  const MODES: { value: AutomationMode; label: string; title: string }[] = [
    { value: "off", label: "Off", title: "Automation off — bypass the lane" },
    { value: "read", label: "Read", title: "Read — always apply the lane" },
    { value: "write", label: "Write", title: "Write — continuously record while playing" },
    { value: "touch", label: "Touch", title: "Touch — record while the fader is held" },
    { value: "latch", label: "Latch", title: "Latch — record while held, then hold the last value" },
  ];
</script>

<div class="automode" role="group" aria-label="Automation mode">
  {#each MODES as m (m.value)}
    <button
      type="button"
      class="tog"
      class:on={mode === m.value}
      aria-pressed={mode === m.value}
      title={m.title}
      onclick={() => onchange(m.value)}
    >{m.label}</button>
  {/each}
</div>

<style>
  .automode {
    display: flex;
    flex: 1;
    min-width: 0;
    gap: 6px;
  }
  .tog {
    flex: 1 1 0;
    min-width: 0;
    height: 20px;
    padding: 0 4px;
    font-family: var(--font-mono);
    font-size: 9px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.15);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .tog.on {
    color: var(--bg-0);
    background: var(--magenta);
    border-color: var(--magenta);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--magenta-rgb) / 0.4);
  }
</style>
