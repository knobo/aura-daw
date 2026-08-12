<script lang="ts">
  /**
   * A MIDI clip on the arrangement timeline. Tick-placed (converted through
   * the tempo map), draggable with beat snapping like audio clips, visually
   * distinct: a mini note-preview matrix instead of a waveform, plus a ▦ MIDI
   * tag. Double-click opens the piano roll.
   */
  import type { MidiClip, TrackState } from "../types/ipc";
  import { midi } from "../state/midi.svelte";
  import { project } from "../state/project.svelte";
  import { transport } from "../state/transport.svelte";
  import { ui } from "../state/ui.svelte";
  import { view } from "../state/view.svelte";

  let { clip, track }: { clip: MidiClip; track: TrackState } = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let pulseEl: HTMLDivElement | undefined = $state();

  // ── note-trigger pulse — rAF-driven opacity, no reactivity in the loop ──
  // The clip body glows briefly whenever the playhead crosses a note onset
  // (same interpolated clock as the playhead; toggled by ui.noteFlash).
  $effect(() => {
    const el = pulseEl;
    const enabled = ui.noteFlash && transport.isPlaying;
    if (!el) return;
    if (!enabled) {
      el.style.opacity = "0";
      return;
    }
    let raf = 0;
    const loop = (now: number) => {
      raf = requestAnimationFrame(loop);
      const posTicks =
        midi.samplesToTicks(transport.positionAt(now)) - clip.timelineStartTicks;
      if (posTicks < 0 || posTicks > clip.lengthTicks || clip.notes.length === 0) {
        el.style.opacity = "0";
        return;
      }
      // latest onset at/before the playhead (notes are tick-sorted)
      let last = -1;
      for (const n of clip.notes) {
        if (n.tick > posTicks) break;
        if (n.tick > last) last = n.tick;
      }
      if (last < 0) {
        el.style.opacity = "0";
        return;
      }
      const ageSec = ((posTicks - last) / midi.ppq) * (60 / project.tempoBpm);
      const glow = Math.exp(-ageSec * 9);
      el.style.opacity = glow > 0.02 ? (0.6 * glow).toFixed(3) : "0";
    };
    raf = requestAnimationFrame(loop);
    return () => {
      cancelAnimationFrame(raf);
      el.style.opacity = "0";
    };
  });

  const startSamples = $derived(midi.ticksToSamples(clip.timelineStartTicks));
  const lengthSamples = $derived(
    midi.ticksToSamples(clip.timelineStartTicks + clip.lengthTicks) - startSamples,
  );
  const leftPx = $derived(view.xOf(startSamples));
  const widthPx = $derived(lengthSamples / view.spp);
  const visL = $derived(Math.max(0, -leftPx));
  const visR = $derived(Math.min(widthPx, view.width - leftPx));
  const selected = $derived(midi.selectedClipId === clip.id);
  const open = $derived(midi.openClipId === clip.id);
  // freshly landed (hum-to-song etc.) — transient glow set by midi.flash()
  const landed = $derived(midi.flashClipId === clip.id);

  // ── mini note preview ──
  $effect(() => {
    const c = canvas;
    if (!c) return;
    const notes = clip.notes;
    const w = Math.max(1, Math.floor(visR - visL));
    void view.spp, view.viewStart, clip.timelineStartTicks, clip.lengthTicks, track.color;

    const h = c.clientHeight || 60;
    const dpr = Math.min(3, window.devicePixelRatio || 1);
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.max(1, Math.round(h * dpr));
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    if (notes.length === 0) return;

    let lo = 127;
    let hi = 0;
    for (const n of notes) {
      if (n.key < lo) lo = n.key;
      if (n.key > hi) hi = n.key;
    }
    lo -= 2;
    hi += 2;
    const rowH = Math.min(4, h / (hi - lo + 1));
    const pxPerTick = widthPx / clip.lengthTicks;
    const offset = visL; // clip-local css px of canvas origin

    const r = parseInt(track.color.slice(1, 3), 16);
    const g = parseInt(track.color.slice(3, 5), 16);
    const b = parseInt(track.color.slice(5, 7), 16);
    for (const n of notes) {
      const x = n.tick * pxPerTick - offset;
      const nw = Math.max(1.5, n.lengthTicks * pxPerTick);
      if (x + nw < 0 || x > w) continue;
      const y = h - ((n.key - lo + 1) / (hi - lo + 1)) * h;
      ctx.fillStyle = `rgba(${r},${g},${b},${0.4 + 0.6 * (n.velocity / 127)})`;
      ctx.fillRect(x, y, nw, Math.max(1.5, rowH - 1));
    }
  });

  // ── drag / select (mirrors ClipView) ──
  let dragging = $state(false);
  let dragStartX = 0;
  let dragOrigTicks = 0;
  let dragMoved = false;

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    midi.select(clip.id);
    project.select(null);
    dragging = true;
    dragMoved = false;
    dragStartX = e.clientX;
    dragOrigTicks = clip.timelineStartTicks;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    const dx = e.clientX - dragStartX;
    if (Math.abs(dx) > 2) dragMoved = true;
    if (!dragMoved) return;
    let targetSamples = midi.ticksToSamples(dragOrigTicks) + dx * view.spp;
    if (!e.altKey) targetSamples = view.snapSamples(targetSamples);
    midi.moveClip(clip.id, midi.samplesToTicks(Math.max(0, targetSamples)));
  }
  function onPointerUp(e: PointerEvent) {
    dragging = false;
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      midi.open(clip.id);
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const dir = e.key === "ArrowLeft" ? -1 : 1;
      midi.moveClip(clip.id, clip.timelineStartTicks + dir * midi.ppq);
    }
  }
</script>

{#if visR > 0 && visL < widthPx}
  <div
    class="mclip"
    class:selected
    class:open
    class:landed
    style:left="{leftPx}px"
    style:width="{Math.max(6, widthPx)}px"
    style:--clip-color={track.color}
    role="button"
    aria-label="MIDI clip {clip.name} — double-click to edit"
    tabindex="0"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    ondblclick={() => midi.open(clip.id)}
    onkeydown={onKeydown}
  >
    <div bind:this={pulseEl} class="pulse"></div>
    <canvas bind:this={canvas} class="mini" style:left="{visL}px" style:width="{Math.max(1, Math.floor(visR - visL))}px"></canvas>
    <span class="tag mono" style:left="{visL + 4}px">▦ {clip.name}</span>
    <span class="count silk">{clip.notes.length}n</span>
  </div>
{/if}

<style>
  .mclip {
    position: absolute;
    top: 6px;
    bottom: 6px;
    border-radius: 6px;
    /* dotted border + tinted body = instantly non-audio */
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
    transition: border-color 120ms, box-shadow 120ms;
  }
  .mclip.selected {
    border-style: solid;
    border-color: color-mix(in srgb, var(--clip-color) 85%, white 5%);
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--clip-color) 40%, transparent),
      0 0 18px color-mix(in srgb, var(--clip-color) 25%, transparent);
  }
  .mclip.open {
    box-shadow:
      inset 0 0 0 1px color-mix(in srgb, var(--clip-color) 45%, transparent),
      0 0 14px color-mix(in srgb, var(--clip-color) 30%, transparent);
  }
  /* a clip a job just landed (hum-to-song melody/accompaniment) */
  .mclip.landed {
    animation: clip-landed 1.1s ease-in-out 3;
  }
  @keyframes clip-landed {
    0%,
    100% {
      box-shadow: 0 0 0 1px color-mix(in srgb, var(--clip-color) 40%, transparent);
    }
    50% {
      border-color: var(--clip-color);
      box-shadow:
        0 0 0 2px color-mix(in srgb, var(--clip-color) 70%, white 10%),
        0 0 26px color-mix(in srgb, var(--clip-color) 60%, transparent);
    }
  }

  .pulse {
    position: absolute;
    inset: 0;
    pointer-events: none;
    opacity: 0;
    background: color-mix(in srgb, var(--clip-color) 22%, transparent);
    box-shadow: inset 0 0 14px color-mix(in srgb, var(--clip-color) 45%, transparent);
    will-change: opacity;
  }

  .mini {
    position: absolute;
    top: 14px;
    bottom: 2px;
    height: calc(100% - 16px);
    pointer-events: none;
    color: var(--clip-color);
  }

  .tag {
    position: absolute;
    top: 3px;
    font-size: 9px;
    letter-spacing: 0.06em;
    color: color-mix(in srgb, var(--clip-color) 85%, white 15%);
    background: rgba(5, 7, 13, 0.55);
    padding: 1px 5px;
    border-radius: 3px;
    pointer-events: none;
    white-space: nowrap;
  }
  .count {
    position: absolute;
    right: 5px;
    bottom: 3px;
    pointer-events: none;
    color: color-mix(in srgb, var(--clip-color) 60%, var(--text-faint));
  }
</style>
