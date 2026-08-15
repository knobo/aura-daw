<script lang="ts">
  /**
   * The arrangement view: bar/beat ruler (click/drag to seek), grid canvas,
   * one lane per track with draggable clips, and a rAF playhead interpolated
   * between transport snapshots (120 FPS-capable — it never touches Svelte
   * reactivity inside the loop).
   */
  import { backend } from "../tauri";
  import { project } from "../state/project.svelte";
  import { transport } from "../state/transport.svelte";
  import { view } from "../state/view.svelte";
  import { midi } from "../state/midi.svelte";
  import { automation } from "../state/automation.svelte";
  import { loopjam } from "../state/loopjam.svelte";
  import { toasts } from "../state/toasts.svelte";
  import { clipSelection } from "../state/clip-selection.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import { applySelection, marqueeClipHits } from "../utils/clip-selection";
  import { selectionModeFor } from "../utils/selection-modifiers";
  import { buildLaneBoxes, laneIndexAt, TRACK_HEIGHT_PX } from "../utils/lane-geometry";
  import { decodeLibraryDrag, hasLibraryDrag } from "../utils/library";
  import { library } from "../state/library.svelte";
  import TrackHeader from "./TrackHeader.svelte";
  import HScrollbar from "./HScrollbar.svelte";
  import ClipView from "./ClipView.svelte";
  import MidiClipView from "./MidiClipView.svelte";
  import AutomationLaneView from "./AutomationLaneView.svelte";
  import ImportDropZone from "./ImportDropZone.svelte";
  import LoopJamPanel from "./loopjam/LoopJamPanel.svelte";
  import type { TrackState } from "../types/ipc";

  const TRACK_PALETTE = ["#52e5ff", "#ff4fd8", "#ffc857", "#9d7bff", "#5cf2b8", "#ff8b5c"];

  let rulerCanvas: HTMLCanvasElement | undefined = $state();
  let gridCanvas: HTMLCanvasElement | undefined = $state();
  let lanesEl: HTMLDivElement | undefined = $state();
  let playheadEl: HTMLDivElement | undefined = $state();
  let markerEl: HTMLDivElement | undefined = $state();

  /** Track id currently under a library drag ("" = none). */
  let dropTrackId = $state("");

  // keep viewport width fresh
  $effect(() => {
    if (!lanesEl) return;
    const el = lanesEl;
    const ro = new ResizeObserver(() => {
      view.width = el.clientWidth;
    });
    ro.observe(el);
    view.width = el.clientWidth;
    return () => ro.disconnect();
  });

  /** Bar step so labeled bars stay ≥ ~70 px apart. */
  function barStep(): number {
    const barPx = project.samplesPerBar / view.spp;
    let step = 1;
    while (barPx * step < 70) step *= 2;
    return step;
  }

  // ── ruler ──

  $effect(() => {
    const c = rulerCanvas;
    if (!c) return;
    // deps
    const spp = view.spp;
    const start = view.viewStart;
    const w = view.width;
    void project.tempoBpm;

    const dpr = Math.min(3, window.devicePixelRatio || 1);
    const h = c.clientHeight;
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.max(1, Math.round(h * dpr));
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const bar = project.samplesPerBar;
    const beat = project.samplesPerBeat;
    const step = barStep();
    const beatPx = beat / spp;

    const firstBar = Math.floor(start / bar);
    const lastBar = Math.ceil((start + w * spp) / bar);

    ctx.font = "9px " + getComputedStyle(document.documentElement).getPropertyValue("--font-mono");
    ctx.textBaseline = "middle";

    for (let b = firstBar; b <= lastBar; b++) {
      const x = (b * bar - start) / spp;
      if (b % step === 0) {
        ctx.fillStyle = "rgba(96,130,190,0.4)";
        ctx.fillRect(Math.round(x), h - 14, 1, 14);
        ctx.fillStyle = "rgba(95,108,133,0.9)";
        ctx.fillText(String(b + 1).padStart(3, "0"), x + 4, h - 8);
      } else {
        ctx.fillStyle = "rgba(96,130,190,0.2)";
        ctx.fillRect(Math.round(x), h - 8, 1, 8);
      }
      // beats
      if (beatPx > 26 && b < lastBar) {
        ctx.fillStyle = "rgba(96,130,190,0.14)";
        for (let q = 1; q < project.timeSignature[0]; q++) {
          const bx = x + (q * beat) / spp;
          ctx.fillRect(Math.round(bx), h - 5, 1, 5);
        }
      }
    }
  });

  // ── lane grid ──

  $effect(() => {
    const c = gridCanvas;
    if (!c) return;
    const spp = view.spp;
    const start = view.viewStart;
    const w = view.width;
    const trackCount = project.tracks.length;
    const trackH = TRACK_HEIGHT_PX;
    const h = Math.max(1, trackCount * trackH);

    const dpr = Math.min(3, window.devicePixelRatio || 1);
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.round(h * dpr);
    c.style.height = `${h}px`;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const bar = project.samplesPerBar;
    const beat = project.samplesPerBeat;
    const step = barStep();
    const beatPx = beat / spp;
    const firstBar = Math.floor(start / bar);
    const lastBar = Math.ceil((start + w * spp) / bar);

    for (let b = firstBar; b <= lastBar; b++) {
      const x = Math.round((b * bar - start) / spp);
      ctx.fillStyle = b % step === 0 ? "rgba(96,130,190,0.16)" : "rgba(96,130,190,0.07)";
      ctx.fillRect(x, 0, 1, h);
      if (beatPx > 26 && b < lastBar) {
        ctx.fillStyle = "rgba(96,130,190,0.045)";
        for (let q = 1; q < project.timeSignature[0]; q++) {
          ctx.fillRect(Math.round(x + (q * beat) / spp), 0, 1, h);
        }
      }
    }
  });

  // ── playhead (rAF, reactivity-free hot path) ──

  $effect(() => {
    let raf = 0;
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick);
      const pos = transport.positionAt(now);
      let x = (pos - view.viewStart) / view.spp;

      // auto-follow: page-scroll when the playhead leaves the right edge
      if (transport.isPlaying && view.follow) {
        if (x > view.width * 0.92 || x < -4) {
          view.scrollToSamples(pos - view.width * 0.08 * view.spp);
          x = (pos - view.viewStart) / view.spp;
        }
      }

      const visible = x >= -1 && x <= view.width + 1;
      if (playheadEl) {
        playheadEl.style.transform = `translateX(${x.toFixed(2)}px)`;
        playheadEl.style.opacity = visible ? "1" : "0";
      }
      if (markerEl) {
        markerEl.style.transform = `translateX(${x.toFixed(2)}px)`;
        markerEl.style.opacity = visible ? "1" : "0";
      }
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  // ── interaction ──

  function onWheel(e: WheelEvent) {
    if (e.ctrlKey || e.metaKey) {
      e.preventDefault();
      const el = lanesEl ?? (e.currentTarget as HTMLElement);
      view.zoomAt(canvasPos(el, e.clientX, e.clientY).x, Math.exp(e.deltaY * 0.002));
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      e.preventDefault();
      view.scrollBy(e.deltaX !== 0 ? e.deltaX : e.deltaY);
    }
  }

  let scrubbing = false;
  function rulerSeek(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    let samples = view.samplesAt(canvasPos(el, e.clientX, e.clientY).x);
    samples = view.snapSamples(Math.max(0, samples));
    void transport.seek(samples);
  }
  function onRulerDown(e: PointerEvent) {
    // Top strip of the ruler = the loop lane: dragging there sets the loop
    // region (snapped, like clip drags). The lower strip keeps seek/scrub.
    const el = e.currentTarget as HTMLElement;
    if (canvasPos(el, e.clientX, e.clientY).y < el.clientHeight * 0.5) {
      const anchor = loopSampleAt(e);
      loopGesture = { kind: "create", anchor };
      loopDraft = { start: anchor, end: anchor };
      try {
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      } catch {
        /* synthetic pointer */
      }
      return;
    }
    scrubbing = true;
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic pointer */
    }
    rulerSeek(e);
  }
  function onRulerMove(e: PointerEvent) {
    if (loopGesture?.kind === "create") {
      const s = loopSampleAt(e);
      loopDraft = {
        start: Math.min(loopGesture.anchor, s),
        end: Math.max(loopGesture.anchor, s),
      };
      return;
    }
    if (scrubbing) rulerSeek(e);
  }
  function onRulerUp(e: PointerEvent) {
    if (loopGesture?.kind === "create") {
      commitLoopDraft(true);
    }
    scrubbing = false;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
  }

  // ── loop region (pins + band in the ruler's loop lane) ──

  let rulerEl: HTMLDivElement | undefined = $state();
  let loopDraft: { start: number; end: number } | null = $state(null);
  let loopGesture:
    | { kind: "create"; anchor: number }
    | { kind: "pin"; which: "start" | "end" }
    | null = null;

  const loopRegion = $derived.by(() => {
    const t = transport.snap;
    const start = loopDraft ? loopDraft.start : t.loopStartSamples;
    const end = loopDraft ? loopDraft.end : t.loopEndSamples;
    return { start, end, has: end > start, on: t.loopEnabled };
  });

  // loop-jam visual language: the band breathes while a generation is in
  // flight and flashes when a swap lands at the wrap
  const jamBreathing = $derived(loopjam.busy);
  let jamFlash = $state(false);
  $effect(() => {
    if (loopjam.appliedFlash === 0) return;
    jamFlash = true;
    const t = setTimeout(() => (jamFlash = false), 1400);
    return () => clearTimeout(t);
  });

  /** Snapped sample under the pointer, measured against the ruler. */
  function loopSampleAt(e: PointerEvent): number {
    const el = rulerEl;
    if (!el) return 0;
    return Math.max(0, view.snapSamples(view.samplesAt(canvasPos(el, e.clientX, e.clientY).x)));
  }

  /** Commit the drag draft: a real span sets (and on create enables) the
   *  region; a zero span from a pin drag clears it. */
  function commitLoopDraft(enableOnCommit: boolean) {
    const d = loopDraft;
    loopDraft = null;
    loopGesture = null;
    if (!d) return;
    if (d.end > d.start) {
      void transport.setLoop(enableOnCommit || transport.snap.loopEnabled, d.start, d.end);
    } else if (loopRegion.has) {
      void transport.setLoop(false, 0, 0);
    }
  }

  function onPinDown(which: "start" | "end", e: PointerEvent) {
    e.stopPropagation();
    loopGesture = { kind: "pin", which };
    loopDraft = {
      start: transport.snap.loopStartSamples,
      end: transport.snap.loopEndSamples,
    };
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic pointer */
    }
  }
  function onPinMove(e: PointerEvent) {
    if (loopGesture?.kind !== "pin" || !loopDraft) return;
    const s = loopSampleAt(e);
    const other =
      loopGesture.which === "start" ? loopDraft.end : loopDraft.start;
    loopDraft =
      loopGesture.which === "start"
        ? { start: Math.min(s, other), end: Math.max(s, other) }
        : { start: Math.min(other, s), end: Math.max(other, s) };
  }
  function onPinUp(e: PointerEvent) {
    if (loopGesture?.kind !== "pin") return;
    commitLoopDraft(false);
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
  }

  async function addTrack() {
    const color = TRACK_PALETTE[project.tracks.length % TRACK_PALETTE.length];
    await project.addTrack({ name: `Track ${project.tracks.length + 1}`, color });
  }

  async function addMidiTrack() {
    const color = TRACK_PALETTE[project.tracks.length % TRACK_PALETTE.length];
    await project.addTrack({ name: `MIDI ${project.tracks.length + 1}`, color, kind: "midi" });
  }

  /**
   * Seed the empty session with the built-in demo song (control plane).
   *
   * This takes a visible moment — it binds three synth instruments (Zyn
   * patches when they are installed) and rebuilds the graph — so the button
   * has to say so. A control that looks inert gets hammered, and every extra
   * click is another seed attempt racing the first.
   */
  let seeding = $state(false);

  async function loadDemoSong() {
    if (!backend.seedDemoProject || seeding) return;
    seeding = true;
    try {
      const snap = await backend.seedDemoProject();
      project.applySnapshot(snap);
      midi.applySnapshot(snap);
    } catch (err) {
      // Was console-only: a silent failure is indistinguishable from a
      // slow one, which is exactly the confusion this flow had.
      console.warn("[aura] seed_demo_project failed:", err);
      toasts.error("DEMO SONG FAILED", String(err));
    } finally {
      seeding = false;
    }
  }

  /** End of the arrangement's content: the furthest clip edge (audio or
   *  MIDI), so the scrollbar spans exactly what there is to reach. */
  const contentEndSamples = $derived.by(() => {
    let end = 0;
    for (const c of project.clips) end = Math.max(end, c.timelineStartSamples + c.lengthSamples);
    for (const c of midi.clips)
      end = Math.max(end, midi.ticksToSamples(c.timelineStartTicks + c.lengthTicks));
    return end;
  });

  /** Drop position in samples. Uses canvasPos (NOT clientX - rect.left) so
   * the mapping is correct under interface zoom — the standing rule for all
   * canvas/lane pointer math. */
  function dropSamples(e: DragEvent): number {
    const { x } = canvasPos(e.currentTarget as HTMLElement, e.clientX, e.clientY);
    return Math.max(0, Math.round(view.snapSamples(view.samplesAt(x))));
  }

  function onLaneDragOver(track: TrackState, e: DragEvent) {
    // `types` is all a dragover may read — see hasLibraryDrag's doc.
    if (!hasLibraryDrag(e.dataTransfer)) return; // OS file drags fall through
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    dropTrackId = track.id;
  }

  function onLaneDrop(track: TrackState, e: DragEvent) {
    dropTrackId = "";
    const payload = decodeLibraryDrag(e.dataTransfer);
    if (!payload) return;
    e.preventDefault();
    void library.dropOnTrack(payload, track.id, dropSamples(e));
  }

  /** Double-click an empty span of a midi lane → create a 2-bar clip there. */
  function onLaneDblClick(track: TrackState, e: MouseEvent) {
    if (track.kind !== "midi") {
      toasts.info("NOT A MIDI LANE", `"${track.name}" is an audio track — midi clips need a midi lane`);
      return;
    }
    if ((e.target as HTMLElement).closest(".mclip, .clip")) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const samples = view.snapSamples(Math.max(0, view.samplesAt(e.clientX - rect.left)));
    const startTicks = Math.round(midi.samplesToTicks(samples) / midi.ppq) * midi.ppq;
    void midi
      .addClip(track.id, Math.max(0, startTicks), 2 * midi.ticksPerBar)
      .then((clip) => clip && midi.open(clip.id));
  }

  // ── marquee selection over the lane area ──
  // Starts only on the lane BACKGROUND, so it never competes with a clip
  // drag. Pointer maths go through canvasPos: under interface zoom
  // (prefs.uiZoom) raw clientX/rect maths drift down-right.

  type Marquee = {
    x0: number;
    y0: number;
    x1: number;
    y1: number;
    baseKeys: Set<string>;
    mode: ReturnType<typeof selectionModeFor>;
  };
  let marquee: Marquee | null = $state(null);

  const marqueeRect = $derived.by(() => {
    const m = marquee;
    if (!m) return null;
    return {
      left: Math.min(m.x0, m.x1),
      top: Math.min(m.y0, m.y1),
      width: Math.abs(m.x1 - m.x0),
      height: Math.abs(m.y1 - m.y0),
    };
  });

  function onLanesPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest(".clip, .mclip, button")) return;
    if (!lanesEl) return;
    const p = canvasPos(lanesEl, e.clientX, e.clientY);
    const mode = selectionModeFor(e);
    marquee = { x0: p.x, y0: p.y, x1: p.x, y1: p.y, baseKeys: new Set(clipSelection.keys), mode };
    if (mode === "replace") clipSelection.clear();
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic pointer */
    }
  }

  function onLanesPointerMove(e: PointerEvent) {
    if (!marquee || !lanesEl) return;
    const p = canvasPos(lanesEl, e.clientX, e.clientY);
    marquee = { ...marquee, x1: p.x, y1: p.y };
    const s0 = view.samplesAt(Math.min(marquee.x0, marquee.x1));
    const s1 = view.samplesAt(Math.max(marquee.x0, marquee.x1));
    const n = project.tracks.length;
    const laneLo = laneIndexAt(Math.min(marquee.y0, marquee.y1), n);
    const laneHi = laneIndexAt(Math.max(marquee.y0, marquee.y1), n);
    const boxes = buildLaneBoxes({
      trackIds: project.tracks.map((t) => t.id),
      audioClips: project.clips,
      midiClips: midi.clips,
      ticksToSamples: (t) => midi.ticksToSamples(t),
    });
    const hits = marqueeClipHits(boxes, s0, s1, laneLo, laneHi);
    // Recomputed from the drag's STARTING selection every move, so dragging
    // the band back over a clip un-hits it instead of latching.
    clipSelection.keys = applySelection(marquee.baseKeys, hits, marquee.mode);
  }

  function onLanesPointerUp(e: PointerEvent) {
    marquee = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
  }
</script>

<div class="timeline" onwheel={onWheel}>
  <!-- ruler row -->
  <div class="ruler-row">
    <div class="corner silk">tracks · {project.tracks.length}</div>
    <div
      class="ruler"
      bind:this={rulerEl}
      role="slider"
      aria-label="Timeline position"
      aria-valuenow={Math.round(transport.snap.positionSamples / transport.snap.sampleRate)}
      tabindex="0"
      title="Drag the top strip to set the loop region · lower strip seeks"
      onpointerdown={onRulerDown}
      onpointermove={onRulerMove}
      onpointerup={onRulerUp}
    >
      <canvas bind:this={rulerCanvas} class="ruler-canvas"></canvas>
      {#if loopRegion.has}
        <div
          class="loopband"
          class:on={loopRegion.on}
          class:breathing={jamBreathing}
          class:jamflash={jamFlash}
          style="left:{view.xOf(loopRegion.start).toFixed(1)}px; width:{Math.max(
            2,
            (loopRegion.end - loopRegion.start) / view.spp,
          ).toFixed(1)}px"
        ></div>
        <div
          class="looppin start"
          class:on={loopRegion.on}
          role="slider"
          aria-label="Loop start"
          aria-valuenow={Math.round(loopRegion.start / transport.snap.sampleRate)}
          tabindex="0"
          title="Loop start — drag to move"
          style="left:{view.xOf(loopRegion.start).toFixed(1)}px"
          onpointerdown={(e) => onPinDown("start", e)}
          onpointermove={onPinMove}
          onpointerup={onPinUp}
        ></div>
        <div
          class="looppin end"
          class:on={loopRegion.on}
          role="slider"
          aria-label="Loop end"
          aria-valuenow={Math.round(loopRegion.end / transport.snap.sampleRate)}
          tabindex="0"
          title="Loop end — drag to move"
          style="left:{view.xOf(loopRegion.end).toFixed(1)}px"
          onpointerdown={(e) => onPinDown("end", e)}
          onpointermove={onPinMove}
          onpointerup={onPinUp}
        ></div>
      {/if}
      <div bind:this={markerEl} class="marker"></div>
    </div>
  </div>

  <!-- Own band rather than an overlay inside .lanes: the jam cluster used to
       sit on top of the first lane. Empty (and zero-height) with no loop
       region, and outside .body so the rail and the lanes stay row-aligned. -->
  <div class="jamband">
    <LoopJamPanel />
  </div>

  <!-- scrollable body -->
  <div class="body">
    <div class="rail">
      {#each project.tracks as track, i (track.id)}
        <TrackHeader {track} index={i} />
      {/each}
      <div class="addrow">
        <button class="add mono" onclick={addTrack}>+ AUDIO</button>
        <button class="add midi mono" onclick={addMidiTrack}>+ MIDI</button>
      </div>
    </div>

    <div
      class="lanes"
      id="timeline-lanes"
      bind:this={lanesEl}
      role="presentation"
      onpointerdown={onLanesPointerDown}
      onpointermove={onLanesPointerMove}
      onpointerup={onLanesPointerUp}
      onpointercancel={onLanesPointerUp}
    >
      <canvas bind:this={gridCanvas} class="grid"></canvas>
      {#each project.tracks as track (track.id)}
        <div
          class="lane"
          class:armed={track.armed}
          class:midilane={track.kind === "midi"}
          class:droptarget={dropTrackId === track.id}
          role="presentation"
          ondblclick={(e) => onLaneDblClick(track, e)}
          ondragover={(e) => onLaneDragOver(track, e)}
          ondragleave={() => (dropTrackId = "")}
          ondrop={(e) => onLaneDrop(track, e)}
        >
          {#each project.clipsOf(track.id) as clip (clip.id)}
            <ClipView {clip} {track} />
          {/each}
          {#each midi.clipsOf(track.id) as clip (clip.id)}
            <MidiClipView {clip} {track} />
          {/each}
          {#if automation.isVisible(track.id)}
            <AutomationLaneView {track} />
          {/if}
        </div>
      {/each}
      {#if project.tracks.length === 0}
        <div class="empty silk">
          <div>no tracks — add one to begin</div>
          {#if backend.seedDemoProject}
            <button
              class="seedbtn mono"
              class:seeding
              disabled={seeding}
              aria-busy={seeding}
              onclick={loadDemoSong}
            >
              {#if seeding}<span class="seedscan" aria-hidden="true"></span>{/if}
              <span class="seedlabel"
                >{seeding ? "BUILDING DEMO SONG…" : "✦ LOAD DEMO SONG"}</span
              >
            </button>
            <div class="seedhint">
              {seeding
                ? "binding synths, laying down clips — a moment"
                : "two synth tracks of demo content — press play to hear AURA"}
            </div>
          {/if}
        </div>
      {/if}
      {#if loopRegion.has && loopRegion.on}
        <div
          class="loopshade"
          class:breathing={jamBreathing}
          class:jamflash={jamFlash}
          style="left:{view.xOf(loopRegion.start).toFixed(1)}px; width:{Math.max(
            2,
            (loopRegion.end - loopRegion.start) / view.spp,
          ).toFixed(1)}px"
        ></div>
      {/if}
      <ImportDropZone />
      {#if marqueeRect && marqueeRect.width > 2}
        <div
          class="marquee"
          style:left="{marqueeRect.left}px"
          style:top="{marqueeRect.top}px"
          style:width="{marqueeRect.width}px"
          style:height="{marqueeRect.height}px"
        ></div>
      {/if}
      <div bind:this={playheadEl} class="playhead"></div>
    </div>
  </div>

  <!-- horizontal time scroll -->
  <div class="scrollrow">
    <div class="scrollcorner"></div>
    <HScrollbar
      start={view.viewStart}
      viewSpan={view.width * view.spp}
      contentEnd={contentEndSamples}
      label="Scroll timeline"
      controls="timeline-lanes"
      onscroll={(s) => view.scrollToSamples(s)}
    />
  </div>
</div>

<style>
  .timeline {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-0);
  }

  .ruler-row {
    display: flex;
    height: var(--ruler-height);
    border-bottom: 1px solid var(--glass-border);
    background: rgba(10, 13, 23, 0.85);
    flex: none;
  }
  .corner {
    width: var(--rail-width);
    flex: none;
    display: flex;
    align-items: center;
    padding-left: 13px;
    border-right: 1px solid var(--glass-border);
  }
  .ruler {
    flex: 1;
    position: relative;
    cursor: ew-resize;
    overflow: hidden;
  }
  .ruler-canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  .marker {
    position: absolute;
    top: 0;
    left: 0;
    width: 0;
    height: 0;
    border-left: 5px solid transparent;
    border-right: 5px solid transparent;
    border-top: 7px solid var(--cyan);
    margin-left: -5px;
    filter: drop-shadow(0 0 4px var(--cyan-dim));
    pointer-events: none;
  }

  /* loop region: band + pins in the ruler's top strip */
  .loopband {
    position: absolute;
    top: 0;
    height: 45%;
    background: rgba(122, 160, 220, 0.12);
    border-bottom: 1px solid rgba(122, 160, 220, 0.25);
    pointer-events: none;
  }
  .loopband.on {
    background: rgba(255, 200, 87, 0.14);
    border-bottom: 1px solid rgba(255, 200, 87, 0.55);
    box-shadow: inset 0 0 10px rgba(255, 200, 87, 0.12);
  }
  .looppin {
    position: absolute;
    top: 0;
    width: 9px;
    height: 55%;
    margin-left: -4.5px;
    cursor: ew-resize;
    z-index: 2;
  }
  .looppin::before {
    content: "";
    position: absolute;
    top: 0;
    left: 3.5px;
    width: 2px;
    height: 100%;
    background: rgba(122, 160, 220, 0.6);
  }
  .looppin::after {
    content: "";
    position: absolute;
    top: 0;
    width: 0;
    height: 0;
    border-top: 6px solid rgba(122, 160, 220, 0.85);
  }
  .looppin.start::after {
    left: 4.5px;
    border-right: 6px solid transparent;
  }
  .looppin.end::after {
    right: 4.5px;
    border-left: 6px solid transparent;
  }
  .looppin.on::before {
    background: var(--amber);
    box-shadow: 0 0 5px rgba(255, 200, 87, 0.6);
  }
  .looppin.on::after {
    border-top-color: var(--amber);
  }

  /* faint full-height echo of the active loop region in the lanes */
  .loopshade {
    position: absolute;
    top: 0;
    bottom: 0;
    background: rgba(255, 200, 87, 0.035);
    border-left: 1px solid rgba(255, 200, 87, 0.18);
    border-right: 1px solid rgba(255, 200, 87, 0.18);
    pointer-events: none;
  }

  /* loop-jam: the region breathes while ACE-Step regenerates it… */
  .loopband.breathing {
    animation: jam-breathe 1.6s ease-in-out infinite;
  }
  .loopshade.breathing {
    animation: jam-breathe-shade 1.6s ease-in-out infinite;
  }
  @keyframes jam-breathe {
    50% {
      background: rgba(255, 79, 216, 0.22);
      border-bottom-color: rgba(255, 79, 216, 0.7);
      box-shadow: inset 0 0 16px rgba(255, 79, 216, 0.25);
    }
  }
  @keyframes jam-breathe-shade {
    50% {
      background: rgba(255, 79, 216, 0.06);
      border-left-color: rgba(255, 79, 216, 0.35);
      border-right-color: rgba(255, 79, 216, 0.35);
    }
  }
  /* …and flashes when the evolved audio swaps in at the wrap */
  .loopband.jamflash {
    animation: jam-flash 1.4s ease-out 1;
  }
  .loopshade.jamflash {
    animation: jam-flash-shade 1.4s ease-out 1;
  }
  @keyframes jam-flash {
    0% {
      background: rgba(92, 242, 184, 0.55);
      box-shadow: inset 0 0 20px rgba(92, 242, 184, 0.6);
    }
    100% {
      background: rgba(255, 200, 87, 0.14);
    }
  }
  @keyframes jam-flash-shade {
    0% {
      background: rgba(92, 242, 184, 0.16);
      border-left-color: rgba(92, 242, 184, 0.7);
      border-right-color: rgba(92, 242, 184, 0.7);
    }
    100% {
      background: rgba(255, 200, 87, 0.035);
    }
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    overflow-y: auto;
    overflow-x: hidden;
  }

  .scrollrow {
    flex: none;
    display: flex;
    border-top: 1px solid var(--glass-border);
    background: rgba(10, 13, 23, 0.85);
  }
  .scrollcorner {
    width: var(--rail-width);
    flex: none;
    border-right: 1px solid var(--glass-border);
  }

  .rail {
    width: var(--rail-width);
    flex: none;
    border-right: 1px solid var(--glass-border);
    background: rgba(10, 13, 23, 0.6);
    display: flex;
    flex-direction: column;
  }
  .addrow {
    display: flex;
    gap: 6px;
    margin: 10px;
  }
  .add {
    flex: 1;
    padding: 8px;
    font-size: 9px;
    letter-spacing: 0.2em;
    color: var(--text-faint);
    background: transparent;
    border: 1px dashed rgba(122, 160, 220, 0.2);
    border-radius: 5px;
    cursor: pointer;
    transition: color 120ms, border-color 120ms;
  }
  .add:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .add.midi:hover {
    color: #5cf2b8;
    border-color: rgba(92, 242, 184, 0.4);
  }
  .lane.midilane {
    background:
      repeating-linear-gradient(
        to bottom,
        transparent 0 13px,
        rgba(96, 130, 190, 0.03) 13px 14px
      );
  }

  .jamband {
    position: relative;
    flex: none;
    /* content box starts at the lane origin, so the panel's `left` stays in
       the same coordinates it used when it lived inside .lanes */
    padding-left: var(--rail-width);
  }

  .lanes {
    flex: 1;
    min-width: 0;
    position: relative;
    overflow: hidden;
  }
  .grid {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    pointer-events: none;
  }
  .lane {
    position: relative;
    height: var(--track-height);
    border-bottom: 1px solid rgba(96, 130, 190, 0.08);
  }
  .lane.armed {
    background: rgba(255, 65, 82, 0.03);
  }
  .lane.droptarget {
    box-shadow: inset 0 0 0 1px var(--cyan);
    background: rgba(82, 229, 255, 0.05);
  }

  .empty {
    padding: 40px;
    text-align: center;
  }
  .seedbtn {
    margin-top: 14px;
    font-size: 10px;
    letter-spacing: 0.18em;
    padding: 7px 14px;
    border-radius: 5px;
    border: 1px solid var(--cyan-dim);
    background: linear-gradient(
      100deg,
      rgba(82, 229, 255, 0.08),
      rgba(255, 79, 216, 0.08)
    );
    color: var(--cyan);
    cursor: pointer;
  }
  .seedbtn:hover:not(:disabled) {
    box-shadow: 0 0 12px rgba(82, 229, 255, 0.25);
  }
  /* Busy: the same scanning cyan→magenta sweep clips wear while a job works
     on them, so "something is happening" reads the same everywhere. */
  .seedbtn.seeding {
    position: relative;
    overflow: hidden;
    cursor: progress;
    border-color: color-mix(in srgb, var(--cyan) 60%, var(--magenta) 40%);
    animation: seed-glow 1.6s ease-in-out infinite;
  }
  .seedbtn:disabled {
    /* Busy is not "unavailable": keep it legible rather than greyed out. */
    opacity: 1;
  }
  .seedlabel {
    position: relative;
    z-index: 1;
  }
  .seedscan {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 40%;
    background: linear-gradient(
      90deg,
      transparent,
      rgba(82, 229, 255, 0.35) 45%,
      rgba(255, 79, 216, 0.3) 65%,
      transparent
    );
    mix-blend-mode: screen;
    animation: seed-sweep 1.4s linear infinite;
  }
  @keyframes seed-glow {
    0%,
    100% {
      box-shadow: 0 0 10px rgba(82, 229, 255, 0.25);
    }
    50% {
      box-shadow: 0 0 18px rgba(255, 79, 216, 0.35);
    }
  }
  @keyframes seed-sweep {
    from {
      transform: translateX(-110%);
      left: 0;
    }
    to {
      transform: translateX(110%);
      left: 100%;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .seedbtn.seeding,
    .seedscan {
      animation: none;
    }
  }
  .seedhint {
    margin-top: 8px;
    font-size: 10px;
    color: var(--text-faint);
  }

  .marquee {
    position: absolute;
    background: rgba(82, 229, 255, 0.08);
    border: 1px solid var(--cyan-dim);
    pointer-events: none;
    z-index: 3;
  }

  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    width: 1px;
    background: var(--cyan);
    box-shadow:
      0 0 6px var(--cyan-dim),
      0 0 1px var(--cyan);
    pointer-events: none;
    will-change: transform;
  }
</style>
