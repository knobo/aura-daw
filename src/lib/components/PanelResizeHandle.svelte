<script lang="ts">
  /**
   * Edge grab-handle for a resizable panel. Sits on the edge nearest the app
   * center (piano roll: top, dock: left); dragging away from the panel grows
   * it. Pointer-captured like the piano roll's note gestures, so the drag
   * survives leaving the hit area. The math lives in utils/panel-resize —
   * this component only turns pointer events into coordinates.
   */
  import {
    clampSize,
    createPanelDrag,
    type PanelDrag,
    type ResizeSpec,
  } from "../utils/panel-resize";

  let {
    axis,
    size,
    spec,
    label,
    onresize,
  }: {
    /** "y": horizontal bar on the top edge (ns-resize). "x": vertical bar on the left edge (ew-resize). */
    axis: "x" | "y";
    /** Current panel size in CSS px (height for "y", width for "x"). */
    size: number;
    spec: ResizeSpec;
    label: string;
    onresize: (px: number) => void;
  } = $props();

  let drag: PanelDrag | null = null;
  let dragging = $state(false);

  const coord = (e: PointerEvent) => (axis === "y" ? e.clientY : e.clientX);
  const viewport = () => (axis === "y" ? window.innerHeight : window.innerWidth);

  function onDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    drag = createPanelDrag(spec, size, coord(e));
    dragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }
  function onMove(e: PointerEvent) {
    if (drag) onresize(drag.update(coord(e), viewport()));
  }
  function onUp(e: PointerEvent) {
    if (!drag) return;
    drag = null;
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }
  function onKeydown(e: KeyboardEvent) {
    const grow = axis === "y" ? "ArrowUp" : "ArrowLeft";
    const shrink = axis === "y" ? "ArrowDown" : "ArrowRight";
    if (e.key !== grow && e.key !== shrink) return;
    e.preventDefault();
    const step = e.key === grow ? 16 : -16;
    onresize(clampSize(size + step, spec, viewport()));
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<!-- ARIA window-splitter pattern: a focusable separator with valuenow IS an
     interactive widget; Svelte's checker just doesn't model that variant. -->
<div
  class="handle {axis}"
  class:dragging
  role="separator"
  aria-orientation={axis === "y" ? "horizontal" : "vertical"}
  aria-label={label}
  aria-valuenow={size}
  aria-valuemin={spec.minPx}
  tabindex="0"
  onpointerdown={onDown}
  onpointermove={onMove}
  onpointerup={onUp}
  onpointercancel={onUp}
  onkeydown={onKeydown}
></div>

<style>
  .handle {
    position: absolute;
    z-index: 40;
    touch-action: none;
    background: transparent;
    outline: none;
  }
  .handle.y {
    top: -3px;
    left: 0;
    right: 0;
    height: 7px;
    cursor: ns-resize;
  }
  .handle.x {
    left: -3px;
    top: 0;
    bottom: 0;
    width: 7px;
    cursor: ew-resize;
  }

  /* the visible affordance: a hairline that wakes up cyan on hover/drag */
  .handle::after {
    content: "";
    position: absolute;
    background: transparent;
    transition:
      background 120ms ease,
      box-shadow 120ms ease;
  }
  .handle.y::after {
    left: 0;
    right: 0;
    top: 3px;
    height: 1px;
  }
  .handle.x::after {
    top: 0;
    bottom: 0;
    left: 3px;
    width: 1px;
  }
  .handle:hover::after,
  .handle:focus-visible::after,
  .handle.dragging::after {
    background: var(--cyan);
    box-shadow: 0 0 8px var(--cyan-dim);
  }
</style>
