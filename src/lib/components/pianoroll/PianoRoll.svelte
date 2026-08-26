<script lang="ts">
  /**
   * Canvas piano roll for one MIDI clip. Pitch rows × tick columns, notes as
   * glowing bars in the track color (velocity → luminance). Draw on empty,
   * drag to move, edge-drag to resize, double-click / Delete to remove;
   * shift-drag marquees a tick region (feeds AMT infill) and selects notes
   * (ctrl = add, alt = subtract, shift-click toggles one). Ctrl+C/X/V work
   * on the note selection; arrows nudge in time / transpose in pitch;
   * Q quantizes selected starts to the grid (Shift+Q also lengths).
   * Velocity lane below; piano keys at left audition through
   * sampler_preview_note when the track carries an instrument.
   * Same discipline as the timeline: canvases repaint on state changes only,
   * the playhead is a rAF-transformed overlay outside Svelte reactivity.
   */
  import { untrack } from "svelte";
  import { clipEditLoop } from "../../state/clip-edit-loop.svelte";
  import { composer } from "../../state/composer.svelte";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { transport } from "../../state/transport.svelte";
  import { instruments } from "../../state/instruments.svelte";
  import { launch } from "../../state/launch.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { openPluginParams } from "../../state/plugin-panel";
  import { openStudio, ui } from "../../state/ui.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import { ROLL_RESIZE } from "../../utils/panel-resize";
  import HScrollbar from "../HScrollbar.svelte";
  import BottomPanelTabs from "../surface/BottomPanelTabs.svelte";
  import { canvasPos } from "../../utils/canvas-pos";
  import {
    applyMarquee,
    channelForNewNote,
    copySelection,
    marqueeHits,
    nudgeSelection,
    pasteNotes,
    quantizeSelection,
    sortWithSelection,
    toggleIndex,
    transposeSelection,
    type MarqueeMode,
  } from "../../utils/note-ops";
  import PanelResizeHandle from "../PanelResizeHandle.svelte";
  import ClipEnvelopeLane from "../ClipEnvelopeLane.svelte";
  import type { MidiNote } from "../../types/ipc";
  import { theme } from "../../theme/theme.svelte";
  import { alpha } from "../../theme/tokens";
  import { roleGloss, roleInk } from "../../utils/palette-colors";

  const KEY_H = 14; // CSS px per pitch row
  const KEYS_W = 64;
  const BLACK = new Set([1, 3, 6, 8, 10]);

  const clip = $derived(midi.openClip);
  const track = $derived(clip ? project.trackById(clip.trackId) : undefined);
  /**
   * The Composer's tint (Plan H1 Task 8): the roll colours its own key rows by
   * what each note DOES over the chord sounding at that point — chord tone,
   * available colour, tension, or the one note that fights the chord. It is the
   * feature's whole thesis in one surface: the 128 rows of a piano roll are not
   * equal, and a beginner cannot see which is which.
   *
   * On by default when the project has a harmony, and a chip turns it off.
   */
  let harmonyTint = $state(true);
  const harmonyOn = $derived(harmonyTint && composer.chords.length > 0);
  /** Chord regions that overlap the open clip, in clip-relative ticks. */
  const clipRegions = $derived.by(() => {
    if (!clip) return [];
    const from = clip.timelineStartTicks;
    const to = from + midi.effectiveContentLengthTicks(clip);
    return composer.chords
      .filter((c) => c.tick < to && c.tick + c.lengthTicks > from)
      .map((c) => ({
        chord: c.chord,
        fromTick: Math.max(0, c.tick - from),
        toTick: Math.min(to - from, c.tick + c.lengthTicks - from),
        palette: composer.regionPalettes[c.tick] ?? null,
      }));
  });
  /**
   * Which region the key gutter shows. Follows the playhead, but is only
   * REASSIGNED when the playhead crosses into a different chord — the rAF loop
   * runs at frame rate and a per-frame write would repaint the gutter canvas
   * sixty times a second to say the same thing.
   */
  let gutterFrom = $state<number | null>(null);
  const gutterRegion = $derived.by(() => {
    if (clipRegions.length === 0) return null;
    return clipRegions.find((r) => r.fromTick === gutterFrom) ?? clipRegions[0];
  });

  /** The chip's tooltip: the actual notes, named — never a category alone. */
  const harmonyHint = $derived.by(() => {
    const pal = gutterRegion?.palette;
    if (!pal) return "Colour the keys by what each note does over the chord in force";
    const names = (roles: string[]) =>
      pal.classes
        .filter((c) => roles.includes(c.role))
        .map((c) => c.tpc)
        .join(" ");
    const chord = names(["root", "third", "fifth", "seventh"]);
    const colour = names(["extension"]);
    const avoid = names(["avoid"]);
    return [
      `over ${pal.chordPretty ?? pal.keyLabel}: ${chord} are the chord`,
      colour ? `${colour} available` : "",
      avoid ? `${avoid} — ${roleGloss("avoid")}` : "",
    ]
      .filter(Boolean)
      .join(" · ");
  });

  // Fetch the palette of every region the clip covers when the clip (or the
  // harmony) changes. One call per region, cached in the store.
  $effect(() => {
    if (!clip || !harmonyTint) return;
    const from = clip.timelineStartTicks;
    const to = from + midi.effectiveContentLengthTicks(clip);
    // Depend on the VIEW, not on the chord count: an edit that changes one
    // chord's symbol without changing how many there are still drops the
    // palette cache, and a length-only dependency would leave the roll
    // untinted until something else happened to invalidate this effect.
    void composer.view;
    void composer.loadRegionPalettes(from, to);
  });
  const color = $derived(track?.color ?? theme.tokens.green);
  const instrument = $derived(instruments.byId(track?.instrumentId));
  const pluginInst = $derived(plugins.instanceForRef(track?.instrumentId));
  const usedLauncher = $derived(clip ? launch.driveMap(clip.id) : null);

  $effect(() => {
    if (!clipEditLoop.playOnce) return;
    clipEditLoop.tickPlayOnce(transport.snap.positionSamples);
  });

  // ── local edit state ──
  let working = $state<MidiNote[]>([]);
  let selection = $state<Set<number>>(new Set());
  let workingClipId: string | null = null;

  // viewport: ticks-per-px + scroll (ticks, and top key index)
  let tpp = $state(8);
  let scrollTick = $state(0);
  let topKey = $state(84);
  let gridDiv = $state(4); // subdivisions of a beat: 4 = 1/16ths
  let gridW = $state(640);
  let gridH = $state(280);

  const gridTicks = $derived(midi.ppq / gridDiv);

  let gridCanvas: HTMLCanvasElement | undefined = $state();
  let keysCanvas: HTMLCanvasElement | undefined = $state();
  let velCanvas: HTMLCanvasElement | undefined = $state();
  let gridWrap: HTMLDivElement | undefined = $state();
  let rollEl: HTMLDivElement | undefined = $state();
  let playheadEl: HTMLDivElement | undefined = $state();
  let flashCanvas: HTMLCanvasElement | undefined = $state();
  let keysFlashCanvas: HTMLCanvasElement | undefined = $state();

  // adopt clip → working copy (never mid-gesture; gestures are sync)
  $effect(() => {
    const c = clip;
    if (!c) return;
    if (c.id !== workingClipId) {
      workingClipId = c.id;
      working = c.notes.map((n) => ({ ...n }));
      selection = new Set();
      // frame the content: start at clip start, center pitch range
      scrollTick = 0;
      const keys = c.notes.map((n) => n.key);
      const mid = keys.length ? Math.round((Math.min(...keys) + Math.max(...keys)) / 2) : 60;
      topKey = Math.min(126, Math.max(24, mid + Math.round(gridH / KEY_H / 2)));
      // fit ~content length + 1 bar into the view (the piano roll edits one
      // repetition of content, not the — possibly looped/stretched —
      // placement; spec §6)
      tpp = Math.max(1, (midi.effectiveContentLengthTicks(c) + midi.ticksPerBar) / Math.max(320, gridW));
    } else {
      // outside edit (AMT merge, backend echo): adopt unless mid-gesture
      if (!gesture) {
        // A commit echo has the same note count (and, via commit()'s
        // pre-sort, the same order), so the index selection stays valid; an
        // outside edit that grew/shrank the array would leave it pointing
        // at the wrong notes — drop it instead of lying. untrack: this
        // effect WRITES working, so reading it tracked would loop it.
        if (c.notes.length !== untrack(() => working.length)) selection = new Set();
        working = c.notes.map((n) => ({ ...n }));
      }
    }
  });

  function commit() {
    if (commitTimer) {
      clearTimeout(commitTimer);
      commitTimer = null;
    }
    if (!clip) return;
    // Pre-sort the way the store will (tick, key) and remap the selection
    // along — otherwise the sorted echo re-adopted by the $effect above
    // leaves the index-based selection pointing at the wrong notes.
    const sorted = sortWithSelection(working, selection);
    working = sorted.notes;
    selection = sorted.selection;
    void midi.setNotes(clip.id, working.map((n) => ({ ...n })));
  }

  // Arrow-key nudges/transposes update `working` instantly but coalesce
  // into ONE midi_set_notes trailing the last key repeat — a held key is
  // one gesture, not thirty invokes a second (D-03).
  let commitTimer: ReturnType<typeof setTimeout> | null = null;
  function commitSoon() {
    if (commitTimer) clearTimeout(commitTimer);
    commitTimer = setTimeout(() => {
      commitTimer = null;
      // never re-sort/remap under a live pointer gesture (its indices are
      // captured); the gesture's own pointerup commit picks the edit up
      if (!gesture) commit();
    }, 250);
  }
  function flushCommit() {
    if (commitTimer) commit();
  }

  // Keyboard focus follows the editor: without this, the double-clicked
  // timeline clip keeps DOM focus, and the first Ctrl+C/V or arrow key hits
  // the TIMELINE handlers — stamping a clip or moving it by a beat while
  // the user thinks they are operating on notes.
  $effect(() => {
    void midi.openClipId;
    rollEl?.focus();
  });

  // ── coordinate helpers ──
  const tickAtX = (x: number) => scrollTick + x * tpp;
  const xOfTick = (t: number) => (t - scrollTick) / tpp;
  const keyAtY = (y: number) => topKey - Math.floor(y / KEY_H);
  const yOfKey = (k: number) => (topKey - k) * KEY_H;
  const snapTick = (t: number, floor = false) =>
    Math.max(0, (floor ? Math.floor : Math.round)(t / gridTicks) * gridTicks);

  function noteAt(x: number, y: number): { index: number; resize: boolean } | null {
    const t = tickAtX(x);
    const k = keyAtY(y);
    for (let i = working.length - 1; i >= 0; i--) {
      const n = working[i];
      if (n.key !== k) continue;
      const endX = xOfTick(n.tick + n.lengthTicks);
      if (t >= n.tick && t < n.tick + n.lengthTicks) {
        return { index: i, resize: x > endX - 6 };
      }
      if (x >= endX - 6 && x <= endX + 2) return { index: i, resize: true };
    }
    return null;
  }

  function audition(key: number, velocity = 96) {
    if (track?.instrumentId) void instruments.preview(track.instrumentId, key, velocity);
  }

  // ── gestures ──
  type Gesture =
    | { mode: "move"; start: { x: number; y: number }; orig: MidiNote[]; indices: number[] }
    | { mode: "resize"; start: { x: number; y: number }; orig: MidiNote[]; indices: number[] }
    | { mode: "marquee"; start: { x: number; y: number }; select: MarqueeMode }
    | null;
  let gesture: Gesture = null;
  let marquee = $state<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  let gestureMoved = false;

  function localPos(e: PointerEvent): { x: number; y: number } {
    return canvasPos(gridCanvas!, e.clientX, e.clientY);
  }

  function onGridDown(e: PointerEvent) {
    if (!clip || e.button === 2) return;
    const p = localPos(e);
    gestureMoved = false;
    flushCommit(); // pending arrow edit lands (and remaps selection) BEFORE gesture indices are captured
    rollEl?.focus();
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);

    if (e.shiftKey) {
      // shift-drag marquee; ctrl adds to the selection, alt subtracts
      const select: MarqueeMode = e.ctrlKey || e.metaKey ? "add" : e.altKey ? "subtract" : "replace";
      gesture = { mode: "marquee", start: p, select };
      marquee = { x0: p.x, y0: p.y, x1: p.x, y1: p.y };
      return;
    }

    const hit = noteAt(p.x, p.y);
    if (hit) {
      if (!selection.has(hit.index)) selection = new Set([hit.index]);
      const indices = [...selection];
      gesture = {
        mode: hit.resize ? "resize" : "move",
        start: p,
        orig: indices.map((i) => ({ ...working[i] })),
        indices,
      };
      return;
    }

    // draw a new note at the snapped cell (inside the clip only)
    const key = keyAtY(p.y);
    if (key < 0 || key > 127) return;
    const tick = snapTick(tickAtX(p.x), true);
    if (tick >= midi.effectiveContentLengthTicks(clip)) return;
    const note: MidiNote = {
      tick,
      lengthTicks: gridTicks,
      key,
      velocity: 100,
      // Inherit the clip's own channel rather than assuming 1 — see
      // `channelForNewNote`. A generated drum part lives on channel 10.
      channel: channelForNewNote(working),
    };
    working = [...working, note];
    const idx = working.length - 1;
    selection = new Set([idx]);
    gesture = { mode: "resize", start: p, orig: [{ ...note }], indices: [idx] };
    gestureMoved = true; // creation always commits
    audition(key, 84);
  }

  function onGridMove(e: PointerEvent) {
    if (!gesture) return;
    const p = localPos(e);
    const dx = p.x - gesture.start.x;
    const dy = p.y - gesture.start.y;
    if (Math.abs(dx) > 2 || Math.abs(dy) > 2) gestureMoved = true;

    if (gesture.mode === "marquee") {
      marquee = { x0: gesture.start.x, y0: gesture.start.y, x1: p.x, y1: p.y };
      return;
    }

    const dTick = dx * tpp;
    const next = [...working];
    if (gesture.mode === "move") {
      const dKey = -Math.round(dy / KEY_H);
      for (let j = 0; j < gesture.indices.length; j++) {
        const o = gesture.orig[j];
        let tick = o.tick + dTick;
        if (!e.altKey) tick = snapTick(tick);
        next[gesture.indices[j]] = {
          ...o,
          tick: Math.max(0, Math.round(tick)),
          key: Math.min(127, Math.max(0, o.key + dKey)),
        };
      }
    } else {
      for (let j = 0; j < gesture.indices.length; j++) {
        const o = gesture.orig[j];
        let end = o.tick + o.lengthTicks + dTick;
        if (!e.altKey) end = snapTick(end);
        next[gesture.indices[j]] = {
          ...o,
          lengthTicks: Math.max(e.altKey ? 12 : gridTicks, Math.round(end - o.tick)),
        };
      }
    }
    working = next;
  }

  function onGridUp(e: PointerEvent) {
    if (!gesture) return;
    if (gesture.mode === "marquee") {
      const m = marquee!;
      if (!gestureMoved) {
        // shift-CLICK, not a drag: toggle the note under the pointer in and
        // out of the selection; on empty space a plain shift-click clears it
        // (the zero-area "replace" marquee), ctrl/alt-variants leave it be.
        const hit = noteAt(gesture.start.x, gesture.start.y);
        selection = hit
          ? toggleIndex(selection, hit.index)
          : applyMarquee(selection, [], gesture.select);
        marquee = null;
        gesture = null;
        return;
      }
      const t0 = tickAtX(Math.min(m.x0, m.x1));
      const t1 = tickAtX(Math.max(m.x0, m.x1));
      const kLo = keyAtY(Math.max(m.y0, m.y1));
      const kHi = keyAtY(Math.min(m.y0, m.y1));
      selection = applyMarquee(selection, marqueeHits(working, t0, t1, kLo, kHi), gesture.select);
      if (clip && Math.abs(m.x1 - m.x0) > 4) {
        midi.region = {
          clipId: clip.id,
          startTicks: snapTick(t0, true),
          endTicks: Math.min(midi.effectiveContentLengthTicks(clip), snapTick(t1)),
        };
      }
      marquee = null;
      gesture = null;
      return;
    }
    const moved = gestureMoved;
    gesture = null;
    if (moved) commit();
    (e.currentTarget as HTMLElement).releasePointerCapture?.(e.pointerId);
  }

  function onGridDblClick(e: MouseEvent) {
    const p = canvasPos(gridCanvas!, e.clientX, e.clientY);
    const hit = noteAt(p.x, p.y);
    if (hit) {
      working = working.filter((_, i) => i !== hit.index);
      selection = new Set();
      commit();
    }
  }

  function deleteSelection() {
    if (selection.size === 0) return;
    working = working.filter((_, i) => !selection.has(i));
    selection = new Set();
    commit();
  }

  function onKeydown(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      deleteSelection();
    } else if (e.key === "Escape") {
      // peel back one layer at a time: selection → region band → editor
      if (selection.size) selection = new Set();
      else if (midi.region && clip && midi.region.clipId === clip.id) midi.region = null;
      else {
        flushCommit();
        midi.closeEditor();
      }
    } else if (mod && e.key.toLowerCase() === "a") {
      e.preventDefault();
      selection = new Set(working.map((_, i) => i));
    } else if (mod && (e.key.toLowerCase() === "c" || e.key.toLowerCase() === "x")) {
      if (selection.size === 0) return;
      e.preventDefault();
      midi.noteClipboard = copySelection(working, selection);
      if (e.key.toLowerCase() === "x") deleteSelection();
    } else if (mod && e.key.toLowerCase() === "v") {
      if (!clip || !midi.noteClipboard?.length) return;
      e.preventDefault();
      // paste in place; the pasted notes become the selection so they can be
      // nudged/transposed (arrows) straight away
      const pasted = pasteNotes(working, midi.noteClipboard, midi.effectiveContentLengthTicks(clip));
      working = pasted.notes;
      selection = pasted.selection;
      if (pasted.dropped > 0)
        toasts.info(
          "Paste clipped",
          `${pasted.dropped} note${pasted.dropped === 1 ? "" : "s"} past the clip's content end were skipped`,
        );
      if (pasted.selection.size > 0) commit();
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      if (selection.size === 0) return;
      e.preventDefault();
      const dTick = (e.key === "ArrowRight" ? 1 : -1) * gridTicks;
      const next = nudgeSelection(working, selection, dTick);
      if (next) {
        working = next;
        commitSoon();
      }
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      if (selection.size === 0) return;
      e.preventDefault();
      // semitone; octave with shift
      const dKey = (e.key === "ArrowUp" ? 1 : -1) * (e.shiftKey ? 12 : 1);
      const next = transposeSelection(working, selection, dKey);
      if (next) {
        working = next;
        commitSoon();
      }
    } else if (e.key.toLowerCase() === "q" && !mod && !e.altKey) {
      if (selection.size === 0) return;
      e.preventDefault();
      e.stopPropagation();
      const q = quantizeSelection(working, selection, gridTicks, 1, e.shiftKey);
      working = q.notes;
      selection = q.selection;
      commit();
    }
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      const x = canvasPos(gridCanvas!, e.clientX, e.clientY).x;
      const anchor = tickAtX(x);
      tpp = Math.min(120, Math.max(0.5, tpp * Math.exp(e.deltaY * 0.002)));
      scrollTick = Math.max(0, anchor - x * tpp);
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
      const d = e.deltaX !== 0 ? e.deltaX : e.deltaY;
      scrollTick = Math.max(0, scrollTick + d * tpp);
    } else {
      topKey = Math.min(127, Math.max(12, topKey + Math.round(e.deltaY / 40) * -1));
    }
  }

  // ── velocity lane ──
  let velDragging = false;
  function velApply(e: PointerEvent) {
    const p = canvasPos(velCanvas!, e.clientX, e.clientY);
    const v = Math.min(127, Math.max(1, Math.round((1 - p.y / VEL_H) * 127)));
    const t = tickAtX(p.x);
    // nearest note onset within 0.6 grid cells
    let best = -1;
    let bestDist = gridTicks * 0.6;
    working.forEach((n, i) => {
      const d = Math.abs(n.tick - t);
      if (d < bestDist) {
        best = i;
        bestDist = d;
      }
    });
    if (best >= 0) {
      const next = [...working];
      const targets = selection.has(best) && selection.size > 1 ? [...selection] : [best];
      for (const i of targets) next[i] = { ...next[i], velocity: v };
      working = next;
    }
  }
  function onVelDown(e: PointerEvent) {
    velDragging = true;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    velApply(e);
  }
  function onVelMove(e: PointerEvent) {
    if (velDragging) velApply(e);
  }
  function onVelUp() {
    if (!velDragging) return;
    velDragging = false;
    commit();
  }

  // ── piano keys (audition) ──
  let heldKey = $state(-1);
  function keyFromEvent(e: PointerEvent): number {
    return keyAtY(canvasPos(keysCanvas!, e.clientX, e.clientY).y);
  }
  function onKeysDown(e: PointerEvent) {
    const k = keyFromEvent(e);
    if (k < 0 || k > 127) return;
    heldKey = k;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    if (track?.instrumentId && !track.instrumentId.startsWith("plugin:")) {
      void instruments.previewDown(track.instrumentId, k, 104);
    } else {
      audition(k, 104);
    }
  }
  function onKeysUp() {
    if (track?.instrumentId && !track.instrumentId.startsWith("plugin:")) {
      void instruments.previewUp();
    }
    heldKey = -1;
  }

  // ── sizing ──
  $effect(() => {
    if (!gridWrap) return;
    const el = gridWrap;
    const ro = new ResizeObserver(() => {
      gridW = el.clientWidth;
      gridH = el.clientHeight;
    });
    ro.observe(el);
    gridW = el.clientWidth;
    gridH = el.clientHeight;
    return () => ro.disconnect();
  });

  // ── painting ──
  function setup(c: HTMLCanvasElement, w: number, h: number): CanvasRenderingContext2D | null {
    const dpr = Math.min(3, window.devicePixelRatio || 1);
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.max(1, Math.round(h * dpr));
    const ctx = c.getContext("2d");
    if (!ctx) return null;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    return ctx;
  }

  // grid + notes
  $effect(() => {
    const c = gridCanvas;
    if (!c || !clip) return;
    // deps
    const notes = working;
    const sel = selection;
    const mq = marquee;
    const region = midi.region;
    void scrollTick, tpp, topKey, gridW, gridH, color, gridDiv;
    // The Composer's tint depends on the harmony and its palettes, so a chord
    // edit has to repaint the grid.
    void harmonyOn, clipRegions;

    const w = gridW;
    const h = gridH;
    const ctx = setup(c, w, h);
    if (!ctx) return;

    // pitch rows
    for (let y = 0; y < h; y += KEY_H) {
      const key = keyAtY(y);
      if (key < 0 || key > 127) continue;
      if (BLACK.has(key % 12)) {
        ctx.fillStyle = alpha(theme.tokens.bg0, 0.55);
        ctx.fillRect(0, y, w, KEY_H);
      }
      if (key % 12 === 0) {
        ctx.fillStyle = alpha(theme.tokens.line, 0.16);
        ctx.fillRect(0, y + KEY_H - 1, w, 1);
      } else {
        ctx.fillStyle = alpha(theme.tokens.line, 0.05);
        ctx.fillRect(0, y + KEY_H - 1, w, 1);
      }
    }

    // The Composer's tint, per CHORD REGION (Plan H1 Task 8). Vertical bands,
    // because the answer to "which notes may I play" changes with the chord —
    // that is the whole difference between this and a static scale highlight.
    if (harmonyOn) {
      for (const r of clipRegions) {
        if (!r.palette) continue;
        const x0 = Math.max(0, xOfTick(r.fromTick));
        const x1 = Math.min(w, xOfTick(r.toTick));
        if (x1 <= 0 || x0 >= w) continue;
        for (let y = 0; y < h; y += KEY_H) {
          const key = keyAtY(y);
          if (key < 0 || key > 127) continue;
          const ink = roleInk(r.palette.classes[key % 12].role, theme.tokens);
          if (ink.rowAlpha <= 0) continue;
          ctx.fillStyle = alpha(ink.color, ink.rowAlpha);
          ctx.fillRect(x0, y, x1 - x0, KEY_H - 1);
        }
        // A hairline at each chord change: the bands have to be readable as
        // regions, not as a gradient.
        ctx.fillStyle = alpha(theme.tokens.cyan, 0.22);
        ctx.fillRect(Math.round(x0), 0, 1, h);
      }
    }

    // content bounds shading (past the content — one repetition — end)
    const endX = xOfTick(midi.effectiveContentLengthTicks(clip));
    if (endX < w) {
      ctx.fillStyle = alpha(theme.tokens.bg0, 0.5);
      ctx.fillRect(Math.max(0, endX), 0, w - Math.max(0, endX), h);
    }

    // vertical grid: subdivision / beat / bar
    const firstTick = scrollTick;
    const lastTick = scrollTick + w * tpp;
    const cellPx = gridTicks / tpp;
    const drawSub = cellPx > 7;
    const step = drawSub ? gridTicks : midi.ppq;
    for (let t = Math.floor(firstTick / step) * step; t <= lastTick; t += step) {
      const x = Math.round(xOfTick(t));
      const isBar = t % midi.ticksPerBar === 0;
      const isBeat = t % midi.ppq === 0;
      ctx.fillStyle = isBar
        ? alpha(theme.tokens.line, 0.28)
        : isBeat
          ? alpha(theme.tokens.line, 0.14)
          : alpha(theme.tokens.line, 0.055);
      ctx.fillRect(x, 0, 1, h);
    }

    // AMT region band
    if (region && region.clipId === clip.id) {
      const x0 = xOfTick(region.startTicks);
      const x1 = xOfTick(region.endTicks);
      ctx.fillStyle = alpha(theme.tokens.cyan, 0.07);
      ctx.fillRect(x0, 0, x1 - x0, h);
      ctx.fillStyle = alpha(theme.tokens.cyan, 0.5);
      ctx.fillRect(Math.round(x0), 0, 1, h);
      ctx.fillRect(Math.round(x1), 0, 1, h);
    }

    // notes
    for (let i = 0; i < notes.length; i++) {
      const n = notes[i];
      const x = xOfTick(n.tick);
      const nw = Math.max(3, n.lengthTicks / tpp - 1);
      const y = yOfKey(n.key);
      if (x + nw < 0 || x > w || y + KEY_H < 0 || y > h) continue;
      const lum = 0.35 + 0.65 * (n.velocity / 127);
      ctx.fillStyle = alpha(color, 0.28 + 0.5 * lum);
      ctx.strokeStyle = alpha(color, Math.min(1, 0.55 + 0.45 * lum));
      ctx.beginPath();
      ctx.roundRect(x, y + 1.5, nw, KEY_H - 3, 2.5);
      ctx.fill();
      ctx.stroke();
      // velocity notch at the left edge
      ctx.fillStyle = alpha(color, Math.min(1, 0.75 + 0.25 * lum));
      ctx.fillRect(x + 1, y + 2.5, 2, KEY_H - 5);
      if (sel.has(i)) {
        ctx.strokeStyle = alpha(theme.tokens.text, 0.95);
        ctx.beginPath();
        ctx.roundRect(x - 0.5, y + 1, nw + 1, KEY_H - 2, 3);
        ctx.stroke();
      }
    }

    // marquee
    if (mq) {
      const x = Math.min(mq.x0, mq.x1);
      const y = Math.min(mq.y0, mq.y1);
      ctx.fillStyle = alpha(theme.tokens.cyan, 0.08);
      ctx.strokeStyle = alpha(theme.tokens.cyan, 0.6);
      ctx.setLineDash([4, 3]);
      ctx.fillRect(x, y, Math.abs(mq.x1 - mq.x0), Math.abs(mq.y1 - mq.y0));
      ctx.strokeRect(x, y, Math.abs(mq.x1 - mq.x0), Math.abs(mq.y1 - mq.y0));
      ctx.setLineDash([]);
    }
  });

  // piano keys
  $effect(() => {
    const c = keysCanvas;
    if (!c) return;
    void topKey, gridH, heldKey, harmonyOn, gutterRegion;
    const w = KEYS_W;
    const h = gridH;
    const ctx = setup(c, w, h);
    if (!ctx) return;
    ctx.font = `8px ${getComputedStyle(document.documentElement).getPropertyValue("--font-mono")}`;
    ctx.textBaseline = "middle";
    const gutter = harmonyOn ? gutterRegion?.palette : null;
    for (let y = 0; y < h; y += KEY_H) {
      const key = keyAtY(y);
      if (key < 0 || key > 127) continue;
      const black = BLACK.has(key % 12);
      ctx.fillStyle = black ? theme.tokens.bg1 : theme.tokens.bg3;
      ctx.fillRect(0, y, w, KEY_H - 1);
      // The Composer's tint: what this note DOES over the chord in force.
      const cls = gutter?.classes[key % 12];
      if (cls) {
        const ink = roleInk(cls.role, theme.tokens);
        if (ink.keyAlpha > 0) {
          ctx.fillStyle = alpha(ink.color, ink.keyAlpha);
          ctx.fillRect(0, y, w - 20, KEY_H - 1);
        }
        // The one note that fights the chord gets struck through, so the
        // warning survives a theme with a narrow palette and a viewer who
        // cannot tell the accents apart.
        if (cls.role === "avoid") {
          ctx.fillStyle = alpha(theme.tokens.red, 0.85);
          ctx.fillRect(2, y + Math.floor((KEY_H - 1) / 2), w - 26, 1);
        }
      }
      if (key === heldKey) {
        ctx.fillStyle = alpha(color, 0.55);
        ctx.fillRect(0, y, w, KEY_H - 1);
      }
      if (key % 12 === 0) {
        ctx.fillStyle = alpha(theme.tokens.text, 0.7);
        ctx.fillText(`C${Math.floor(key / 12) - 1}`, w - 18, y + KEY_H / 2);
      }
    }
    ctx.fillStyle = alpha(theme.tokens.line, 0.25);
    ctx.fillRect(w - 1, 0, 1, h);
  });

  // velocity lane
  const VEL_H = 56;
  $effect(() => {
    const c = velCanvas;
    if (!c || !clip) return;
    const notes = working;
    const sel = selection;
    void scrollTick, tpp, gridW, color;
    const w = gridW;
    const h = VEL_H;
    const ctx = setup(c, w, h);
    if (!ctx) return;
    ctx.fillStyle = alpha(theme.tokens.line, 0.12);
    ctx.fillRect(0, 0, w, 1);
    for (let i = 0; i < notes.length; i++) {
      const n = notes[i];
      const x = Math.round(xOfTick(n.tick));
      if (x < -4 || x > w + 4) continue;
      const bh = Math.max(2, (n.velocity / 127) * (h - 6));
      const selHere = sel.has(i);
      ctx.fillStyle = selHere ? alpha(theme.tokens.text, 0.9) : alpha(color, 0.55);
      ctx.fillRect(x, h - bh, 3, bh);
      ctx.fillStyle = selHere ? theme.tokens.textOnAccent : alpha(color, 0.95);
      ctx.fillRect(x, h - bh - 2, 3, 2);
    }
  });

  // ── note flash + selection pulse overlay — rAF, no reactivity inside ──
  //
  // Two composed effects on dedicated overlay canvases (the state-driven
  // grid/keys repaints stay untouched):
  //  * play-flash (governed by the FLASH toggle): notes flare the instant
  //    the playhead crosses their onset and decay fast (~180 ms), with a
  //    faint sustain while held; the piano-key column lights sounding keys.
  //  * selection pulse (always on — it is a selection affordance): selected
  //    notes breathe a slow glow, clearly distinct from the punchy flash.
  //    Both compose: a selected note still flares white when hit.
  const FLASH_DECAY = 9; // 1/s — exp decay rate of the onset flare
  const PULSE_HZ = 0.9; // breathing rate of the selection glow
  function clearFlash() {
    for (const c of [flashCanvas, keysFlashCanvas]) {
      const ctx = c?.getContext("2d");
      if (c && ctx) {
        ctx.setTransform(1, 0, 0, 1, 0, 0);
        ctx.clearRect(0, 0, c.width, c.height);
      }
    }
  }

  /** Like setup(), but only reallocates the bitmap when the size changed. */
  function overlayCtx(c: HTMLCanvasElement, w: number, h: number): CanvasRenderingContext2D | null {
    const dpr = Math.min(3, window.devicePixelRatio || 1);
    const pw = Math.max(1, Math.round(w * dpr));
    const ph = Math.max(1, Math.round(h * dpr));
    if (c.width !== pw || c.height !== ph) {
      c.width = pw;
      c.height = ph;
    }
    const ctx = c.getContext("2d");
    if (!ctx) return null;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.globalCompositeOperation = "source-over";
    ctx.clearRect(0, 0, w, h);
    return ctx;
  }

  function drawFlash(nowMs: number) {
    const c = clip;
    const fc = flashCanvas;
    const kc = keysFlashCanvas;
    if (!c || !fc || !kc) return;
    const fctx = overlayCtx(fc, gridW, gridH);
    const kctx = overlayCtx(kc, KEYS_W, gridH);
    if (!fctx || !kctx) return;

    // selection pulse — slow breathing glow, independent of playback
    if (selection.size > 0) {
      const breathe = 0.5 + 0.5 * Math.sin(nowMs * 0.001 * PULSE_HZ * 2 * Math.PI);
      const glow = 0.3 + 0.5 * breathe;
      for (const i of selection) {
        const n = working[i];
        if (!n) continue;
        const y = yOfKey(n.key);
        const x = xOfTick(n.tick);
        const nw = Math.max(3, n.lengthTicks / tpp - 1);
        if (x + nw < 0 || x > gridW || y + KEY_H < 0 || y > gridH) continue;
        fctx.strokeStyle = alpha(theme.tokens.cyan, 0.35 + 0.55 * glow);
        fctx.lineWidth = 1.5 + 1.5 * glow;
        fctx.beginPath();
        fctx.roundRect(x - 1.5, y + 0.5, nw + 3, KEY_H - 1, 3.5);
        fctx.stroke();
        fctx.fillStyle = alpha(theme.tokens.cyan, 0.16 * glow);
        fctx.beginPath();
        fctx.roundRect(x - 3, y - 1, nw + 6, KEY_H + 1, 4.5);
        fctx.fill();
      }
    }

    // play-flash — only while rolling with the FLASH toggle on.
    // Notes are already bright, so the flash must OUTSHINE them: at onset the
    // note body inverts to hot white inside a magenta bloom (the opposite
    // half of the duotone — nothing else in the roll is magenta), decaying
    // fast to a soft track-color sustain while the note is held.
    if (!(prefs.values.noteFlash && transport.isPlaying)) return;
    // Visibility gate stays against the PLACEMENT (the flash must stop once
    // playback runs past the clip entirely), but note matching wraps modulo
    // the CONTENT length — the piano roll shows one repetition, so every
    // repeat's playback flashes the same displayed notes.
    const posTicks = midi.samplesToTicks(transport.positionAt(nowMs)) - c.timelineStartTicks;
    if (posTicks < 0 || posTicks > c.lengthTicks) return;
    const contentTicks = midi.effectiveContentLengthTicks(c);
    const posInContent = ((posTicks % contentTicks) + contentTicks) % contentTicks;
    const secPerTick = 60 / (project.tempoBpm * midi.ppq);
    const MAG = theme.tokens.magenta;
    fctx.globalCompositeOperation = "lighter";
    const keySustain = new Map<number, number>();
    const keyFlare = new Map<number, number>();
    for (const n of working) {
      const age = posInContent - n.tick;
      if (age < 0) continue;
      const held = age < n.lengthTicks;
      const flare = Math.exp(-age * secPerTick * FLASH_DECAY);
      if (!held && flare < 0.03) continue;
      const y = yOfKey(n.key);
      const x = xOfTick(n.tick);
      const nw = Math.max(3, n.lengthTicks / tpp - 1);
      if (x + nw >= 0 && x <= gridW && y + KEY_H >= 0 && y <= gridH) {
        if (flare >= 0.03) {
          // wide magenta bloom around the note
          fctx.fillStyle = alpha(MAG, 0.55 * flare);
          fctx.beginPath();
          fctx.roundRect(x - 6, y - 3.5, nw + 12, KEY_H + 4, 6);
          fctx.fill();
          // magenta rim
          fctx.strokeStyle = alpha(MAG, Math.min(1, 1.2 * flare));
          fctx.lineWidth = 2;
          fctx.beginPath();
          fctx.roundRect(x - 2.5, y - 1, nw + 5, KEY_H, 4);
          fctx.stroke();
          // hot white core — at peak the note body reads solid white
          fctx.fillStyle = alpha(theme.tokens.textOnAccent, Math.min(1, 0.95 * flare));
          fctx.beginPath();
          fctx.roundRect(x - 0.5, y + 1, nw + 1, KEY_H - 2, 2.5);
          fctx.fill();
        }
        if (held) {
          // soft sustain glow in the track color while the note sounds
          fctx.fillStyle = alpha(color, 0.25);
          fctx.beginPath();
          fctx.roundRect(x - 2, y - 0.5, nw + 4, KEY_H - 1, 4);
          fctx.fill();
        }
      }
      if (held) keySustain.set(n.key, Math.max(keySustain.get(n.key) ?? 0, 0.3));
      if (flare > 0.05) keyFlare.set(n.key, Math.max(keyFlare.get(n.key) ?? 0, flare));
    }
    for (const [key, glow] of keySustain) {
      const y = yOfKey(key);
      if (y + KEY_H < 0 || y > gridH) continue;
      kctx.fillStyle = alpha(color, 2 * glow);
      kctx.fillRect(0, y, KEYS_W, KEY_H - 1);
    }
    for (const [key, flare] of keyFlare) {
      const y = yOfKey(key);
      if (y + KEY_H < 0 || y > gridH) continue;
      kctx.fillStyle = alpha(MAG, 0.85 * flare);
      kctx.fillRect(0, y, KEYS_W, KEY_H - 1);
    }
  }

  $effect(() => {
    // deps: clip presence, overlay mounts, selection, toggle, run state
    const c = clip;
    const hasSelection = selection.size > 0;
    const flashing = prefs.values.noteFlash && transport.isPlaying;
    void flashCanvas, keysFlashCanvas;
    if (!c || (!hasSelection && !flashing)) {
      clearFlash();
      return;
    }
    let raf = 0;
    const loop = (now: number) => {
      raf = requestAnimationFrame(loop);
      drawFlash(now);
    };
    raf = requestAnimationFrame(loop);
    return () => {
      cancelAnimationFrame(raf);
      clearFlash();
    };
  });

  // playhead overlay — rAF, no reactivity
  $effect(() => {
    const c = clip;
    if (!c) return;
    let raf = 0;
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick);
      if (!playheadEl) return;
      // The overlay stays visible for the full PLACEMENT duration but wraps
      // its X position modulo the CONTENT length, so it re-enters at the
      // left edge on every repeat instead of running off the right side of
      // the (one-repetition-wide) grid (spec §6).
      const posTicks = midi.samplesToTicks(transport.positionAt(now)) - c.timelineStartTicks;
      const contentTicks = midi.effectiveContentLengthTicks(c);
      const posInContent = ((posTicks % contentTicks) + contentTicks) % contentTicks;
      const x = xOfTick(posInContent);
      const visible = posTicks >= 0 && posTicks <= c.lengthTicks && x >= -1 && x <= gridW + 1;
      playheadEl.style.opacity = visible ? "1" : "0";
      if (visible) playheadEl.style.transform = `translateX(${x.toFixed(2)}px)`;
      // The Composer's gutter follows the chord under the playhead. Assigned
      // only on a crossing (see `gutterFrom`).
      const region = untrack(() => clipRegions).find(
        (r) => posInContent >= r.fromTick && posInContent < r.toTick,
      );
      const from = region?.fromTick ?? null;
      if (from !== untrack(() => gutterFrom)) gutterFrom = from;
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  // scrollbar reach: one repetition of content plus a bar of headroom
  const contentEndTicks = $derived(
    clip ? midi.effectiveContentLengthTicks(clip) + midi.ticksPerBar : 0,
  );

  const GRID_CHOICES = [
    { label: "1/4", div: 1 },
    { label: "1/8", div: 2 },
    { label: "1/16", div: 4 },
    { label: "1/32", div: 8 },
  ];
</script>

{#if clip}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- role="application": the editor is one composite widget; keyboard is handled at the container -->
  <div
    bind:this={rollEl}
    class="roll glass"
    role="application"
    aria-label="Piano roll — {clip.name}"
    tabindex="-1"
    style:height="{ui.rollHeight}px"
    onkeydown={onKeydown}
  >
    <PanelResizeHandle
      axis="y"
      size={ui.rollHeight}
      spec={ROLL_RESIZE}
      label="Resize piano roll"
      onresize={(px) => (ui.rollHeight = px)}
    />
    <header class="head">
      <BottomPanelTabs current="roll" />
      <span class="dot" style:background={color}></span>
      <span class="title mono">{clip.name}</span>
      <span class="silk">on {track?.name ?? "?"}</span>

      <button
        class="inst mono"
        class:launch={!!usedLauncher}
        title={usedLauncher
          ? `Uses launcher ${usedLauncher.name} — open the launch map`
          : pluginInst
            ? `Plugin: ${pluginInst.name} (${pluginInst.format}, ${pluginInst.status}) — open params`
            : instrument
              ? `Instrument: ${instrument.name}`
              : "No instrument — open the browser"}
        onclick={() => {
          if (usedLauncher) {
            launch.selectMap(usedLauncher.id);
            launch.panelOpen = true;
          } else if (pluginInst) {
            void openPluginParams(pluginInst.id);
          } else {
            ui.dock = "instruments";
          }
        }}
      >
        {#if usedLauncher}
          ▶ {launch.overlay ? `${usedLauncher.name} · playing ${launch.overlay.name}` : usedLauncher.name}
        {:else if pluginInst}
          ⚡ {pluginInst.name}
        {:else}
          ⌁ {instrument ? instrument.name : "no instrument"}
        {/if}
      </button>

      <span class="spacer"></span>

      <div class="gridsel" role="group" aria-label="Grid">
        <span class="silk">grid</span>
        {#each GRID_CHOICES as g (g.div)}
          <button class="chip mono" class:on={gridDiv === g.div} onclick={() => (gridDiv = g.div)}>
            {g.label}
          </button>
        {/each}
      </div>

      <button
        class="chip mono"
        class:on={clipEditLoop.playOnce}
        title="Play this clip once — other clips do not drive the launcher"
        onclick={() => {
          if (!clip) return;
          const start = midi.ticksToSamples(clip.timelineStartTicks);
          const end = midi.ticksToSamples(clip.timelineStartTicks + midi.effectiveContentLengthTicks(clip));
          void clipEditLoop.playThisClip(clip.id, start, end);
        }}
      >
        {clipEditLoop.playOnce ? "■ stop" : "▶ play"}
      </button>

      <button
        class="chip mono"
        class:on={clipEditLoop.solo}
        title="Loop the clip solo (other tracks muted); off = loop with the full mix"
        onclick={() => void clipEditLoop.setSolo(!clipEditLoop.solo)}
      >
        ◎ solo
      </button>

      <button
        class="chip mono"
        class:on={prefs.values.noteFlash}
        title="Flash notes at the moment they play"
        onclick={() => prefs.set("noteFlash", !prefs.values.noteFlash)}
      >
        ⚡ flash
      </button>

      {#if composer.chords.length > 0}
        <!-- The Composer's tint. The label is the chord in force, so the chip
             doubles as a readout of what the colours currently mean. -->
        <button
          class="chip mono"
          class:on={harmonyTint}
          title={harmonyHint}
          onclick={() => (harmonyTint = !harmonyTint)}
        >
          ♪ {gutterRegion?.palette?.chordPretty ?? gutterRegion?.chord ?? "harmony"}{gutterRegion
            ?.palette?.roman
            ? ` · ${gutterRegion.palette.roman}`
            : ""}
        </button>
      {/if}

      {#if midi.region && midi.region.clipId === clip.id}
        <button
          class="infill mono"
          title="Fill the selected region with AMT"
          onclick={() => openStudio("amtInfill")}
        >
          <span class="spark">✦</span> INFILL REGION
        </button>
      {/if}

      <button
        class="close mono"
        title="Close (esc)"
        onclick={() => {
          flushCommit();
          midi.closeEditor();
        }}>×</button
      >
    </header>

    <div class="body">
      <div class="keyswrap" style:width="{KEYS_W}px">
        <canvas
          bind:this={keysCanvas}
          class="keys"
          style:width="{KEYS_W}px"
          onpointerdown={onKeysDown}
          onpointerup={onKeysUp}
          onpointerleave={onKeysUp}
        ></canvas>
        <canvas bind:this={keysFlashCanvas} class="overlay" style:width="{KEYS_W}px"></canvas>
      </div>

      <div class="gridwrap" id="pianoroll-grid" bind:this={gridWrap} onwheel={onWheel}>
        <canvas
          bind:this={gridCanvas}
          class="grid"
          onpointerdown={onGridDown}
          onpointermove={onGridMove}
          onpointerup={onGridUp}
          ondblclick={onGridDblClick}
        ></canvas>
        <canvas bind:this={flashCanvas} class="overlay"></canvas>
        <div bind:this={playheadEl} class="playhead"></div>
      </div>
    </div>

    <div class="scrollrow">
      <div class="scrollcorner" style:width="{KEYS_W}px"></div>
      <HScrollbar
        start={scrollTick}
        viewSpan={gridW * tpp}
        contentEnd={contentEndTicks}
        label="Scroll piano roll"
        controls="pianoroll-grid"
        onscroll={(t) => (scrollTick = t)}
      />
    </div>

    <div class="velrow">
      <div class="vellabel silk" style:width="{KEYS_W}px">vel</div>
      <canvas
        bind:this={velCanvas}
        class="vel"
        style:height="{VEL_H}px"
        onpointerdown={onVelDown}
        onpointermove={onVelMove}
        onpointerup={onVelUp}
        onpointerleave={onVelUp}
      ></canvas>
    </div>

    {#if clip.contentId && track}
      <ClipEnvelopeLane
        contentId={clip.contentId}
        {track}
        {tpp}
        {scrollTick}
        contentLengthTicks={midi.effectiveContentLengthTicks(clip)}
        keysWidth={KEYS_W}
      />
    {/if}

    <footer class="hints silk">
      draw · drag to move · edge-drag resize · dbl-click delete · shift-drag select (+ctrl add, +alt
      remove) · shift-click toggle · ctrl+a all · esc none · ctrl+c/x/v copy/cut/paste · arrows
      nudge, ±pitch (shift = octave) · ctrl+wheel zoom
    </footer>
  </div>
{:else}
  <div class="roll glass" style:height="{ui.rollHeight}px">
    <PanelResizeHandle
      axis="y"
      size={ui.rollHeight}
      spec={ROLL_RESIZE}
      label="Resize piano roll"
      onresize={(px) => (ui.rollHeight = px)}
    />
    <header class="head">
      <BottomPanelTabs current="roll" />
      <span class="silk">open a MIDI clip to edit notes</span>
    </header>
  </div>
{/if}

<style>
  .roll {
    flex: none;
    height: 340px;
    display: flex;
    flex-direction: column;
    border-left: none;
    border-right: none;
    border-bottom: none;
    background: rgb(var(--bg-sunken-rgb) / 0.92);
    position: relative;
    z-index: 10;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 12px;
    border-bottom: var(--border-width) solid var(--glass-border);
    flex: none;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    box-shadow: 0 0 calc(8px * var(--glow-scale)) currentColor;
  }
  .title {
    font-size: 12px;
    color: var(--text);
  }
  .spacer {
    flex: 1;
  }

  .inst {
    font-size: 9px;
    letter-spacing: 0.1em;
    padding: 3px 8px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.5);
    color: var(--text-dim);
    cursor: pointer;
  }
  .inst:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .inst.launch {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .gridsel {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .chip {
    font-size: 9px;
    padding: 3px 6px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .chip.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .infill {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 4px 9px;
    border-radius: 4px;
    border: none;
    cursor: pointer;
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--cyan), var(--violet));
    box-shadow: 0 0 calc(12px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.3);
  }
  .infill:hover {
    filter: brightness(1.15);
  }
  .spark {
    font-size: 10px;
  }

  .close {
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 15px;
    cursor: pointer;
    padding: 0 4px;
  }
  .close:hover {
    color: var(--red);
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  .keyswrap {
    flex: none;
    position: relative;
    height: 100%;
  }
  .keys {
    position: absolute;
    inset: 0;
    height: 100%;
    cursor: pointer;
    touch-action: none;
  }
  .overlay {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
  .gridwrap {
    flex: 1;
    min-width: 0;
    position: relative;
    overflow: hidden;
  }
  .grid {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    touch-action: none;
    cursor: crosshair;
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    width: 1px;
    background: var(--cyan);
    box-shadow: 0 0 calc(6px * var(--glow-scale)) var(--cyan-dim);
    pointer-events: none;
    will-change: transform;
  }

  .scrollrow {
    flex: none;
    display: flex;
    border-top: 1px solid rgb(var(--line-rgb) / 0.12);
  }
  .scrollcorner {
    flex: none;
    border-right: 1px solid rgb(var(--line-rgb) / 0.25);
  }

  .velrow {
    flex: none;
    display: flex;
    border-top: 1px solid var(--glass-border);
  }
  .vellabel {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    padding-right: 10px;
    border-right: 1px solid rgb(var(--line-rgb) / 0.25);
  }
  .vel {
    flex: 1;
    min-width: 0;
    touch-action: none;
    cursor: ns-resize;
  }

  .hints {
    flex: none;
    padding: 4px 12px 6px;
    border-top: 1px solid rgb(var(--line-rgb) / 0.08);
  }
</style>
