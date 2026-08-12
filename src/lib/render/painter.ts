/**
 * WaveformPainter — the abstraction both renderer backends implement.
 * WebGPU is preferred; Canvas2D is the automatic fallback (WebGPU is absent
 * in WebKitGTK and many dev browsers). The choice is made once per app and
 * cached, but each canvas gets its own painter instance.
 */

export interface RGBA {
  r: number;
  g: number;
  b: number;
  a: number;
}

export interface WaveformPainter {
  readonly kind: "webgpu" | "canvas2d";
  /**
   * Draw peak columns. `columns` holds width×2 floats, interleaved
   * [min, max] in -1..1, one pair per CSS pixel column. The painter is
   * responsible for the device-pixel backing store.
   */
  draw(columns: Float32Array, cssWidth: number, cssHeight: number, dpr: number, color: RGBA): void;
  destroy(): void;
}

export function hexToRgba(hex: string, alpha = 1): RGBA {
  const n = parseInt(hex.replace("#", ""), 16);
  return {
    r: ((n >> 16) & 0xff) / 255,
    g: ((n >> 8) & 0xff) / 255,
    b: (n & 0xff) / 255,
    a: alpha,
  };
}

import { createWebGpuPainter, webGpuAvailable } from "./webgpu-painter";
import { createCanvas2dPainter } from "./canvas2d";

let gpuDecision: Promise<boolean> | null = null;

/** One-time async probe: can we actually get a WebGPU device? */
function probeWebGpu(): Promise<boolean> {
  if (!gpuDecision) gpuDecision = webGpuAvailable();
  return gpuDecision;
}

export async function createPainter(canvas: HTMLCanvasElement): Promise<WaveformPainter> {
  if (await probeWebGpu()) {
    try {
      return await createWebGpuPainter(canvas);
    } catch (err) {
      console.warn("[aura] WebGPU painter failed, falling back to Canvas2D:", err);
    }
  }
  return createCanvas2dPainter(canvas);
}

/** Synchronous fallback for contexts that can't await (rare). */
export function createPainterSync(canvas: HTMLCanvasElement): WaveformPainter {
  return createCanvas2dPainter(canvas);
}
