/**
 * Pure note-selection operations for the piano roll. The roll keeps its
 * selection as INDICES into the working array, and the midi store re-sorts
 * notes by (tick, key) on every setNotes — so every operation that reorders
 * or appends must hand back the matching selection, or the highlight drifts
 * to the wrong notes after the store echo. Everything here is pure so it
 * unit-tests without a canvas.
 */

import type { MidiNote } from "../types/ipc";

export type MarqueeMode = "replace" | "add" | "subtract";

/** THE note ordering — (tick, key), matching the backend's
 * `assign_incoming_note_ids` sort. Import this everywhere frontend code
 * sorts notes (midi store, demo backend) so the index-based selection
 * remap in `sortWithSelection` can never diverge from the echoed order. */
export const byTickKey = (a: MidiNote, b: MidiNote) => a.tick - b.tick || a.key - b.key;

/** Deep-copy a note WITHOUT its backend-minted id: a copy is a new note,
 * and resending an existing id makes the backend's keep-rule re-mint both
 * the copy and the original (id stability lost). Exported so any call site
 * that materializes "new" notes from someone else's ids (paste, AMT infill
 * fills) can strip them the same way instead of re-deriving the rule. */
export function copyNote(n: MidiNote): MidiNote {
  const { noteId: _dropId, ...copy } = n;
  return copy;
}

/** Sort notes the way the store does and remap selection indices along. */
export function sortWithSelection(
  notes: MidiNote[],
  selection: Set<number>,
): { notes: MidiNote[]; selection: Set<number> } {
  const order = notes.map((_, i) => i).sort((a, b) => byTickKey(notes[a], notes[b]));
  return {
    notes: order.map((i) => notes[i]),
    selection: new Set(
      order.map((from, to) => (selection.has(from) ? to : -1)).filter((i) => i >= 0),
    ),
  };
}

/** Indices of notes overlapping [t0, t1] in time with key in [kLo, kHi]. */
export function marqueeHits(
  notes: MidiNote[],
  t0: number,
  t1: number,
  kLo: number,
  kHi: number,
): number[] {
  const hits: number[] = [];
  notes.forEach((n, i) => {
    if (n.tick + n.lengthTicks > t0 && n.tick < t1 && n.key >= kLo && n.key <= kHi) hits.push(i);
  });
  return hits;
}

export function applyMarquee(
  current: Set<number>,
  hits: number[],
  mode: MarqueeMode,
): Set<number> {
  if (mode === "replace") return new Set(hits);
  const next = new Set(current);
  for (const i of hits) mode === "add" ? next.add(i) : next.delete(i);
  return next;
}

export function toggleIndex(selection: Set<number>, index: number): Set<number> {
  const next = new Set(selection);
  next.has(index) ? next.delete(index) : next.add(index);
  return next;
}

/** Transpose the selected notes by dKey semitones; null = blocked because a
 * note would leave 0..127 (blocking preserves chord intervals). */
export function transposeSelection(
  notes: MidiNote[],
  selection: Set<number>,
  dKey: number,
): MidiNote[] | null {
  for (const i of selection) {
    const k = notes[i].key + dKey;
    if (k < 0 || k > 127) return null;
  }
  return notes.map((n, i) => (selection.has(i) ? { ...n, key: n.key + dKey } : n));
}

/** Shift the selected notes by dTick; null = blocked because a note would
 * start before tick 0 (blocking preserves the rhythm's internal offsets). */
export function nudgeSelection(
  notes: MidiNote[],
  selection: Set<number>,
  dTick: number,
): MidiNote[] | null {
  for (const i of selection) {
    if (notes[i].tick + dTick < 0) return null;
  }
  return notes.map((n, i) => (selection.has(i) ? { ...n, tick: n.tick + dTick } : n));
}

/** Deep copies of the selected notes (ids stripped), sorted by (tick, key). */
export function copySelection(notes: MidiNote[], selection: Set<number>): MidiNote[] {
  return [...selection]
    .map((i) => copyNote(notes[i]))
    .sort(byTickKey);
}

function snapToward(value: number, grid: number, strength: number): number {
  if (grid <= 0) return value;
  const target = Math.round(value / grid) * grid;
  const s = Math.min(1, Math.max(0, strength));
  return Math.round(value + (target - value) * s);
}

/** Snap selected note starts (and optionally lengths) toward `gridTicks`.
 * `strength` is 0..1 — 1 is full snap, 0.5 lands halfway. One `midi_set_notes`
 * commit at the call site = one undo step. */
export function quantizeSelection(
  notes: MidiNote[],
  selection: Set<number>,
  gridTicks: number,
  strength: number,
  lengths = false,
): { notes: MidiNote[]; selection: Set<number> } {
  if (selection.size === 0 || gridTicks <= 0) return { notes, selection };
  const next = notes.map((n, i) => {
    if (!selection.has(i)) return n;
    const tick = Math.max(0, snapToward(n.tick, gridTicks, strength));
    const lengthTicks = lengths
      ? Math.max(1, snapToward(n.lengthTicks, gridTicks, strength))
      : n.lengthTicks;
    return { ...n, tick, lengthTicks };
  });
  return sortWithSelection(next, selection);
}

/** Append deep copies of the clipboard and select exactly the pasted notes.
 * Notes starting past the content end are dropped — `dropped` reports how
 * many, so the caller can tell the user instead of losing them silently. */
export function pasteNotes(
  notes: MidiNote[],
  clipboard: MidiNote[],
  contentLengthTicks: number,
): { notes: MidiNote[]; selection: Set<number>; dropped: number } {
  const pasted = clipboard.filter((n) => n.tick < contentLengthTicks).map(copyNote);
  return {
    notes: [...notes, ...pasted],
    selection: new Set(pasted.map((_, j) => notes.length + j)),
    dropped: clipboard.length - pasted.length,
  };
}
