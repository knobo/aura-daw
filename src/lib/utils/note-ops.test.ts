import { describe, expect, it } from "vitest";
import type { MidiNote } from "../types/ipc";
import {
  applyMarquee,
  copySelection,
  marqueeHits,
  nudgeSelection,
  pasteNotes,
  quantizeSelection,
  sortWithSelection,
  toggleIndex,
  transposeSelection,
} from "./note-ops";

const note = (tick: number, key: number, lengthTicks = 240, velocity = 100): MidiNote => ({
  tick,
  lengthTicks,
  key,
  velocity,
  channel: 0,
});

describe("sortWithSelection", () => {
  it("sorts by (tick, key) and remaps selection indices to the new order", () => {
    const notes = [note(960, 60), note(0, 64), note(0, 60)];
    const { notes: sorted, selection } = sortWithSelection(notes, new Set([0, 2]));
    expect(sorted.map((n) => [n.tick, n.key])).toEqual([
      [0, 60],
      [0, 64],
      [960, 60],
    ]);
    // note(960,60) was index 0 → now 2; note(0,60) was index 2 → now 0
    expect(selection).toEqual(new Set([2, 0]));
  });

  it("keeps an already-sorted array's selection unchanged", () => {
    const notes = [note(0, 60), note(240, 62)];
    const { selection } = sortWithSelection(notes, new Set([1]));
    expect(selection).toEqual(new Set([1]));
  });
});

describe("marqueeHits", () => {
  const notes = [note(0, 60), note(240, 64), note(960, 72)];

  it("selects notes overlapping the tick range within the key range", () => {
    expect(marqueeHits(notes, 300, 500, 60, 71)).toEqual([1]);
    expect(marqueeHits(notes, 0, 1000, 60, 72)).toEqual([0, 1, 2]);
  });

  it("counts a note whose tail overlaps the range", () => {
    // note at 0 with length 240 reaches into [100, 200]
    expect(marqueeHits(notes, 100, 200, 60, 60)).toEqual([0]);
  });

  it("excludes notes outside the key range", () => {
    expect(marqueeHits(notes, 0, 1000, 65, 80)).toEqual([2]);
  });
});

describe("applyMarquee", () => {
  it("replace: the hits become the selection", () => {
    expect(applyMarquee(new Set([5]), [1, 2], "replace")).toEqual(new Set([1, 2]));
  });
  it("add: unions hits into the selection", () => {
    expect(applyMarquee(new Set([5]), [1, 2], "add")).toEqual(new Set([1, 2, 5]));
  });
  it("subtract: removes hits from the selection", () => {
    expect(applyMarquee(new Set([1, 2, 5]), [1, 2], "subtract")).toEqual(new Set([5]));
  });
});

describe("toggleIndex", () => {
  it("adds an unselected note and removes a selected one", () => {
    expect(toggleIndex(new Set([1]), 2)).toEqual(new Set([1, 2]));
    expect(toggleIndex(new Set([1, 2]), 2)).toEqual(new Set([1]));
  });
});

describe("transposeSelection", () => {
  it("transposes only the selected notes", () => {
    const notes = [note(0, 60), note(240, 64)];
    const out = transposeSelection(notes, new Set([1]), 12);
    expect(out?.map((n) => n.key)).toEqual([60, 76]);
  });

  it("returns null when any selected note would leave the MIDI range", () => {
    // blocking (instead of clamping) preserves chord intervals
    const notes = [note(0, 120), note(0, 60)];
    expect(transposeSelection(notes, new Set([0, 1]), 12)).toBeNull();
    expect(transposeSelection(notes, new Set([1]), -61)).toBeNull();
  });
});

describe("nudgeSelection", () => {
  it("moves only the selected notes in time", () => {
    const notes = [note(0, 60), note(240, 64)];
    const out = nudgeSelection(notes, new Set([1]), 240);
    expect(out?.map((n) => n.tick)).toEqual([0, 480]);
  });

  it("returns null when any selected note would land before tick 0", () => {
    const notes = [note(120, 60), note(960, 64)];
    expect(nudgeSelection(notes, new Set([0, 1]), -240)).toBeNull();
  });
});

describe("copySelection", () => {
  it("returns deep copies of the selected notes sorted by (tick, key)", () => {
    const notes = [note(960, 60), note(0, 64)];
    const copied = copySelection(notes, new Set([0, 1]));
    expect(copied.map((n) => n.tick)).toEqual([0, 960]);
    copied[0].tick = 999;
    expect(notes[1].tick).toBe(0);
  });

  it("strips the backend-minted noteId so copies get fresh identities", () => {
    // keeping the id would make a same-clip paste send it twice, and the
    // backend's keep-rule then re-mints BOTH notes — the original loses
    // its stable id
    const notes = [{ ...note(0, 60), noteId: 7 }];
    const copied = copySelection(notes, new Set([0]));
    expect("noteId" in copied[0]).toBe(false);
  });

  it("returns an empty array for an empty selection", () => {
    expect(copySelection([note(0, 60)], new Set())).toEqual([]);
  });
});

describe("pasteNotes", () => {
  it("appends deep copies and selects exactly the pasted notes", () => {
    const existing = [note(0, 60)];
    const clip = [note(240, 64), note(480, 67)];
    const { notes, selection, dropped } = pasteNotes(existing, clip, 1920);
    expect(notes).toHaveLength(3);
    expect(selection).toEqual(new Set([1, 2]));
    expect(dropped).toBe(0);
    // deep copy: mutating the result must not touch the clipboard
    notes[1].key = 0;
    expect(clip[0].key).toBe(64);
  });

  it("drops clipboard notes starting at or past the content length and reports the count", () => {
    const { notes, selection, dropped } = pasteNotes([], [note(0, 60), note(1920, 62)], 1920);
    expect(notes).toHaveLength(1);
    expect(selection).toEqual(new Set([0]));
    expect(dropped).toBe(1);
  });
});

describe("quantizeSelection", () => {
  it("snaps selected note starts toward the nearest grid tick", () => {
    const notes = [note(100, 60, 200), note(500, 64, 200)];
    const { notes: out, selection } = quantizeSelection(notes, new Set([0, 1]), 480, 1);
    expect(out.map((n) => n.tick)).toEqual([0, 480]);
    expect(out.map((n) => n.lengthTicks)).toEqual([200, 200]);
    expect(selection).toEqual(new Set([0, 1]));
  });

  it("strength blends between the original tick and the grid", () => {
    const notes = [note(100, 60)];
    const { notes: out } = quantizeSelection(notes, new Set([0]), 480, 0.5);
    expect(out[0].tick).toBe(50); // 100 + (0 - 100) * 0.5
  });

  it("leaves unselected notes alone", () => {
    const notes = [note(100, 60), note(500, 64)];
    const { notes: out } = quantizeSelection(notes, new Set([0]), 480, 1);
    expect(out.map((n) => n.tick)).toEqual([0, 500]);
  });

  it("optionally snaps lengths to the grid as well", () => {
    const notes = [note(100, 60, 250)];
    const { notes: out } = quantizeSelection(notes, new Set([0]), 480, 1, true);
    expect(out[0].tick).toBe(0);
    expect(out[0].lengthTicks).toBe(480);
  });

  it("never lets a quantized length fall below 1 tick", () => {
    const notes = [note(0, 60, 10)];
    const { notes: out } = quantizeSelection(notes, new Set([0]), 480, 1, true);
    expect(out[0].lengthTicks).toBe(1);
  });

  it("is a no-op for an empty selection", () => {
    const notes = [note(100, 60)];
    const { notes: out, selection } = quantizeSelection(notes, new Set(), 480, 1);
    expect(out).toEqual(notes);
    expect(selection.size).toBe(0);
  });
});
