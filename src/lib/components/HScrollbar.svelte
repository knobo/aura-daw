<script lang="ts">
  /**
   * Slim horizontal scrollbar for the virtual (canvas-scrolled) time views —
   * the timeline lanes and the piano roll grid — which have no native
   * overflow to scroll. Unit-agnostic: the timeline feeds samples, the roll
   * feeds ticks; the mapping math lives in utils/hscroll. Dragging freezes
   * the extent at grab time so the thumb tracks the pointer 1:1, and a
   * press on the empty track centers the thumb there and keeps dragging.
   */
  import { startFromThumbX, thumbGeometry, totalExtent } from "../utils/hscroll";
  import { uiZoomFactor } from "../utils/ui-zoom";

  let {
    start,
    viewSpan,
    contentEnd,
    label,
    controls,
    onscroll,
  }: {
    /** left edge of the viewport, in content units */
    start: number;
    /** visible span of the viewport, in content units */
    viewSpan: number;
    /** end of the content, in content units */
    contentEnd: number;
    label: string;
    /** DOM id of the scrolled viewport (aria-controls) */
    controls: string;
    onscroll: (start: number) => void;
  } = $props();

  const MIN_THUMB = 24;

  let trackEl: HTMLDivElement | undefined = $state();
  let trackW = $state(0);
  /** frozen at pointerdown: {extent, pointer-x within thumb} */
  let drag = $state<{ total: number; grabX: number } | null>(null);
  const dragging = $derived(drag !== null);

  const geom = $derived(
    thumbGeometry(start, viewSpan, drag?.total ?? totalExtent(start, viewSpan, contentEnd), trackW, MIN_THUMB),
  );

  $effect(() => {
    if (!trackEl) return;
    const el = trackEl;
    const ro = new ResizeObserver(() => (trackW = el.clientWidth));
    ro.observe(el);
    trackW = el.clientWidth;
    return () => ro.disconnect();
  });

  function trackX(e: PointerEvent): number {
    // e.clientX and the rect are VISUAL px; trackW (below) is clientWidth,
    // i.e. LAYOUT px — divide out the interface zoom so the thumb tracks
    // the pointer 1:1 at any zoom level.
    return (e.clientX - trackEl!.getBoundingClientRect().left) / uiZoomFactor();
  }

  function onDown(e: PointerEvent) {
    if (e.button !== 0 || !geom.scrollable) return;
    e.preventDefault();
    const x = trackX(e);
    const onThumb = x >= geom.x && x <= geom.x + geom.w;
    drag = {
      total: totalExtent(start, viewSpan, contentEnd),
      grabX: onThumb ? x - geom.x : geom.w / 2,
    };
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    if (!onThumb) applyDrag(e);
  }
  function applyDrag(e: PointerEvent) {
    if (!drag) return;
    onscroll(startFromThumbX(trackX(e) - drag.grabX, viewSpan, drag.total, trackW, MIN_THUMB));
  }
  function onMove(e: PointerEvent) {
    applyDrag(e);
  }
  function onUp(e: PointerEvent) {
    if (!drag) return;
    drag = null;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }
  function onKeydown(e: KeyboardEvent) {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const step = viewSpan * (e.shiftKey ? 1 : 0.1) * (e.key === "ArrowLeft" ? -1 : 1);
    const max = totalExtent(start, viewSpan, contentEnd) - viewSpan;
    onscroll(Math.min(max, Math.max(0, start + step)));
  }

  const valueNow = $derived.by(() => {
    const max = totalExtent(start, viewSpan, contentEnd) - viewSpan;
    return max > 0 ? Math.round((start / max) * 100) : 0;
  });
</script>

<div
  class="track"
  class:dragging
  class:inert={!geom.scrollable}
  bind:this={trackEl}
  role="scrollbar"
  aria-label={label}
  aria-orientation="horizontal"
  aria-controls={controls}
  aria-valuenow={valueNow}
  aria-valuemin={0}
  aria-valuemax={100}
  tabindex="0"
  onpointerdown={onDown}
  onpointermove={onMove}
  onpointerup={onUp}
  onpointercancel={onUp}
  onkeydown={onKeydown}
>
  <div class="thumb" style:left="{geom.x.toFixed(1)}px" style:width="{geom.w.toFixed(1)}px"></div>
</div>

<style>
  .track {
    position: relative;
    flex: 1;
    min-width: 0;
    height: 10px;
    background: rgb(var(--bg-0-rgb) / 0.6);
    touch-action: none;
    outline: none;
    cursor: pointer;
  }
  .track.inert {
    cursor: default;
  }
  .thumb {
    position: absolute;
    top: 2px;
    bottom: 2px;
    border-radius: 3px;
    background: rgb(var(--line-rgb) / 0.35);
    transition: background 120ms ease, box-shadow 120ms ease;
  }
  .track.inert .thumb {
    background: rgb(var(--line-rgb) / 0.12);
  }
  .track:hover:not(.inert) .thumb,
  .track:focus-visible .thumb {
    background: rgb(var(--edge-rgb) / 0.55);
  }
  .track.dragging .thumb {
    background: var(--cyan-dim);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.35);
  }
</style>
