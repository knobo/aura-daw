/**
 * Clip stamping: Ctrl+C/V/D copy independent clips (fresh backend id, fresh
 * note ids — the backend's midi_set_notes keep-rule already mints fresh ids
 * whenever the target clip has no existing notes, so no new backend surface
 * is needed here; see progress.md's ruling). Placement math only — the
 * keyboard wiring lives in App.svelte (untested, no component-test infra).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MidiClip } from "../types/ipc";

let nextId = 1;
const midiAddClip = vi.fn(
  (trackId: string, name: string | null, timelineStartTicks: number, lengthTicks: number) =>
    Promise.resolve({
      id: `new-${nextId++}`,
      trackId,
      name: name ?? "MIDI Clip",
      timelineStartTicks,
      lengthTicks,
      notes: [],
    } as MidiClip),
);
const midiSetNotes = vi.fn((clipId: string, notes: MidiClip["notes"]) =>
  Promise.resolve({ id: clipId, notes } as MidiClip),
);
const midiSetClipBounds = vi.fn(
  (clipId: string, timelineStartTicks: number, lengthTicks: number, contentLengthTicks: number | null) =>
    Promise.resolve({
      id: clipId,
      timelineStartTicks,
      lengthTicks,
      contentLengthTicks: contentLengthTicks ?? undefined,
    } as MidiClip),
);

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    midiAddClip: (...a: Parameters<typeof midiAddClip>) => midiAddClip(...a),
    midiSetNotes: (...a: Parameters<typeof midiSetNotes>) => midiSetNotes(...a),
    midiSetClipBounds: (...a: Parameters<typeof midiSetClipBounds>) => midiSetClipBounds(...a),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { midi } = await import("./midi.svelte");

function sourceClip(overrides: Partial<MidiClip> = {}): MidiClip {
  return {
    id: "src",
    trackId: "A",
    name: "riff",
    timelineStartTicks: 960,
    lengthTicks: 1920,
    notes: [{ tick: 0, lengthTicks: 480, key: 60, velocity: 100 }],
    ...overrides,
  } as MidiClip;
}

beforeEach(() => {
  nextId = 1;
  vi.clearAllMocks();
  midi.clips = [];
  midi.selectedClipId = null;
  midi.clipboard = null;
});

describe("copySelected", () => {
  it("stashes the selected clip and no-ops with nothing selected", () => {
    midi.selectedClipId = null;
    midi.copySelected();
    expect(midi.clipboard).toBeNull();

    midi.clips = [sourceClip()];
    midi.selectedClipId = "src";
    midi.copySelected();
    expect(midi.clipboard?.id).toBe("src");
  });
});

describe("pasteAtPlayhead", () => {
  it("creates an independent clip at the playhead on the copied clip's track", async () => {
    midi.clips = [sourceClip()];
    midi.selectedClipId = "src";
    midi.copySelected();

    const pasted = await midi.pasteAtPlayhead(5000);

    expect(midiAddClip).toHaveBeenCalledWith("A", "riff", 5000, 1920);
    expect(midiSetNotes).toHaveBeenCalledWith("new-1", sourceClip().notes);
    expect(pasted?.id).toBe("new-1");
    expect(midi.clips.some((c) => c.id === "new-1")).toBe(true);
    // Source clip is untouched.
    expect(midi.clips.find((c) => c.id === "src")?.timelineStartTicks).toBe(960);
  });

  it("carries the content length forward via midi_set_clip_bounds when the source is looped", async () => {
    midi.clips = [sourceClip({ lengthTicks: 3840, contentLengthTicks: 960 })];
    midi.selectedClipId = "src";
    midi.copySelected();

    await midi.pasteAtPlayhead(5000);

    expect(midiSetClipBounds).toHaveBeenCalledWith("new-1", 5000, 3840, 960);
  });

  it("does nothing with an empty clipboard", async () => {
    const pasted = await midi.pasteAtPlayhead(5000);
    expect(pasted).toBeNull();
    expect(midiAddClip).not.toHaveBeenCalled();
  });

  it("targets the selected clip's track, not the copied clip's track, when one is selected", async () => {
    midi.clips = [sourceClip(), sourceClip({ id: "other", trackId: "B" })];
    midi.selectedClipId = "src";
    midi.copySelected();
    midi.selectedClipId = "other"; // selection moved to a clip on another track before paste

    await midi.pasteAtPlayhead(5000);

    expect(midiAddClip).toHaveBeenCalledWith("B", "riff", 5000, 1920);
  });
});

describe("duplicateSelected", () => {
  it("creates an independent copy immediately after the source clip, on its own track", async () => {
    midi.clips = [sourceClip()];
    midi.selectedClipId = "src";

    const dup = await midi.duplicateSelected();

    // immediately after: source start (960) + source length (1920) = 2880
    expect(midiAddClip).toHaveBeenCalledWith("A", "riff", 2880, 1920);
    expect(midiSetNotes).toHaveBeenCalledWith("new-1", sourceClip().notes);
    expect(dup?.id).toBe("new-1");
  });

  it("does nothing with no clip selected", async () => {
    midi.selectedClipId = null;
    const dup = await midi.duplicateSelected();
    expect(dup).toBeNull();
    expect(midiAddClip).not.toHaveBeenCalled();
  });
});
