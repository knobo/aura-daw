<script lang="ts">
  /**
   * One binding's automation overlay (scope ruling 8: an overlay INSIDE the
   * track's existing timeline row, not an added row — the left rail and the
   * lane column share `--track-height` and inserting a sub-row would
   * desynchronise them).
   *
   * Curve values are normalized [0,1] (R3). Gain is a linear multiplier on
   * top of the fader (scope ruling 6): y=top is 1.0, y=bottom is 0.0. Pan
   * uses the same 0..1 draw space (0 = hard left, 1 = hard right).
   *
   * Edit shape: pointerdown opens a gesture and either grabs an existing
   * point or inserts one; pointermove PREVIEWS locally (no invoke);
   * pointerup commits ONCE through `modulation_set_curve` and closes the
   * gesture (scope ruling 7). Alt/right-click deletes a point.
   */
  import type { Binding, Curve, TrackState } from "../types/ipc";
  import { modulation } from "../state/modulation.svelte";
  import { midi } from "../state/midi.svelte";
  import { project } from "../state/project.svelte";
  import { view } from "../state/view.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import { deletePoint, hitTest, insertPoint, movePoint } from "../utils/automation-edit";

  let { track, binding, curve }: { track: TrackState; binding: Binding; curve: Curve } =
    $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let dragIndex = $state(-1);
  /** True between the pointerdown that OPENED an edit gesture and the
   * pointerup that closes it. The alt-click delete path closes its own
   * gesture (after its commit lands), so without this the pointerup that
   * follows would close it a second time — and early, leaving the delete's
   * commit outside the bracket with its own undo entry and its own persist. */
  let gestureOpen = false;
  const HIT_RADIUS_PX = 6;

  const live = $derived(modulation.curves.find((c) => c.id === curve.id) ?? curve);

  function xOfTick(tick: number): number {
    return view.xOf(midi.ticksToSamples(tick));
  }
  function tickAtX(x: number): number {
    return midi.samplesToTicks(view.snapSamples(view.samplesAt(x)));
  }

  $effect(() => {
    // touch the reactive inputs so the canvas repaints on any of them
    void [live.points, view.viewStart, view.spp, view.width, canvas];
    paint();
  });

  function paint() {
    const c = canvas;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth;
    const h = c.clientHeight;
    if (w === 0 || h === 0) return;
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.max(1, Math.round(h * dpr));
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const pts = live.points ?? [];
    if (pts.length === 0) return;

    ctx.strokeStyle = strokeOf();
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    // hold before the first and after the last point (the compile contract)
    ctx.moveTo(0, (1 - pts[0].value) * h);
    for (const p of pts) ctx.lineTo(xOfTick(p.tick), (1 - p.value) * h);
    ctx.lineTo(w, (1 - pts[pts.length - 1].value) * h);
    ctx.stroke();
    ctx.fillStyle = strokeOf();
    for (const p of pts) {
      ctx.beginPath();
      ctx.arc(xOfTick(p.tick), (1 - p.value) * h, 3, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  function strokeOf(): string {
    const t = binding.target;
    if (t.kind === "trackParam" && t.param === "pan") return "var(--violet)";
    if (t.kind === "pluginParam") return "var(--cyan)";
    return track.color ?? "var(--cyan)";
  }

  function labelOf(): string {
    const t = binding.target;
    if (t.kind === "trackParam") return t.param;
    if (t.kind === "pluginParam") return "plugin param";
    return "automation";
  }

  function domainAt(e: PointerEvent | MouseEvent) {
    const p = canvasPos(canvas!, e.clientX, e.clientY);
    const h = canvas!.clientHeight || 1;
    return { tick: tickAtX(p.x), value: Math.min(1, Math.max(0, 1 - p.y / h)) };
  }

  function onPointerDown(e: PointerEvent) {
    if (!canvas) return;
    const { tick, value } = domainAt(e);
    const c = live;
    const tickPerPx = Math.max(1e-9, midi.samplesToTicks(view.spp) - midi.samplesToTicks(0));
    const valuePerPx = 1 / Math.max(1, canvas.clientHeight);
    const hit = hitTest(c.points, tick, value, tickPerPx, valuePerPx, HIT_RADIUS_PX);

    if (e.button === 2 || e.altKey) {
      if (hit < 0) return;
      project.beginGesture("automation delete point");
      void modulation
        .commitInGesture(binding.id, { ...c, points: deletePoint(c.points, hit) })
        .then(() => project.endGesture()); // close only once the delete lands
      return;
    }
    canvas.setPointerCapture(e.pointerId);
    project.beginGesture("automation edit");
    gestureOpen = true;
    if (hit >= 0) {
      dragIndex = hit;
      if (c.id) modulation.preview(c.id, c.points);
    } else {
      const points = insertPoint(c.points, { tick, value });
      dragIndex = points.findIndex((p) => p.tick === tick);
      // an unminted curve must reach the backend before it can be previewed
      void modulation.commitInGesture(binding.id, { ...c, points });
    }
  }

  function onPointerMove(e: PointerEvent) {
    if (dragIndex < 0 || !canvas) return;
    const c = modulation.curves.find((x) => x.id === live.id);
    if (!c) return;
    const { tick, value } = domainAt(e);
    const moved = movePoint(c.points, dragIndex, tick, value, 0, 1);
    dragIndex = moved.index;
    modulation.preview(c.id, moved.points); // LOCAL only — no invoke per move
  }

  function onPointerUp(e: PointerEvent) {
    if (!canvas) return;
    canvas.releasePointerCapture?.(e.pointerId);
    const wasDragging = dragIndex >= 0;
    dragIndex = -1;
    if (!gestureOpen) return; // the delete path owns (and closes) its own
    gestureOpen = false;
    if (!wasDragging) {
      project.endGesture();
      return;
    }
    // `commitLatest` WAITS for a click-insert's reply before reading the
    // store: reading now could commit the pre-insert point set back over the
    // point just drawn (see its doc comment). The gesture closes after it.
    void modulation.commitLatest(binding.id).then(() => project.endGesture());
  }
</script>

<canvas
  bind:this={canvas}
  class="autolane"
  aria-label="{labelOf()} automation for {track.name}"
  oncontextmenu={(e) => e.preventDefault()}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
></canvas>

<style>
  .autolane {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    z-index: 3;
    background: transparent;
    cursor: crosshair;
  }
</style>
