/**
 * Pure presentation helpers for the take report.
 *
 * Same discipline as `lane.ts`: no DOM, no Svelte, no backend. The backend
 * decides every number in a `PitchScoreReport` — including the rating word —
 * and nothing here recomputes one (ADR 0006). What lives here is the
 * arithmetic that turns those numbers into pixels and row order.
 */

import { bandFor, type PitchBand } from "./lane";
import type { NoteScore } from "../types/ipc";

/**
 * Half the bar shows this many tolerances' worth of error.
 *
 * Scaling to the tolerance rather than to a fixed cents span is the point:
 * the strict tier and the loose tier disagree about what 25 cents means, and
 * a bar that draws them identically hides exactly the distinction the tier
 * was chosen to make. Four is wide enough that an ordinary miss is a visible
 * length rather than a pin against the end.
 */
const FULL_SCALE_TOLERANCES = 4;

/** A bar drawn out from the centre of a `widthPx`-wide track. */
export interface CentsBar {
  /** Left edge, px. */
  x: number;
  /** Length, px. Zero exactly on pitch — the caller draws its own minimum
   * so an on-pitch note still has a visible mark. */
  w: number;
  /** Same banding the lane paints the live trail with. */
  band: PitchBand;
}

/**
 * Geometry of the signed cents bar: it grows out of the centre line, right
 * when sharp and left when flat.
 *
 * Signed, not absolute, because the direction is the only part a singer can
 * act on — "you are under it" is an instruction and "you are off it" is not.
 * A wild reading clamps to the end of the track instead of drawing outside
 * it; the `far` band is what says it was clamped.
 */
export function centsBarGeometry(cents: number, toleranceCents: number, widthPx: number): CentsBar {
  const half = widthPx / 2;
  const band = bandFor(cents, toleranceCents);
  // A zero (or absent) tolerance would otherwise divide by zero and put the
  // bar at NaN, which silently blanks the whole column.
  const fullScale = Math.max(1e-6, toleranceCents * FULL_SCALE_TOLERANCES);
  const clamped = Math.max(-1, Math.min(1, (cents || 0) / fullScale));
  const px = clamped * half;
  return px >= 0 ? { x: half, w: px, band } : { x: half + px, w: -px, band };
}

export type NoteSort = "time" | "worst";

/**
 * Row order. Returns a new array — the caller's list is the store's, and
 * sorting it in place would reorder the melody itself.
 *
 * `"worst"` exists so practice starts where it pays. Ties break on coverage
 * (a note that was never sung ranks below one that was sung out of tune —
 * same zero hit fraction, different problem) and then on time, so the order
 * is stable between renders instead of shuffling on every refetch.
 */
export function sortNotes(notes: NoteScore[], by: NoteSort): NoteScore[] {
  const rows = notes.slice();
  if (by === "time") {
    rows.sort((a, b) => a.startSample - b.startSample || a.noteId - b.noteId);
    return rows;
  }
  rows.sort(
    (a, b) =>
      a.hitFraction - b.hitFraction ||
      a.coverage - b.coverage ||
      a.startSample - b.startSample ||
      a.noteId - b.noteId,
  );
  return rows;
}

const NOTE_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/** Scientific pitch name for an integer MIDI key. Reference notes are
 * integers, so the semitone-boundary strobe that bans note names on the live
 * trail does not apply here — the target genuinely is one named note. */
export function keyName(key: number): string {
  const k = Math.round(key);
  return `${NOTE_NAMES[((k % 12) + 12) % 12]}${Math.floor(k / 12) - 1}`;
}
