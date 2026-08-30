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
    type PanelEdge,
    type ResizeSpec,
  } from "../utils/panel-resize";
  import { uiZoomFactor } from "../utils/ui-zoom";

  let {
    axis,
    size,
    spec,
    label,
    edge = "start",
    onresize,
  }: {
    /** "y": horizontal bar on a top/bottom edge (ns-resize). "x": vertical bar on a left/right edge (ew-resize). */
    axis: "x" | "y";
    /** Current panel size in CSS px (height for "y", width for "x"). */
    size: number;
    spec: ResizeSpec;
    label: string;
    /** Which edge of the panel this handle sits on. "start" (default) is
     * the edge nearest the app centre, where dragging INWARD grows the
     * panel — the roll's top and the dock's left. The track rail is
     * anchored to the window's left, so its handle is on the "end" edge
     * and every direction flips: the CSS side, the drag sign, and which
     * arrow key grows it. */
    edge?: PanelEdge;
    onresize: (px: number) => void;
  } = $props();

  let drag: PanelDrag | null = null;
  let dragging = $state(false);

  // clientX/Y and window.inner{Width,Height} are VISUAL px; `size`/`spec`
  // (panel-resize.ts) are LAYOUT px ("current panel size in CSS px") —
  // divide out the interface zoom so a drag doesn't resize the panel by
  // more than the pointer actually moved.
  const coord = (e: PointerEvent) => (axis === "y" ? e.clientY : e.clientX) / uiZoomFactor();
  const viewport = () => (axis === "y" ? window.innerHeight : window.innerWidth) / uiZoomFactor();

  function onDown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    // The rail is a `role="grid"` with its own pointerdown for lane
    // selection, and a resize gesture is not a lane click. Harmless where
    // the parent has no handler (the roll, the dock).
    e.stopPropagation();
    drag = createPanelDrag(spec, size, coord(e), edge);
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
    const inward = edge === "end";
    const grow = axis === "y" ? (inward ? "ArrowDown" : "ArrowUp") : inward ? "ArrowRight" : "ArrowLeft";
    const shrink = axis === "y" ? (inward ? "ArrowUp" : "ArrowDown") : inward ? "ArrowLeft" : "ArrowRight";
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
  class="handle {axis} {edge}"
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
  .handle.x.end {
    left: auto;
    right: -3px;
  }
  .handle.y.end {
    top: auto;
    bottom: -3px;
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
    box-shadow: 0 0 calc(8px * var(--glow-scale)) var(--cyan-dim);
  }
</style>
