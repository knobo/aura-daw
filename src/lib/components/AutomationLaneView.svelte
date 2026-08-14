<script lang="ts">
  /**
   * Per-track automation overlay (scope ruling 8: an overlay INSIDE the
   * track's existing timeline row, not an added row — the left rail and the
   * lane column share `--track-height` and inserting a sub-row would
   * desynchronise them).
   *
   * Values are LINEAR GAIN MULTIPLIERS in [0,1] applied on top of the fader
   * (scope ruling 6): y=top is 1.0 (fader unchanged), y=bottom is 0.0
   * (silence).
   *
   * Edit shape: pointerdown opens a gesture and either grabs an existing
   * point or inserts one; pointermove PREVIEWS locally (no invoke);
   * pointerup commits ONCE through `automation_set` and closes the gesture
   * (scope ruling 7). Alt/right-click deletes a point.
   */
  import type { TrackState, AutomationLane } from "../types/ipc";
  import { automation, TRACK_PARAM_GAIN, trackTarget } from "../state/automation.svelte";
  import { midi } from "../state/midi.svelte";
  import { project } from "../state/project.svelte";
  import { view } from "../state/view.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import { deletePoint, hitTest, insertPoint, movePoint } from "../utils/automation-edit";

  let { track }: { track: TrackState } = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let dragIndex = $state(-1);
  /** True between the pointerdown that OPENED an edit gesture and the
   * pointerup that closes it. The alt-click delete path closes its own
   * gesture (after its commit lands), so without this the pointerup that
   * follows would close it a second time — and early, leaving the delete's
   * commit outside the bracket with its own undo entry and its own persist. */
  let gestureOpen = false;
  const HIT_RADIUS_PX = 6;

  const lane = $derived(automation.gainLaneFor(track.id));

  /** The lane this overlay edits, minted locally when the track has none
   * yet. Empty id = "the backend mints one" (`automation_set`'s contract). */
  function laneOrNew(): AutomationLane {
    return (
      lane ?? {
        id: "",
        targetNode: trackTarget(track.id),
        paramId: TRACK_PARAM_GAIN,
        points: [],
      }
    );
  }

  function xOfTick(tick: number): number {
    return view.xOf(midi.ticksToSamples(tick));
  }
  function tickAtX(x: number): number {
    return midi.samplesToTicks(view.snapSamples(view.samplesAt(x)));
  }

  $effect(() => {
    // touch the reactive inputs so the canvas repaints on any of them
    void [lane?.points, view.viewStart, view.spp, view.width, canvas];
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
    const pts = lane?.points ?? [];
    if (pts.length === 0) return;

    ctx.strokeStyle = track.color ?? "var(--cyan)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    // hold before the first and after the last point (the compile contract)
    ctx.moveTo(0, (1 - pts[0].value) * h);
    for (const p of pts) ctx.lineTo(xOfTick(p.tick), (1 - p.value) * h);
    ctx.lineTo(w, (1 - pts[pts.length - 1].value) * h);
    ctx.stroke();
    ctx.fillStyle = track.color ?? "var(--cyan)";
    for (const p of pts) {
      ctx.beginPath();
      ctx.arc(xOfTick(p.tick), (1 - p.value) * h, 3, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  function domainAt(e: PointerEvent | MouseEvent) {
    const p = canvasPos(canvas!, e.clientX, e.clientY);
    const h = canvas!.clientHeight || 1;
    return { tick: tickAtX(p.x), value: Math.min(1, Math.max(0, 1 - p.y / h)) };
  }

  function onPointerDown(e: PointerEvent) {
    if (!canvas) return;
    const { tick, value } = domainAt(e);
    const l = laneOrNew();
    const tickPerPx = Math.max(1e-9, midi.samplesToTicks(view.spp) - midi.samplesToTicks(0));
    const valuePerPx = 1 / Math.max(1, canvas.clientHeight);
    const hit = hitTest(l.points, tick, value, tickPerPx, valuePerPx, HIT_RADIUS_PX);

    if (e.button === 2 || e.altKey) {
      if (hit < 0) return;
      project.beginGesture("automation delete point");
      void automation
        .commitInGesture({ ...l, points: deletePoint(l.points, hit) })
        .then(() => project.endGesture()); // close only once the delete lands
      return;
    }
    canvas.setPointerCapture(e.pointerId);
    project.beginGesture("automation edit");
    gestureOpen = true;
    if (hit >= 0) {
      dragIndex = hit;
      if (l.id) automation.preview(l.id, l.points);
    } else {
      const points = insertPoint(l.points, { tick, value });
      dragIndex = points.findIndex((p) => p.tick === tick);
      // an unminted lane must reach the backend before it can be previewed
      void automation.commitInGesture({ ...l, points });
    }
  }

  function onPointerMove(e: PointerEvent) {
    if (dragIndex < 0 || !canvas) return;
    const l = automation.gainLaneFor(track.id);
    if (!l) return;
    const { tick, value } = domainAt(e);
    const moved = movePoint(l.points, dragIndex, tick, value, 0, 1);
    dragIndex = moved.index;
    automation.preview(l.id, moved.points); // LOCAL only — no invoke per move
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
    void automation.commitLatest(track.id).then(() => project.endGesture());
  }
</script>

<canvas
  bind:this={canvas}
  class="autolane"
  aria-label="Gain automation for {track.name}"
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
    background: color-mix(in srgb, var(--bg-0) 45%, transparent);
    cursor: crosshair;
  }
</style>
