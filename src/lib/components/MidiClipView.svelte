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
  import { prefs } from "../prefs/prefs.svelte";
  import { midiPreviewLayout } from "../utils/midi-preview";
  import { view } from "../state/view.svelte";
  import { clipSelection } from "../state/clip-selection.svelte";
  import { clipDrag } from "../state/clip-drag.svelte";
  import { selectionModeFor } from "../utils/selection-modifiers";

  let { clip, track }: { clip: MidiClip; track: TrackState } = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let pulseEl: HTMLDivElement | undefined = $state();

  // ── note-trigger pulse — rAF-driven opacity, no reactivity in the loop ──
  // The clip body glows briefly whenever the playhead crosses a note onset
  // (same interpolated clock as the playhead; toggled by the noteFlash pref).
  $effect(() => {
    const el = pulseEl;
    const enabled = prefs.values.noteFlash && transport.isPlaying;
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
      const contentTicks = midi.effectiveContentLengthTicks(clip);
      const posInContent = posTicks % contentTicks;
      if (posTicks < 0 || posTicks > clip.lengthTicks || clip.notes.length === 0) {
        el.style.opacity = "0";
        return;
      }
      // latest onset at/before the playhead within the CURRENT repeat
      // (notes are tick-sorted, content-relative)
      let last = -1;
      for (const n of clip.notes) {
        if (n.tick > posInContent) break;
        if (n.tick > last) last = n.tick;
      }
      if (last < 0) {
        el.style.opacity = "0";
        return;
      }
      const ageSec = ((posInContent - last) / midi.ppq) * (60 / project.tempoBpm);
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
  const selected = $derived(clipSelection.has({ kind: "midi", id: clip.id }));
  /** True while the pointer hovers ANY selected MIDI clip's right edge —
   * so a group resize announces itself on every clip it will affect, not
   * just the one under the pointer. */
  const groupEdgeHover = $derived(clipDrag.edgeHoverActive && clipSelection.count() > 1);
  const open = $derived(midi.openClipId === clip.id);
  // freshly landed (hum-to-song etc.) — transient glow set by midi.flash()
  const landed = $derived(midi.flashClipId === clip.id);

  // ── mini note preview ──
  $effect(() => {
    const c = canvas;
    if (!c) return;
    const notes = clip.notes;
    const w = Math.max(1, Math.floor(visR - visL));
    void view.spp,
      view.viewStart,
      clip.timelineStartTicks,
      clip.lengthTicks,
      clip.contentLengthTicks,
      track.color;

    const h = c.clientHeight || 60;
    const dpr = Math.min(3, window.devicePixelRatio || 1);
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.max(1, Math.round(h * dpr));
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    if (notes.length === 0) return;

    const { rects, separatorXs } = midiPreviewLayout({
      notes,
      lengthTicks: clip.lengthTicks,
      contentLengthTicks: midi.effectiveContentLengthTicks(clip),
      widthPx,
      offsetPx: visL,
      canvasW: w,
      canvasH: h,
    });

    const r = parseInt(track.color.slice(1, 3), 16);
    const g = parseInt(track.color.slice(3, 5), 16);
    const b = parseInt(track.color.slice(5, 7), 16);
    for (const rect of rects) {
      ctx.fillStyle = `rgba(${r},${g},${b},${0.4 + 0.6 * (rect.velocity / 127)})`;
      ctx.fillRect(rect.x, rect.y, rect.w, rect.h);
    }
    // Separator line at every content boundary (skip rep 0's leading edge).
    ctx.fillStyle = `rgba(${r},${g},${b},0.35)`;
    for (const sepX of separatorXs) {
      ctx.fillRect(sepX, 0, 1, h);
    }
  });

  // ── drag / select / edge-resize (mirrors ClipView + the ruler's loop pins) ──
  let dragging = $derived(clipDrag.active && selected);
  let dragMode: "move" | "resize" = $state("move");

  const EDGE_PX = 8;

  let hoverEdge = $state(false);
  function updateHoverEdge(e: PointerEvent) {
    if (dragging) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    hoverEdge = rect.right - e.clientX <= EDGE_PX;
    clipDrag.edgeHoverActive = hoverEdge;
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const ref = { kind: "midi", id: clip.id } as const;
    const mode = selectionModeFor(e);
    if (!(mode === "replace" && clipSelection.has(ref))) {
      clipSelection.apply([ref], mode);
    } else {
      clipSelection.anchor = ref;
    }
    midi.select(clip.id);
    project.select(null);
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const nearRightEdge = rect.right - e.clientX <= EDGE_PX;
    dragMode = nearRightEdge ? "resize" : "move";
    // Which half of the clip the gesture started in, decided by geometry
    // rather than by e.target: the capture below retargets the following
    // click/dblclick to this element, and the name tag is a 9px-tall hit
    // target nobody can reliably hit anyway.
    downOnHeader = e.clientY - rect.top <= HEADER_PX;
    clipDrag.begin(ref, e.clientX, dragMode);
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
  }
  function onPointerMove(e: PointerEvent) {
    updateHoverEdge(e);
    clipDrag.move(e.clientX, e.altKey);
  }
  function onPointerUp(e: PointerEvent) {
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
    void clipDrag.end();
  }
  // ── rename ──
  // The clip body's double-click is already taken (piano roll), so the name
  // strip along the top gets it instead — the band above the mini preview,
  // which starts at 14px. Plus F2, the DAW-conventional key.
  const HEADER_PX = 14;
  let renaming = $state(false);
  let draft = $state("");
  let downOnHeader = false;
  let inputEl: HTMLInputElement | undefined = $state();

  function startRename() {
    draft = clip.name;
    renaming = true;
  }

  // The input is inserted after this flips, so focus it once it exists —
  // `autofocus` is unreliable for an element that appears mid-session.
  $effect(() => {
    if (renaming && inputEl) {
      inputEl.focus();
      inputEl.select();
    }
  });

  async function commitRename() {
    if (!renaming) return;
    renaming = false;
    await midi.renameClip(clip.id, draft);
  }

  function onRenameKeydown(e: KeyboardEvent) {
    e.stopPropagation(); // the clip's own handler moves/opens the clip
    if (e.key === "Enter") {
      e.preventDefault();
      void commitRename();
    } else if (e.key === "Escape") {
      e.preventDefault();
      renaming = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "F2") {
      e.preventDefault();
      startRename();
    } else if (e.key === "Enter") {
      midi.open(clip.id);
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const dir = e.key === "ArrowLeft" ? -1 : 1;
      midi.moveClip(clip.id, clip.timelineStartTicks + dir * midi.ppq);
      void midi.commitBounds(clip.id);
    }
  }
</script>

{#if visR > 0 && visL < widthPx}
  <div
    class="mclip"
    class:selected
    class:open
    class:landed
    class:edge={hoverEdge || (dragging && dragMode === "resize") || (selected && groupEdgeHover)}
    style:left="{leftPx}px"
    style:width="{Math.max(6, widthPx)}px"
    style:--clip-color={track.color}
    role="button"
    aria-label="MIDI clip {clip.name} — double-click to edit, F2 to rename, drag the right edge to loop"
    tabindex="0"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={() => clipDrag.cancel()}
    onpointerleave={() => {
      hoverEdge = false;
      clipDrag.edgeHoverActive = false;
    }}
    ondblclick={() => (downOnHeader ? startRename() : midi.open(clip.id))}
    onkeydown={onKeydown}
  >
    <div bind:this={pulseEl} class="pulse"></div>
    <canvas bind:this={canvas} class="mini" style:left="{visL}px" style:width="{Math.max(1, Math.floor(visR - visL))}px"></canvas>
    {#if renaming}
      <input
        bind:this={inputEl}
        class="tag rename mono"
        style:left="{visL + 4}px"
        value={draft}
        oninput={(e) => (draft = e.currentTarget.value)}
        onpointerdown={(e) => e.stopPropagation()}
        onkeydown={onRenameKeydown}
        onblur={() => void commitRename()}
      />
    {:else}
      <!-- The strip is the affordance: it makes the whole name band show a
           text cursor, so the rename target is the width of the clip rather
           than the width of the label. Hit-testing is geometric (see
           onPointerDown) — this element only carries the cursor. -->
      <div class="namestrip" title="Double-click to rename"></div>
      <span class="tag mono" style:left="{visL + 4}px">▦ {clip.name}</span>
    {/if}
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
  /* right-edge drag-to-loop zone — same idiom as the ruler's loop pins */
  .mclip.edge {
    cursor: ew-resize;
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
  .namestrip {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 14px; /* HEADER_PX */
    cursor: text;
  }
  .namestrip:hover {
    background: rgba(255, 255, 255, 0.06);
  }
  .rename {
    top: 2px;
    width: 12ch;
    pointer-events: auto; /* .tag turns them off; the input needs clicking */
    border: 1px solid var(--clip-color);
    outline: none;
    font: inherit;
    font-size: 9px;
    color: var(--text);
    background: rgba(5, 7, 13, 0.92);
  }
  .count {
    position: absolute;
    right: 5px;
    bottom: 3px;
    pointer-events: none;
    color: color-mix(in srgb, var(--clip-color) 60%, var(--text-faint));
  }
</style>
