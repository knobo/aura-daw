/**
 * Pure geometry and colour-banding helpers for the Pitch Coach lane.
 *
 * No DOM, no Svelte, no backend — everything here is a function of numbers,
 * so the lane's behaviour is testable without a canvas. The component owns
 * the pixels; this file owns the arithmetic that decides where they go.
 *
 * Nothing here converts ticks or samples: the reference notes arrive already
 * in samples (`midi.ticksToSamples()`), per ADR 0002 and ADR 0006.
 */

import type { PitchFrame } from "../types/ipc";

/** A reference-melody note, already resolved to device samples. */
export interface TargetNote {
  /** Stable identity for keyed redraws; the caller decides what it means. */
  noteId: number;
  startSample: number;
  endSample: number;
  /** MIDI key number (integer). */
  key: number;
}

/** Vertical extent of the lane, in MIDI key numbers. */
export interface LaneRange {
  lowKey: number;
  highKey: number;
}

/** Semitones of air above and below the phrase. */
const PAD_KEYS = 2;
/**
 * Smallest lane, in semitones. An octave is not decoration: someone who
 * cannot hit a pitch misses by more than the two-semitone padding, and a
 * lane fitted tightly to a short phrase would park their trail off-screen
 * for the whole take.
 */
const MIN_SPAN_KEYS = 12;

/**
 * Vertical range that fits `notes` with padding, never narrower than
 * `MIN_SPAN_KEYS`. With no notes it centres an octave on `fallbackKey`.
 */
export function autoFitRange(notes: TargetNote[], fallbackKey = 60): LaneRange {
  let low: number;
  let high: number;
  if (notes.length === 0) {
    low = fallbackKey;
    high = fallbackKey;
  } else {
    low = Infinity;
    high = -Infinity;
    for (const n of notes) {
      if (n.key < low) low = n.key;
      if (n.key > high) high = n.key;
    }
  }

  low -= PAD_KEYS;
  high += PAD_KEYS;

  const grow = MIN_SPAN_KEYS - (high - low);
  if (grow > 0) {
    low -= grow / 2;
    high += grow / 2;
  }
  return { lowKey: low, highKey: high };
}

/**
 * Pixel y of a key: `highKey` at the top (0), `lowKey` at the bottom
 * (`height`). Accepts a float key — the trail draws `PitchFrame.midi`, not a
 * rounded note, because the rounded note flips across a semitone boundary
 * while the pitch barely moves.
 */
export function yOfKey(key: number, range: LaneRange, height: number): number {
  const span = range.highKey - range.lowKey;
  if (span <= 0) return height / 2;
  return ((range.highKey - key) / span) * height;
}

/**
 * Signed cents from `midi` to the nearest occurrence of `key` in ANY octave,
 * in [-600, 600). Folding means an octave slip reads as on-pitch, which is
 * the honest call: a bass singing the melody an octave down is singing the
 * melody.
 */
export function centsToTarget(midi: number, key: number): number {
  let diff = midi - key;
  diff -= 12 * Math.round(diff / 12);
  // Math.round breaks the tie upward, so +6 folds to -6 and the range stays
  // half-open rather than containing both endpoints.
  return diff * 100;
}

export type PitchBand = "in" | "near" | "far";

/**
 * Which colour band a cents error falls in. `near` runs to twice the
 * tolerance — the zone where someone is audibly reaching for the note and
 * should see encouragement, not a red line.
 */
export function bandFor(cents: number, toleranceCents: number): PitchBand {
  const err = Math.abs(cents);
  if (err <= toleranceCents) return "in";
  if (err <= toleranceCents * 2) return "near";
  return "far";
}

/**
 * Split frames into runs of consecutive voiced frames. The trail must break
 * on a breath rather than interpolate across it: a straight line drawn over
 * a two-second silence claims the singer held a glide they never sang.
 */
export function trailSegments(frames: readonly PitchFrame[]): PitchFrame[][] {
  const segments: PitchFrame[][] = [];
  let current: PitchFrame[] | null = null;
  for (const frame of frames) {
    if (!frame.voiced) {
      current = null;
      continue;
    }
    if (!current) {
      current = [];
      segments.push(current);
    }
    current.push(frame);
  }
  return segments;
}
