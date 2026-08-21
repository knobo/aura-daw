<script lang="ts">
  /**
   * A rotary control that is actually a knob: a domed cap with a milled
   * edge, a machined indicator, a ring of ticks and a value arc.
   *
   * Everything physical about it comes from the material tokens rather than
   * from constants here, so the SAME component is a milled aluminium pot
   * under Console Noir, a moulded plastic one under Studio Ivory and a flat
   * circle under High Contrast — no variants, no per-theme markup.
   *
   * The gesture is the one every DAW has trained people on: drag vertically
   * (never radially — radial drags lose the pointer the moment it crosses
   * the centre), hold Shift for fine, double-click to return to the
   * default. `FULL_TRAVEL_PX` is the drag distance that covers the whole
   * range, and 200px is deliberately long: a knob you can slam end to end
   * in a flick is unusable for the 0.1 dB adjustments this is for.
   */

  interface Props {
    value: number;
    min: number;
    max: number;
    /** Where a double-click returns to. Defaults to `min`, or 0 if bipolar. */
    resetTo?: number;
    /** Diameter in px of the cap; ticks and ring scale from it. */
    size?: number;
    /** Silkscreen legend under the knob. Omit for a bare knob. */
    label?: string;
    /** The value arc's colour. Defaults to the primary interactive accent. */
    color?: string;
    /** Renders the arc growing out from 12 o'clock in both directions —
     * what a pan or a ±dB trim wants, as opposed to a level's 0-to-full. */
    bipolar?: boolean;
    /** The readout under the knob. Omit for no readout. */
    format?: (v: number) => string;
    ariaLabel: string;
    oninput?: (v: number) => void;
    /** Fired once when a drag starts and once when it ends, so a caller can
     * bracket the whole gesture in one undo entry — the same contract the
     * faders already use. */
    onstart?: () => void;
    onend?: () => void;
  }

  const {
    value,
    min,
    max,
    resetTo,
    size = 38,
    label,
    color,
    bipolar = false,
    format,
    ariaLabel,
    oninput,
    onstart,
    onend,
  }: Props = $props();

  /** Drag distance covering the full range. See the note above. */
  const FULL_TRAVEL_PX = 200;
  /** The sweep of a knob, in degrees. 270° with the gap at the bottom is the
   * shape every hardware pot has, and the gap is what makes "off" legible. */
  const SWEEP = 270;

  const CX = 50;
  const R = 41;
  const CIRC = 2 * Math.PI * R;
  /** Arc length of the live part of the ring. */
  const ARC = CIRC * (SWEEP / 360);

  const span = $derived(max - min || 1);
  /** 0..1 position of the current value within the range. */
  const frac = $derived(Math.min(1, Math.max(0, (value - min) / span)));
  /** Degrees of rotation for the indicator, 0° pointing straight up. */
  const angle = $derived(-SWEEP / 2 + frac * SWEEP);
  const home = $derived(resetTo ?? (bipolar ? 0 : min));

  /**
   * The value arc as a dash pattern. Unipolar grows from the start of the
   * sweep; bipolar grows out of the middle in whichever direction the value
   * went, which is why it needs the leading `0 <gap>` pair.
   */
  const dash = $derived.by(() => {
    if (!bipolar) return `${(frac * ARC).toFixed(2)} ${CIRC.toFixed(2)}`;
    const from = Math.min(frac, 0.5);
    const len = Math.abs(frac - 0.5);
    return `0 ${(from * ARC).toFixed(2)} ${(len * ARC).toFixed(2)} ${CIRC.toFixed(2)}`;
  });

  /** Tick marks around the ring — the printed scale on the panel. */
  const TICKS = 11;
  const ticks = Array.from({ length: TICKS }, (_, i) => -SWEEP / 2 + (i / (TICKS - 1)) * SWEEP);

  let dragging = $state(false);
  /** Pointer Y and the value when the drag began. A drag is measured from
   * its own origin rather than accumulated per move event, so a run of
   * rounded steps cannot drift away from the pointer. */
  let originY = 0;
  let originValue = 0;

  function clamp(v: number): number {
    return Math.min(max, Math.max(min, v));
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragging = true;
    originY = e.clientY;
    originValue = value;
    onstart?.();
  }

  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    // Up is more, which is why the delta is negated.
    const dy = originY - e.clientY;
    const scale = e.shiftKey ? 0.2 : 1;
    oninput?.(clamp(originValue + (dy / FULL_TRAVEL_PX) * span * scale));
  }

  function endDrag(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    onend?.();
  }

  function onDoubleClick() {
    onstart?.();
    oninput?.(clamp(home));
    onend?.();
  }

  /** Arrow keys nudge by a hundredth of the range, Page keys by a tenth. */
  function onKeyDown(e: KeyboardEvent) {
    const step =
      e.key === "PageUp" || e.key === "PageDown" ? span / 10 : span / 100;
    let next: number | undefined;
    if (e.key === "ArrowUp" || e.key === "ArrowRight" || e.key === "PageUp") next = value + step;
    else if (e.key === "ArrowDown" || e.key === "ArrowLeft" || e.key === "PageDown") next = value - step;
    else if (e.key === "Home") next = home;
    if (next === undefined) return;
    e.preventDefault();
    onstart?.();
    oninput?.(clamp(next));
    onend?.();
  }
</script>

<div class="knob-unit" style:--knob-size="{size}px" style:--knob-accent={color ?? "var(--cyan)"}>
  <div
    class="knob"
    class:dragging
    role="slider"
    tabindex="0"
    aria-label={ariaLabel}
    aria-valuemin={min}
    aria-valuemax={max}
    aria-valuenow={value}
    aria-valuetext={format ? format(value) : undefined}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={endDrag}
    onpointercancel={endDrag}
    ondblclick={onDoubleClick}
    onkeydown={onKeyDown}
  >
    <svg class="dial" viewBox="0 0 100 100" aria-hidden="true">
      <g transform="rotate(135 {CX} {CX})">
        <circle class="dial-track" cx={CX} cy={CX} r={R} stroke-dasharray="{ARC.toFixed(2)} {CIRC.toFixed(2)}" />
        <circle class="dial-arc" cx={CX} cy={CX} r={R} stroke-dasharray={dash} />
      </g>
      <g class="dial-ticks">
        {#each ticks as t (t)}
          <line x1={CX} y1="4" x2={CX} y2="9" transform="rotate({t} {CX} {CX})" />
        {/each}
      </g>
    </svg>

    <!-- The cap. `.grain` needs a positioned parent, which `.cap` is. -->
    <div class="cap grain">
      <div class="pointer" style:transform="rotate({angle}deg)"></div>
    </div>
  </div>

  {#if label}<span class="silk knob-label">{label}</span>{/if}
  {#if format}<output class="mono knob-value">{format(value)}</output>{/if}
</div>

<style>
  .knob-unit {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    /* The ring sits outside the cap, so the unit is wider than --knob-size.
       1.3 is the tightest multiplier that still leaves the tick marks clear
       of the cap's cast shadow — and a channel strip is 132px tall, so the
       difference between 1.3 and 1.42 is the difference between fitting and
       overflowing into the track below. */
    --ring-size: calc(var(--knob-size) * 1.3);
  }

  .knob {
    position: relative;
    width: var(--ring-size);
    height: var(--ring-size);
    display: grid;
    place-items: center;
    cursor: ns-resize;
    touch-action: none;
    border-radius: 50%;
  }
  .knob:focus-visible {
    outline: var(--focus-width) solid var(--cyan);
    outline-offset: 2px;
  }

  /* Every class here is prefixed. This project imports Tailwind, which
     ships bare utilities like `.ring`; a plain `class="ring"` on the svg
     picked one up and drew a rectangle behind every knob. Svelte's scoping
     does not prevent it — the element still carries the bare class name. */
  .dial {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    overflow: visible;
  }
  .dial circle {
    fill: none;
    stroke-width: 5;
    stroke-linecap: butt;
  }
  .dial-track {
    stroke: rgb(var(--line-rgb) / 0.22);
  }
  .dial-arc {
    stroke: var(--knob-accent);
    filter: drop-shadow(0 0 calc(4px * var(--glow-scale)) var(--knob-accent));
    transition: stroke-dasharray 40ms linear;
  }
  .dial-ticks line {
    stroke: rgb(var(--line-rgb) / 0.35);
    stroke-width: 2;
    stroke-linecap: round;
  }

  /* The cap: a dome, not a disc. The radial sheen puts the highlight up and
     left of centre, the bevel gives it a milled rim, and the cast shadow
     lifts it off the ring behind it. */
  .cap {
    position: relative;
    width: var(--knob-size);
    height: var(--knob-size);
    border-radius: 50%;
    background-color: var(--bg-3);
    background-image: var(--sheen-dome);
    box-shadow:
      inset 0 var(--border-width) 0 0 var(--bevel-hi),
      inset 0 calc(-1 * var(--border-width)) 0 0 var(--bevel-lo),
      inset 0 0 0 calc(1px + 1px * var(--bevel)) rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.35)),
      var(--relief-2);
    transition: box-shadow 120ms, transform 120ms;
  }
  /* Under the thumb the cap settles into the panel — the same 1px drop the
     `.raised` utility uses, for the same reason. */
  .knob.dragging .cap {
    transform: translateY(calc(1px * var(--relief)));
    box-shadow:
      inset 0 var(--border-width) 0 0 var(--bevel-hi),
      inset 0 0 0 calc(1px + 1px * var(--bevel)) rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.45)),
      var(--relief-1);
  }

  /* The indicator is a machined slot, so it is lit like one: the accent
     line with a shadow above it rather than a flat painted stripe. */
  .pointer {
    position: absolute;
    inset: 0;
    border-radius: inherit;
  }
  .pointer::before {
    content: "";
    position: absolute;
    left: 50%;
    top: 8%;
    width: 2px;
    height: 34%;
    margin-left: -1px;
    border-radius: 1px;
    background: var(--knob-accent);
    box-shadow:
      0 0 calc(5px * var(--glow-scale)) var(--knob-accent),
      0 -1px 0 rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.6));
  }

  .knob-label {
    line-height: 1;
  }
  .knob-value {
    font-size: 10px;
    color: var(--text-mid);
    line-height: 1;
  }
</style>
