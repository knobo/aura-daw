/**
 * AWTF tile parsing + LRU cache + peak-column assembly.
 *
 * Wire format (little-endian):
 *   [magic u32 = 0x41575446 "AWTF"][version u16 = 1][channels u16]
 *   [lod u32][tileIndex u64][bins u32]
 *   then channels × bins × (min i16, max i16)
 *
 * Bin width = 2^(8+lod) source samples; a tile spans 1024 bins.
 */

import { backend } from "../tauri";
import { AWTF_BINS_PER_TILE, AWTF_MAGIC, type WaveformTile } from "../types/ipc";

export function parseAwtf(buf: ArrayBuffer): WaveformTile {
  const view = new DataView(buf);
  const magic = view.getUint32(0, true);
  if (magic !== AWTF_MAGIC) throw new Error(`bad AWTF magic 0x${magic.toString(16)}`);
  const version = view.getUint16(4, true);
  if (version !== 1) throw new Error(`unsupported AWTF version ${version}`);
  const channels = view.getUint16(6, true);
  const lod = view.getUint32(8, true);
  const tileIndex = Number(view.getBigUint64(12, true));
  const bins = view.getUint32(20, true);
  const data = new Int16Array(buf, 24, channels * bins * 2);
  return { channels, lod, tileIndex, bins, data };
}

const MAX_CACHED_TILES = 512;

class TileCache {
  private cache = new Map<string, Promise<WaveformTile>>();

  get(clipId: string, lod: number, tileIndex: number): Promise<WaveformTile> {
    const key = `${clipId}/${lod}/${tileIndex}`;
    let entry = this.cache.get(key);
    if (entry) {
      // refresh LRU position
      this.cache.delete(key);
      this.cache.set(key, entry);
      return entry;
    }
    entry = backend
      .getWaveformTile({ clipId, lod, tileIndex })
      .then(parseAwtf)
      .catch((err) => {
        this.cache.delete(key);
        throw err;
      });
    this.cache.set(key, entry);
    while (this.cache.size > MAX_CACHED_TILES) {
      const oldest = this.cache.keys().next().value as string;
      this.cache.delete(oldest);
    }
    return entry;
  }

  evictClip(clipId: string) {
    for (const key of [...this.cache.keys()]) {
      if (key.startsWith(clipId + "/")) this.cache.delete(key);
    }
  }
}

export const tileCache = new TileCache();

/** Pick the finest LOD whose bins are not finer than one pixel column. */
export function lodForSamplesPerPixel(spp: number): number {
  const lod = Math.floor(Math.log2(Math.max(256, spp))) - 8;
  return Math.max(0, Math.min(16, lod));
}

/**
 * Build per-pixel-column [min,max] pairs (normalized -1..1, channels merged)
 * for `width` columns covering source samples [startSample, startSample +
 * width*spp) of a clip. Async: awaits the covering tiles.
 */
export async function buildPeakColumns(
  clipId: string,
  startSample: number,
  spp: number,
  width: number,
): Promise<Float32Array> {
  const lod = lodForSamplesPerPixel(spp);
  const binWidth = Math.pow(2, 8 + lod);
  const firstBin = Math.floor(Math.max(0, startSample) / binWidth);
  const lastBin = Math.floor(Math.max(0, startSample + width * spp) / binWidth);
  const firstTile = Math.floor(firstBin / AWTF_BINS_PER_TILE);
  const lastTile = Math.floor(lastBin / AWTF_BINS_PER_TILE);

  const tiles = new Map<number, WaveformTile>();
  const fetches: Promise<void>[] = [];
  for (let t = firstTile; t <= lastTile; t++) {
    fetches.push(
      tileCache.get(clipId, lod, t).then((tile) => {
        tiles.set(t, tile);
      }),
    );
  }
  await Promise.all(fetches);

  const columns = new Float32Array(width * 2);
  const inv = 1 / 32767;
  for (let x = 0; x < width; x++) {
    const s0 = startSample + x * spp;
    const s1 = s0 + spp;
    let b0 = Math.floor(s0 / binWidth);
    let b1 = Math.max(b0, Math.ceil(s1 / binWidth) - 1);
    let min = 0;
    let max = 0;
    let any = false;
    for (let b = b0; b <= b1; b++) {
      if (b < 0) continue;
      const tile = tiles.get(Math.floor(b / AWTF_BINS_PER_TILE));
      if (!tile) continue;
      const local = b - tile.tileIndex * AWTF_BINS_PER_TILE;
      if (local < 0 || local >= tile.bins) continue;
      for (let ch = 0; ch < tile.channels; ch++) {
        const base = ch * tile.bins * 2 + local * 2;
        const lo = tile.data[base] * inv;
        const hi = tile.data[base + 1] * inv;
        if (!any) {
          min = lo;
          max = hi;
          any = true;
        } else {
          if (lo < min) min = lo;
          if (hi > max) max = hi;
        }
      }
    }
    columns[x * 2] = any ? min : 0;
    columns[x * 2 + 1] = any ? max : 0;
  }
  return columns;
}
