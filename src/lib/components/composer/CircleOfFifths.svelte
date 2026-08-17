<script lang="ts">
  /**
   * The circle of fifths as an INSTRUMENT (product doc §4.4), not a diagram.
   *
   * The key's seven diatonic chords are a contiguous arc — that is a fact about
   * the circle, not a drawing decision — so the arc is lit and everything
   * outside it is dimmed and labelled with what it would be if you borrowed it.
   * Click a slot to append that chord; the outer ring is the major key, the
   * inner ring its relative minor.
   *
   * Every label, numeral, function and spelling comes from the backend
   * (`harmony_get`). This component owns exactly one computation, and it is
   * geometry: `circle-geometry.ts`.
   */
  import { circleLayout } from "./circle-geometry";
  import type { CircleWedge } from "../../types/ipc";

  let {
    wedges,
    size = 236,
    onpick,
  }: {
    wedges: CircleWedge[];
    size?: number;
    onpick: (symbol: string) => void;
  } = $props();

  let hovered = $state<number | null>(null);

  const tonicIndex = $derived(Math.max(0, wedges.findIndex((w) => w.isTonic)));
  const layout = $derived(circleLayout(wedges.length, tonicIndex, size));
  const hoveredWedge = $derived(hovered === null ? null : wedges[hovered]);

  function chordOf(w: CircleWedge): string | null {
    return w.chord ?? w.borrowedChord ?? null;
  }

  function pick(w: CircleWedge) {
    const symbol = chordOf(w);
    if (symbol) onpick(symbol);
  }
</script>

<div class="circle" style:width="{size}px">
  <svg viewBox="0 0 {size} {size}" width={size} height={size} role="group" aria-label="Circle of fifths">
    {#each layout.wedges as g (g.index)}
      {@const w = wedges[g.index]}
      <g
        class="wedge"
        class:inkey={w.inKey}
        class:tonic={w.isTonic}
        class:hovered={hovered === g.index}
        data-function={w.function ?? "outside"}
        role="button"
        tabindex="0"
        aria-label="{w.major} — {w.roman ?? 'outside the key'}"
        onclick={() => pick(w)}
        onkeydown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            pick(w);
          }
        }}
        onmouseenter={() => (hovered = g.index)}
        onmouseleave={() => (hovered = null)}
        onfocus={() => (hovered = g.index)}
        onblur={() => (hovered = null)}
      >
        <path class="seg outer" d={g.outerPath} />
        <path class="seg inner" d={g.innerPath} />
        <text class="major" x={g.outerLabel.x} y={g.outerLabel.y}>{w.major}</text>
        <text class="minor" x={g.innerLabel.x} y={g.innerLabel.y}>{w.minor}m</text>
      </g>
    {/each}
    <circle class="hub" cx={layout.centre} cy={layout.centre} r={size * 0.16} />
    <text class="hubtext" x={layout.centre} y={layout.centre - 3}>
      {hoveredWedge ? (hoveredWedge.roman ?? "—") : "KEY"}
    </text>
    <text class="hubsub" x={layout.centre} y={layout.centre + 9}>
      {hoveredWedge ? (chordOf(hoveredWedge) ?? "") : wedges.find((w) => w.isTonic)?.major ?? ""}
    </text>
  </svg>
  <p class="hint mono">
    {#if hoveredWedge}
      {hoveredWedge.inKey
        ? `in ${wedges.find((w) => w.isTonic)?.major ?? "the key"} — click to add`
        : "borrowed — click to add"}
    {:else}
      the lit arc is the key's own seven chords
    {/if}
  </p>
</div>

<style>
  .circle {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    flex: none;
  }

  .wedge {
    cursor: pointer;
    outline: none;
  }

  .seg {
    stroke: var(--bg-0);
    stroke-width: 1;
    fill: rgb(var(--bg-3-rgb) / 0.55);
    transition:
      fill 120ms ease,
      opacity 120ms ease;
  }
  .seg.inner {
    fill: rgb(var(--bg-2-rgb) / 0.7);
  }

  /* Outside the arc: present, but clearly not part of the key. */
  .wedge:not(.inkey) .seg {
    fill: rgb(var(--bg-1-rgb) / 0.65);
  }
  .wedge:not(.inkey) text {
    fill: var(--text-faint);
  }

  /* Function colouring: the same three-way split the analysis uses. */
  .wedge.inkey[data-function="tonic"] .seg.outer {
    fill: rgb(var(--green-rgb) / 0.22);
  }
  .wedge.inkey[data-function="predominant"] .seg.outer {
    fill: rgb(var(--cyan-rgb) / 0.2);
  }
  .wedge.inkey[data-function="dominant"] .seg.outer {
    fill: rgb(var(--amber-rgb) / 0.22);
  }

  .wedge.tonic .seg.outer {
    stroke: var(--cyan);
    stroke-width: 1.5;
  }

  .wedge.hovered .seg,
  .wedge:focus-visible .seg {
    fill: rgb(var(--cyan-rgb) / 0.4);
  }

  text {
    fill: var(--text-mid);
    font-family: var(--font-mono);
    text-anchor: middle;
    dominant-baseline: middle;
    pointer-events: none;
    user-select: none;
  }
  .major {
    font-size: 10px;
    fill: var(--text);
  }
  .minor {
    font-size: 8px;
  }

  .hub {
    fill: rgb(var(--bg-0-rgb) / 0.92);
    stroke: var(--glass-border);
  }
  .hubtext {
    font-size: 12px;
    fill: var(--cyan);
  }
  .hubsub {
    font-size: 8px;
    fill: var(--text-dim);
  }

  .hint {
    margin: 0;
    font-size: 8px;
    letter-spacing: 0.08em;
    color: var(--text-dim);
    text-align: center;
    min-height: 10px;
  }
</style>
