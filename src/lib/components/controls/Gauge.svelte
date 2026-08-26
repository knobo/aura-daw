<script lang="ts">
  /**
   * Analog VU: milled bezel, glass window, needle driven from the meter
   * bus inside rAF. Range matches Meter.svelte (−60..+6 dBFS). Read-only.
   */
  import { latestMeter } from "../../state/meters.svelte";
  import { formatDb, linToDb } from "../../utils/format";

  interface Props {
    trackId: string;
    label?: string;
    size?: number;
  }

  const { trackId, label, size = 88 }: Props = $props();

  const DB_MIN = -60;
  const DB_MAX = 6;
  const SWEEP = 90;
  const START = -SWEEP / 2;

  let needleEl: SVGGElement | undefined = $state();
  let readout = $state("-∞");
  let clipped = $state(false);

  function angleOf(db: number): number {
    const n = Math.max(0, Math.min(1, (db - DB_MIN) / (DB_MAX - DB_MIN)));
    return START + n * SWEEP;
  }

  $effect(() => {
    const id = trackId;
    const needle = needleEl;
    if (!needle) return;
    let raf = 0;
    let disp = DB_MIN;
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick);
      void now;
      const m = latestMeter(id);
      const peak = m ? linToDb(Math.max(m.peakL, m.peakR)) : DB_MIN;
      // Fast attack, slower release — a needle, not a bar.
      disp = peak > disp ? peak : disp + (peak - disp) * 0.18;
      needle.setAttribute("transform", `rotate(${angleOf(disp)} 50 78)`);
      readout = formatDb(disp);
      if (m?.clipped) clipped = true;
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });
</script>

<div class="gauge-unit" style:--gauge-size="{size}px">
  <div class="bezel grain">
    <div class="window">
      <svg class="face" viewBox="0 0 100 100" aria-hidden="true">
        <path class="arc" d="M18 78 A32 32 0 0 1 82 78" />
        {#each [-60, -24, -12, -6, 0, 6] as db (db)}
          <line
            class="tick"
            class:hot={db >= 0}
            x1="50"
            y1="48"
            x2="50"
            y2={db >= 0 ? 44 : 46}
            transform="rotate({angleOf(db)} 50 78)"
          />
        {/each}
        <g bind:this={needleEl} class="needle">
          <line x1="50" y1="78" x2="50" y2="46" />
        </g>
        <circle class="hub" cx="50" cy="78" r="4.5" />
      </svg>
      <div class="glass"></div>
      <button
        class="clip"
        class:on={clipped}
        type="button"
        aria-label="Clip indicator — click to reset"
        onclick={() => (clipped = false)}
      ></button>
    </div>
  </div>
  {#if label}<span class="silk gauge-label">{label}</span>{/if}
  <output class="mono gauge-value">{readout}</output>
</div>

<style>
  .gauge-unit {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    width: var(--gauge-size);
  }
  .bezel {
    position: relative;
    width: var(--gauge-size);
    height: var(--gauge-size);
    border-radius: 50%;
    background-color: var(--bg-3);
    background-image: var(--sheen-dome);
    box-shadow: var(--bevel-raised), var(--relief-2);
    padding: 7px;
  }
  .window {
    position: relative;
    width: 100%;
    height: 100%;
    border-radius: 50%;
    background: var(--bg-sunken);
    box-shadow: var(--bevel-inset);
    overflow: hidden;
  }
  .face {
    width: 100%;
    height: 100%;
    display: block;
  }
  .arc {
    fill: none;
    stroke: rgb(var(--line-rgb) / 0.35);
    stroke-width: 1.6;
  }
  .tick {
    stroke: rgb(var(--line-rgb) / 0.55);
    stroke-width: 1.4;
  }
  .tick.hot {
    stroke: var(--red);
  }
  .needle line {
    stroke: var(--amber);
    stroke-width: 1.6;
    stroke-linecap: round;
    filter: drop-shadow(0 0 calc(3px * var(--glow-scale)) var(--amber));
  }
  .hub {
    fill: var(--bg-3);
    stroke: var(--edge);
    stroke-width: 0.8;
  }
  .glass {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background-image: var(--sheen-dome);
    opacity: calc(var(--sheen) * 0.55);
    pointer-events: none;
  }
  .clip {
    position: absolute;
    top: 10%;
    right: 18%;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: none;
    padding: 0;
    background: rgb(var(--line-rgb) / 0.3);
    cursor: pointer;
  }
  .clip.on {
    background: var(--red);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) var(--red);
  }
  .gauge-label {
    line-height: 1;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gauge-value {
    font-size: 10px;
    color: var(--text-mid);
    line-height: 1;
  }
</style>
