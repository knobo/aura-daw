<script lang="ts">
  /**
   * Piano-roll envelope lane (design §10.3). Keyed by content, so a looping
   * clip loops the same shape. Ticks are content-relative — the same space
   * the notes sit in — and the gestures match the timeline overlay
   * (preview on move, one commit on pointerup, alt/right-click delete).
   */
  import type { Binding, TrackState } from "../types/ipc";
  import { modulation } from "../state/modulation.svelte";
  import { project } from "../state/project.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import { deletePoint, hitTest, insertPoint, movePoint } from "../utils/automation-edit";
  import LanePickerMenu from "./LanePickerMenu.svelte";

  let {
    contentId,
    track,
    tpp,
    scrollTick,
    contentLengthTicks,
    keysWidth,
  }: {
    contentId: string;
    track: TrackState;
    tpp: number;
    scrollTick: number;
    contentLengthTicks: number;
    keysWidth: number;
  } = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let pickerOpen = $state(false);
  let dragIndex = $state(-1);
  let gestureOpen = false;
  let token: Promise<string | undefined> | undefined;
  const HIT_RADIUS_PX = 6;

  function openGesture(label: string) {
    token = project.beginGesture(label);
    return token;
  }
  function closeGesture() {
    const idp = token;
    token = undefined;
    void idp?.then((id) => project.endGesture(id));
  }
  const ENV_H = 56;

  const envelopes = $derived(modulation.clipEnvelopeBindings(contentId));
  const binding = $derived.by((): Binding | undefined => {
    const visible = modulation.visibleBindingsFor(contentId);
    return visible[visible.length - 1] ?? envelopes[envelopes.length - 1];
  });
  const live = $derived(binding ? modulation.curveOf(binding) : undefined);

  function xOfTick(tick: number): number {
    return (tick - scrollTick) / tpp;
  }
  function tickAtX(x: number): number {
    const raw = scrollTick + x * tpp;
    return Math.min(Math.max(0, Math.round(raw)), Math.max(0, contentLengthTicks - 1));
  }

  $effect(() => {
    void [live?.points, scrollTick, tpp, contentLengthTicks, canvas, binding];
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
    const pts = live?.points ?? [];
    if (pts.length === 0) return;

    ctx.strokeStyle = strokeOf();
    ctx.lineWidth = 1.5;
    ctx.beginPath();
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
    const t = binding?.target;
    if (t?.kind === "selfTrackParam" && t.param === "pan") return "var(--violet)";
    if (t?.kind === "selfInstrumentParam" || t?.kind === "pluginParam") return "var(--cyan)";
    return track.color ?? "var(--cyan)";
  }

  function labelOf(): string {
    const t = binding?.target;
    if (!t) return "env";
    if (t.kind === "selfTrackParam") return t.param;
    if (t.kind === "selfInstrumentParam") return `p${t.paramId}`;
    return "env";
  }

  function domainAt(e: PointerEvent | MouseEvent) {
    const p = canvasPos(canvas!, e.clientX, e.clientY);
    const h = canvas!.clientHeight || 1;
    return { tick: tickAtX(p.x), value: Math.min(1, Math.max(0, 1 - p.y / h)) };
  }

  function onPointerDown(e: PointerEvent) {
    if (!canvas) return;
    if (!binding || !live) {
      void modulation.pickClipEnvelope(
        contentId,
        { kind: "selfTrackParam", param: "gain" },
        1,
        contentLengthTicks,
      );
      return;
    }
    const { tick, value } = domainAt(e);
    const c = live;
    const tickPerPx = Math.max(1e-9, tpp);
    const valuePerPx = 1 / Math.max(1, canvas.clientHeight);
    const hit = hitTest(c.points, tick, value, tickPerPx, valuePerPx, HIT_RADIUS_PX);

    if (e.button === 2 || e.altKey) {
      if (hit < 0) return;
      const gid = openGesture("clip envelope delete point");
      void modulation
        .commitInGesture(binding.id, { ...c, points: deletePoint(c.points, hit) })
        .then(async () => project.endGesture(await gid));
      token = undefined;
      return;
    }
    canvas.setPointerCapture(e.pointerId);
    openGesture("clip envelope edit");
    gestureOpen = true;
    if (hit >= 0) {
      dragIndex = hit;
      if (c.id) modulation.preview(c.id, c.points);
    } else {
      const points = insertPoint(c.points, { tick, value });
      dragIndex = points.findIndex((p) => p.tick === tick);
      void modulation.commitInGesture(binding.id, { ...c, points });
    }
  }

  function onPointerMove(e: PointerEvent) {
    if (dragIndex < 0 || !canvas || !live) return;
    const c = modulation.curves.find((x) => x.id === live.id);
    if (!c) return;
    const { tick, value } = domainAt(e);
    const moved = movePoint(c.points, dragIndex, tick, value, 0, 1);
    dragIndex = moved.index;
    modulation.preview(c.id, moved.points);
  }

  function onPointerUp(e: PointerEvent) {
    if (!canvas) return;
    canvas.releasePointerCapture?.(e.pointerId);
    const wasDragging = dragIndex >= 0;
    dragIndex = -1;
    if (!gestureOpen) return;
    gestureOpen = false;
    if (!wasDragging || !binding) {
      closeGesture();
      return;
    }
    const gid = token;
    token = undefined;
    void modulation.commitLatest(binding.id).then(async () => project.endGesture(await gid));
  }
</script>

<div class="envrow">
  <div class="envlabel silk" style:width="{keysWidth}px">
    <span class="picker">
      <button
        class="pick mono"
        class:on={!!binding || pickerOpen}
        title="Clip envelope target"
        aria-haspopup="menu"
        aria-expanded={pickerOpen}
        onclick={() => (pickerOpen = !pickerOpen)}>{labelOf()}</button
      >
      {#if pickerOpen}
        <LanePickerMenu {track} {contentId} onclose={() => (pickerOpen = false)} />
      {/if}
    </span>
  </div>
  <canvas
    bind:this={canvas}
    class="env"
    style:height="{ENV_H}px"
    aria-label="{labelOf()} clip envelope"
    oncontextmenu={(e) => e.preventDefault()}
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
  ></canvas>
</div>

<style>
  .envrow {
    flex: none;
    display: flex;
    border-top: 1px solid var(--glass-border);
  }
  .envlabel {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    padding-right: 8px;
    border-right: 1px solid rgb(var(--line-rgb) / 0.25);
    position: relative;
  }
  .picker {
    position: relative;
  }
  .pick {
    font-size: 9px;
    letter-spacing: 0.12em;
    padding: 2px 6px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .pick.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .env {
    flex: 1;
    min-width: 0;
    touch-action: none;
    cursor: crosshair;
  }
</style>
