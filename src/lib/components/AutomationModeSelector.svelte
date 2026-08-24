<script lang="ts">
   /**
   * Off/Read/Write/Touch/Latch picker for a track's automation mode. Dumb
   * mode-in/onchange-out component — TrackHeader owns the `project` call,
   * this only renders the five options and reports clicks.
   *
   * The chips carry a single LETTER, not the mode word (owner note,
   * 2026-08-24): five words per lane, on every lane, was more text than the
   * track header could carry. The full name still reaches everyone who
   * needs it — `title` on hover, `aria-label` for assistive tech — which is
   * the same deal the A / M / S chips above already make.
   */
  import type { AutomationMode } from "../types/ipc";

  let {
    mode,
    onchange,
  }: { mode: AutomationMode; onchange: (mode: AutomationMode) => void } = $props();

  /** `label` is what the chip SHOWS; `name` is what it is CALLED — the
   * latter feeds both the tooltip and the accessible name, because a bare
   * "T" is not a name anyone can act on. */
  const MODES: { value: AutomationMode; label: string; name: string; title: string }[] = [
    { value: "off", label: "O", name: "Off", title: "Off — bypass the lane" },
    { value: "read", label: "R", name: "Read", title: "Read — always apply the lane" },
    { value: "write", label: "W", name: "Write", title: "Write — continuously record while playing" },
    { value: "touch", label: "T", name: "Touch", title: "Touch — record while the fader is held" },
    { value: "latch", label: "L", name: "Latch", title: "Latch — record while held, then hold the last value" },
  ];
</script>

<div class="automode" role="group" aria-label="Automation mode">
  {#each MODES as m (m.value)}
    <button
      type="button"
      class="tog"
      class:on={mode === m.value}
      aria-pressed={mode === m.value}
      aria-label={m.name}
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
    /* Letters, so the chips size to their content instead of stretching
       across the row the way five words had to. */
    flex: 0 0 auto;
    min-width: 20px;
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
