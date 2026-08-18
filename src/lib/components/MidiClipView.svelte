<script lang="ts">
  /**
   * A MIDI clip on the arrangement timeline. Tick-placed (converted through
   * the tempo map), draggable with beat snapping like audio clips, visually
   * distinct: a mini note-preview matrix instead of a waveform, plus a ▦ MIDI
   * tag. Double-click opens the piano roll.
   */
  import type { MidiClip, TrackState } from "../types/ipc";
  import { midi } from "../state/midi.svelte";
  import { midiIo } from "../state/midiio.svelte";
  import { launch } from "../state/launch.svelte";
  import { project } from "../state/project.svelte";
  import { transport } from "../state/transport.svelte";
  import { prefs } from "../prefs/prefs.svelte";
  import { midiPreviewLayout } from "../utils/midi-preview";
  import { view } from "../state/view.svelte";
  import { clipSelection } from "../state/clip-selection.svelte";
  import { clipDrag } from "../state/clip-drag.svelte";
  import { selectionModeFor } from "../utils/selection-modifiers";
  import { focusAndSelect } from "../utils/focusAndSelect";

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
    if (launch.marking) return;
    // The × button (main's clip-delete) lives INSIDE the clip, so its
    // pointerdown must not select, must not move the anchor and must not
    // open a drag — otherwise deleting one clip silently collapses a
    // multi-clip selection on the way out. Ahead of everything, as in
    // ClipView.
    if ((e.target as HTMLElement).closest("button")) return;
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

  function startRename() {
    draft = clip.name;
    renaming = true;
  }

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

  // ── MIDI output override (right-click) ──
  // No context-menu infra exists on clips today (rename is inline
  // double-click/F2). A single-entry popover — the same toggle+backdrop
  // shape ProjectMenu.svelte uses — opens a small inline port+channel
  // picker, positioned like the rename input. "Inherit from track" clears
  // the override so the clip falls back to its track's routing.
  let menuOpen = $state(false);
  let pickerOpen = $state(false);
  let launcherOpen = $state(false);
  const override = $derived(midiIo.clipOverride(clip.id));
  const usedLauncher = $derived(launch.driveMap(clip.id));

  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    pickerOpen = false;
    launcherOpen = false;
    menuOpen = true;
  }

  function openLauncherPicker() {
    menuOpen = false;
    launcherOpen = true;
  }

  function pickLauncher(mapId: string | null) {
    launcherOpen = false;
    void launch.setClipLauncher(clip.id, mapId);
  }

  function openMidiOutputPicker() {
    menuOpen = false;
    pickerOpen = true;
  }

  async function mapToNote() {
    menuOpen = false;
    launch.panelOpen = true;
    const b = await launch.mapClip(clip.id);
    if (b) launch.focus(b.id);
  }

  function selectOverridePort(portId: string) {
    void midiIo.setClipRoute(clip.id, portId || null, override?.channel ?? 0);
  }

  function selectOverrideChannel(channel: number) {
    if (!override) return;
    void midiIo.setClipRoute(clip.id, override.portId, channel);
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
    } else if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      e.stopPropagation();
      void midi.removeClip(clip.id);
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
    oncontextmenu={onContextMenu}
  >
    <div bind:this={pulseEl} class="pulse"></div>
    <canvas bind:this={canvas} class="mini" style:left="{visL}px" style:width="{Math.max(1, Math.floor(visR - visL))}px"></canvas>
    {#if renaming}
      <input
        use:focusAndSelect
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
    {#if selected}
      <button
        class="use-launch mono"
        type="button"
        title={usedLauncher ? `Using ${usedLauncher.name} — click to change` : "Use a launcher — this clip's notes fire that map"}
        onclick={(e) => {
          e.stopPropagation();
          launcherOpen = !launcherOpen;
          menuOpen = false;
          pickerOpen = false;
        }}>{usedLauncher ? `▶ ${usedLauncher.name}` : "USE"}</button
      >
      <button
        class="del"
        title="Delete clip"
        aria-label="Delete clip {clip.name}"
        onclick={(e) => {
          e.stopPropagation();
          void midi.removeClip(clip.id);
        }}>×</button
      >
    {/if}
    <span class="count silk">{clip.notes.length}n</span>
    {#if override}
      <span class="routedot" title="MIDI output override: {override.portId}, channel {override.channel + 1}">⇥</span>
    {/if}
    {#if usedLauncher}
      <span class="routedot" title="This clip uses launcher {usedLauncher.name}">▶</span>
    {/if}
    {#if menuOpen}
      <div class="clipmenu-backdrop" role="presentation" onpointerdown={() => (menuOpen = false)}></div>
      <div class="clipmenu" style:left="{visL + 4}px" role="menu">
        <button type="button" role="menuitem" onclick={openMidiOutputPicker}>MIDI output…</button>
        <button type="button" role="menuitem" onclick={openLauncherPicker}
          >{usedLauncher ? `Using ${usedLauncher.name}…` : "Use launcher…"}</button
        >
        <button type="button" role="menuitem" onclick={() => void mapToNote()}>Map to MIDI note…</button>
      </div>
    {/if}
    {#if launcherOpen}
      <div class="clipmenu-backdrop" role="presentation" onpointerdown={() => (launcherOpen = false)}></div>
      <div class="clipmenu" style:left="{visL + 4}px" role="menu">
        <button
          type="button"
          role="menuitem"
          class:picked={!usedLauncher}
          onclick={() => pickLauncher(null)}>Don't use launcher</button
        >
        {#each launch.maps as m (m.id)}
          <button
            type="button"
            role="menuitem"
            class:picked={usedLauncher?.id === m.id}
            onclick={() => pickLauncher(m.id)}>{usedLauncher?.id === m.id ? `✓ ${m.name}` : m.name}</button
          >
        {/each}
      </div>
    {/if}
    {#if pickerOpen}
      <div class="clipmenu-backdrop" role="presentation" onpointerdown={() => (pickerOpen = false)}></div>
      <div class="midipicker mono" role="presentation" style:left="{visL + 4}px" onpointerdown={(e) => e.stopPropagation()}>
        <select
          value={override?.portId ?? ""}
          onchange={(e) => selectOverridePort((e.currentTarget as HTMLSelectElement).value)}
        >
          <option value="">Inherit from track</option>
          {#each midiIo.outputs as o (o.port.id)}
            <option value={o.port.id}>{o.port.name}</option>
          {/each}
        </select>
        {#if override}
          <input
            type="number"
            min="0"
            max="15"
            value={override.channel}
            onchange={(e) =>
              selectOverrideChannel(Math.max(0, Math.min(15, (e.currentTarget as HTMLInputElement).valueAsNumber || 0)))}
          />
        {/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .mclip {
    position: absolute;
    top: 6px;
    bottom: 6px;
    border-radius: 6px;
    /* dotted border + tinted body = instantly non-audio */
    border: var(--border-width) dashed color-mix(in srgb, var(--clip-color) 55%, transparent);
    background:
      linear-gradient(
        to bottom,
        color-mix(in srgb, var(--clip-color) 14%, transparent),
        color-mix(in srgb, var(--clip-color) 5%, transparent)
      ),
      rgb(var(--bg-1-rgb) / 0.6);
    overflow: hidden;
    cursor: grab;
    touch-action: none;
    transition: border-color 120ms, box-shadow 120ms;
  }
  .mclip.selected {
    border-style: solid;
    border-color: color-mix(in srgb, var(--clip-color) 85%, var(--text) 5%);
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--clip-color) 40%, transparent),
      0 0 calc(18px * var(--glow-scale)) color-mix(in srgb, var(--clip-color) 25%, transparent);
  }
  .mclip.open {
    box-shadow:
      inset 0 0 0 1px color-mix(in srgb, var(--clip-color) 45%, transparent),
      0 0 calc(14px * var(--glow-scale)) color-mix(in srgb, var(--clip-color) 30%, transparent);
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
        0 0 0 2px color-mix(in srgb, var(--clip-color) 70%, var(--text) 10%),
        0 0 calc(26px * var(--glow-scale)) color-mix(in srgb, var(--clip-color) 60%, transparent);
    }
  }

  .pulse {
    position: absolute;
    inset: 0;
    pointer-events: none;
    opacity: 0;
    background: color-mix(in srgb, var(--clip-color) 22%, transparent);
    box-shadow: inset 0 0 calc(14px * var(--glow-scale)) color-mix(in srgb, var(--clip-color) 45%, transparent);
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
    color: color-mix(in srgb, var(--clip-color) 85%, var(--text) 15%);
    background: rgb(var(--bg-0-rgb) / 0.55);
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
    background: rgb(var(--text-on-accent-rgb) / 0.06);
  }
  .rename {
    top: 2px;
    width: 12ch;
    pointer-events: auto; /* .tag turns them off; the input needs clicking */
    border: var(--border-width) solid var(--clip-color);
    outline: none;
    font: inherit;
    font-size: 9px;
    color: var(--text);
    background: rgb(var(--bg-0-rgb) / 0.92);
  }
  .count {
    position: absolute;
    right: 5px;
    bottom: 3px;
    pointer-events: none;
    color: color-mix(in srgb, var(--clip-color) 60%, var(--text-faint));
  }

  .use-launch {
    position: absolute;
    top: 1px;
    right: 22px;
    height: 12px;
    line-height: 12px;
    border: var(--border-width) solid color-mix(in srgb, var(--clip-color) 40%, transparent);
    background: rgb(var(--bg-0-rgb) / 0.55);
    color: var(--cyan);
    font-size: 8px;
    letter-spacing: 0.08em;
    padding: 0 5px;
    cursor: pointer;
    border-radius: 3px;
    z-index: 1;
    max-width: 46%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .use-launch:hover {
    border-color: var(--cyan);
  }
  .del {
    position: absolute;
    top: 1px;
    right: 4px;
    width: 15px;
    height: 12px;
    line-height: 1;
    border: none;
    background: rgb(var(--bg-0-rgb) / 0.55);
    color: var(--text-faint);
    font-size: 12px;
    cursor: pointer;
    border-radius: 3px;
    z-index: 1;
  }
  .del:hover {
    color: var(--red);
  }

  .routedot {
    position: absolute;
    left: 5px;
    bottom: 3px;
    font-size: 9px;
    color: var(--cyan);
    pointer-events: none;
  }

  .clipmenu-backdrop {
    position: fixed;
    inset: 0;
    z-index: 30;
  }
  .clipmenu {
    position: absolute;
    top: 16px;
    z-index: 31;
    display: flex;
    flex-direction: column;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    background: rgb(var(--bg-1-rgb) / 0.96);
    box-shadow: 0 4px 14px rgb(var(--shadow-rgb) / 0.4);
    overflow: hidden;
  }
  .clipmenu button {
    all: unset;
    padding: 5px 10px;
    font-size: 10px;
    color: var(--text);
    cursor: pointer;
    white-space: nowrap;
  }
  .clipmenu button:hover {
    background: color-mix(in srgb, var(--clip-color) 25%, transparent);
  }
  .clipmenu button.picked {
    color: var(--cyan);
  }
  .midipicker {
    position: absolute;
    top: 16px;
    z-index: 31;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px 6px;
    border: var(--border-width) solid var(--clip-color);
    border-radius: 4px;
    background: rgb(var(--bg-0-rgb) / 0.96);
  }
  .midipicker select,
  .midipicker input {
    font: inherit;
    font-size: 9px;
    color: var(--text);
    background: rgb(var(--bg-0-rgb) / 0.8);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 3px;
    padding: 2px 4px;
  }
  .midipicker input {
    width: 3ch;
  }
</style>
