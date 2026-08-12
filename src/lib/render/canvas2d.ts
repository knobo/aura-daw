/**
 * Canvas2D waveform painter — the universal fallback path.
 * Draws a filled min/max silhouette with a brighter core band (pseudo-RMS)
 * and a hairline centerline, devicePixelRatio-correct.
 */

import type { RGBA, WaveformPainter } from "./painter";

const css = (c: RGBA, a: number) =>
  `rgba(${Math.round(c.r * 255)},${Math.round(c.g * 255)},${Math.round(c.b * 255)},${a})`;

export function createCanvas2dPainter(canvas: HTMLCanvasElement): WaveformPainter {
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("2d context unavailable");

  return {
    kind: "canvas2d",

    draw(columns, cssWidth, cssHeight, dpr, color) {
      const pw = Math.max(1, Math.round(cssWidth * dpr));
      const ph = Math.max(1, Math.round(cssHeight * dpr));
      if (canvas.width !== pw) canvas.width = pw;
      if (canvas.height !== ph) canvas.height = ph;

      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, cssWidth, cssHeight);

      const width = Math.min(cssWidth, columns.length / 2);
      const mid = cssHeight / 2;
      const half = cssHeight / 2;

      // silhouette: top edge left→right, bottom edge right→left
      ctx.beginPath();
      for (let x = 0; x < width; x++) {
        const max = columns[x * 2 + 1];
        const y = mid - Math.max(0.6 / half, Math.min(1, max)) * half;
        if (x === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      for (let x = width - 1; x >= 0; x--) {
        const min = columns[x * 2];
        const y = mid - Math.max(-1, Math.min(-0.6 / half, min)) * half;
        ctx.lineTo(x, y);
      }
      ctx.closePath();
      ctx.fillStyle = css(color, color.a * 0.55);
      ctx.fill();

      // brighter inner band at 55% amplitude — reads like RMS energy
      ctx.beginPath();
      for (let x = 0; x < width; x++) {
        const max = columns[x * 2 + 1] * 0.55;
        const y = mid - Math.min(1, max) * half;
        if (x === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      for (let x = width - 1; x >= 0; x--) {
        const min = columns[x * 2] * 0.55;
        const y = mid - Math.max(-1, min) * half;
        ctx.lineTo(x, y);
      }
      ctx.closePath();
      ctx.fillStyle = css(color, color.a * 0.8);
      ctx.fill();

      // hairline centerline
      ctx.fillStyle = css(color, Math.min(1, color.a * 1.1));
      ctx.fillRect(0, mid - 0.5, width, 1);
    },

    destroy() {
      // nothing to release for 2d
    },
  };
}
