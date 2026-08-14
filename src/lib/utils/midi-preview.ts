/**
 * Pure geometry for the timeline clip's mini note-preview (MidiClipView's
 * canvas). Content repeats across the placement (clip looping): px-per-tick
 * derives from the PLACEMENT length — the canvas spans the whole placement —
 * so each repeat occupies its real share of the clip instead of one
 * iteration stretching across all of it.
 */

import type { MidiNote } from "../types/ipc";

export interface PreviewNoteRect {
  x: number;
  y: number;
  w: number;
  h: number;
  /** 1..127 — the component maps this to fill alpha. */
  velocity: number;
}

export interface PreviewLayout {
  rects: PreviewNoteRect[];
  /** Canvas-x of each content-boundary separator (rep 0's edge excluded). */
  separatorXs: number[];
}

export function midiPreviewLayout(opts: {
  notes: Pick<MidiNote, "tick" | "lengthTicks" | "key" | "velocity">[];
  /** Placement length, ticks. */
  lengthTicks: number;
  /** Effective content length, ticks (>= 1). */
  contentLengthTicks: number;
  /** Full clip width, css px. */
  widthPx: number;
  /** Canvas origin within the clip, css px (the visible-left crop). */
  offsetPx: number;
  canvasW: number;
  canvasH: number;
}): PreviewLayout {
  const { notes, lengthTicks, contentLengthTicks, widthPx, offsetPx, canvasW, canvasH } = opts;
  const rects: PreviewNoteRect[] = [];
  const separatorXs: number[] = [];
  if (notes.length === 0) return { rects, separatorXs };

  let lo = 127;
  let hi = 0;
  for (const n of notes) {
    if (n.key < lo) lo = n.key;
    if (n.key > hi) hi = n.key;
  }
  lo -= 2;
  hi += 2;
  const rowH = Math.min(4, canvasH / (hi - lo + 1));
  const contentTicks = Math.max(1, contentLengthTicks);
  const pxPerTick = widthPx / Math.max(1, lengthTicks);
  const repeats = Math.max(1, Math.ceil(lengthTicks / contentTicks));

  for (let rep = 0; rep < repeats; rep++) {
    const repOffsetTicks = rep * contentTicks;
    if (repOffsetTicks >= lengthTicks) break;
    for (const n of notes) {
      const tick = repOffsetTicks + n.tick;
      if (tick >= lengthTicks) continue; // cropped by the placement end
      const x = tick * pxPerTick - offsetPx;
      const w = Math.max(1.5, n.lengthTicks * pxPerTick);
      if (x + w <= 0 || x >= canvasW) continue;
      const y = canvasH - ((n.key - lo + 1) / (hi - lo + 1)) * canvasH;
      rects.push({ x, y, w, h: Math.max(1.5, rowH - 1), velocity: n.velocity });
    }
    if (rep > 0) {
      const sepX = repOffsetTicks * pxPerTick - offsetPx;
      if (sepX >= 0 && sepX <= canvasW) separatorXs.push(sepX);
    }
  }
  return { rects, separatorXs };
}
