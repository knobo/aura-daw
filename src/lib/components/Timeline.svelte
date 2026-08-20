<script lang="ts">
  /**
   * The arrangement view: bar/beat ruler (click/drag to seek), grid canvas,
   * one lane per track with draggable clips, and a rAF playhead interpolated
   * between transport snapshots (120 FPS-capable — it never touches Svelte
   * reactivity inside the loop).
   */
  import { untrack } from "svelte";
  import { backend } from "../tauri";
  import { project } from "../state/project.svelte";
  import { transport } from "../state/transport.svelte";
  import { view } from "../state/view.svelte";
  import { midi } from "../state/midi.svelte";
  import { modulation } from "../state/modulation.svelte";
  import { loopjam } from "../state/loopjam.svelte";
  import { toasts } from "../state/toasts.svelte";
  import { clipSelection } from "../state/clip-selection.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import {
    applySelection,
    marqueeClipHits,
    movedPastMarqueeSlop,
    startsMarquee,
  } from "../utils/clip-selection";
  import { selectionModeFor } from "../utils/selection-modifiers";
  import { buildLaneBoxes } from "../utils/lane-geometry";
  import { bandForTrackRange, buildLaneLayout, nearestTrackIndexAtY } from "../utils/lane-layout";
  import { arrangementForMove, dropTargetAtY } from "../utils/lane-arrange";
  import { lanes } from "../state/lanes.svelte";
  import { decodeLibraryDrag, hasLibraryDrag } from "../utils/library";
  import { library } from "../state/library.svelte";
  import TrackHeader from "./TrackHeader.svelte";
  import LaneGroupHeader from "./LaneGroupHeader.svelte";
  import HScrollbar from "./HScrollbar.svelte";
  import ClipView from "./ClipView.svelte";
  import MidiClipView from "./MidiClipView.svelte";
  import ModulationLaneView from "./ModulationLaneView.svelte";
  import AutomationTrackRow from "./AutomationTrackRow.svelte";
  import ImportDropZone from "./ImportDropZone.svelte";
  import LoopJamPanel from "./loopjam/LoopJamPanel.svelte";
  import { launch } from "../state/launch.svelte";
  import { overlayBox, regionFromMarquee, resizeRegion, shiftRegion } from "../utils/launch-map";
  import type { TrackState } from "../types/ipc";
  import { theme } from "../theme/theme.svelte";
  import { alpha } from "../theme/tokens";

  let rulerCanvas: HTMLCanvasElement | undefined = $state();
  let gridCanvas: HTMLCanvasElement | undefined = $state();
  let lanesEl: HTMLDivElement | undefined = $state();
  let playheadEl: HTMLDivElement | undefined = $state();
  let markerEl: HTMLDivElement | undefined = $state();

  /** Track id currently under a library drag ("" = none). */
  let dropTrackId = $state("");

  /**
   * The row table every part of this view measures against — the rail, the
   * lane column, the grid canvas, the marquee and the launch overlay all
   * read THIS, so they cannot disagree about where lane 7 is. Rebuilt
   * whenever the tracks or the fold state change; cheap (one pass).
   */
  const layout = $derived(
    buildLaneLayout({
      tracks: project.tracks,
      collapsedTracks: lanes.collapsedTracks,
      collapsedGroups: lanes.collapsedGroups,
    }),
  );

  /** Visible lane order (painted rows only — a folded group's hidden
   * members are not in this list) — what lane shift-click range-extends
   * over, so a shift-click can never reach into a fold. */
  const visibleTrackIds = $derived(
    layout.rows.filter((r) => r.kind === "track").map((r) => r.track.id),
  );

  // Fold state belongs to a project, so it has to follow project switches
  // and forget tracks and groups that are gone. Cheap and idempotent.
  $effect(() => {
    void project.projectDir;
    void project.tracks;
    lanes.sync();
  });

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
    const beat = bar / Math.max(1, project.timeSignature[0]);
    const step = barStep();
    const beatPx = beat / spp;

    const firstBar = Math.floor(start / bar);
    const lastBar = Math.ceil((start + w * spp) / bar);

    ctx.font = "9px " + getComputedStyle(document.documentElement).getPropertyValue("--font-mono");
    ctx.textBaseline = "middle";

    for (let b = firstBar; b <= lastBar; b++) {
      const x = (b * bar - start) / spp;
      if (b % step === 0) {
        ctx.fillStyle = alpha(theme.tokens.line, 0.4);
        ctx.fillRect(Math.round(x), h - 14, 1, 14);
        ctx.fillStyle = alpha(theme.tokens.textDim, 0.9);
        ctx.fillText(String(b + 1).padStart(3, "0"), x + 4, h - 8);
      } else {
        ctx.fillStyle = alpha(theme.tokens.line, 0.2);
        ctx.fillRect(Math.round(x), h - 8, 1, 8);
      }
      // beats
      if (beatPx > 26 && b < lastBar) {
        ctx.fillStyle = alpha(theme.tokens.line, 0.14);
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
    // The grid spans whatever the rows actually add up to — folded lanes
    // and group strips included. `trackCount * TRACK_HEIGHT_PX` stopped
    // being that number the moment lanes could collapse.
    const h = Math.max(1, layout.totalHeight);

    const dpr = Math.min(3, window.devicePixelRatio || 1);
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.round(h * dpr);
    c.style.height = `${h}px`;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const bar = project.samplesPerBar;
    const beat = bar / Math.max(1, project.timeSignature[0]);
    const step = barStep();
    const beatPx = beat / spp;
    const firstBar = Math.floor(start / bar);
    const lastBar = Math.ceil((start + w * spp) / bar);

    for (let b = firstBar; b <= lastBar; b++) {
      const x = Math.round((b * bar - start) / spp);
      ctx.fillStyle = alpha(theme.tokens.line, b % step === 0 ? 0.16 : 0.07);
      ctx.fillRect(x, 0, 1, h);
      if (beatPx > 26 && b < lastBar) {
        ctx.fillStyle = alpha(theme.tokens.line, 0.045);
        for (let q = 1; q < project.timeSignature[0]; q++) {
          ctx.fillRect(Math.round(x + (q * beat) / spp), 0, 1, h);
        }
      }
    }
  });

  // Follow while STOPPED. The rAF tick above only follows while rolling,
  // so "rewind to start" (or any seek, or a stop that snaps the playhead
  // back) used to leave the view wherever it was. Keyed on the transport
  // snapshot rather than the interpolated position: while stopped the two
  // are the same number, and the snapshot is the only one that is
  // reactive — so this fires on seeks and nothing else. Scrolling away by
  // hand does not change it, which is why the view stays put afterwards.
  // untrack the reveal itself: `revealSamples` READS `view.viewStart` and
  // may WRITE it, so a tracked call would re-run on every scroll — and
  // then yank the view back the instant the user scrolled away, which is
  // the exact behaviour this effect must not have.
  $effect(() => {
    const pos = transport.snap.positionSamples;
    untrack(() => {
      if (transport.isPlaying || !view.follow) return;
      view.revealSamples(pos);
    });
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
        view.revealSamples(pos);
        x = (pos - view.viewStart) / view.spp;
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
    const color = theme.tokens.trackPalette[project.tracks.length % theme.tokens.trackPalette.length];
    await project.addTrack({ name: `Track ${project.tracks.length + 1}`, color });
  }

  async function addMidiTrack() {
    const color = theme.tokens.trackPalette[project.tracks.length % theme.tokens.trackPalette.length];
    await project.addTrack({ name: `MIDI ${project.tracks.length + 1}`, color, kind: "midi" });
  }

  async function addAutomationTrack() {
    const color = theme.tokens.trackPalette[project.tracks.length % theme.tokens.trackPalette.length];
    await project.addTrack({
      name: `Auto ${project.tracks.length + 1}`,
      color,
      kind: "automation",
    });
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
    for (const c of modulation.automationClips)
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
    if (track.kind === "automation") return;
    // `types` is all a dragover may read — see hasLibraryDrag's doc.
    if (!hasLibraryDrag(e.dataTransfer)) return; // OS file drags fall through
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    dropTrackId = track.id;
  }

  function onLaneDrop(track: TrackState, e: DragEvent) {
    dropTrackId = "";
    if (track.kind === "automation") return;
    const payload = decodeLibraryDrag(e.dataTransfer);
    if (!payload) return;
    e.preventDefault();
    void library.dropOnTrack(payload, track.id, dropSamples(e));
  }

  /** Double-click an empty span of a midi lane → create a 2-bar clip there. */
  function onLaneDblClick(track: TrackState, e: MouseEvent) {
    if (track.kind === "automation") {
      if ((e.target as HTMLElement).closest(".aclip, .clip, .mclip")) return;
      const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
      const samples = view.snapSamples(Math.max(0, view.samplesAt(e.clientX - rect.left)));
      const startTicks = Math.round(midi.samplesToTicks(samples) / midi.ppq) * midi.ppq;
      void modulation.addClip(track.id, Math.max(0, startTicks), 2 * midi.ticksPerBar);
      return;
    }
    if ((e.target as HTMLElement).closest(".mclip, .clip")) return;
    if (track.kind !== "midi") {
      toasts.info("NOT A MIDI LANE", `"${track.name}" is an audio track — midi clips need a midi lane`);
      return;
    }
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
  /** Press on empty lane that has not moved far enough to be a marquee.
   * Capturing on pointerdown (the old path) ate `dblclick` on WebKit, so
   * double-click-to-create a MIDI clip never fired. */
  let pendingMarquee: Marquee | null = null;

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
    if (!launch.marking && !startsMarquee(e.target as HTMLElement)) return;
    if (!lanesEl) return;
    const p = canvasPos(lanesEl, e.clientX, e.clientY);
    const mode = selectionModeFor(e);
    pendingMarquee = {
      x0: p.x,
      y0: p.y,
      x1: p.x,
      y1: p.y,
      baseKeys: new Set(clipSelection.keys),
      mode,
    };
    // Capture only after slop — see pendingMarquee. Without it the second
    // click of a double-click is retargeted and onLaneDblClick never runs.
  }

  function onLanesPointerMove(e: PointerEvent) {
    if (!lanesEl) return;
    const p = canvasPos(lanesEl, e.clientX, e.clientY);
    if (pendingMarquee && !marquee) {
      if (!movedPastMarqueeSlop(p.x - pendingMarquee.x0, p.y - pendingMarquee.y0)) return;
      marquee = { ...pendingMarquee, x1: p.x, y1: p.y };
      pendingMarquee = null;
      if (marquee.mode === "replace") clipSelection.clear();
      try {
        (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      } catch {
        /* synthetic pointer */
      }
    }
    if (!marquee) return;
    marquee = { ...marquee, x1: p.x, y1: p.y };
    if (launch.marking) return;
    const s0 = view.samplesAt(Math.min(marquee.x0, marquee.x1));
    const s1 = view.samplesAt(Math.max(marquee.x0, marquee.x1));
    const laneLo = nearestTrackIndexAtY(layout, Math.min(marquee.y0, marquee.y1));
    const laneHi = nearestTrackIndexAtY(layout, Math.max(marquee.y0, marquee.y1));
    // No visible lane under the band at all (every lane folded away into
    // groups): nothing to hit-test, and clamping to lane 0 would select
    // clips the band never touched.
    if (laneLo === null || laneHi === null) return;
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
    // MARK mode: a finished marquee is a new region, even over clips.
    if (launch.marking && marquee) {
      const s0 = view.samplesAt(Math.min(marquee.x0, marquee.x1));
      const s1 = view.samplesAt(Math.max(marquee.x0, marquee.x1));
      const laneLo = nearestTrackIndexAtY(layout, Math.min(marquee.y0, marquee.y1));
      const laneHi = nearestTrackIndexAtY(layout, Math.max(marquee.y0, marquee.y1));
      const region =
        laneLo === null || laneHi === null
          ? null
          : regionFromMarquee({
              startSamples: s0,
              endSamples: s1,
              laneLo,
              laneHi,
              tracks: project.tracks,
              samplesToTicks: (s) => midi.samplesToTicks(s),
              snapTicks: view.snap ? midi.ppq : undefined,
            });
      if (region) void launch.createRegion(region.startTicks, region.lengthTicks, region.trackIds);
      pendingMarquee = null;
      marquee = null;
      try {
        (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
      } catch {
        /* not captured */
      }
      return;
    }
    // A press that never crossed slop is a click on empty lane: drop the
    // selection, same as the old immediate-replace clear, but without
    // eating the following dblclick.
    if (pendingMarquee && !marquee && pendingMarquee.mode === "replace") {
      clipSelection.clear();
    }
    pendingMarquee = null;
    marquee = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
  }

  const launchMarks = $derived.by(() => {
    if (!launch.panelOpen) return [];
    return launch.bindings
      .map((b) => {
        const box = overlayBox(
          b,
          project.tracks,
          midi.clips,
          (t) => midi.ticksToSamples(t),
        );
        if (!box) return null;
        return { binding: b, box };
      })
      .filter((x): x is { binding: (typeof launch.bindings)[number]; box: NonNullable<ReturnType<typeof overlayBox>> } => x !== null);
  });

  type MarkDrag = {
    id: string;
    mode: "move" | "resize";
    edge?: "start" | "end";
    startTicks: number;
    lengthTicks: number;
    trackIds: string[];
    originX: number;
    originY: number;
    moved: boolean;
  };
  let markDrag: MarkDrag | null = $state(null);

  function persistMarkDrag() {
    const d = markDrag;
    markDrag = null;
    if (!d) return;
    const b = launch.bindings.find((x) => x.id === d.id);
    void (async () => {
      try {
        if (d.moved && b) await launch.update(d.id, { target: b.target });
      } finally {
        await backend.gestureEnd?.();
      }
    })();
  }

  function onMarkPointerDown(id: string, e: PointerEvent) {
    if (e.button !== 0) return;
    e.stopPropagation();
    launch.selectedId = id;
    const b = launch.bindings.find((x) => x.id === id);
    if (!b || b.target.kind !== "region") {
      launch.focus(id);
      return;
    }
    const p = lanesEl ? canvasPos(lanesEl, e.clientX, e.clientY) : { x: 0, y: 0 };
    markDrag = {
      id,
      mode: "move",
      startTicks: b.target.startTicks,
      lengthTicks: b.target.lengthTicks,
      trackIds: [...b.target.trackIds],
      originX: p.x,
      originY: p.y,
      moved: false,
    };
    void backend.gestureBegin?.("move launch");
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic */
    }
  }

  function onMarkPointerMove(e: PointerEvent) {
    if (!markDrag || !lanesEl) return;
    const b = launch.bindings.find((x) => x.id === markDrag!.id);
    if (!b || b.target.kind !== "region") return;
    const p = canvasPos(lanesEl, e.clientX, e.clientY);
    if (!markDrag.moved && !movedPastMarqueeSlop(p.x - markDrag.originX, p.y - markDrag.originY)) {
      return;
    }
    markDrag = { ...markDrag, moved: true };
    if (markDrag.mode === "resize" && markDrag.edge) {
      const tick = Math.max(0, Math.round(midi.samplesToTicks(view.samplesAt(p.x))));
      const snapped = view.snap ? Math.round(tick / midi.ppq) * midi.ppq : tick;
      const next = resizeRegion(markDrag.startTicks, markDrag.lengthTicks, markDrag.edge, snapped);
      launch.patchLocal(markDrag.id, { target: { ...b.target, ...next } });
      return;
    }
    const originTick = midi.samplesToTicks(view.samplesAt(markDrag.originX));
    const nowTick = midi.samplesToTicks(view.samplesAt(p.x));
    let deltaTicks = Math.round(nowTick - originTick);
    if (view.snap) deltaTicks = Math.round(deltaTicks / midi.ppq) * midi.ppq;
    const originLane = nearestTrackIndexAtY(layout, markDrag.originY);
    const nowLane = nearestTrackIndexAtY(layout, p.y);
    // Both are only null when NOTHING is painted (every track folded into
    // one group) — keep the time shift live rather than freezing the whole
    // drag; there's simply no lane delta to apply until a lane is visible.
    const laneDelta = originLane !== null && nowLane !== null ? nowLane - originLane : 0;
    launch.patchLocal(markDrag.id, {
      target: shiftRegion(
        {
          kind: "region",
          startTicks: markDrag.startTicks,
          lengthTicks: markDrag.lengthTicks,
          trackIds: markDrag.trackIds,
        },
        deltaTicks,
        laneDelta,
        project.tracks,
      ),
    });
  }

  function onMarkPointerUp() {
    if (markDrag && !markDrag.moved) launch.focus(markDrag.id);
    persistMarkDrag();
  }

  // ── lane reorder drag (the rail's colour stripe is the handle) ──
  //
  // The gesture lives here rather than in TrackHeader because it needs the
  // ROW TABLE — where every lane sits, and which of them are inside a
  // group — and that is this component's `layout`. The header only marks
  // its grip with `data-lane-grip`.

  let laneDrag: { trackId: string; originY: number; moved: boolean } | null = null;

  /** Pointer y in ROW space (the lane column's coordinates). Measured
   * against `lanesEl` and not the rail so the drop indicator, the row tops
   * and the pointer are all in one space — and via `canvasPos`, which is
   * the standing rule: raw rect maths drift under interface zoom. */
  function rowY(e: PointerEvent): number {
    return lanesEl ? canvasPos(lanesEl, e.clientX, e.clientY).y : 0;
  }

  function onRailPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const grip = (e.target as HTMLElement).closest<HTMLElement>("[data-lane-grip]");
    const trackId = grip?.dataset.laneGrip;
    if (!trackId) return;
    e.preventDefault();
    laneDrag = { trackId, originY: rowY(e), moved: false };
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic pointer */
    }
  }

  function onRailPointerMove(e: PointerEvent) {
    if (!laneDrag) return;
    const y = rowY(e);
    // Slop before committing to a drag: a plain click on the stripe should
    // not fold the lanes' order into the undo stack.
    if (!laneDrag.moved) {
      if (Math.abs(y - laneDrag.originY) < 4) return;
      laneDrag.moved = true;
      lanes.draggingTrackId = laneDrag.trackId;
    }
    const target = dropTargetAtY(layout, project.tracks, y);
    lanes.dropIndicator = { y: target.y, group: target.group };
  }

  function onRailPointerUp(e: PointerEvent) {
    const drag = laneDrag;
    laneDrag = null;
    lanes.draggingTrackId = "";
    lanes.dropIndicator = null;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      /* not captured */
    }
    if (!drag?.moved) return;
    const target = dropTargetAtY(layout, project.tracks, rowY(e));
    void project.arrangeLanes(
      arrangementForMove(project.tracks, drag.trackId, target.index, target.group),
    );
  }

  /** Escape abandons a lane drag — nothing has been committed until pointerup. */
  function onRailKeydown(e: KeyboardEvent) {
    if (e.key !== "Escape" || !laneDrag) return;
    laneDrag = null;
    lanes.draggingTrackId = "";
    lanes.dropIndicator = null;
  }

  function onHandleDown(id: string, edge: "start" | "end", e: PointerEvent) {
    e.stopPropagation();
    e.preventDefault();
    const b = launch.bindings.find((x) => x.id === id);
    if (!b || b.target.kind !== "region") return;
    const p = lanesEl ? canvasPos(lanesEl, e.clientX, e.clientY) : { x: 0, y: 0 };
    markDrag = {
      id,
      mode: "resize",
      edge,
      startTicks: b.target.startTicks,
      lengthTicks: b.target.lengthTicks,
      trackIds: [...b.target.trackIds],
      originX: p.x,
      originY: p.y,
      moved: false,
    };
    launch.selectedId = id;
    void backend.gestureBegin?.("resize launch");
    try {
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    } catch {
      /* synthetic */
    }
  }
</script>

<div class="timeline" onwheel={onWheel}>
  <!-- ruler row -->
  <div class="ruler-row">
    <div class="corner silk" title="Ctrl+C copy · Ctrl+V paste at playhead · Ctrl+Shift+V paste onto new tracks · Ctrl+Shift+M export selection as MIDI">
      {clipSelection.count() > 0
        ? `${clipSelection.count()} clip${clipSelection.count() === 1 ? "" : "s"} selected`
        : `tracks · ${project.tracks.length}`}
    </div>
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

  <!-- Scrollable body. `.bodyinner` is not decoration: `.body` used to be
       the flex row itself, which made both columns flex ITEMS of the
       scroller — so the rail's headers were shrunk to fit the viewport
       while the lane column, clipped by its own `overflow: hidden`, simply
       lost every lane below the fold and never scrolled at all. With a
       plain block scroller wrapping a content-sized flex row, the columns
       size to their rows and the scrollbar measures what is really there. -->
  <div class="body">
    <div class="bodyinner">
    <div
      class="rail"
      role="grid"
      aria-label="Tracks"
      aria-multiselectable="true"
      aria-rowcount={project.tracks.length}
      tabindex="-1"
      onpointerdown={onRailPointerDown}
      onpointermove={onRailPointerMove}
      onpointerup={onRailPointerUp}
      onpointercancel={onRailPointerUp}
      onkeydown={onRailKeydown}
    >
      {#each layout.rows as row (row.kind === "group" ? `g:${row.group}` : row.track.id)}
        {#if row.kind === "group"}
          <LaneGroupHeader {row} />
        {:else}
          <TrackHeader
            track={row.track}
            index={row.trackIndex}
            collapsed={row.collapsed}
            orderedTrackIds={visibleTrackIds}
          />
        {/if}
      {/each}
      <div class="addrow">
        <button class="add mono" onclick={addTrack}>+ AUDIO</button>
        <button class="add midi mono" onclick={addMidiTrack}>+ MIDI</button>
        <button class="add auto mono" onclick={addAutomationTrack}>+ AUTO</button>
      </div>
      {#if project.tracks.length > 1}
        <div class="foldrow">
          <button
            class="foldall mono"
            title="Fold every lane to a strip, or unfold them all"
            onclick={() => lanes.setAllCollapsed(!lanes.anyCollapsed())}
            >{lanes.anyCollapsed() ? "⤢ UNFOLD ALL" : "⤡ FOLD ALL"}</button
          >
        </div>
      {/if}
    </div>

    <div
      class="lanes"
      class:marking={launch.marking}
      id="timeline-lanes"
      bind:this={lanesEl}
      role="presentation"
      onpointerdown={onLanesPointerDown}
      onpointermove={onLanesPointerMove}
      onpointerup={onLanesPointerUp}
      onpointercancel={onLanesPointerUp}
    >
      <canvas bind:this={gridCanvas} class="grid"></canvas>
      {#each layout.rows as row (row.kind === "group" ? `g:${row.group}` : row.track.id)}
        {#if row.kind === "group"}
          <!-- The lane-column half of a group header: a tinted strip that
               keeps the two columns row-for-row identical. Clicking it
               folds, so the whole strip is a target, not just the rail's
               little triangle. -->
          <div
            class="grouplane"
            class:folded={row.collapsed}
            style:height="{row.height}px"
            style:--group-color={row.color}
            role="presentation"
            ondblclick={() => lanes.toggleGroup(row.group)}
          ></div>
        {:else}
          {@const track = row.track}
          <div
            class="lane"
            class:armed={track.armed}
            class:midilane={track.kind === "midi"}
            class:autolane={track.kind === "automation"}
            class:lanefolded={row.collapsed}
            class:droptarget={dropTrackId === track.id}
            style:height="{row.height}px"
            role="presentation"
            ondblclick={(e) => onLaneDblClick(track, e)}
            ondragover={(e) => onLaneDragOver(track, e)}
            ondragleave={() => (dropTrackId = "")}
            ondrop={(e) => onLaneDrop(track, e)}
          >
            {#if track.kind === "automation"}
              <AutomationTrackRow {track} />
            {:else}
            {#each project.clipsOf(track.id) as clip (clip.id)}
              <ClipView {clip} {track} />
            {/each}
            {#each midi.clipsOf(track.id) as clip (clip.id)}
              <MidiClipView {clip} {track} />
            {/each}
            {#if modulation.hasVisible(track.id) && !row.collapsed}
              <div class="lane-shade"></div>
              {#each modulation.visibleBindingsFor(track.id) as binding (binding.id)}
                {@const curve = modulation.curveOf(binding)}
                {#if curve}
                  <ModulationLaneView {track} {binding} {curve} />
                {/if}
              {/each}
            {/if}
            {/if}
          </div>
        {/if}
      {/each}
      {#if lanes.dropIndicator}
        <div
          class="dropline"
          class:intogroup={lanes.dropIndicator.group !== null}
          style:top="{lanes.dropIndicator.y}px"
        >
          {#if lanes.dropIndicator.group !== null}
            <span class="droplabel mono">{lanes.dropIndicator.group}</span>
          {/if}
        </div>
      {/if}
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
      {#each launchMarks as mark (mark.binding.id)}
        {@const left = view.xOf(mark.box.startSamples)}
        {@const width = Math.max(4, (mark.box.endSamples - mark.box.startSamples) / view.spp)}
        <!-- The band covers only the lanes that are actually painted: a
             region spanning a folded group shrinks onto what is visible
             instead of drawing a block over the fold. `null` = every lane
             it names is hidden, so the mark is not drawn at all. -->
        {@const band = bandForTrackRange(layout, project.tracks, mark.box.laneLo, mark.box.laneHi)}
        {#if band}
        {@const top = band.top}
        {@const height = band.height}
        <div
          class="launchmark"
          class:sel={launch.selectedId === mark.binding.id}
          class:clip={mark.binding.target.kind === "clip"}
          role="button"
          tabindex="0"
          title="{mark.binding.name} — drag to move, edges to resize"
          style:left="{left}px"
          style:top="{top}px"
          style:width="{width}px"
          style:height="{height}px"
          onpointerdown={(e) => onMarkPointerDown(mark.binding.id, e)}
          onpointermove={onMarkPointerMove}
          onpointerup={onMarkPointerUp}
          onpointercancel={onMarkPointerUp}
        >
          <span class="launchlab mono">{mark.binding.name}</span>
          {#if launch.selectedId === mark.binding.id && mark.binding.target.kind === "region"}
            <div
              class="launchhandle start"
              role="separator"
              aria-label="Resize launch start"
              onpointerdown={(e) => onHandleDown(mark.binding.id, "start", e)}
              onpointermove={onMarkPointerMove}
              onpointerup={onMarkPointerUp}
            ></div>
            <div
              class="launchhandle end"
              role="separator"
              aria-label="Resize launch end"
              onpointerdown={(e) => onHandleDown(mark.binding.id, "end", e)}
              onpointermove={onMarkPointerMove}
              onpointerup={onMarkPointerUp}
            ></div>
          {/if}
        </div>
        {/if}
      {/each}
      {#if marqueeRect && marqueeRect.width > 2}
        <div
          class="marquee"
          class:launching={launch.panelOpen}
          style:left="{marqueeRect.left}px"
          style:top="{marqueeRect.top}px"
          style:width="{marqueeRect.width}px"
          style:height="{marqueeRect.height}px"
        ></div>
      {/if}
      <div bind:this={playheadEl} class="playhead"></div>
    </div>
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
    /* Also the cross-axis floor: with the side panel docked, `.main` is a
       ROW and an unconstrained flex item refuses to shrink past its content,
       pushing the dock off-screen. */
    min-width: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-0);
  }

  .ruler-row {
    display: flex;
    height: var(--ruler-height);
    border-bottom: 1px solid var(--glass-border);
    background: rgb(var(--bg-1-rgb) / 0.85);
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
    filter: drop-shadow(0 0 calc(4px * var(--glow-scale)) var(--cyan-dim));
    pointer-events: none;
  }

  /* loop region: band + pins in the ruler's top strip */
  .loopband {
    position: absolute;
    top: 0;
    height: 45%;
    background: rgb(var(--edge-rgb) / 0.12);
    border-bottom: 1px solid rgb(var(--edge-rgb) / 0.25);
    pointer-events: none;
  }
  .loopband.on {
    background: rgb(var(--amber-rgb) / 0.14);
    border-bottom: 1px solid rgb(var(--amber-rgb) / 0.55);
    box-shadow: inset 0 0 calc(10px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.12);
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
    background: rgb(var(--edge-rgb) / 0.6);
  }
  .looppin::after {
    content: "";
    position: absolute;
    top: 0;
    width: 0;
    height: 0;
    border-top: 6px solid rgb(var(--edge-rgb) / 0.85);
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
    box-shadow: 0 0 calc(5px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.6);
  }
  .looppin.on::after {
    border-top-color: var(--amber);
  }

  /* faint full-height echo of the active loop region in the lanes */
  .loopshade {
    position: absolute;
    top: 0;
    bottom: 0;
    background: rgb(var(--amber-rgb) / 0.035);
    border-left: 1px solid rgb(var(--amber-rgb) / 0.18);
    border-right: 1px solid rgb(var(--amber-rgb) / 0.18);
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
      background: rgb(var(--magenta-rgb) / 0.22);
      border-bottom-color: rgb(var(--magenta-rgb) / 0.7);
      box-shadow: inset 0 0 calc(16px * var(--glow-scale)) rgb(var(--magenta-rgb) / 0.25);
    }
  }
  @keyframes jam-breathe-shade {
    50% {
      background: rgb(var(--magenta-rgb) / 0.06);
      border-left-color: rgb(var(--magenta-rgb) / 0.35);
      border-right-color: rgb(var(--magenta-rgb) / 0.35);
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
      background: rgb(var(--green-rgb) / 0.55);
      box-shadow: inset 0 0 calc(20px * var(--glow-scale)) rgb(var(--green-rgb) / 0.6);
    }
    100% {
      background: rgb(var(--amber-rgb) / 0.14);
    }
  }
  @keyframes jam-flash-shade {
    0% {
      background: rgb(var(--green-rgb) / 0.16);
      border-left-color: rgb(var(--green-rgb) / 0.7);
      border-right-color: rgb(var(--green-rgb) / 0.7);
    }
    100% {
      background: rgb(var(--amber-rgb) / 0.035);
    }
  }

  /*
   * The vertical scroller. It is a plain BLOCK, not the flex row it used
   * to be, and that distinction is the whole fix for three separate
   * symptoms that were all one bug:
   *
   *   - `.rail` and `.lanes` were flex ITEMS of a container with a definite
   *     height, so `align-items: stretch` sized both to the VIEWPORT rather
   *     than to their content;
   *   - the rail's headers, being flex items themselves with the default
   *     `flex-shrink: 1`, were then squeezed to fit — 88 px lanes against
   *     44 px headers, drifting further apart with every row, which is the
   *     "the two columns don't line up" report;
   *   - `.lanes` clipped its overflow (it must, horizontally), so lanes past
   *     the fold were painted nowhere and, since a clipped child adds no
   *     scroll height, the body never became scrollable at all — the lanes
   *     "behind" the piano roll were simply unreachable, and interface zoom
   *     made it worse by shrinking the viewport in layout px.
   *
   * Measured before: 10 tracks in a 473 px body → scrollHeight == clientHeight
   * (no scroll), header 44 px vs lane 88 px, row 5 off by 445 px. After:
   * scrollHeight 918, every row aligned to 0.00 px at every scroll offset.
   */
  .body {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    /* Keep the wheel inside the arrangement instead of chaining to the
       shell once the lanes hit an end. */
    overscroll-behavior-y: contain;
  }
  /* The flex row lives INSIDE the scroller, sized by its rows.
     `min-height: 100%` only keeps the columns' backgrounds filling a
     short arrangement; it never caps them. */
  .bodyinner {
    display: flex;
    align-items: stretch;
    min-height: 100%;
  }

  .scrollrow {
    flex: none;
    display: flex;
    border-top: 1px solid var(--glass-border);
    background: rgb(var(--bg-1-rgb) / 0.85);
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
    background: rgb(var(--bg-1-rgb) / 0.6);
    display: flex;
    flex-direction: column;
    /* The rail's own children must never shrink either — see the note on
       `.header` in TrackHeader.svelte. Belt and braces: the two columns
       drifting apart is a silent, cumulative failure. */
    align-items: stretch;
  }
  .rail > :global(*) {
    flex: none;
  }
  .addrow {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin: 10px;
  }
  .foldrow {
    display: flex;
    margin: 0 10px 10px;
  }
  .foldall {
    flex: 1;
    padding: 5px;
    font-size: 8px;
    letter-spacing: 0.2em;
    color: var(--text-faint);
    background: transparent;
    border: 1px solid rgb(var(--edge-rgb) / 0.12);
    border-radius: 5px;
    cursor: pointer;
    transition: color 120ms, border-color 120ms;
  }
  .foldall:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .add {
    flex: 1;
    padding: 8px;
    font-size: 9px;
    letter-spacing: 0.2em;
    color: var(--text-faint);
    background: transparent;
    border: var(--border-width) dashed rgb(var(--edge-rgb) / 0.2);
    border-radius: 5px;
    cursor: pointer;
    transition: color 120ms, border-color 120ms;
  }
  .add:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .add.midi:hover {
    color: var(--green);
    border-color: rgb(var(--green-rgb) / 0.4);
  }
  .add.auto:hover {
    color: var(--violet);
    border-color: rgb(var(--violet-rgb) / 0.45);
  }
  .lane.midilane {
    background:
      repeating-linear-gradient(
        to bottom,
        transparent 0 13px,
        rgb(var(--line-rgb) / 0.03) 13px 14px
      );
  }

  .jamband {
    position: relative;
    flex: none;
    /* content box starts at the lane origin, so the panel's `left` stays in
       the same coordinates it used when it lived inside .lanes */
    padding-left: var(--rail-width);
  }

  /* Height is now content-driven (the rows are in normal flow) and then
     stretched to the flex line, which is always at least as tall — the
     rail carries the same rows PLUS the add/fold buttons. So
     `overflow: hidden` only ever clips HORIZONTALLY, which is what it is
     for: clips run past the right edge, and the timeline scrolls time by
     redrawing rather than by scrollLeft. */
  .lanes {
    flex: 1;
    min-width: 0;
    position: relative;
    overflow: hidden;
  }
  .lanes.marking {
    cursor: crosshair;
  }
  .lanes.marking :global(.clip),
  .lanes.marking :global(.mclip),
  .lanes.marking :global(.launchmark) {
    pointer-events: none;
  }
  .grid {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    pointer-events: none;
  }
  /* Height comes from the row table (inline), not from `--track-height`:
     a lane is full, folded, or a group strip. The token stays the default
     the row table reads. */
  .lane {
    position: relative;
    border-bottom: 1px solid rgb(var(--line-rgb) / 0.08);
  }
  /* A folded lane keeps its clips — that is the point of the strip: you
     can still see WHERE the material is, just not what it looks like. The
     clip chrome that needs room is hidden rather than squashed. */
  .lane.lanefolded :global(.cliplabel),
  .lane.lanefolded :global(.launchlab) {
    display: none;
  }
  .grouplane {
    position: relative;
    border-bottom: 1px solid rgb(var(--line-rgb) / 0.14);
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--group-color) 14%, transparent),
      transparent 40%
    );
  }
  /* The drop indicator during a lane drag. It spans the lane column so the
     landing place is unmistakable, and it is tinted when the drop would
     join a group — the one thing a plain line cannot say. */
  .dropline {
    position: absolute;
    left: 0;
    right: 0;
    height: 2px;
    margin-top: -1px;
    background: var(--cyan);
    box-shadow: 0 0 8px var(--cyan-dim);
    pointer-events: none;
    z-index: 6;
  }
  .dropline.intogroup {
    background: var(--amber);
    box-shadow: 0 0 8px rgb(var(--amber-rgb) / 0.5);
  }
  .droplabel {
    position: absolute;
    left: 8px;
    top: -14px;
    font-size: 8px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--bg-0);
    background: var(--amber);
    border-radius: 3px;
    padding: 1px 5px;
    white-space: nowrap;
  }
  .lane.armed {
    background: rgb(var(--red-rgb) / 0.03);
  }
  .lane.droptarget {
    box-shadow: inset 0 0 0 1px var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.05);
  }
  .lane-shade {
    position: absolute;
    inset: 0;
    z-index: 2;
    pointer-events: none;
    background: color-mix(in srgb, var(--bg-0) 45%, transparent);
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
    border: var(--border-width) solid var(--cyan-dim);
    background: linear-gradient(
      100deg,
      rgb(var(--cyan-rgb) / 0.08),
      rgb(var(--magenta-rgb) / 0.08)
    );
    color: var(--cyan);
    cursor: pointer;
  }
  .seedbtn:hover:not(:disabled) {
    box-shadow: 0 0 calc(12px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.25);
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
      rgb(var(--cyan-rgb) / 0.35) 45%,
      rgb(var(--magenta-rgb) / 0.3) 65%,
      transparent
    );
    mix-blend-mode: screen;
    animation: seed-sweep 1.4s linear infinite;
  }
  @keyframes seed-glow {
    0%,
    100% {
      box-shadow: 0 0 calc(10px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.25);
    }
    50% {
      box-shadow: 0 0 calc(18px * var(--glow-scale)) rgb(var(--magenta-rgb) / 0.35);
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
    background: rgb(var(--cyan-rgb) / 0.08);
    border: var(--border-width) solid var(--cyan-dim);
    pointer-events: none;
    z-index: 3;
  }
  .marquee.launching {
    background: rgb(var(--amber-rgb) / 0.1);
    border-color: rgb(var(--amber-rgb) / 0.55);
  }
  .launchmark {
    position: absolute;
    z-index: 2;
    box-sizing: border-box;
    border: var(--border-width) solid rgb(var(--amber-rgb) / 0.45);
    background: rgb(var(--amber-rgb) / 0.08);
    pointer-events: auto;
    cursor: grab;
  }
  .launchmark:active {
    cursor: grabbing;
  }
  .launchmark.clip {
    border-style: dashed;
  }
  .launchmark.sel {
    border-color: var(--amber);
    background: rgb(var(--amber-rgb) / 0.16);
    box-shadow: inset 0 0 calc(12px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.12);
  }
  .launchlab {
    position: absolute;
    top: 3px;
    left: 6px;
    font-size: 9px;
    letter-spacing: 0.12em;
    color: var(--amber);
    pointer-events: none;
    white-space: nowrap;
  }
  .launchhandle {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 8px;
    cursor: ew-resize;
  }
  .launchhandle.start {
    left: 0;
  }
  .launchhandle.end {
    right: 0;
  }

  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    width: 1px;
    background: var(--cyan);
    box-shadow:
      0 0 calc(6px * var(--glow-scale)) var(--cyan-dim),
      0 0 calc(1px * var(--glow-scale)) var(--cyan);
    pointer-events: none;
    will-change: transform;
  }
</style>
