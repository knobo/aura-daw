<script lang="ts">
  /**
   * Timeline row for a kind:"automation" track. Clips are placements of a
   * curve (same field names / loop rule as MidiClip — design §3.5). Drag,
   * right-edge resize, delete, and point-edit (content-relative ticks).
   */
  import type { AutomationClip, Curve, TrackState } from "../types/ipc";
  import { modulation } from "../state/modulation.svelte";
  import { midi } from "../state/midi.svelte";
  import { project } from "../state/project.svelte";
  import { view } from "../state/view.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import { deletePoint, hitTest, insertPoint, movePoint } from "../utils/automation-edit";

  let { track }: { track: TrackState } = $props();

  const clips = $derived(modulation.clipsOn(track.id));
  let selectedId = $state<string | null>(null);
  const canvases = new Map<string, HTMLCanvasElement>();

  function curveCanvas(node: HTMLCanvasElement, clip: AutomationClip) {
    canvases.set(clip.id, node);
    paint(clip);
    return {
      update(next: AutomationClip) {
        canvases.delete(clip.id);
        clip = next;
        canvases.set(clip.id, node);
        paint(clip);
      },
      destroy() {
        canvases.delete(clip.id);
      },
    };
  }

  function startSamples(c: AutomationClip): number {
    return midi.ticksToSamples(c.timelineStartTicks);
  }
  function lengthSamplesOf(c: AutomationClip): number {
    return midi.ticksToSamples(c.timelineStartTicks + c.lengthTicks) - startSamples(c);
  }
  function contentTicks(c: AutomationClip): number {
    return Math.max(1, c.contentLengthTicks ?? c.lengthTicks);
  }
  function curveOf(c: AutomationClip): Curve | undefined {
    return modulation.curves.find((x) => x.id === c.curveId);
  }

  $effect(() => {
    void [clips, view.viewStart, view.spp, view.width, modulation.curves];
    for (const clip of clips) paint(clip);
  });

  function paint(clip: AutomationClip) {
    const c = canvases.get(clip.id);
    const curve = curveOf(clip);
    if (!c || !curve) return;
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
    const pts = curve.points ?? [];
    if (pts.length === 0) return;
    const period = contentTicks(clip);
    const leftPx = view.xOf(startSamples(clip));
    const visL = Math.max(0, -leftPx);
    const color = track.color ?? "var(--cyan)";
    ctx.strokeStyle = color;
    ctx.fillStyle = color;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    let started = false;
    const repeats = Math.max(1, Math.ceil(clip.lengthTicks / period));
    for (let r = 0; r < repeats; r++) {
      const origin = clip.timelineStartTicks + r * period;
      for (const p of pts) {
        const tick = origin + p.tick;
        if (tick >= clip.timelineStartTicks + clip.lengthTicks) continue;
        const x = view.xOf(midi.ticksToSamples(tick)) - leftPx - visL;
        const y = (1 - p.value) * h;
        if (!started) {
          ctx.moveTo(x, y);
          started = true;
        } else ctx.lineTo(x, y);
      }
    }
    ctx.stroke();
    for (let r = 0; r < repeats; r++) {
      const origin = clip.timelineStartTicks + r * period;
      for (const p of pts) {
        const tick = origin + p.tick;
        if (tick >= clip.timelineStartTicks + clip.lengthTicks) continue;
        const x = view.xOf(midi.ticksToSamples(tick)) - leftPx - visL;
        ctx.beginPath();
        ctx.arc(x, (1 - p.value) * h, 3, 0, Math.PI * 2);
        ctx.fill();
      }
    }
    if (repeats > 1) {
      ctx.fillStyle = `color-mix(in srgb, ${color} 35%, transparent)`;
      for (let r = 1; r < repeats; r++) {
        const x = view.xOf(midi.ticksToSamples(clip.timelineStartTicks + r * period)) - leftPx - visL;
        ctx.fillRect(x, 0, 1, h);
      }
    }
  }

  const EDGE_PX = 8;
  let dragging = $state(false);
  let dragMode: "move" | "resize" | "point" = "move";
  let dragClipId = "";
  let dragStartX = 0;
  let dragOrigTicks = 0;
  let dragOrigLength = 0;
  let dragOrigContent = 0;
  let dragMoved = false;
  let dragIndex = -1;
  let hoverEdge = $state(false);
  let gestureOpen = false;

  function localTickValue(clip: AutomationClip, e: PointerEvent, el: HTMLElement) {
    const p = canvasPos(el, e.clientX, e.clientY);
    const absTick = midi.samplesToTicks(view.snapSamples(view.samplesAt(view.xOf(startSamples(clip)) + p.x)));
    const period = contentTicks(clip);
    const rel = Math.max(0, absTick - clip.timelineStartTicks);
    const tick = Math.round(rel % period);
    const h = el.clientHeight || 1;
    const value = Math.min(1, Math.max(0, 1 - p.y / h));
    return { tick, value };
  }

  function onPointerDown(clip: AutomationClip, e: PointerEvent) {
    if (e.button !== 0 && e.button !== 2) return;
    selectedId = clip.id;
    const el = e.currentTarget as HTMLElement;
    const rect = el.getBoundingClientRect();
    const nearRight = rect.right - e.clientX <= EDGE_PX;
    const curve = curveOf(clip);
    const canvas = canvases.get(clip.id);

    if (curve && canvas && (e.button === 2 || e.altKey || !nearRight)) {
      const { tick, value } = localTickValue(clip, e, canvas);
      const tickPerPx = Math.max(1e-9, midi.samplesToTicks(view.spp) - midi.samplesToTicks(0));
      const valuePerPx = 1 / Math.max(1, canvas.clientHeight);
      const hit = hitTest(curve.points, tick, value, tickPerPx, valuePerPx, 6);
      if (e.button === 2 || e.altKey) {
        e.preventDefault();
        if (hit < 0) return;
        project.beginGesture("automation delete point");
        void modulation
          .commitInGesture(clip.id, { ...curve, points: deletePoint(curve.points, hit) })
          .then(() => project.endGesture());
        return;
      }
      if (hit >= 0 || !nearRight) {
        canvas.setPointerCapture(e.pointerId);
        project.beginGesture("automation edit");
        gestureOpen = true;
        dragMode = "point";
        dragClipId = clip.id;
        dragging = true;
        if (hit >= 0) {
          dragIndex = hit;
          modulation.preview(curve.id, curve.points);
        } else {
          const points = insertPoint(curve.points, { tick, value });
          dragIndex = points.findIndex((p) => p.tick === tick);
          void modulation.commitInGesture(clip.id, { ...curve, points });
        }
        return;
      }
    }

    dragMode = nearRight ? "resize" : "move";
    dragging = true;
    dragMoved = false;
    dragClipId = clip.id;
    dragStartX = e.clientX;
    dragOrigTicks = clip.timelineStartTicks;
    dragOrigLength = clip.lengthTicks;
    dragOrigContent = contentTicks(clip);
    el.setPointerCapture?.(e.pointerId);
  }

  function onPointerMove(clip: AutomationClip, e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    if (!dragging) {
      const rect = el.getBoundingClientRect();
      hoverEdge = selectedId === clip.id && rect.right - e.clientX <= EDGE_PX;
      return;
    }
    if (dragClipId !== clip.id) return;
    if (dragMode === "point") {
      const curve = curveOf(clip);
      const canvas = canvases.get(clip.id);
      if (!curve || !canvas || dragIndex < 0) return;
      const { tick, value } = localTickValue(clip, e, canvas);
      const moved = movePoint(curve.points, dragIndex, tick, value, 0, 1);
      dragIndex = moved.index;
      modulation.preview(curve.id, moved.points);
      return;
    }
    const dx = e.clientX - dragStartX;
    if (Math.abs(dx) > 2) dragMoved = true;
    if (!dragMoved) return;
    if (dragMode === "resize") {
      let targetSamples = midi.ticksToSamples(dragOrigTicks + dragOrigLength) + dx * view.spp;
      if (!e.altKey) targetSamples = view.snapSamples(targetSamples);
      const newLength = Math.max(1, Math.round(midi.samplesToTicks(Math.max(0, targetSamples)) - dragOrigTicks));
      const next = { ...clip, lengthTicks: newLength, contentLengthTicks: dragOrigContent };
      modulation.automationClips = modulation.automationClips.map((c) =>
        c.id === clip.id ? next : c,
      );
    } else {
      let targetSamples = midi.ticksToSamples(dragOrigTicks) + dx * view.spp;
      if (!e.altKey) targetSamples = view.snapSamples(targetSamples);
      const next = {
        ...clip,
        timelineStartTicks: Math.max(0, Math.round(midi.samplesToTicks(Math.max(0, targetSamples)))),
      };
      modulation.automationClips = modulation.automationClips.map((c) =>
        c.id === clip.id ? next : c,
      );
    }
  }

  function onPointerUp(clip: AutomationClip, e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    el.releasePointerCapture?.(e.pointerId);
    const was = dragging && dragClipId === clip.id;
    const mode = dragMode;
    const moved = dragMoved;
    dragging = false;
    dragClipId = "";
    hoverEdge = false;
    if (!was) return;
    if (mode === "point") {
      if (!gestureOpen) return;
      gestureOpen = false;
      const curve = curveOf(clip);
      if (curve) void modulation.commitInGesture(clip.id, curve).then(() => project.endGesture());
      dragIndex = -1;
      return;
    }
    if (moved) {
      const live = modulation.automationClips.find((c) => c.id === clip.id) ?? clip;
      void modulation.setClip(live);
    }
  }

  function onLaneKey(e: KeyboardEvent) {
    if ((e.key === "Delete" || e.key === "Backspace") && selectedId) {
      e.preventDefault();
      void modulation.removeClip(selectedId);
      selectedId = null;
    }
  }
</script>

<div class="autorow">
  {#each clips as clip (clip.id)}
    {@const leftPx = view.xOf(startSamples(clip))}
    {@const widthPx = lengthSamplesOf(clip) / view.spp}
    {@const visL = Math.max(0, -leftPx)}
    {@const visR = Math.min(widthPx, view.width - leftPx)}
    {#if visR > 0 && visL < widthPx}
      <div
        class="aclip"
        class:selected={selectedId === clip.id}
        class:edge={hoverEdge && selectedId === clip.id}
        style:left="{leftPx}px"
        style:width="{Math.max(6, widthPx)}px"
        style:--clip-color={track.color}
        role="button"
        aria-label="Automation clip — drag to move, right edge to loop, Delete to remove"
        tabindex="0"
        oncontextmenu={(e) => e.preventDefault()}
        onpointerdown={(e) => onPointerDown(clip, e)}
        onpointermove={(e) => onPointerMove(clip, e)}
        onpointerup={(e) => onPointerUp(clip, e)}
        onpointercancel={(e) => onPointerUp(clip, e)}
        onkeydown={onLaneKey}
      >
        <canvas
          use:curveCanvas={clip}
          class="curve"
          style:left="{visL}px"
          style:width="{Math.max(1, Math.floor(visR - visL))}px"
        ></canvas>
        {#if selectedId === clip.id}
          <button
            class="del"
            title="Delete clip"
            aria-label="Delete automation clip"
            onclick={(e) => {
              e.stopPropagation();
              void modulation.removeClip(clip.id);
              selectedId = null;
            }}>×</button
          >
        {/if}
      </div>
    {/if}
  {/each}
</div>

<style>
  .autorow {
    position: absolute;
    inset: 0;
    outline: none;
  }
  .aclip {
    position: absolute;
    top: 6px;
    bottom: 6px;
    border-radius: 6px;
    border: 1px dashed color-mix(in srgb, var(--clip-color) 55%, transparent);
    background:
      linear-gradient(
        to bottom,
        color-mix(in srgb, var(--clip-color) 14%, transparent),
        color-mix(in srgb, var(--clip-color) 5%, transparent)
      ),
      rgba(10, 13, 23, 0.6);
    overflow: hidden;
    cursor: grab;
    touch-action: none;
  }
  .aclip.selected {
    border-style: solid;
    border-color: color-mix(in srgb, var(--clip-color) 85%, white 5%);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--clip-color) 40%, transparent);
  }
  .aclip.edge {
    cursor: ew-resize;
  }
  .curve {
    position: absolute;
    top: 0;
    bottom: 0;
    height: 100%;
    pointer-events: none;
  }
  .del {
    position: absolute;
    top: 2px;
    right: 4px;
    width: 14px;
    height: 14px;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    z-index: 2;
  }
  .del:hover {
    color: var(--red);
  }
</style>
