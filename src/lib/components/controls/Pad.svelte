<script lang="ts">
  /**
   * Rubber performance pad. LED brightness is driven from the meter bus
   * inside a rAF loop (not Svelte state — same contract as Meter.svelte).
   * A toggle pad lights solid when `lit` is true; otherwise the LED
   * breathes with RMS.
   */
  import { latestMeter } from "../../state/meters.svelte";
  import { linToDb } from "../../utils/format";

  interface Props {
    label: string;
    /** Track whose RMS feeds the LED. Null = idle glow only. */
    meterTrackId?: string | null;
    /** Solid lamp (toggle on, or currently launching). */
    lit?: boolean;
    color?: string;
    disabled?: boolean;
    ariaLabel: string;
    onpress?: () => void;
  }

  const {
    label,
    meterTrackId = null,
    lit = false,
    color,
    disabled = false,
    ariaLabel,
    onpress,
  }: Props = $props();

  let ledEl: HTMLElement | undefined = $state();
  let pressed = $state(false);

  $effect(() => {
    const id = meterTrackId;
    const solid = lit;
    const el = ledEl;
    if (!el) return;
    let raf = 0;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      let brightness = 0.12;
      if (solid) {
        brightness = 1;
      } else if (id) {
        const m = latestMeter(id);
        if (m) {
          const rms = Math.max(m.rmsL, m.rmsR);
          const db = linToDb(rms);
          brightness = 0.12 + 0.88 * Math.max(0, Math.min(1, (db + 48) / 48));
        }
      }
      el.style.opacity = String(brightness);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  // The pointer pair only drives the pressed visual. Firing happens on
  // `click`, which a real <button> also delivers for Enter and Space — a
  // pointerup-only pad cannot be played from the keyboard at all.
  function down(e: PointerEvent) {
    if (e.button !== 0 || disabled) return;
    pressed = true;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }
  function up(e: PointerEvent) {
    if (!pressed) return;
    pressed = false;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }
  function fire() {
    if (disabled) return;
    onpress?.();
  }
</script>

<button
  class="pad grain"
  class:pressed
  class:lit
  class:dead={disabled}
  type="button"
  style:--pad-led={color ?? "var(--cyan)"}
  aria-label={ariaLabel}
  aria-pressed={lit}
  disabled={disabled}
  onpointerdown={down}
  onpointerup={up}
  onpointercancel={() => (pressed = false)}
  onclick={fire}
>
  <span class="led" bind:this={ledEl}></span>
  <span class="silk caption">{label}</span>
</button>

<style>
  .pad {
    position: relative;
    width: 56px;
    height: 56px;
    border: none;
    border-radius: 14px;
    background-color: var(--bg-2);
    background-image: var(--sheen-dome);
    box-shadow:
      var(--bevel-raised),
      var(--relief-2),
      inset 0 0 0 1px rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.35));
    cursor: pointer;
    color: var(--text-mid);
    overflow: hidden;
  }
  .pad:focus-visible {
    outline: var(--focus-width) solid var(--cyan);
    outline-offset: 2px;
  }
  .pad.pressed,
  .pad:active {
    transform: translateY(calc(2px * var(--relief))) scale(0.97);
    box-shadow: var(--bevel-inset), var(--relief-1);
  }
  .pad.dead {
    opacity: 0.4;
    cursor: default;
  }
  .led {
    position: absolute;
    inset: 8px;
    border-radius: 10px;
    background: var(--pad-led);
    opacity: 0.12;
    box-shadow: 0 0 calc(14px * var(--glow-scale)) var(--pad-led);
    pointer-events: none;
    /* Opacity is rewritten every frame from the rAF loop above. */
    will-change: opacity;
  }
  .pad.lit .led {
    opacity: 1;
  }
  .caption {
    position: absolute;
    left: 4px;
    right: 4px;
    bottom: 5px;
    text-align: center;
    font-size: 8px;
    letter-spacing: 0.08em;
    color: var(--text);
    text-shadow: 0 1px 2px rgb(var(--shadow-rgb) / 0.8);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
