<script lang="ts">
  /**
   * One clip on the timeline. Pointer-drag with beat snapping (Alt bypasses),
   * a tile-fed waveform canvas that renders only its visible span (WebGPU
   * with Canvas2D fallback), selection, and the "AI Split Stems" magic
   * button + processing scan overlay. Double-click opens the clip's source
   * file in the OS's default external editor (e.g. Audacity), mirroring
   * MidiClipView's double-click-opens-piano-roll.
   */
  import type { Clip, TrackState } from "../types/ipc";
  import { project } from "../state/project.svelte";
  import { view } from "../state/view.svelte";
  import { jobs } from "../state/jobs.svelte";
  import { ui } from "../state/ui.svelte";
  import { clipSelection } from "../state/clip-selection.svelte";
  import { clipDrag } from "../state/clip-drag.svelte";
  import { launch } from "../state/launch.svelte";
  import { toasts } from "../state/toasts.svelte";
  import { backend } from "../tauri";
  import { analyzePitchTrack, type PitchAnalysisState } from "../pitch/analyze";
  import { invalidatePitchCache } from "../state/pitch.svelte";
  import { selectionModeFor } from "../utils/selection-modifiers";
  import { buildPeakColumns } from "../render/tiles";
  import { createPainter, hexToRgba, type WaveformPainter } from "../render/painter";

  let { clip, track }: { clip: Clip; track: TrackState } = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let painter: WaveformPainter | null = null;

  const leftPx = $derived(view.xOf(clip.timelineStartSamples));
  const widthPx = $derived(clip.lengthSamples / view.spp);
  // visible span, clip-local CSS px
  const visL = $derived(Math.max(0, -leftPx));
  const visR = $derived(Math.min(widthPx, view.width - leftPx));
  const visW = $derived(Math.max(0, Math.floor(visR - visL)));
  const selected = $derived(clipSelection.has({ kind: "audio", id: clip.id }));
  const job = $derived(jobs.jobForClip(clip.id));
  let pitchAnalysis = $state<PitchAnalysisState>({ phase: "idle" });
  let analysisGeneration = 0;

  $effect(() => {
    void clip.id;
    return () => {
      // Invalidate in-flight analysis callbacks when this view unmounts or changes clip
      analysisGeneration++;
    };
  });

  async function analyzePitch() {
    if (pitchAnalysis.phase === "analyzing") return;
    const gen = ++analysisGeneration;
    await analyzePitchTrack(clip.id, (id) => backend.pitchAnalyzeClip(id), (state) => {
      if (gen !== analysisGeneration) return;
      pitchAnalysis = state;
      if (state.phase === "done") {
        invalidatePitchCache(clip.id);
        toasts.success("PITCH TRACK READY", `${clip.name}: ${state.frames} analysis frames cached`);
      } else if (state.phase === "error") {
        toasts.error("PITCH ANALYSIS FAILED", state.message);
      }
    });
  }

  // ── waveform painting ──

  $effect(() => {
    if (!canvas) return;
    const el = canvas;
    let alive = true;
    createPainter(el).then((p) => {
      if (!alive) {
        p.destroy();
        return;
      }
      painter = p;
      if (!ui.rendererKind) ui.rendererKind = p.kind === "webgpu" ? "WEBGPU" : "CANVAS2D";
      scheduleDraw();
    });
    return () => {
      alive = false;
      painter?.destroy();
      painter = null;
    };
  });

  let drawToken = 0;
  function scheduleDraw() {
    const token = ++drawToken;
    const spp = view.spp;
    const w = visW;
    const startLocal = visL;
    const color = track.color;
    if (!painter || w <= 0) return;
    const srcStart = clip.offsetSamples + startLocal * spp;
    void buildPeakColumns(clip.id, srcStart, spp, w).then((columns) => {
      if (token !== drawToken || !painter) return;
      const h = el2?.clientHeight ?? 0;
      if (h <= 0) return;
      painter.draw(columns, w, h, Math.min(3, window.devicePixelRatio || 1), hexToRgba(color, 0.95));
    });
  }

  let el2: HTMLDivElement | undefined = $state();

  // redraw when the mapping or clip geometry changes
  $effect(() => {
    // reactive deps:
    void view.spp;
    void view.viewStart;
    void view.width;
    void clip.timelineStartSamples;
    void clip.lengthSamples;
    void track.color;
    scheduleDraw();
  });

  // ── drag ──

  const dragging = $derived(clipDrag.active && selected);

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    if (launch.marking) return;
    if ((e.target as HTMLElement).closest("button")) return;
    const ref = { kind: "audio", id: clip.id } as const;
    const mode = selectionModeFor(e);
    // A plain click on an ALREADY-selected clip must not collapse the
    // selection — that is what makes dragging a group possible at all.
    if (!(mode === "replace" && clipSelection.has(ref))) {
      clipSelection.apply([ref], mode);
    } else {
      clipSelection.anchor = ref;
    }
    // Focused clip (scope ruling F): SPLIT STEMS, the Dock and AI-Studio
    // targeting still act on exactly one clip.
    project.select(clip.id);
    clipDrag.begin(ref, e.clientX);
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic pointers have no active id */
    }
  }

  function onPointerMove(e: PointerEvent) {
    clipDrag.move(e.clientX, e.altKey);
  }

  function onPointerUp(e: PointerEvent) {
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
    void clipDrag.end();
  }

  // ── double-click / context menu ──
  // Double-click opens the plugin manager (effect chain access).
  // Right-click shows a menu with "open in external editor", add effect, etc.
  let menuOpen = $state(false);

  function onDblClick(e: MouseEvent) {
    e.stopPropagation();
    if ((e.target as HTMLElement).closest("button")) return;
    // Focus the plugin manager in the dock — the user brought audio in,
    // the most likely next step is adding an effect.
    ui.dock = "plugins";
  }

  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    menuOpen = true;
  }

  function openExternalEditor() {
    menuOpen = false;
    void project.openInExternalEditor(clip.id);
  }

  function onAddEffect() {
    menuOpen = false;
    ui.dock = "plugins";
  }

  function onKeydown(e: KeyboardEvent) {
    const beat = project.samplesPerBeat;
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      const dir = e.key === "ArrowLeft" ? -1 : 1;
      project.moveClip(clip.id, clip.timelineStartSamples + dir * beat);
      void project.commitClipMove(clip.id);
    } else if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      e.stopPropagation();
      void project.removeClip(clip.id);
    }
  }
</script>

{#if visR > 0 && visL < widthPx}
  <div
    bind:this={el2}
    class="clip"
    class:selected
    class:dragging
    class:processing={!!job}
    style:left="{leftPx}px"
    style:width="{Math.max(6, widthPx)}px"
    style:--clip-color={track.color}
    role="button"
    aria-label="Clip {clip.name} — double-click to manage effects, right-click for more"
    tabindex="0"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={() => clipDrag.cancel()}
    ondblclick={onDblClick}
    oncontextmenu={onContextMenu}
    onkeydown={onKeydown}
  >
    <canvas bind:this={canvas} class="wave" style:left="{visL}px" style:width="{visW}px"></canvas>

    <span class="cname mono" style:left="{visL + 5}px">{clip.name}</span>

    {#if selected}
      <button
        class="del"
        title="Delete clip"
        aria-label="Delete clip {clip.name}"
        onclick={(e) => {
          e.stopPropagation();
          void project.removeClip(clip.id);
        }}>×</button
      >
    {/if}

    {#if selected && !job}
      <button
        class="magic mono"
        title="Separate into Drums / Bass / Vocals / Other stems"
        onclick={() => jobs.splitStems(clip)}
      >
        <span class="spark">✦</span> SPLIT STEMS
      </button>
      <button
        class="pitch-action mono"
        type="button"
        class:error={pitchAnalysis.phase === "error"}
        disabled={pitchAnalysis.phase === "analyzing"}
        title={pitchAnalysis.phase === "error"
          ? `Pitch analysis failed: ${pitchAnalysis.message}. Click to retry.`
          : "Analyse this audio clip and persist its APTF pitch track"}
        aria-live="polite"
        onclick={(e) => {
          e.stopPropagation();
          void analyzePitch();
        }}
      >
        {#if pitchAnalysis.phase === "analyzing"}
          ANALYSING PITCH…
        {:else if pitchAnalysis.phase === "done"}
          ✓ PITCH READY · {pitchAnalysis.frames}
        {:else if pitchAnalysis.phase === "error"}
          RETRY PITCH
        {:else}
          ANALYSE PITCH
        {/if}
      </button>
    {/if}

    {#if job}
      <div class="scanbox" aria-live="polite">
        <div class="scan"></div>
        <div class="scan lag"></div>
        <div class="jobinfo" style:left="{Math.max(visL + 4, visR - 150)}px">
          <span class="stage silk">{job.stage}</span>
          <span class="pct mono">{Math.round(job.progress * 100)}%</span>
          <button class="cancel mono" title="Cancel job" onclick={() => jobs.cancel(job.jobId)}>×</button>
        </div>
        <div class="pbar">
          <div class="pfill" style:width="{job.progress * 100}%"></div>
        </div>
      </div>
    {/if}
    {#if menuOpen}
      <div class="clipmenu-backdrop" role="presentation" onpointerdown={() => (menuOpen = false)}></div>
      <div class="clipmenu" role="menu" style:left="{Math.max(visL + 4, 4)}px">
        <button type="button" role="menuitem" onclick={openExternalEditor}>
          Open in external editor
        </button>
        <button type="button" role="menuitem" onclick={onAddEffect}>
          Add effect to track…
        </button>
      </div>
    {/if}
  </div>
{/if}

<style>
  .clip {
    position: absolute;
    top: 6px;
    bottom: 6px;
    border-radius: 6px;
    border: var(--border-width) solid color-mix(in srgb, var(--clip-color) 35%, transparent);
    background: color-mix(in srgb, var(--clip-color) 8%, rgb(var(--bg-1-rgb) / 0.55));
    overflow: hidden;
    cursor: grab;
    touch-action: none;
    transition: border-color 120ms, box-shadow 120ms;
  }
  .clip.dragging {
    cursor: grabbing;
  }
  .clip.selected {
    border-color: color-mix(in srgb, var(--clip-color) 85%, var(--text) 5%);
    box-shadow:
      0 0 0 1px color-mix(in srgb, var(--clip-color) 40%, transparent),
      0 0 calc(18px * var(--glow-scale)) color-mix(in srgb, var(--clip-color) 25%, transparent);
  }

  .wave {
    position: absolute;
    top: 0;
    height: 100%;
    pointer-events: none;
  }

  .cname {
    position: absolute;
    top: 3px;
    font-size: 9px;
    letter-spacing: 0.06em;
    color: color-mix(in srgb, var(--clip-color) 80%, var(--text) 20%);
    background: rgb(var(--bg-0-rgb) / 0.55);
    padding: 1px 5px;
    border-radius: 3px;
    pointer-events: none;
    white-space: nowrap;
  }

  .del {
    position: absolute;
    bottom: 4px;
    right: 5px;
    width: 15px;
    height: 15px;
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

  .magic {
    position: absolute;
    top: 4px;
    right: 5px;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 4px 9px;
    border-radius: 4px;
    cursor: pointer;
    color: var(--bg-0);
    border: none;
    background: linear-gradient(100deg, var(--cyan), var(--magenta));
    box-shadow:
      0 0 calc(14px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.35),
      0 0 calc(22px * var(--glow-scale)) rgb(var(--magenta-rgb) / 0.25);
    transition: filter 120ms, transform 120ms;
  }
  .magic:hover {
    filter: brightness(1.15);
    transform: translateY(-1px);
  }
  .pitch-action {
    position: absolute;
    left: 5px;
    bottom: 4px;
    z-index: 1;
    padding: 3px 7px;
    border-radius: 4px;
    border: var(--border-width) solid color-mix(in srgb, var(--cyan) 55%, var(--glass-border));
    background: rgb(var(--bg-0-rgb) / 0.78);
    color: var(--cyan);
    font-size: 8px;
    letter-spacing: 0.1em;
    cursor: pointer;
  }
  .pitch-action:hover:not(:disabled) {
    border-color: var(--cyan);
    filter: brightness(1.15);
  }
  .pitch-action:disabled {
    cursor: wait;
    color: var(--text-faint);
  }
  .pitch-action.error {
    border-color: var(--red);
    color: var(--red);
  }
  .spark {
    font-size: 10px;
  }

  /* ── processing scan — the signature ── */

  .clip.processing {
    border-color: color-mix(in srgb, var(--cyan) 60%, var(--magenta) 40%);
    animation: proc-border 1.6s ease-in-out infinite;
  }
  @keyframes proc-border {
    0%,
    100% {
      box-shadow: 0 0 calc(10px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.25);
    }
    50% {
      box-shadow: 0 0 calc(18px * var(--glow-scale)) rgb(var(--magenta-rgb) / 0.35);
    }
  }

  .scanbox {
    position: absolute;
    inset: 0;
    background: rgb(var(--bg-0-rgb) / 0.35);
  }
  .scan {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 34%;
    background: linear-gradient(
      90deg,
      transparent,
      rgb(var(--cyan-rgb) / 0.18) 35%,
      rgb(var(--cyan-rgb) / 0.45) 50%,
      rgb(var(--magenta-rgb) / 0.3) 65%,
      transparent
    );
    mix-blend-mode: screen;
    animation: scan-sweep 1.7s linear infinite;
  }
  .scan.lag {
    animation-delay: -0.85s;
    opacity: 0.5;
    filter: hue-rotate(80deg);
  }
  @keyframes scan-sweep {
    from {
      transform: translateX(-110%);
      left: 0;
    }
    to {
      transform: translateX(110%);
      left: 66%;
    }
  }

  .jobinfo {
    position: absolute;
    top: 4px;
    width: max-content;
    display: flex;
    align-items: center;
    gap: 7px;
    background: rgb(var(--bg-0-rgb) / 0.7);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    padding: 2px 7px;
  }
  .stage {
    color: var(--cyan);
  }
  .pct {
    font-size: 10px;
    color: var(--text);
  }
  .cancel {
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 12px;
    cursor: pointer;
    padding: 0 2px;
  }
  .cancel:hover {
    color: var(--red);
  }

  .pbar {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 3px;
    background: rgb(var(--line-rgb) / 0.15);
  }
  .pfill {
    height: 100%;
    background: linear-gradient(90deg, var(--cyan), var(--magenta));
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.5);
    transition: width 180ms linear;
  }

  /* ── context menu ── */

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
</style>
