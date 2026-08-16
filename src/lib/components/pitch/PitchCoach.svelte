<script lang="ts">
  /**
   * Pitch Coach — the bottom panel that shows what you are actually singing
   * against the reference melody.
   *
   * Two views on one canvas, one rAF loop. While the transport rolls it is a
   * LANE: target bars scrolling under a pinned playhead with your pitch drawn
   * as a continuous trail. Stopped, it is a TUNER: the note you are holding,
   * how far off it is, and — the number this panel leads with — how steady
   * you are holding it.
   *
   * Steadiness is the headline for a measured reason (spec R3). Distance to
   * the nearest note saturates near 50 cents for anyone sitting midway
   * between two semitones, which is where a person who cannot yet hit a
   * pitch lives: it reads as failure and is not one. Steadiness improves
   * while accuracy is still poor, so it is the number that shows progress.
   *
   * Thin renderer (ADR 0006): nothing here decides anything. Frames come
   * from the bus, notes from the midi store through its own bijection, and
   * mode changes are pushed back by `pitch://state` — never set locally.
   * The frame ring is read only inside the rAF loop, never in a reactive
   * block, or 100 Hz of frames would invalidate the whole UI.
   */
  import { backend } from "../../tauri";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { transport } from "../../state/transport.svelte";
  import { ui } from "../../state/ui.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import { TOLERANCE_CENTS } from "../../prefs/schema";
  import { pitchMode, recentFrames, startPitchStream, stopPitchStream } from "../../state/pitch.svelte";
  import { autoFitRange, bandFor, centsToTarget, trailSegments, yOfKey, type LaneRange } from "../../pitch/lane";
  import { laneScrollFor, stabilityCents, targetNotesFor } from "./panel-logic";
  import { view } from "../../state/view.svelte";
  import { ROLL_RESIZE } from "../../utils/panel-resize";
  import PanelResizeHandle from "../PanelResizeHandle.svelte";
  import RehearseButton from "./RehearseButton.svelte";
  import type { PitchFrame } from "../../types/ipc";

  const NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

  /** Colours by band. Magenta, not red: red means recording here, and being
   * 40 cents flat is ordinary singing, not an error. */
  const BAND_COLOR: Record<string, string> = {
    in: "#52e5ff",
    near: "#ffc857",
    far: "#ff4fd8",
  };
  /** No target sounding: the trail is information, not a verdict. */
  const NO_TARGET_COLOR = "#8fa3c4";

  // Canvas `font` does not resolve `var(--font-mono)` — an unparsable font
  // string is ignored outright and every label silently falls back to 10px
  // sans. These mirror the two stacks in app.css.
  const MONO = '"SFMono-Regular", "JetBrains Mono", "Cascadia Mono", ui-monospace, Menlo, Consolas, monospace';
  const SANS = 'Inter, "SF Pro Text", system-ui, "Segoe UI", sans-serif';

  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  const referenceTrack = $derived(
    pitchMode.referenceTrackId ? project.trackById(pitchMode.referenceTrackId) : undefined,
  );
  const tolerance = $derived(TOLERANCE_CENTS[prefs.values.pitchTolerance]);

  /** Reference notes as sample spans. Reactive: clips change rarely. */
  const targets = $derived(
    pitchMode.referenceTrackId
      ? targetNotesFor(midi.clipsOf(pitchMode.referenceTrackId), (t) => midi.ticksToSamples(t))
      : [],
  );
  const range = $derived<LaneRange>(autoFitRange(targets, 60));

  let canvas: HTMLCanvasElement | undefined = $state();
  let wrap: HTMLDivElement | undefined = $state();
  let boxW = $state(800);
  let boxH = $state(200);
  /** Latest steadiness reading, in cents — mirrored out of the rAF loop for
   * the header readout. Throttled to STEADY_HZ: it is the one reactive write
   * the loop makes, and a 60 Hz scalar write would invalidate the header on
   * every frame for a number nobody can read that fast. */
  let steadiness = $state<number | null>(null);
  const STEADY_MS = 250;
  let steadyAt = 0;
  /** Lane viewport start, samples. Owned by the loop in fixed mode. */
  let laneStart = 0;
  /** Lane range in use when there is no reference melody: fitted to the
   * singer, and re-centred only when they leave it. Re-fitting every frame
   * would make the whole lane breathe with the vibrato. */
  let liveRange: LaneRange = { lowKey: 54, highKey: 66 };

  // ── panel lifetime: an open panel is a listening panel (ruling R6) ──
  $effect(() => {
    void startPitchStream();
    void backend.pitchListenStart().catch((err) => console.warn("[aura] pitch_listen_start failed:", err));
    return () => {
      void backend.pitchListenStop().catch(() => {});
      stopPitchStream();
    };
  });

  $effect(() => {
    if (!wrap) return;
    const el = wrap;
    const ro = new ResizeObserver(() => {
      boxW = el.clientWidth;
      boxH = el.clientHeight;
    });
    ro.observe(el);
    boxW = el.clientWidth;
    boxH = el.clientHeight;
    return () => ro.disconnect();
  });

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

  /** Frame sample positions shifted by the user's round-trip compensation. */
  function latencyShift(): number {
    const rate = pitchMode.deviceRate || project.sampleRate || 48000;
    return (prefs.values.pitchLatencyOffsetMs / 1000) * rate;
  }

  /**
   * The range to draw in. With a reference melody it is the melody's, fixed.
   * Without one there is nothing to fit but the voice, so the lane follows
   * it — otherwise anyone who does not sing near C4 spends the session with
   * their trail pinned to an edge.
   */
  function rangeFor(last: PitchFrame | null): LaneRange {
    if (targets.length > 0) return range;
    if (last && (last.midi < liveRange.lowKey + 1 || last.midi > liveRange.highKey - 1)) {
      const centre = Math.round(last.midi);
      liveRange = { lowKey: centre - 6, highKey: centre + 6 };
    }
    return liveRange;
  }

  /** Newest voiced frame, or null. */
  function lastVoicedOf(frames: readonly PitchFrame[]): PitchFrame | null {
    for (let i = frames.length - 1; i >= 0; i--) if (frames[i].voiced) return frames[i];
    return null;
  }

  /** The target note sounding at `sample`, or null. */
  function targetAt(sample: number): number | null {
    for (const n of targets) {
      if (sample >= n.startSample && sample < n.endSample) return n.key;
    }
    return null;
  }

  function noteName(m: number): string {
    const k = Math.round(m);
    return `${NOTE_NAMES[((k % 12) + 12) % 12]}${Math.floor(k / 12) - 1}`;
  }

  // ── the loop ──
  $effect(() => {
    const c = canvas;
    if (!c) return;
    let raf = 0;
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick);
      draw(now);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  function draw(now: number) {
    const c = canvas;
    if (!c) return;
    const w = boxW;
    const h = boxH;
    const ctx = setup(c, w, h);
    if (!ctx) return;

    const frames = recentFrames();
    const position = transport.positionAt(now);

    // Steadiness over the last second of frames — the header's number.
    const rate = pitchMode.deviceRate || project.sampleRate || 48000;
    if (now - steadyAt >= STEADY_MS) {
      steadyAt = now;
      const next = stabilityCents(frames.slice(-100));
      if (next !== steadiness) steadiness = next;
    }

    if (transport.isPlaying) drawLane(ctx, w, h, frames, position, rate);
    else drawTuner(ctx, w, h, frames, rate);
  }

  function drawGround(ctx: CanvasRenderingContext2D, w: number, h: number) {
    ctx.fillStyle = "rgba(5, 7, 13, 0.55)";
    ctx.fillRect(0, 0, w, h);
  }

  function drawLane(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    frames: readonly PitchFrame[],
    position: number,
    rate: number,
  ) {
    drawGround(ctx, w, h);
    const spp = view.spp;
    laneStart = laneScrollFor(prefs.values.pitchLaneFollow, position, spp, w, view.viewStart);
    const xOf = (sample: number) => (sample - laneStart) / spp;
    const last = lastVoicedOf(frames);
    const lane = rangeFor(last);

    // semitone rules — brighter at every C, so the eye has an octave anchor
    for (let key = Math.ceil(lane.lowKey); key <= Math.floor(lane.highKey); key++) {
      const y = yOfKey(key, lane, h);
      ctx.strokeStyle = key % 12 === 0 ? "rgba(96, 130, 190, 0.22)" : "rgba(96, 130, 190, 0.1)";
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
    }

    // target bars
    const rowH = Math.max(4, h / Math.max(1, lane.highKey - lane.lowKey));
    for (const n of targets) {
      const x0 = xOf(n.startSample);
      const x1 = xOf(n.endSample);
      if (x1 < 0 || x0 > w) continue;
      const y = yOfKey(n.key, lane, h) - rowH / 2;
      ctx.fillStyle = "rgba(157, 123, 255, 0.16)";
      ctx.strokeStyle = "rgba(157, 123, 255, 0.5)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.roundRect(x0, y, Math.max(2, x1 - x0), rowH, Math.min(rowH / 2, 6));
      ctx.fill();
      ctx.stroke();
    }

    // hit fill: the part of each bar the singer was inside the tolerance for
    const shift = latencyShift();
    ctx.fillStyle = "rgba(82, 229, 255, 0.35)";
    for (const f of frames) {
      if (!f.voiced) continue;
      const sample = f.sample + shift;
      const key = targetAt(sample);
      if (key === null) continue;
      if (bandFor(centsToTarget(f.midi, key), tolerance) !== "in") continue;
      const x = xOf(sample);
      if (x < -2 || x > w + 2) continue;
      const y = yOfKey(key, lane, h) - rowH / 2;
      ctx.fillRect(x, y, Math.max(1, (rate / 100 / spp) * 1.2), rowH);
    }

    // the trail
    const snap = prefs.values.pitchTrailSnap;
    ctx.lineWidth = 2;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    for (const segment of trailSegments(frames)) {
      let prevBand: string | null = null;
      ctx.beginPath();
      let started = false;
      for (const f of segment) {
        const sample = f.sample + shift;
        const x = xOf(sample);
        if (x < -20 || x > w + 20) continue;
        const key = targetAt(sample);
        const band = key === null ? "none" : bandFor(centsToTarget(f.midi, key), tolerance);
        // Game mode parks an in-tolerance point on the target line. Never
        // mix the two within a frame: snapped is legible, unsnapped is
        // truthful, half-snapped is neither.
        const drawKey = snap && key !== null && band === "in" ? key : f.midi;
        const y = yOfKey(drawKey, lane, h);
        if (band !== prevBand) {
          if (started) ctx.stroke();
          ctx.beginPath();
          ctx.strokeStyle = BAND_COLOR[band] ?? NO_TARGET_COLOR;
          ctx.moveTo(x, y);
          prevBand = band;
          started = true;
        } else {
          ctx.lineTo(x, y);
        }
      }
      if (started) ctx.stroke();
    }

    // playhead
    const px = xOf(position);
    ctx.strokeStyle = "rgba(216, 227, 242, 0.65)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(px, 0);
    ctx.lineTo(px, h);
    ctx.stroke();

    // off-lane arrow: the trail left the visible range, say which way
    if (last) {
      if (last.midi > lane.highKey) drawChevron(ctx, px, 10, -1);
      else if (last.midi < lane.lowKey) drawChevron(ctx, px, h - 10, 1);
    }

    if (pitchMode.rehearseHold) drawRehearseVeil(ctx, w, h);
  }

  function drawChevron(ctx: CanvasRenderingContext2D, x: number, y: number, dir: number) {
    ctx.strokeStyle = BAND_COLOR.far;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(x - 7, y + 6 * dir);
    ctx.lineTo(x, y);
    ctx.lineTo(x + 7, y + 6 * dir);
    ctx.stroke();
  }

  function drawRehearseVeil(ctx: CanvasRenderingContext2D, w: number, h: number) {
    ctx.save();
    ctx.strokeStyle = "rgba(255, 200, 87, 0.16)";
    ctx.lineWidth = 1;
    for (let x = -h; x < w; x += 9) {
      ctx.beginPath();
      ctx.moveTo(x, h);
      ctx.lineTo(x + h, 0);
      ctx.stroke();
    }
    ctx.fillStyle = "rgba(255, 200, 87, 0.85)";
    ctx.font = `600 9px ${MONO}`;
    ctx.fillText("REHEARSING — TAKE IS SILENT", 12, 18);
    ctx.restore();
  }

  function drawTuner(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    frames: readonly PitchFrame[],
    rate: number,
  ) {
    drawGround(ctx, w, h);
    const voiced = frames.filter((f) => f.voiced);
    const last = voiced.length ? voiced[voiced.length - 1] : null;

    if (!pitchMode.listening) {
      ctx.fillStyle = "rgba(95, 108, 133, 1)";
      ctx.font = `12px ${SANS}`;
      ctx.fillText("Turn on LISTEN to see your pitch.", 16, h / 2);
      return;
    }

    // ── the reading ──
    const cx = 18;
    ctx.textBaseline = "alphabetic";
    if (last) {
      ctx.fillStyle = "#d8e3f2";
      ctx.font = `300 44px ${MONO}`;
      ctx.fillText(`${last.hz.toFixed(1)}`, cx, 58);
      const hzW = ctx.measureText(`${last.hz.toFixed(1)}`).width;
      ctx.fillStyle = "#5f6c85";
      ctx.font = `11px ${MONO}`;
      ctx.fillText("Hz", cx + hzW + 8, 58);
      // The name is deliberately secondary: it flips across a semitone
      // boundary while the pitch barely moves (G5 795 Hz → G#5 811 Hz).
      ctx.fillText(`≈ ${noteName(last.midi)}`, cx + hzW + 34, 58);
    } else {
      ctx.fillStyle = "#39435c";
      ctx.font = `300 44px ${MONO}`;
      ctx.fillText("—", cx, 58);
    }

    // ── stability ribbon: the signature. Width IS the steadiness. ──
    const ribbonTop = 74;
    const ribbonH = Math.max(28, h - ribbonTop - 18);
    const midY = ribbonTop + ribbonH / 2;

    // Last 3 seconds, centred on the run's own mean and scaled to the run's
    // own swing — this is a steadiness view, not a pitch view, and a fixed
    // scale either clips a wobble or flattens a hold. The floor keeps a
    // genuinely steady note from being amplified into noise; the printed
    // span at the bottom says what the height is worth.
    const window = voiced.slice(-300);
    let spanCents = 0;
    let centre = 0;
    if (window.length > 1) {
      centre = window.reduce((a, f) => a + f.midi, 0) / window.length;
      let swing = 0;
      for (const f of window) swing = Math.max(swing, Math.abs(f.midi - centre));
      spanCents = Math.max(25, Math.ceil((swing * 100) / 25) * 25);
    }

    // The ribbon, behind the line: a band as wide as the steadiness reading,
    // on the same cents scale as the line inside it. Steady reads as a thin
    // bright seam; a wobble opens it up. The panel's one loud element.
    if (last && steadiness !== null && spanCents) {
      const halfPx = Math.min(ribbonH / 2, ((steadiness / spanCents) * ribbonH) / 2);
      const grad = ctx.createLinearGradient(0, midY - halfPx, 0, midY + halfPx);
      grad.addColorStop(0, "rgba(82, 229, 255, 0)");
      grad.addColorStop(0.5, "rgba(82, 229, 255, 0.3)");
      grad.addColorStop(1, "rgba(82, 229, 255, 0)");
      ctx.fillStyle = grad;
      ctx.fillRect(0, midY - halfPx - 1, w, halfPx * 2 + 2);
    }

    if (spanCents) {
      ctx.strokeStyle = "#52e5ff";
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      window.forEach((f, i) => {
        const x = (i / (window.length - 1)) * w;
        const y = midY - ((f.midi - centre) * 100 * (ribbonH / 2)) / spanCents;
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      });
      ctx.stroke();
    }

    ctx.strokeStyle = "rgba(96, 130, 190, 0.25)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(0, midY);
    ctx.lineTo(w, midY);
    ctx.stroke();

    ctx.fillStyle = "#39435c";
    ctx.font = `9px ${MONO}`;
    const scale = spanCents ? ` · ±${spanCents}¢ SHOWN` : "";
    ctx.fillText(`LAST ${(window.length / 100).toFixed(1)}s${scale} · ${rate ? `${rate} Hz` : "NO INPUT"}`, cx, h - 6);
  }
</script>

<div class="pitch glass" style:height="{ui.rollHeight}px">
  <PanelResizeHandle
    axis="y"
    size={ui.rollHeight}
    spec={ROLL_RESIZE}
    label="Resize pitch coach"
    onresize={(px) => (ui.rollHeight = px)}
  />

  <header class="head">
    <div class="tabs" role="group" aria-label="Bottom panel">
      <button
        class="tab mono"
        disabled={!midi.openClip}
        title={midi.openClip ? "Piano roll" : "Open a MIDI clip to edit notes"}
        onclick={() => (ui.bottomPanel = "roll")}
      >
        ROLL
      </button>
      <button class="tab mono on" aria-current="page" onclick={() => (ui.bottomPanel = "pitch")}>PITCH</button>
    </div>

    <label class="ref">
      <span class="silk">reference</span>
      <select
        value={pitchMode.referenceTrackId ?? ""}
        onchange={(e) => {
          const id = (e.currentTarget as HTMLSelectElement).value;
          void backend.pitchSetReference(id === "" ? null : id);
        }}
      >
        <option value="">none</option>
        {#each midiTracks as track (track.id)}
          <option value={track.id}>{track.name}</option>
        {/each}
      </select>
    </label>

    <span class="spacer"></span>

    <span class="steady mono" title="How steadily you are holding the pitch — median deviation over the last second">
      <span class="silk">steady</span>
      {steadiness === null ? "—" : `±${steadiness.toFixed(1)}¢`}
    </span>

    <RehearseButton />

    <button
      class="tab mono listen"
      class:on={pitchMode.listening}
      title="Open the microphone for live pitch"
      onclick={() =>
        void (pitchMode.listening ? backend.pitchListenStop() : backend.pitchListenStart()).catch((err) =>
          console.warn("[aura] listen toggle failed:", err),
        )}
    >
      {pitchMode.listening ? "◉ LISTENING" : "○ LISTEN"}
    </button>
  </header>

  <div class="lane" bind:this={wrap}>
    <canvas bind:this={canvas} style:width="100%" style:height="100%"></canvas>
  </div>

  <footer class="foot">
    {#if referenceTrack}
      <span class="silk">target · {referenceTrack.name}</span>
    {:else}
      <span class="silk">pick a midi track to sing against</span>
    {/if}
    <span class="spacer"></span>
    <span class="bleed">Singing along to speakers? The backing track gets detected too — use headphones.</span>
  </footer>
</div>

<style>
  .pitch {
    position: relative;
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-left: 0;
    border-right: 0;
    border-bottom: 0;
  }
  .head,
  .foot {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 12px;
  }
  .head {
    border-bottom: 1px solid var(--glass-border);
  }
  .foot {
    border-top: 1px solid var(--glass-border);
    min-height: 24px;
  }
  .spacer {
    flex: 1;
  }
  .tabs {
    display: flex;
    gap: 2px;
  }
  .tab {
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 3px 10px;
    border-radius: 999px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .tab:hover:not(:disabled) {
    color: var(--text);
  }
  .tab:disabled {
    color: var(--text-faint);
    cursor: default;
  }
  .tab.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .listen.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    box-shadow: 0 0 14px rgba(82, 229, 255, 0.2);
  }
  .ref {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .ref select {
    background: var(--bg-2);
    color: var(--text);
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    font-family: var(--font-mono);
    font-size: 11px;
    padding: 2px 6px;
  }
  .steady {
    font-size: 11px;
    color: var(--text);
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .lane {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .lane canvas {
    display: block;
  }
  .bleed {
    font-size: 10px;
    color: var(--text-faint);
  }
</style>
