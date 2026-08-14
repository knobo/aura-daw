<script lang="ts">
  /**
   * Canvas piano roll for one MIDI clip. Pitch rows × tick columns, notes as
   * glowing bars in the track color (velocity → luminance). Draw on empty,
   * drag to move, edge-drag to resize, double-click / Delete to remove;
   * shift-drag marquees a tick region (feeds AMT infill) and selects notes.
   * Velocity lane below; piano keys at left audition through
   * sampler_preview_note when the track carries an instrument.
   * Same discipline as the timeline: canvases repaint on state changes only,
   * the playhead is a rAF-transformed overlay outside Svelte reactivity.
   */
  import { clipEditLoop } from "../../state/clip-edit-loop.svelte";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { transport } from "../../state/transport.svelte";
  import { instruments } from "../../state/instruments.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { openStudio, ui } from "../../state/ui.svelte";
  import { ROLL_RESIZE } from "../../utils/panel-resize";
  import HScrollbar from "../HScrollbar.svelte";
  import PanelResizeHandle from "../PanelResizeHandle.svelte";
  import type { MidiNote } from "../../types/ipc";

  const KEY_H = 14; // CSS px per pitch row
  const KEYS_W = 64;
  const BLACK = new Set([1, 3, 6, 8, 10]);

  const clip = $derived(midi.openClip);
  const track = $derived(clip ? project.trackById(clip.trackId) : undefined);
  const color = $derived(track?.color ?? "#5cf2b8");
  const instrument = $derived(instruments.byId(track?.instrumentId));
  const pluginInst = $derived(plugins.instanceForRef(track?.instrumentId));

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
      if (!gesture) working = c.notes.map((n) => ({ ...n }));
    }
  });

  function commit() {
    if (!clip) return;
    void midi.setNotes(clip.id, working.map((n) => ({ ...n })));
  }

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
    | { mode: "marquee"; start: { x: number; y: number }; toX: number; toY: number }
    | null;
  let gesture: Gesture = null;
  let marquee = $state<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  let gestureMoved = false;

  function localPos(e: PointerEvent): { x: number; y: number } {
    const rect = gridCanvas!.getBoundingClientRect();
    return { x: e.clientX - rect.left, y: e.clientY - rect.top };
  }

  function onGridDown(e: PointerEvent) {
    if (!clip || e.button === 2) return;
    const p = localPos(e);
    gestureMoved = false;
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);

    if (e.shiftKey) {
      gesture = { mode: "marquee", start: p, toX: p.x, toY: p.y };
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
    const note: MidiNote = { tick, lengthTicks: gridTicks, key, velocity: 100, channel: 0 };
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
      const t0 = tickAtX(Math.min(m.x0, m.x1));
      const t1 = tickAtX(Math.max(m.x0, m.x1));
      const kLo = keyAtY(Math.max(m.y0, m.y1));
      const kHi = keyAtY(Math.min(m.y0, m.y1));
      const sel = new Set<number>();
      working.forEach((n, i) => {
        if (n.tick + n.lengthTicks > t0 && n.tick < t1 && n.key >= kLo && n.key <= kHi) sel.add(i);
      });
      selection = sel;
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
    const rect = gridCanvas!.getBoundingClientRect();
    const hit = noteAt(e.clientX - rect.left, e.clientY - rect.top);
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
    if (e.key === "Delete" || e.key === "Backspace") {
      e.preventDefault();
      deleteSelection();
    } else if (e.key === "Escape") {
      if (selection.size) selection = new Set();
      else midi.closeEditor();
    } else if (e.key === "a" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      selection = new Set(working.map((_, i) => i));
    }
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      const rect = gridCanvas!.getBoundingClientRect();
      const x = e.clientX - rect.left;
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
    const rect = velCanvas!.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const v = Math.min(127, Math.max(1, Math.round((1 - (e.clientY - rect.top) / rect.height) * 127)));
    const t = tickAtX(x);
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
    const rect = keysCanvas!.getBoundingClientRect();
    return keyAtY(e.clientY - rect.top);
  }
  function onKeysDown(e: PointerEvent) {
    const k = keyFromEvent(e);
    if (k < 0 || k > 127) return;
    heldKey = k;
    audition(k, 104);
  }
  function onKeysUp() {
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

    const w = gridW;
    const h = gridH;
    const ctx = setup(c, w, h);
    if (!ctx) return;

    // pitch rows
    for (let y = 0; y < h; y += KEY_H) {
      const key = keyAtY(y);
      if (key < 0 || key > 127) continue;
      if (BLACK.has(key % 12)) {
        ctx.fillStyle = "rgba(5, 7, 13, 0.55)";
        ctx.fillRect(0, y, w, KEY_H);
      }
      if (key % 12 === 0) {
        ctx.fillStyle = "rgba(96,130,190,0.16)";
        ctx.fillRect(0, y + KEY_H - 1, w, 1);
      } else {
        ctx.fillStyle = "rgba(96,130,190,0.05)";
        ctx.fillRect(0, y + KEY_H - 1, w, 1);
      }
    }

    // content bounds shading (past the content — one repetition — end)
    const endX = xOfTick(midi.effectiveContentLengthTicks(clip));
    if (endX < w) {
      ctx.fillStyle = "rgba(5,7,13,0.5)";
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
        ? "rgba(96,130,190,0.28)"
        : isBeat
          ? "rgba(96,130,190,0.14)"
          : "rgba(96,130,190,0.055)";
      ctx.fillRect(x, 0, 1, h);
    }

    // AMT region band
    if (region && region.clipId === clip.id) {
      const x0 = xOfTick(region.startTicks);
      const x1 = xOfTick(region.endTicks);
      ctx.fillStyle = "rgba(82,229,255,0.07)";
      ctx.fillRect(x0, 0, x1 - x0, h);
      ctx.fillStyle = "rgba(82,229,255,0.5)";
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
      ctx.fillStyle = hexA(color, 0.28 + 0.5 * lum);
      ctx.strokeStyle = hexA(color, Math.min(1, 0.55 + 0.45 * lum));
      ctx.beginPath();
      ctx.roundRect(x, y + 1.5, nw, KEY_H - 3, 2.5);
      ctx.fill();
      ctx.stroke();
      // velocity notch at the left edge
      ctx.fillStyle = hexA(color, Math.min(1, 0.75 + 0.25 * lum));
      ctx.fillRect(x + 1, y + 2.5, 2, KEY_H - 5);
      if (sel.has(i)) {
        ctx.strokeStyle = "rgba(216,227,242,0.95)";
        ctx.beginPath();
        ctx.roundRect(x - 0.5, y + 1, nw + 1, KEY_H - 2, 3);
        ctx.stroke();
      }
    }

    // marquee
    if (mq) {
      const x = Math.min(mq.x0, mq.x1);
      const y = Math.min(mq.y0, mq.y1);
      ctx.fillStyle = "rgba(82,229,255,0.08)";
      ctx.strokeStyle = "rgba(82,229,255,0.6)";
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
    void topKey, gridH, heldKey;
    const w = KEYS_W;
    const h = gridH;
    const ctx = setup(c, w, h);
    if (!ctx) return;
    ctx.font = `8px ${getComputedStyle(document.documentElement).getPropertyValue("--font-mono")}`;
    ctx.textBaseline = "middle";
    for (let y = 0; y < h; y += KEY_H) {
      const key = keyAtY(y);
      if (key < 0 || key > 127) continue;
      const black = BLACK.has(key % 12);
      ctx.fillStyle = black ? "#0a0d17" : "#1b2340";
      ctx.fillRect(0, y, w, KEY_H - 1);
      if (key === heldKey) {
        ctx.fillStyle = hexA(color, 0.55);
        ctx.fillRect(0, y, w, KEY_H - 1);
      }
      if (key % 12 === 0) {
        ctx.fillStyle = "rgba(216,227,242,0.7)";
        ctx.fillText(`C${Math.floor(key / 12) - 1}`, w - 18, y + KEY_H / 2);
      }
    }
    ctx.fillStyle = "rgba(96,130,190,0.25)";
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
    ctx.fillStyle = "rgba(96,130,190,0.12)";
    ctx.fillRect(0, 0, w, 1);
    for (let i = 0; i < notes.length; i++) {
      const n = notes[i];
      const x = Math.round(xOfTick(n.tick));
      if (x < -4 || x > w + 4) continue;
      const bh = Math.max(2, (n.velocity / 127) * (h - 6));
      const selHere = sel.has(i);
      ctx.fillStyle = selHere ? "rgba(216,227,242,0.9)" : hexA(color, 0.55);
      ctx.fillRect(x, h - bh, 3, bh);
      ctx.fillStyle = selHere ? "#fff" : hexA(color, 0.95);
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
        fctx.strokeStyle = hexA("#52e5ff", 0.35 + 0.55 * glow);
        fctx.lineWidth = 1.5 + 1.5 * glow;
        fctx.beginPath();
        fctx.roundRect(x - 1.5, y + 0.5, nw + 3, KEY_H - 1, 3.5);
        fctx.stroke();
        fctx.fillStyle = hexA("#52e5ff", 0.16 * glow);
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
    if (!(ui.noteFlash && transport.isPlaying)) return;
    // Visibility gate stays against the PLACEMENT (the flash must stop once
    // playback runs past the clip entirely), but note matching wraps modulo
    // the CONTENT length — the piano roll shows one repetition, so every
    // repeat's playback flashes the same displayed notes.
    const posTicks = midi.samplesToTicks(transport.positionAt(nowMs)) - c.timelineStartTicks;
    if (posTicks < 0 || posTicks > c.lengthTicks) return;
    const contentTicks = midi.effectiveContentLengthTicks(c);
    const posInContent = ((posTicks % contentTicks) + contentTicks) % contentTicks;
    const secPerTick = 60 / (project.tempoBpm * midi.ppq);
    const MAG = "#ff4fd8";
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
          fctx.fillStyle = hexA(MAG, 0.55 * flare);
          fctx.beginPath();
          fctx.roundRect(x - 6, y - 3.5, nw + 12, KEY_H + 4, 6);
          fctx.fill();
          // magenta rim
          fctx.strokeStyle = hexA(MAG, Math.min(1, 1.2 * flare));
          fctx.lineWidth = 2;
          fctx.beginPath();
          fctx.roundRect(x - 2.5, y - 1, nw + 5, KEY_H, 4);
          fctx.stroke();
          // hot white core — at peak the note body reads solid white
          fctx.fillStyle = `rgba(255,255,255,${Math.min(1, 0.95 * flare).toFixed(3)})`;
          fctx.beginPath();
          fctx.roundRect(x - 0.5, y + 1, nw + 1, KEY_H - 2, 2.5);
          fctx.fill();
        }
        if (held) {
          // soft sustain glow in the track color while the note sounds
          fctx.fillStyle = hexA(color, 0.25);
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
      kctx.fillStyle = hexA(color, 2 * glow);
      kctx.fillRect(0, y, KEYS_W, KEY_H - 1);
    }
    for (const [key, flare] of keyFlare) {
      const y = yOfKey(key);
      if (y + KEY_H < 0 || y > gridH) continue;
      kctx.fillStyle = hexA(MAG, 0.85 * flare);
      kctx.fillRect(0, y, KEYS_W, KEY_H - 1);
    }
  }

  $effect(() => {
    // deps: clip presence, overlay mounts, selection, toggle, run state
    const c = clip;
    const hasSelection = selection.size > 0;
    const flashing = ui.noteFlash && transport.isPlaying;
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
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  function hexA(hex: string, a: number): string {
    const r = parseInt(hex.slice(1, 3), 16);
    const g = parseInt(hex.slice(3, 5), 16);
    const b = parseInt(hex.slice(5, 7), 16);
    return `rgba(${r},${g},${b},${a})`;
  }

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
      <span class="dot" style:background={color}></span>
      <span class="title mono">{clip.name}</span>
      <span class="silk">on {track?.name ?? "?"}</span>

      <button
        class="inst mono"
        title={pluginInst
          ? `Plugin: ${pluginInst.name} (${pluginInst.format}, ${pluginInst.status}) — open params`
          : instrument
            ? `Instrument: ${instrument.name}`
            : "No instrument — open the browser"}
        onclick={() => {
          if (pluginInst) {
            ui.dock = "plugins";
            void plugins.openParams(pluginInst.id);
          } else {
            ui.dock = "instruments";
          }
        }}
      >
        {#if pluginInst}
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
        class:on={clipEditLoop.solo}
        title="Loop the clip solo (other tracks muted); off = loop with the full mix"
        onclick={() => void clipEditLoop.setSolo(!clipEditLoop.solo)}
      >
        ◎ solo
      </button>

      <button
        class="chip mono"
        class:on={ui.noteFlash}
        title="Flash notes at the moment they play"
        onclick={() => (ui.noteFlash = !ui.noteFlash)}
      >
        ⚡ flash
      </button>

      {#if midi.region && midi.region.clipId === clip.id}
        <button
          class="infill mono"
          title="Fill the selected region with AMT"
          onclick={() => openStudio("amtInfill")}
        >
          <span class="spark">✦</span> INFILL REGION
        </button>
      {/if}

      <button class="close mono" title="Close (esc)" onclick={() => midi.closeEditor()}>×</button>
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

    <footer class="hints silk">
      draw · drag to move · edge-drag to resize · dbl-click delete · shift-drag region · ctrl+wheel zoom
    </footer>
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
    background: rgba(8, 10, 19, 0.92);
    position: relative;
    z-index: 10;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 12px;
    border-bottom: 1px solid var(--glass-border);
    flex: none;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    box-shadow: 0 0 8px currentColor;
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
    border: 1px solid var(--glass-border);
    background: rgba(5, 7, 13, 0.5);
    color: var(--text-dim);
    cursor: pointer;
  }
  .inst:hover {
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
    border: 1px solid var(--glass-border);
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
    box-shadow: 0 0 12px rgba(82, 229, 255, 0.3);
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
    box-shadow: 0 0 6px var(--cyan-dim);
    pointer-events: none;
    will-change: transform;
  }

  .scrollrow {
    flex: none;
    display: flex;
    border-top: 1px solid rgba(96, 130, 190, 0.12);
  }
  .scrollcorner {
    flex: none;
    border-right: 1px solid rgba(96, 130, 190, 0.25);
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
    border-right: 1px solid rgba(96, 130, 190, 0.25);
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
    border-top: 1px solid rgba(96, 130, 190, 0.08);
  }
</style>
