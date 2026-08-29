<script lang="ts">
  /**
   * A rotary control that is actually a knob: a collared cap with a raised
   * inner dome, a single lit indicator, and a ring of LEDs for its value.
   *
   * The dot ring replaced a painted arc and a row of tick lines, and it is
   * worth saying why, because an arc is less code. An arc shows you a
   * QUANTITY; a ring of discrete LEDs shows you a POSITION ON A SCALE, and
   * a scale is what a knob's travel actually is — you learn where two
   * o'clock is on this control and reach for it again. The dots are also
   * what make the unlit part of the travel visible: an arc that has not got
   * there yet is nothing at all, while an unlit LED is still a stop you can
   * see and aim for.
   *
   * The leading dot lights FRACTIONALLY (`litness` below), so the display
   * stays continuous under a slow drag rather than stepping between 31
   * positions. That single detail is the difference between a readout that
   * feels like hardware and one that feels like a progress bar.
   *
   * Everything physical about it comes from the material tokens rather than
   * from constants here, so the SAME component is a milled aluminium pot
   * under Console Noir, a moulded plastic one under Studio Ivory, a brushed
   * steel one under Brushed Steel and a flat circle under High Contrast —
   * no variants, no per-theme markup.
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
  /**
   * The LED ring, in viewBox units. `LED_R + LED_DOT` must stay under 50 or
   * the outer dots are cut off by the `.knob` box, and `LED_R - LED_DOT`
   * must stay clear of the cap: the cap is `--knob-size` inside a box of
   * `--ring-size` (1.3×), so its rim reaches 100 / 1.3 / 2 ≈ 38.5 units.
   * 44.1 is where the dots begin, which is the 3px gap the reference gear
   * leaves between the cap and its collar of lamps.
   */
  const LED_R = 46.5;
  const LED_DOT = 2.4;
  /** Enough that the ring reads as a scale rather than as eight lamps, and
   * few enough that each dot is still a dot at a 38px cap. */
  const LEDS = 31;
  /** Fraction of the range one dot covers. */
  const STEP = 1 / (LEDS - 1);

  /** Each lamp's angle from 12 o'clock and its position along the travel. */
  const leds = Array.from({ length: LEDS }, (_, i) => ({
    deg: -SWEEP / 2 + (i / (LEDS - 1)) * SWEEP,
    at: i * STEP,
  }));

  const span = $derived(max - min || 1);
  /** 0..1 position of the current value within the range. */
  const frac = $derived(Math.min(1, Math.max(0, (value - min) / span)));
  /** Degrees of rotation for the indicator, 0° pointing straight up. */
  const angle = $derived(-SWEEP / 2 + frac * SWEEP);
  const home = $derived(resetTo ?? (bipolar ? 0 : min));

  /**
   * How lit one lamp is, 0..1. Fully lit once the value has reached it, dark
   * one step before, and linearly in between — so the head of the ring
   * fades in as the value crosses it and the display never steps.
   *
   * Bipolar measures from the centre outwards rather than from the start, so
   * a pan or a ±dB trim lights a band either side of twelve o'clock and a
   * centred value lights exactly the middle lamp.
   */
  const lit = $derived.by(() =>
    leds.map(({ at }) => {
      const reach = bipolar
        ? Math.min(at - Math.min(frac, 0.5), Math.max(frac, 0.5) - at)
        : frac - at;
      return Math.min(1, Math.max(0, reach / STEP + 1));
    }),
  );

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
      <!-- The unlit ring is drawn whole and once: an LED you can see but
           have not reached is a stop you can aim for, and it is what makes
           the travel legible on a knob that is off. -->
      <g class="dial-holes">
        {#each leds as l (l.deg)}
          <circle cx={CX} cy={CX - LED_R} r={LED_DOT} transform="rotate({l.deg} {CX} {CX})" />
        {/each}
      </g>
      <!-- and the lit ones over them, the head of the ring part-lit. -->
      <g class="dial-lamps">
        {#each leds as l, i (l.deg)}
          {#if lit[i] > 0}
            <circle
              cx={CX}
              cy={CX - LED_R}
              r={LED_DOT}
              opacity={lit[i]}
              transform="rotate({l.deg} {CX} {CX})"
            />
          {/if}
        {/each}
      </g>
    </svg>

    <!-- The cap. `.grain` needs a positioned parent, which `.cap` is — and
         it owns both ::before and ::after, so the inner dome and the
         indicator are real elements rather than pseudo-elements on it. -->
    <div class="cap grain">
      <div class="cap-face"></div>
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
  /* An unlit lamp is a drilled hole with a dark lens in it. `--line` rather
     than `--shadow` so it survives on a theme whose ground already IS the
     shadow colour — on High Contrast Dark a black dot on black is no scale
     at all. */
  .dial-holes circle {
    fill: rgb(var(--line-rgb) / 0.26);
    stroke: rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.55));
    stroke-width: 0.7;
  }
  /* The glow is one filter over the whole group rather than one per lamp:
     31 drop-shadows re-evaluated on every pointer move is a different
     component. At `glowScale: 0` it collapses to the bare dots. */
  .dial-lamps {
    filter: drop-shadow(0 0 calc(2.5px * var(--glow-scale)) var(--knob-accent));
  }
  .dial-lamps circle {
    fill: var(--knob-accent);
  }

  /* The cap is two parts, the way a moulded knob is: a COLLAR that meets the
     panel, and a raised inner dome sitting in it. One disc with a gradient
     is a circle; a disc with a second disc casting a shadow into it is an
     object, and the ring of shadow between the two is the whole cue.

     The collar is `bg-2` and the dome `bg-3` — the same "more raised is a
     step up the ramp" reading every other surface uses, which is what makes
     it work on a light theme, where up the ramp means lighter, without a
     single per-theme rule. */
  .cap {
    position: relative;
    width: var(--knob-size);
    height: var(--knob-size);
    border-radius: 50%;
    background-color: var(--bg-2);
    background-image: var(--sheen-dome);
    box-shadow:
      inset 0 var(--border-width) 0 0 var(--bevel-hi),
      inset 0 calc(-1 * var(--border-width)) 0 0 var(--bevel-lo),
      inset 0 0 0 calc(1px + 1px * var(--bevel)) rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.35)),
      var(--relief-2);
    transition: box-shadow 120ms, transform 120ms;
  }

  /* The inner dome. Its first shadow is cast OUTWARDS onto the collar — the
     dark ring that separates the two — and the two lips light its own rim. */
  .cap-face {
    position: absolute;
    inset: 16%;
    border-radius: 50%;
    background-color: var(--bg-3);
    background-image: var(--sheen-dome);
    box-shadow:
      0 calc(1px * var(--relief)) calc(2px + 3px * var(--relief))
        rgb(var(--shadow-rgb) / calc(var(--relief) * 0.55)),
      inset 0 var(--border-width) 0 0 var(--bevel-hi),
      inset 0 calc(-1 * var(--border-width)) 0 0 var(--bevel-lo);
    pointer-events: none;
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

  /* The indicator is a single lamp set into the collar, not a painted line:
     it is round, it glows, and it is lit from inside, which is why it takes
     an inset lip along its bottom rather than a shadow above it. Percentage
     sizing keeps it in proportion at every `size` a caller passes. */
  .pointer {
    position: absolute;
    inset: 0;
    border-radius: inherit;
    pointer-events: none;
  }
  .pointer::before {
    content: "";
    position: absolute;
    left: 50%;
    top: 5.5%;
    width: 14%;
    height: 14%;
    margin-left: -7%;
    border-radius: 50%;
    background: var(--knob-accent);
    box-shadow:
      0 0 calc(6px * var(--glow-scale)) var(--knob-accent),
      inset 0 calc(-1px * var(--bevel)) 0 rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.5));
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
