/** Engine range pinned by `channel_properties` TempoSet generator. */
export const TEMPO_MIN = 40;
export const TEMPO_MAX = 300;

/** Presets shown in the transport-bar meter chips. */
export const COMMON_METERS: ReadonlyArray<readonly [number, number]> = [
  [2, 4],
  [3, 4],
  [4, 4],
  [5, 4],
  [6, 8],
  [7, 8],
  [12, 8],
];

/** One decimal, clamped to the engine range. Non-finite → 120. */
export function clampTempo(bpm: number): number {
  if (!Number.isFinite(bpm)) return 120;
  const rounded = Math.round(bpm * 10) / 10;
  return Math.min(TEMPO_MAX, Math.max(TEMPO_MIN, rounded));
}

/** Typed field → clamped bpm, or null if the string is not a number. */
export function parseTempo(raw: string): number | null {
  const v = parseFloat(raw.trim());
  if (!Number.isFinite(v)) return null;
  return clampTempo(v);
}

const METER_DENS = new Set([1, 2, 4, 8, 16, 32]);

export function isValidMeter(num: number, den: number): boolean {
  if (!Number.isInteger(num) || !Number.isInteger(den)) return false;
  if (num < 1 || num > 16) return false;
  return METER_DENS.has(den);
}

/** Quarter-notes spanned by one bar of `num/den`. 6/8 is 3, not 6. */
export function quartersPerBar(num: number, den: number): number {
  if (den <= 0) return num;
  return (num * 4) / den;
}

/** One wheel tick from `current`. `deltaY < 0` is up / faster. */
export function nudgeTempo(current: number, deltaY: number, fine: boolean): number {
  const step = fine ? 0.1 : 1;
  const dir = deltaY < 0 ? step : -step;
  return clampTempo(current + dir);
}
