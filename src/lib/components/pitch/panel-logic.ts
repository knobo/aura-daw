/**
 * The Pitch Coach panel's arithmetic, kept out of the component so it can be
 * tested without a canvas or a DOM. Pure functions only: no Svelte, no
 * backend, no `performance.now()`.
 *
 * The tick↔sample bijection is NOT reimplemented here — `targetNotesFor`
 * takes the caller's `midi.ticksToSamples` as an argument (ADR 0002: the
 * shipped section table is the only converter).
 */

import type { MidiClip, PitchFrame } from "../../types/ipc";
import type { TargetNote } from "../../pitch/lane";

/**
 * Where the playhead sits in fixed mode: 35% in, so two thirds of the lane
 * is the notes still coming. The trail behind is a record; the bars ahead
 * are what the singer has to act on.
 */
const PLAYHEAD_FRACTION = 0.35;

/**
 * Left edge of the lane viewport, in samples.
 *
 * `fixed` pins the playhead and scrolls the lane under it. `free` returns
 * `currentStart` untouched — the timeline's own scroll is in charge, and the
 * lane must not fight it.
 */
export function laneScrollFor(
  mode: "fixed" | "free",
  positionSamples: number,
  spp: number,
  widthPx: number,
  currentStart = 0,
): number {
  if (mode === "free") return currentStart;
  return Math.max(0, positionSamples - PLAYHEAD_FRACTION * widthPx * spp);
}

/**
 * Whether a `KeyboardEvent.key` is the configured rehearse-hold key.
 * Case-insensitive: caps lock must not disarm the hold mid-take. `"none"`
 * disables it, and never matches the literal string "none".
 */
export function rehearseKeyMatches(pref: string, key: string): boolean {
  if (pref === "none" || key.length !== 1) return false;
  return pref.toLowerCase() === key.toLowerCase();
}

/**
 * Reference-melody notes as sample spans, repeats expanded.
 *
 * The repeat rule is the timeline preview's (`utils/midi-preview.ts`):
 * `ceil(placement / content)` iterations, cropped at the placement end.
 * Phase 3 Task 12 lifts that rule into one shared Rust helper and the lane
 * will read the backend's answer instead; until then the lane must agree
 * with what the piano roll and the timeline already draw.
 *
 * Phase 2 draws EVERY note, chords included. Which note of a chord counts as
 * the target is `reference_melody`'s call in Rust (ADR 0006, ruling R4), and
 * the lane dims the ambiguous ones from phase 3 on.
 */
export function targetNotesFor(
  clips: MidiClip[],
  toSamples: (ticks: number) => number,
  /** `midi.effectiveContentLengthTicks` — the store documents itself as the
   * single accessor every content-relative reader goes through, so the loop
   * rule is taken by injection rather than inlined here a second time. */
  effectiveContentLength: (clip: MidiClip) => number = (c) => Math.max(1, c.contentLengthTicks ?? c.lengthTicks),
): TargetNote[] {
  const out: TargetNote[] = [];
  for (const clip of clips) {
    const contentTicks = Math.max(1, effectiveContentLength(clip));
    const repeats = Math.max(1, Math.ceil(clip.lengthTicks / contentTicks));
    const transpose = clip.transposeSemitones ?? 0;
    for (let rep = 0; rep < repeats; rep++) {
      const repOffset = rep * contentTicks;
      if (repOffset >= clip.lengthTicks) break;
      for (const note of clip.notes) {
        const tick = repOffset + note.tick;
        if (tick >= clip.lengthTicks) continue;
        const endTick = Math.min(tick + note.lengthTicks, clip.lengthTicks);
        out.push({
          noteId: out.length,
          startSample: toSamples(clip.timelineStartTicks + tick),
          endSample: toSamples(clip.timelineStartTicks + endTick),
          key: note.key + transpose,
        });
      }
    }
  }
  // Sorted by start, and the sort is part of the contract: the panel walks
  // this list with a cursor instead of rescanning it per frame, and clips
  // arrive in track order, not timeline order — two clips out of order
  // would make the cursor skip every note of the earlier one.
  out.sort((a, b) => a.startSample - b.startSample);
  return out.map((n, i) => ({ ...n, noteId: i }));
}

/** Median of a non-empty array; the mean of the middle pair when even. */
function median(sorted: number[]): number {
  const mid = sorted.length >> 1;
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * How steadily a run of frames holds its pitch: the median absolute
 * deviation from the median, in cents. `null` when fewer than two frames
 * are voiced.
 *
 * This is the panel's headline number, NOT distance to the nearest note.
 * The R3 checkpoint found the reason: distance-to-nearest saturates near 50
 * cents for anyone sitting midway between two semitones, which is exactly
 * where a person who cannot yet hit a pitch lives — it reads as a failure
 * and is not one. Steadiness improves visibly while accuracy is still poor,
 * and it is measured against the same run rather than against a note the
 * singer may not be aiming at. For scale: a synthetic tone through this
 * chain reads 0.1 cents; a held human vowel read 9.6.
 *
 * The median (not the mean) is what keeps a single octave-slip frame from
 * swamping an otherwise steady second.
 */
export function stabilityCents(frames: readonly PitchFrame[]): number | null {
  const midis: number[] = [];
  for (const f of frames) if (f.voiced) midis.push(f.midi);
  if (midis.length < 2) return null;
  const centre = median([...midis].sort((a, b) => a - b));
  const deviations = midis.map((m) => Math.abs(m - centre) * 100).sort((a, b) => a - b);
  return median(deviations);
}
