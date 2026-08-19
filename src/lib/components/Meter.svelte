<script lang="ts">
  /**
   * Peak-hold VU meter (stereo, horizontal). Canvas + rAF, fed imperatively
   * from the meter bus (no Svelte reactivity in the 60 Hz path).
   * dBFS scale -60..+6, fast attack / smooth release, 900 ms peak hold with
   * decay, latched clip LED (click to reset).
   */
  import { latestMeter } from "../state/meters.svelte";
  import { linToDb } from "../utils/format";
  import { theme } from "../theme/theme.svelte";
  import { alpha } from "../theme/tokens";

  let {
    trackId,
    height = 16,
    scale = false,
  }: { trackId: string; height?: number; scale?: boolean } = $props();

  const DB_MIN = -60;
  const DB_MAX = 6;
  const norm = (db: number) => Math.max(0, Math.min(1, (db - DB_MIN) / (DB_MAX - DB_MIN)));

  let canvas: HTMLCanvasElement;
  let clipLatched = $state(false);

  function resetClip() {
    clipLatched = false;
  }

  function onMeterKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      resetClip();
    }
  }

  interface Lane {
    disp: number; // displayed peak dB
    rms: number; // displayed rms dB
    holdDb: number;
    holdUntil: number;
  }

  $effect(() => {
    const id = trackId; // re-run when target changes
    const tokens = theme.tokens; // subscribe to theme changes
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const lanes: Lane[] = [
      { disp: DB_MIN, rms: DB_MIN, holdDb: DB_MIN, holdUntil: 0 },
      { disp: DB_MIN, rms: DB_MIN, holdDb: DB_MIN, holdUntil: 0 },
    ];
    let raf = 0;
    let last = performance.now();
    let w = 0;
    let h = 0;
    let dpr = 1;
    let gradient: CanvasGradient | null = null;

    const ro = new ResizeObserver(() => {
      w = canvas.clientWidth;
      h = canvas.clientHeight;
      dpr = Math.min(3, window.devicePixelRatio || 1);
      canvas.width = Math.max(1, Math.round(w * dpr));
      canvas.height = Math.max(1, Math.round(h * dpr));
      gradient = null;
    });
    ro.observe(canvas);

    const ledW = 7; // clip LED zone
    const gap = 2;

    const draw = (now: number) => {
      raf = requestAnimationFrame(draw);
      const dt = Math.min(0.1, (now - last) / 1000);
      last = now;
      if (w === 0 || h === 0) return;

      const m = latestMeter(id);
      const src = [
        { peak: m?.peakL ?? 0, rms: m?.rmsL ?? 0 },
        { peak: m?.peakR ?? 0, rms: m?.rmsR ?? 0 },
      ];
      if (m?.clipped) clipLatched = true;

      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);

      const meterW = w - ledW - 3;
      const laneH = (h - gap) / 2;

      if (!gradient) {
        gradient = ctx.createLinearGradient(0, 0, meterW, 0);
        gradient.addColorStop(0, tokens.cyanDeep);
        gradient.addColorStop(norm(-18), tokens.cyan);
        gradient.addColorStop(norm(-6), tokens.cyanBright);
        gradient.addColorStop(norm(-3), tokens.magenta);
        gradient.addColorStop(1, tokens.red);
      }

      for (let i = 0; i < 2; i++) {
        const lane = lanes[i];
        const peakDb = linToDb(src[i].peak);
        const rmsDb = linToDb(src[i].rms);

        // fast attack, ~30 dB/s release
        lane.disp = peakDb > lane.disp ? peakDb : Math.max(peakDb, lane.disp - 30 * dt);
        lane.rms = rmsDb > lane.rms ? rmsDb : Math.max(rmsDb, lane.rms - 30 * dt);

        // peak hold
        if (peakDb >= lane.holdDb) {
          lane.holdDb = peakDb;
          lane.holdUntil = now + 900;
        } else if (now > lane.holdUntil) {
          lane.holdDb = Math.max(DB_MIN, lane.holdDb - 14 * dt);
        }

        const y = i * (laneH + gap);

        // track bed
        ctx.fillStyle = alpha(tokens.line, 0.10);
        ctx.fillRect(0, y, meterW, laneH);

        // peak body (dim) + rms core (bright)
        const px = norm(lane.disp) * meterW;
        const rx = norm(lane.rms) * meterW;
        ctx.globalAlpha = 0.4;
        ctx.fillStyle = gradient;
        ctx.fillRect(0, y, px, laneH);
        ctx.globalAlpha = 1;
        ctx.fillRect(0, y, rx, laneH);

        // hold tick
        if (lane.holdDb > DB_MIN + 0.5) {
          const hx = norm(lane.holdDb) * meterW;
          ctx.fillStyle = lane.holdDb > -3 ? tokens.redSoft : tokens.text;
          ctx.fillRect(Math.min(meterW - 2, hx), y, 2, laneH);
        }
      }

      // scale ticks
      if (scale) {
        ctx.fillStyle = alpha(tokens.text, 0.28);
        for (const db of [-48, -36, -24, -18, -12, -6, -3, 0]) {
          const x = Math.round(norm(db) * meterW);
          ctx.fillRect(x, 0, 1, h);
        }
      }

      // clip LED
      ctx.fillStyle = clipLatched ? tokens.red : alpha(tokens.red, 0.15);
      ctx.fillRect(w - ledW, 0, ledW, h);
      if (clipLatched) {
        ctx.shadowColor = tokens.red;
        ctx.shadowBlur = 6 * (parseFloat(tokens.glowScale) || 0);
        ctx.fillRect(w - ledW, 0, ledW, h);
        ctx.shadowBlur = 0;
      }
    };

    raf = requestAnimationFrame(draw);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
    };
  });
</script>

<canvas
  bind:this={canvas}
  class="meter"
  style:height="{height}px"
  role="button"
  tabindex="0"
  aria-label="Track level meter; activate to reset clip indicator"
  onclick={resetClip}
  onkeydown={onMeterKeydown}
  title="dBFS · click to reset clip"
></canvas>

<style>
  .meter {
    width: 100%;
    display: block;
    cursor: pointer;
  }
</style>
