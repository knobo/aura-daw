/**
 * A recorded MIDI take reaching the timeline. `project://changed` carries
 * only the audio-shaped `Project`, so nothing else pulls the take's MIDI
 * clip in — the store adopts it from `recording://state`'s `midiClipId`.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AuraEventName, MidiClip } from "../types/ipc";

const takeClip: MidiClip = {
  id: "m-take",
  trackId: "A",
  name: "MIDI Take 1",
  timelineStartTicks: 1920,
  lengthTicks: 960,
  notes: [{ tick: 0, lengthTicks: 240, key: 60, velocity: 100 }],
} as MidiClip;

const midiGetClips = vi.fn(() => Promise.resolve([takeClip]));
const handlers: Record<string, ((payload: unknown) => void)[]> = {};

vi.mock("../tauri", () => ({
  backend: {
    on: (event: AuraEventName, cb: (payload: unknown) => void) => {
      (handlers[event] ??= []).push(cb);
      return () => {};
    },
    midiGetClips: () => midiGetClips(),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { midi } = await import("./midi.svelte");

function emitStop(payload: Record<string, unknown>) {
  for (const cb of handlers["recording://state"] ?? []) cb(payload);
}

// `handlers` is deliberately NOT cleared between tests: the subscription is
// process-lifetime state, and the last test asserts it never doubled up.
beforeEach(async () => {
  vi.clearAllMocks();
  midi.clips = [];
  midi.selectedClipId = null;
  midi.flashClipId = null;
  await midi.init();
});

describe("recording://state → the take's MIDI clip", () => {
  it("pulls, selects and flashes the clip the take registered", async () => {
    emitStop({ recording: false, trackIds: ["A"], clips: [], midiClipId: "m-take" });
    await vi.waitFor(() => expect(midi.clips.map((c) => c.id)).toEqual(["m-take"]));
    expect(midi.selectedClipId).toBe("m-take");
    expect(midi.flashClipId).toBe("m-take");
  });

  it("ignores an audio-only take (no midiClipId) and the start notification", async () => {
    emitStop({ recording: false, trackIds: ["A"], clips: [] });
    emitStop({ recording: true, trackIds: ["A"], midiClipId: "m-take" });
    await Promise.resolve();
    expect(midiGetClips).not.toHaveBeenCalled();
    expect(midi.selectedClipId).toBeNull();
  });

  it("subscribes once no matter how often init() runs (undo/redo re-pulls it)", async () => {
    await midi.init();
    await midi.init();
    expect(handlers["recording://state"]?.length ?? 0).toBe(1);
  });
});
