<script lang="ts">
  /** ROLL / PITCH / SURFACE — shared by the three bottom panels. */
  import { midi } from "../../state/midi.svelte";
  import { ui, type BottomPanel } from "../../state/ui.svelte";

  const { current }: { current: BottomPanel } = $props();
</script>

<div class="tabs" role="group" aria-label="Bottom panel">
  <button
    class="tabchip mono"
    class:on={current === "roll"}
    aria-current={current === "roll" ? "page" : undefined}
    title={midi.openClip ? "Piano roll" : "Open a MIDI clip to edit notes"}
    onclick={() => (ui.bottomPanel = "roll")}
  >
    ROLL
  </button>
  <button
    class="tabchip mono"
    class:on={current === "pitch"}
    aria-current={current === "pitch" ? "page" : undefined}
    title="Pitch Coach"
    onclick={() => (ui.bottomPanel = "pitch")}
  >
    PITCH
  </button>
  <button
    class="tabchip mono"
    class:on={current === "surface"}
    aria-current={current === "surface" ? "page" : undefined}
    title="Control surface — mixer, pads, gauges"
    onclick={() => (ui.bottomPanel = "surface")}
  >
    SURFACE
  </button>
</div>

<style>
  .tabs {
    display: flex;
    gap: 2px;
  }
  .tabchip {
    font-family: var(--font-mono);
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 3px 10px;
    border-radius: 999px;
    border: var(--border-width) solid transparent;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .tabchip:hover:not(:disabled) {
    color: var(--text);
  }
  .tabchip.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .tabchip:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
