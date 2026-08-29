<script lang="ts">
  /**
   * The level meter on the control surface, in four faces. Which one you get
   * is the `meterFace` preference; none of them is a different component,
   * because all four read the same bus the same way and the only thing that
   * varies is what the numbers are drawn as.
   *
   * - **LED ARC** — a ring of segments around a sunken well with the number
   *   inside it. The face that matches the knob: same 270° travel, same
   *   lamps, so a rack of knobs and meters reads as one instrument rather
   *   than as two widgets that happen to be round.
   * - **DIAL** — the same well with a single thin arc filling round it and a
   *   fine needle riding its head. Quieter than the segments at a glance and
   *   the most precise of the four to read a small change on, because a
   *   continuous arc has no step size.
   * - **LADDER** — the horizontal PPM bargraph, in a routed slot. It is the
   *   only face that is not round, which is the point: in a row of channel
   *   strips the eye compares BAR LENGTHS across the row far faster than it
   *   compares angles, so this is the face for balancing a mix.
   * - **ANALOG VU** — the swinging needle over a printed scale. It reads
   *   ballistics better than any of the others: you see the SHAPE of a
   *   transient rather than a number that already happened.
   *
   * Every face carries a peak hold except the VU, which by construction
   * cannot: a needle IS the peak, and once it has fallen back the peak is
   * gone. That is the one real thing the old face could not do.
   *
   * No face carries a colour of its own. The lamp ramp is
   * `green → amber → red` off the palette, so a theme with no green in it is
   * not a meter with the wrong green in it.
   *
   * Range matches Meter.svelte (−60..+6 dBFS). Read-only.
   *
   * CANVAS, not SVG (STANDING-CONSTRAINTS.md "No continuously-animated
   * SVG"): the previous SVG build mutated up to 44 segment elements'
   * classes/attributes every frame, and a WebKit Timelines capture on the
   * native (WebKitGTK) build showed that folded into a full-viewport
   * repaint — ~600ms/paint, on every meter tick, for every gauge on the
   * page. One canvas per gauge, redrawn imperatively like Meter.svelte,
   * touches no DOM/CSSOM at all once mounted.
   */
  import { latestMeter } from "../../state/meters.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import { formatDb, linToDb } from "../../utils/format";
  import { runWhileVisible } from "../../utils/visible-raf";
  import { theme } from "../../theme/theme.svelte";
  import { alpha } from "../../theme/tokens";

  interface Props {
    trackId: string;
    label?: string;
    size?: number;
  }

  const { trackId, label, size = 88 }: Props = $props();

  const DB_MIN = -60;
  const DB_MAX = 6;
  /** The VU face's sweep. A real VU movement covers about 90°. */
  const VU_SWEEP = 90;
  const VU_START = -VU_SWEEP / 2;

  /** The round faces' sweep — the knob's, so the two agree by construction. */
  const SWEEP = 270;
  /** Segments around the LED ring. Denser than the knob's, because a meter is
   * read at a glance and a coarser ring steps visibly under a fade. */
  const SEGS = 44;
  const SEG_R = 43;
  const SEG_W = 2.3;
  /** Rungs on the ladder. Fewer, because each one has to stay a rung at the
   * 72px a channel strip gives it. */
  const RUNGS = 26;
  /** 100×100 viewBox the old SVG drew in — every geometry constant above is
   * inherited from it unchanged, so the canvas is redrawn in the same local
   * coordinate space (see `draw`'s `ctx.scale`) rather than re-derived. */
  const CX = 50;
  const VIEWBOX = 100;

  const face = $derived(prefs.values.meterFace);
  const round = $derived(face === "led" || face === "dial");

  /**
   * The lamp ramp. Green up to −12 dB, amber to 0, red above — the
   * boundaries a mixer already has in their head, expressed as positions
   * along the scale rather than as segment indices, so moving DB_MIN cannot
   * silently move where "hot" starts.
   */
  const HOT = (0 - DB_MIN) / (DB_MAX - DB_MIN);
  const WARM = (-12 - DB_MIN) / (DB_MAX - DB_MIN);

  /** 0..1 along the scale. */
  function normal(db: number): number {
    return Math.max(0, Math.min(1, (db - DB_MIN) / (DB_MAX - DB_MIN)));
  }
  function vuAngle(db: number): number {
    return VU_START + normal(db) * VU_SWEEP;
  }
  /** The zone a level is in. */
  function zoneOf(n: number): "ok" | "warm" | "hot" {
    return n >= HOT ? "hot" : n >= WARM ? "warm" : "ok";
  }
  /** Degrees-clockwise-from-straight-up (the SVG build's rotation
   * convention throughout) → radians in canvas's from-positive-x-axis
   * convention. Both wind clockwise, so this is a constant 90° offset. */
  function toCanvasRad(degFromUp: number): number {
    return ((degFromUp - 90) * Math.PI) / 180;
  }

  let canvas: HTMLCanvasElement | undefined = $state();
  let readoutEl: HTMLElement | undefined = $state();
  let clipped = $state(false);

  function resetClip() {
    clipped = false;
  }
  function onClipKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      resetClip();
    }
  }

  // Every face is written imperatively inside the rAF loop; none of its
  // per-frame values may be `$state` (ruling S-3: the meter bus does not go
  // on the reactivity graph, and add-all-tracks puts one gauge per mixable
  // track on the page). `clipped` is the one exception — a rare latch, reset
  // by a click, exactly as in Meter.svelte.
  $effect(() => {
    const id = trackId;
    const el = canvas;
    const readout = readoutEl;
    const currentFace = face;
    if (!el) return;
    const ctx = el.getContext("2d");
    if (!ctx) return;
    const c = ctx;

    let disp = DB_MIN;
    let peakHold = DB_MIN;
    let peakUntil = 0;
    let shown = "";
    let w = 0;
    let h = 0;
    let dpr = 1;

    const ro = new ResizeObserver(() => {
      w = el.clientWidth;
      h = el.clientHeight;
      dpr = Math.min(3, window.devicePixelRatio || 1);
      el.width = Math.max(1, Math.round(w * dpr));
      el.height = Math.max(1, Math.round(h * dpr));
    });
    ro.observe(el);

    /** A radial line from `r1` to `r2` at `degFromUp`, about (CX, CX). */
    function radial(deg: number, r1: number, r2: number) {
      const a = toCanvasRad(deg);
      const cos = Math.cos(a);
      const sin = Math.sin(a);
      c.moveTo(CX + r1 * cos, CX + r1 * sin);
      c.lineTo(CX + r2 * cos, CX + r2 * sin);
    }

    function zoneColor(zone: "ok" | "warm" | "hot"): string {
      const t = theme.tokens;
      return zone === "hot" ? t.red : zone === "warm" ? t.amber : t.green;
    }

    function drawLed(n: number, p: number) {
      const t = theme.tokens;
      const glow = 2.5 * (parseFloat(t.glowScale) || 0);
      const lit = Math.max(0, Math.min(SEGS, Math.round(n * SEGS)));
      // Dim printed-scale pass, no glow — the whole ring, cheaply, in one path.
      c.beginPath();
      for (let i = 0; i < SEGS; i++) {
        const deg = -SWEEP / 2 + (i / (SEGS - 1)) * SWEEP;
        radial(deg, SEG_R - SEG_W * 2, SEG_R + SEG_W * 2);
      }
      c.strokeStyle = alpha(t.line, 0.24);
      c.lineWidth = SEG_W;
      c.lineCap = "round";
      c.stroke();
      // Lit segments, grouped by zone so each zone pays for one glow pass.
      if (lit > 0) {
        for (const zone of ["ok", "warm", "hot"] as const) {
          c.beginPath();
          let any = false;
          for (let i = 0; i < lit; i++) {
            const at = i / (SEGS - 1);
            const segZone = at >= HOT ? "hot" : at >= WARM ? "warm" : "ok";
            if (segZone !== zone) continue;
            const deg = -SWEEP / 2 + at * SWEEP;
            radial(deg, SEG_R - SEG_W * 2, SEG_R + SEG_W * 2);
            any = true;
          }
          if (!any) continue;
          c.strokeStyle = zoneColor(zone);
          c.shadowColor = zoneColor(zone);
          c.shadowBlur = glow;
          c.stroke();
        }
        c.shadowBlur = 0;
      }
      // Peak mark, drawn in the text colour so a peak that touched red does
      // not keep reading as "you are in the red".
      if (peakHold > DB_MIN) {
        const mark = Math.max(0, Math.min(SEGS - 1, Math.round(p * (SEGS - 1))));
        const deg = -SWEEP / 2 + (mark / (SEGS - 1)) * SWEEP;
        c.beginPath();
        radial(deg, SEG_R - SEG_W * 2, SEG_R + SEG_W * 2);
        c.strokeStyle = t.text;
        c.lineWidth = SEG_W * 1.1;
        c.shadowColor = t.text;
        c.shadowBlur = 3 * (parseFloat(t.glowScale) || 0);
        c.stroke();
        c.shadowBlur = 0;
      }
    }

    function drawDial(n: number, p: number) {
      const t = theme.tokens;
      const start = toCanvasRad(-SWEEP / 2 + 0); // -135° from up == the LED ring's own start
      // static track
      c.beginPath();
      c.arc(CX, CX, SEG_R, start, start + (SWEEP * Math.PI) / 180);
      c.strokeStyle = alpha(t.line, 0.28);
      c.lineWidth = 4;
      c.lineCap = "butt";
      c.stroke();
      // live arc
      if (n > 0) {
        const zone = zoneOf(n);
        c.beginPath();
        c.arc(CX, CX, SEG_R, start, start + (n * SWEEP * Math.PI) / 180);
        c.strokeStyle = zoneColor(zone);
        c.shadowColor = zoneColor(zone);
        c.shadowBlur = 3 * (parseFloat(t.glowScale) || 0);
        c.stroke();
        c.shadowBlur = 0;
      }
      // needle riding the arc's head — a longer line than the peak tick,
      // reaching from just outside the ring to well inside it (matches the
      // old SVG's y1={CX-SEG_R-SEG_W*2}/y2={CX-SEG_R+SEG_W*4}).
      c.beginPath();
      radial(-SWEEP / 2 + n * SWEEP, SEG_R - SEG_W * 4, SEG_R + SEG_W * 2);
      c.strokeStyle = t.text;
      c.lineWidth = 1.8;
      c.lineCap = "round";
      c.stroke();
      // peak tick, continuous (no rung quantisation on this face)
      if (peakHold > DB_MIN) {
        c.beginPath();
        radial(-SWEEP / 2 + p * SWEEP, SEG_R - SEG_W, SEG_R + SEG_W);
        c.strokeStyle = t.text;
        c.globalAlpha = 0.55;
        c.lineWidth = 1.4;
        c.lineCap = "round";
        c.stroke();
        c.globalAlpha = 1;
      }
    }

    function drawVu(db: number) {
      const t = theme.tokens;
      const pivotX = 50;
      const pivotY = 78;
      // printed scale — the upper semicircle above the pivot (old SVG:
      // `M18 78 A32 32 0 0 1 82 78`, i.e. 180°..360° through straight up).
      c.beginPath();
      c.arc(pivotX, pivotY, 32, Math.PI, Math.PI * 2);
      c.strokeStyle = alpha(t.line, 0.35);
      c.lineWidth = 1.6;
      c.stroke();
      for (const tickDb of [-60, -24, -12, -6, 0, 6]) {
        const a = toCanvasRad(vuAngle(tickDb));
        const r1 = 30;
        const r2 = tickDb >= 0 ? 34 : 32;
        c.beginPath();
        c.moveTo(pivotX + r1 * Math.cos(a), pivotY + r1 * Math.sin(a));
        c.lineTo(pivotX + r2 * Math.cos(a), pivotY + r2 * Math.sin(a));
        c.strokeStyle = tickDb >= 0 ? t.red : alpha(t.line, 0.55);
        c.lineWidth = 1.4;
        c.stroke();
      }
      // needle
      const deg = vuAngle(db);
      const a = toCanvasRad(deg);
      const glow = 3 * (parseFloat(t.glowScale) || 0);
      c.beginPath();
      c.moveTo(pivotX, pivotY);
      c.lineTo(pivotX + 32 * Math.cos(a), pivotY + 32 * Math.sin(a));
      c.strokeStyle = t.amber;
      c.lineWidth = 1.6;
      c.lineCap = "round";
      c.shadowColor = t.amber;
      c.shadowBlur = glow;
      c.stroke();
      c.shadowBlur = 0;
      // hub
      c.beginPath();
      c.arc(pivotX, pivotY, 4.5, 0, Math.PI * 2);
      c.fillStyle = t.bg3;
      c.fill();
      c.strokeStyle = t.edge;
      c.lineWidth = 0.8;
      c.stroke();
    }

    function drawLadder(n: number, p: number) {
      const t = theme.tokens;
      const gap = 1;
      const rw = (w - gap * (RUNGS - 1)) / RUNGS;
      const lit = Math.max(0, Math.min(RUNGS, Math.round(n * RUNGS)));
      const mark = Math.max(0, Math.min(RUNGS - 1, Math.round(p * (RUNGS - 1))));
      const r = Math.min(1, rw / 2, h / 2);
      for (let i = 0; i < RUNGS; i++) {
        const x = i * (rw + gap);
        const at = i / (RUNGS - 1);
        const isPeak = peakHold > DB_MIN && i === mark;
        const on = i < lit;
        let fill = alpha(t.line, 0.2);
        let glowColor: string | null = null;
        if (isPeak) {
          fill = t.text;
          glowColor = t.text;
        } else if (on) {
          const zone = at >= HOT ? "hot" : at >= WARM ? "warm" : "ok";
          fill = zoneColor(zone);
          glowColor = fill;
        }
        c.beginPath();
        if (typeof c.roundRect === "function") c.roundRect(x, 0, rw, h, r);
        else c.rect(x, 0, rw, h);
        c.fillStyle = fill;
        if (glowColor) {
          c.shadowColor = glowColor;
          c.shadowBlur = 4 * (parseFloat(t.glowScale) || 0);
        } else {
          c.shadowBlur = 0;
        }
        c.fill();
      }
      c.shadowBlur = 0;
    }

    const draw = () => {
      if (w === 0 || h === 0) return;
      const m = latestMeter(id);
      const now = performance.now();
      const peak = m ? linToDb(Math.max(m.peakL, m.peakR)) : DB_MIN;
      // Fast attack, slower release — a needle, not a bar.
      disp = peak > disp ? peak : disp + (peak - disp) * 0.18;

      // Peak hold: the highest reading of the last second, shown ahead of
      // the level as its own mark. 1000ms is the AES convention, and it is
      // long enough to catch a transient you were not watching for. The VU
      // face carries no peak hold by construction (a needle IS the peak).
      if (currentFace !== "needle") {
        if (peak >= peakHold || now > peakUntil) {
          peakHold = peak;
          peakUntil = now + 1000;
        }
      }

      const n = normal(disp);
      const p = normal(peakHold);

      c.setTransform(dpr, 0, 0, dpr, 0, 0);
      c.clearRect(0, 0, w, h);

      if (currentFace === "ladder") {
        drawLadder(n, p);
      } else {
        // Round faces share the old SVG's 100×100 local coordinate space.
        const scale = Math.min(w, h) / VIEWBOX;
        c.setTransform(dpr * scale, 0, 0, dpr * scale, 0, 0);
        if (currentFace === "led") drawLed(n, p);
        else if (currentFace === "dial") drawDial(n, p);
        else drawVu(disp);
      }

      if (readout) {
        const text = formatDb(disp);
        if (text !== shown) {
          readout.textContent = text;
          shown = text;
        }
      }
      if (m?.clipped) clipped = true;
    };

    const stop = runWhileVisible(el, draw);
    return () => {
      stop();
      ro.disconnect();
    };
  });
</script>

<div class="gauge-unit" class:wide={face === "ladder"} style:--gauge-size="{size}px">
  {#if round}
    <div class="bezel led grain">
      <div class="window">
        <canvas bind:this={canvas} class="face-canvas"></canvas>
        <div class="glass"></div>
        <button
          class="clip"
          class:on={clipped}
          type="button"
          aria-label="Clip indicator — click to reset"
          onclick={resetClip}
          onkeydown={onClipKeydown}
        ></button>
        <!-- The number lives INSIDE the well on the round faces: the ring is
             the glanceable reading and the digits the exact one, and putting
             both in the same object is what stops the unit needing two. -->
        <output class="mono well-value" bind:this={readoutEl}>-∞</output>
      </div>
    </div>
  {:else if face === "ladder"}
    <div class="slot grain">
      <canvas bind:this={canvas} class="ladder-canvas"></canvas>
      <button
        class="clip"
        class:on={clipped}
        type="button"
        aria-label="Clip indicator — click to reset"
        onclick={resetClip}
        onkeydown={onClipKeydown}
      ></button>
    </div>
  {:else}
    <div class="bezel grain">
      <div class="window">
        <canvas bind:this={canvas} class="face-canvas"></canvas>
        <div class="glass"></div>
        <button
          class="clip"
          class:on={clipped}
          type="button"
          aria-label="Clip indicator — click to reset"
          onclick={resetClip}
          onkeydown={onClipKeydown}
        ></button>
      </div>
    </div>
  {/if}

  {#if label}<span class="silk gauge-label">{label}</span>{/if}
  {#if !round}<output class="mono gauge-value" bind:this={readoutEl}>-∞</output>{/if}
</div>

<style>
  .gauge-unit {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    width: var(--gauge-size);
  }

  /* ── the round faces share a bezel and a well ── */
  .bezel {
    position: relative;
    width: var(--gauge-size);
    height: var(--gauge-size);
    border-radius: 50%;
    background-color: var(--bg-3);
    background-image: var(--sheen-dome);
    box-shadow: var(--bevel-frame), var(--relief-2);
    padding: 7px;
  }
  /* The lit faces want a thinner collar: the ring IS the rim, and a wide
     milled one outside it leaves the lit part looking small. */
  .bezel.led {
    padding: 3px;
  }
  .window {
    position: relative;
    width: 100%;
    height: 100%;
    border-radius: 50%;
    background: var(--bg-sunken);
    box-shadow: var(--bevel-inset);
    overflow: hidden;
  }
  /* On the lit faces the sunken part is the DISC INSIDE the ring, not the
     whole window — the ring sits on the rim, level with the bezel, and the
     well is what it is ranged around. An inset ring rather than a smaller
     element, so the ring still has the full canvas to draw in. */
  .bezel.led .window {
    background: var(--bg-2);
    box-shadow:
      inset 0 0 0 calc(11% + 1px) var(--bg-sunken),
      var(--bevel-inset);
  }
  .face-canvas {
    width: 100%;
    height: 100%;
    display: block;
    /* Redrawn every frame from the meter bus, off the DOM entirely — see
       Meter.svelte's .meter rule and STANDING-CONSTRAINTS.md. */
    contain: paint;
  }
  .ladder-canvas {
    width: 100%;
    height: 13px;
    display: block;
    contain: paint;
  }

  /* ── shared trim ── */
  .well-value {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    font-size: calc(var(--gauge-size) * 0.17);
    letter-spacing: 0.02em;
    color: var(--text-mid);
    pointer-events: none;
  }
  .glass {
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background-image: var(--sheen-dome);
    opacity: calc(var(--sheen) * 0.55);
    pointer-events: none;
  }
  .clip {
    position: absolute;
    top: 10%;
    right: 18%;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: none;
    padding: 0;
    background: rgb(var(--line-rgb) / 0.3);
    cursor: pointer;
  }
  /* The clip lamp belongs on the printed face of the VU, but on the lit faces
     that spot is the ring itself, so it moves into the well; on the ladder it
     goes to the hot end of the slot, where a clip lamp is on real gear. */
  .bezel.led .clip {
    top: 26%;
    right: 50%;
    margin-right: -3.5px;
  }
  /* Set INTO the hot end of the slot rather than beside it: hung outside, it
     reads as a stray dot on the panel and it is the first thing a narrower
     channel strip clips off. */
  .slot .clip {
    top: 50%;
    right: 7px;
    margin-top: -3.5px;
    box-shadow: inset 0 0 0 1px rgb(var(--shadow-rgb) / calc(var(--bevel) * 0.6));
  }
  .clip.on {
    background: var(--red);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) var(--red);
  }
  .gauge-label {
    line-height: 1;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gauge-value {
    font-size: 10px;
    color: var(--text-mid);
    line-height: 1;
  }

  /* ── LADDER ──
     A routed slot with the canvas lying in it. `--bevel-frame-inset` rather
     than a border: the bar has to read as sunk into the panel, or it is a
     progress bar drawn on top of one. */
  .slot {
    position: relative;
    width: 100%;
    padding: 4px 5px;
    border-radius: var(--ctrl-radius);
    background: var(--bg-sunken);
    box-shadow: var(--bevel-frame-inset), var(--bevel-inset);
  }
</style>
