<script lang="ts">
  /**
   * Vertical fader: a milled slot and a domed cap that travels it.
   * Same gesture contract as Knob — vertical drag, Shift fine, double-click
   * reset, begin/end brackets for one undo entry.
   */
  interface Props {
    value: number;
    min: number;
    max: number;
    resetTo?: number;
    /** Slot height in px. */
    size?: number;
    label?: string;
    color?: string;
    format?: (v: number) => string;
    ariaLabel: string;
    oninput?: (v: number) => void;
    onstart?: () => void;
    onend?: () => void;
  }

  const {
    value,
    min,
    max,
    resetTo,
    size = 120,
    label,
    color,
    format,
    ariaLabel,
    oninput,
    onstart,
    onend,
  }: Props = $props();

  const FULL_TRAVEL_PX = 160;
  const span = $derived(max - min || 1);
  const frac = $derived(Math.min(1, Math.max(0, (value - min) / span)));
  const home = $derived(resetTo ?? min);

  let dragging = $state(false);
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

  function onKeyDown(e: KeyboardEvent) {
    const step = e.key === "PageUp" || e.key === "PageDown" ? span / 10 : span / 100;
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

<div class="fader-unit" style:--fader-h="{size}px" style:--fader-accent={color ?? "var(--cyan)"}>
  {#if label}<span class="silk fader-label">{label}</span>{/if}
  <div
    class="fader"
    class:dragging
    role="slider"
    tabindex="0"
    aria-label={ariaLabel}
    aria-valuemin={min}
    aria-valuemax={max}
    aria-valuenow={value}
    aria-valuetext={format ? format(value) : undefined}
    aria-orientation="vertical"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={endDrag}
    onpointercancel={endDrag}
    ondblclick={onDoubleClick}
    onkeydown={onKeyDown}
  >
    <div class="slot">
      <div class="fill" style:height="{frac * 100}%"></div>
    </div>
    <div class="cap grain" style:bottom="calc({frac} * (100% - 18px))">
      <div class="ridge"></div>
    </div>
  </div>
  {#if format}<output class="mono fader-value">{format(value)}</output>{/if}
</div>

<style>
  .fader-unit {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    width: 36px;
  }
  .fader-label {
    line-height: 1;
  }
  .fader {
    position: relative;
    width: 28px;
    height: var(--fader-h);
    cursor: ns-resize;
    touch-action: none;
  }
  .fader:focus-visible {
    outline: var(--focus-width) solid var(--cyan);
    outline-offset: 2px;
  }
  .slot {
    position: absolute;
    left: 50%;
    top: 8px;
    bottom: 8px;
    width: 8px;
    margin-left: -4px;
    border-radius: 4px;
    background: var(--bg-sunken);
    box-shadow: var(--bevel-inset);
    overflow: hidden;
  }
  .fill {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    background: var(--fader-accent);
    opacity: 0.55;
    box-shadow: 0 0 calc(6px * var(--glow-scale)) var(--fader-accent);
  }
  .cap {
    position: absolute;
    left: 50%;
    width: 22px;
    height: 18px;
    margin-left: -11px;
    border-radius: calc(var(--ctrl-radius) + 2px);
    background-color: var(--bg-3);
    background-image: var(--sheen-face);
    box-shadow:
      var(--bevel-raised),
      var(--relief-2);
    z-index: 1;
  }
  .fader.dragging .cap {
    transform: translateY(calc(1px * var(--relief)));
  }
  .ridge {
    position: absolute;
    left: 3px;
    right: 3px;
    top: 50%;
    height: 2px;
    margin-top: -1px;
    border-radius: 1px;
    background: var(--fader-accent);
    box-shadow: 0 0 calc(4px * var(--glow-scale)) var(--fader-accent);
  }
  .fader-value {
    font-size: 10px;
    color: var(--text-mid);
    line-height: 1;
  }
</style>
