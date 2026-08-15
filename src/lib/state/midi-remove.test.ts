/**
 * MIDI clip delete: `midi.removeClip` is the MIDI-side twin of
 * `project.removeClip` — optimistic local removal, then `midi_remove_clip`
 * through the backend, clearing any selection/editor/flash state that
 * pointed at the removed clip.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MidiClip } from "../types/ipc";

const midiRemoveClip = vi.fn(() => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    midiRemoveClip: (...a: Parameters<typeof midiRemoveClip>) => midiRemoveClip(...a),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { midi } = await import("./midi.svelte");

function testClip(overrides: Partial<MidiClip> = {}): MidiClip {
  return {
    id: "c-1",
    trackId: "t-1",
    name: "riff",
    timelineStartTicks: 0,
    lengthTicks: 1920,
    notes: [],
    ...overrides,
  } as MidiClip;
}

beforeEach(() => {
  vi.clearAllMocks();
  midi.clips = [];
  midi.selectedClipId = null;
  midi.openClipId = null;
  midi.flashClipId = null;
});

describe("removeClip", () => {
  it("removes the clip from the store and invokes backend.midiRemoveClip", async () => {
    midi.clips = [testClip({ id: "c-1" }), testClip({ id: "c-2" })];

    await midi.removeClip("c-1");

    expect(midi.clips.map((c) => c.id)).toEqual(["c-2"]);
    expect(midiRemoveClip).toHaveBeenCalledWith("c-1");
  });

  it("clears selection, editor, and flash state pointing at the removed clip", async () => {
    midi.clips = [testClip({ id: "c-1" })];
    midi.select("c-1");
    midi.openClipId = "c-1";
    midi.flashClipId = "c-1";

    await midi.removeClip("c-1");

    expect(midi.selectedClipId).toBeNull();
    expect(midi.openClipId).toBeNull();
    expect(midi.flashClipId).toBeNull();
  });

  it("leaves unrelated selection/editor state untouched", async () => {
    midi.clips = [testClip({ id: "c-1" }), testClip({ id: "c-2" })];
    midi.select("c-2");
    midi.openClipId = "c-2";

    await midi.removeClip("c-1");

    expect(midi.selectedClipId).toBe("c-2");
    expect(midi.openClipId).toBe("c-2");
  });
});
