/**
 * Group drag: the delta comes from the ANCHOR (snapped once), every clip
 * moves by it, and the whole drag is ONE gesture-wrapped move_clips call.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Clip, MidiClip } from "../types/ipc";

const calls: string[] = [];
const gestureBegin = vi.fn(async () => {
  calls.push("gestureBegin");
});
const gestureEnd = vi.fn(async () => {
  calls.push("gestureEnd");
});
const moveClips = vi.fn(async (_placements: unknown) => {
  calls.push("moveClips");
});

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    gestureBegin: (...a: unknown[]) => gestureBegin(...(a as [])),
    gestureEnd: () => gestureEnd(),
    moveClips: (p: unknown) => moveClips(p as never),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { view } = await import("./view.svelte");
const { clipSelection } = await import("./clip-selection.svelte");
const { clipDrag } = await import("./clip-drag.svelte");

beforeEach(() => {
  calls.length = 0;
  vi.clearAllMocks();
  project.sampleRate = 48000;
  project.tempoBpm = 120;
  project.timeSignature = [4, 4];
  // 1 sample per pixel keeps the pointer maths readable in the assertions
  view.spp = 1;
  view.snap = false;
  project.clips = [
    { id: "a1", trackId: "t1", timelineStartSamples: 1000, lengthSamples: 100 } as Clip,
    { id: "a2", trackId: "t1", timelineStartSamples: 3000, lengthSamples: 100 } as Clip,
  ];
  // a flat 120bpm/960ppq map: 1 beat = 24000 samples = 960 ticks -> 25 samples/tick
  // (period = 60/120 * 508_032_000 superticks/quarter, same construction as
  // clip-edit-loop.test.ts's flat-tempo section row)
  midi.ppq = 960;
  midi.sectionTable = [
    { startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 },
  ];
  midi.clips = [{ id: "m1", trackId: "t2", timelineStartTicks: 400, lengthTicks: 960 } as MidiClip];
  clipSelection.clear();
});

describe("clipDrag group move", () => {
  it("moves every selected clip by the anchor's delta, preserving offsets", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(500, true); // alt = no snapping
    expect(project.clips[0].timelineStartSamples).toBe(1500);
    expect(project.clips[1].timelineStartSamples).toBe(3500);
    // the gap between them is untouched — the offsets requirement
    expect(project.clips[1].timelineStartSamples - project.clips[0].timelineStartSamples).toBe(2000);
  });

  it("clamps the DELTA at zero, so the group keeps its shape at the left edge", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a2" }, 0);
    clipDrag.move(-9999, true);
    expect(project.clips[0].timelineStartSamples).toBe(0);
    // offsets survive the clamp
    expect(project.clips[1].timelineStartSamples).toBe(2000);
  });

  it("snaps the anchor only — the other clips keep their off-grid offsets", () => {
    view.snap = true; // beat grid = 24000 samples at 120bpm/48k
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(23000, false);
    // the anchor lands on the grid; a2 keeps its exact 2000-sample offset
    expect(project.clips[0].timelineStartSamples % 24000).toBe(0);
    expect(project.clips[1].timelineStartSamples - project.clips[0].timelineStartSamples).toBe(2000);
  });

  it("moves a mixed audio+MIDI selection by the same wall-clock delta", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(2500, true); // +2500 samples = +100 ticks at 25 samples/tick
    expect(project.clips[0].timelineStartSamples).toBe(3500);
    expect(midi.clips[0].timelineStartTicks).toBe(500);
  });

  it("emits gestureBegin, then exactly one moveClips, then gestureEnd — in that order", async () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(100, true);
    clipDrag.move(200, true);
    clipDrag.move(300, true);
    await clipDrag.end();
    expect(calls).toEqual(["gestureBegin", "moveClips", "gestureEnd"]);
    expect(moveClips).toHaveBeenCalledTimes(1);
    expect(moveClips).toHaveBeenCalledWith([
      { kind: "audio", clipId: "a1", timelineStartSamples: 1300 },
      { kind: "midi", clipId: "m1", timelineStartTicks: 412 },
    ]);
  });

  it("a drag that never moved sends no moveClips but still closes the gesture", async () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    await clipDrag.end();
    expect(moveClips).not.toHaveBeenCalled();
    expect(calls).toEqual(["gestureBegin", "gestureEnd"]);
  });

  it("dragging an unselected clip drags only that clip", () => {
    clipSelection.apply([{ kind: "audio", id: "a2" }], "replace");
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(500, true);
    expect(project.clips[0].timelineStartSamples).toBe(1500);
    // a2 was not part of this drag
    expect(project.clips[1].timelineStartSamples).toBe(3000);
  });

  it("resize snaps the clip's END, not its start — parity with the pre-refactor drag", () => {
    view.snap = true; // beat/4 grid = 6000 samples = 240 ticks at spp=1
    // length (500 ticks) is deliberately NOT a multiple of the 240-tick
    // grid, so start-anchored vs end-anchored snapping disagree.
    midi.clips = [{ id: "m2", trackId: "t2", timelineStartTicks: 0, lengthTicks: 500 } as MidiClip];
    clipSelection.apply([{ kind: "midi", id: "m2" }], "replace");
    clipDrag.begin({ kind: "midi", id: "m2" }, 0, "resize");
    // tiny nudge (dx=3 samples raw) — the snap does the real work: raw end
    // 12500+3=12503 samples snaps to the nearest 6000-multiple, 12000.
    clipDrag.move(3, false);
    // end lands at 12000 samples = 480 ticks, a multiple of the 240-tick
    // grid; length equals endTicks since the clip starts at 0. Anchoring
    // the START instead (the bug) would leave the length at 500 unchanged
    // for this same input.
    expect(midi.clips[0].lengthTicks).toBe(480);
    expect(midi.clips[0].lengthTicks % 240).toBe(0);
  });

  it("cancel restores every previewed position and sends no moveClips", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "audio", id: "a1" }, 0);
    clipDrag.move(500, true);
    // preview applied — confirms there is something to undo
    expect(project.clips[0].timelineStartSamples).toBe(1500);
    expect(midi.clips[0].timelineStartTicks).toBe(420);
    clipDrag.cancel();
    expect(project.clips[0].timelineStartSamples).toBe(1000);
    expect(midi.clips[0].timelineStartTicks).toBe(400);
    expect(moveClips).not.toHaveBeenCalled();
    expect(calls).toEqual(["gestureBegin", "gestureEnd"]);
  });

  it("cancel restores a resize's length preview too, not just position", () => {
    clipSelection.apply([{ kind: "midi", id: "m1" }], "replace");
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(2500, true); // +2500 samples = +100 ticks
    // preview applied — confirms there is something to undo
    expect(midi.clips[0].lengthTicks).toBe(1060);
    expect(midi.clips[0].contentLengthTicks).toBe(960);
    clipDrag.cancel();
    expect(midi.clips[0].lengthTicks).toBe(960);
    expect(midi.clips[0].timelineStartTicks).toBe(400);
    expect(moveClips).not.toHaveBeenCalled();
    expect(calls).toEqual(["gestureBegin", "gestureEnd"]);
  });
});

describe("clipDrag group resize (loop length)", () => {
  beforeEach(() => {
    midi.clips = [
      { id: "m1", trackId: "t2", timelineStartTicks: 0, lengthTicks: 960 } as MidiClip,
      { id: "m2", trackId: "t2", timelineStartTicks: 4800, lengthTicks: 1920 } as MidiClip,
    ];
  });

  it("adds the same tick delta to every selected MIDI clip's length", () => {
    clipSelection.apply(
      [
        { kind: "midi", id: "m1" },
        { kind: "midi", id: "m2" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(2500, true); // +2500 samples = +100 ticks
    expect(midi.clips[0].lengthTicks).toBe(1060);
    expect(midi.clips[1].lengthTicks).toBe(2020);
  });

  it("never shrinks a clip below one tick", () => {
    clipSelection.apply([{ kind: "midi", id: "m1" }], "replace");
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(-999999, true);
    expect(midi.clips[0].lengthTicks).toBe(1);
  });

  it("pins each clip's content length at drag start, so a first resize establishes it", () => {
    clipSelection.apply([{ kind: "midi", id: "m1" }], "replace");
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(2500, true);
    // m1 had no explicit content length; its start-of-gesture placement
    // length (960) becomes the content length, as MidiClipView's rule says
    expect(midi.clips[0].contentLengthTicks).toBe(960);
  });

  it("sends one move_clips carrying bounds, and leaves audio clips out", async () => {
    project.clips = [
      { id: "a1", trackId: "t1", timelineStartSamples: 0, lengthSamples: 100 } as Clip,
    ];
    clipSelection.apply(
      [
        { kind: "midi", id: "m1" },
        { kind: "audio", id: "a1" },
      ],
      "replace",
    );
    clipDrag.begin({ kind: "midi", id: "m1" }, 0, "resize");
    clipDrag.move(2500, true);
    await clipDrag.end();
    expect(moveClips).toHaveBeenCalledWith([
      {
        kind: "midi",
        clipId: "m1",
        timelineStartTicks: 0,
        lengthTicks: 1060,
        contentLengthTicks: 960,
      },
    ]);
  });
});
