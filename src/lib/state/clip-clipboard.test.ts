/**
 * Copy/paste orchestration. The payload is built and consumed BACKEND-side
 * (ADR 0006); this store only routes it, prefers the OS clipboard over its
 * own memory so a second instance can paste, and surfaces skipped clips.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AuraClipsPayload, Clip, MidiClip, SkippedClip, TrackState } from "../types/ipc";

const payload: AuraClipsPayload = {
  mime: "application/x-aura-clips",
  schemaVersion: 1,
  anchorSamples: 0,
  anchorTicks: 0,
  ppq: 960,
  sourceProjectDir: null,
  clips: [],
};

const clipsCopy = vi.fn(async (_audioIds: string[], _midiIds: string[]) => payload);
const clipsPaste = vi.fn(async (_request: unknown) => ({
  audioClips: [] as Clip[],
  midiClips: [] as MidiClip[],
  createdTracks: [] as TrackState[],
  skipped: [] as SkippedClip[],
}));
let clipboardText = "";
const osClipboardWriteText = vi.fn(async (t: string) => {
  clipboardText = t;
});
const osClipboardReadText = vi.fn(async () => clipboardText);

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    clipsCopy: (a: string[], m: string[]) => clipsCopy(a as never, m as never),
    clipsPaste: (r: unknown) => clipsPaste(r as never),
    osClipboardWriteText: (t: string) => osClipboardWriteText(t),
    osClipboardReadText: () => osClipboardReadText(),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { clipSelection } = await import("./clip-selection.svelte");
const { toasts } = await import("./toasts.svelte");
const { clipClipboard } = await import("./clip-clipboard.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  clipboardText = "";
  project.clips = [{ id: "a1", trackId: "t1" } as Clip];
  midi.clips = [{ id: "m1", trackId: "t2" } as MidiClip];
  clipSelection.clear();
  clipClipboard.payload = null;
});

describe("clipClipboard.copy", () => {
  it("passes the selection's ids, split by store, and does nothing when empty", async () => {
    await clipClipboard.copy();
    expect(clipsCopy).not.toHaveBeenCalled();

    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    await clipClipboard.copy();
    expect(clipsCopy).toHaveBeenCalledWith(["a1"], ["m1"]);
  });

  it("keeps the payload in memory AND writes the envelope to the OS clipboard", async () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    await clipClipboard.copy();
    expect(clipClipboard.payload).toEqual(payload);
    expect(clipboardText.startsWith("AURA-CLIPS/1\n")).toBe(true);
  });

  it("still keeps the in-memory payload when the OS clipboard write fails", async () => {
    osClipboardWriteText.mockRejectedValueOnce(new Error("no display"));
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    await clipClipboard.copy();
    expect(clipClipboard.payload).toEqual(payload);
  });
});

describe("clipClipboard.paste", () => {
  it("prefers a valid AURA envelope on the OS clipboard over its own memory", async () => {
    const foreign = { ...payload, anchorSamples: 12345 };
    clipboardText = `AURA-CLIPS/1\n${JSON.stringify(foreign)}`;
    clipClipboard.payload = payload;
    await clipClipboard.paste(96000, false);
    expect(clipsPaste).toHaveBeenCalledWith({
      payload: foreign,
      atSamples: 96000,
      toNewTracks: false,
    });
  });

  it("falls back to the in-memory payload when the clipboard holds something else", async () => {
    clipboardText = "some unrelated text";
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, false);
    expect(clipsPaste).toHaveBeenCalledWith({ payload, atSamples: 0, toNewTracks: false });
  });

  it("does nothing at all when there is no payload anywhere", async () => {
    await clipClipboard.paste(0, false);
    expect(clipsPaste).not.toHaveBeenCalled();
  });

  it("passes toNewTracks through", async () => {
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, true);
    expect(clipsPaste).toHaveBeenCalledWith({ payload, atSamples: 0, toNewTracks: true });
  });

  it("applies the pasted rows to the stores and selects exactly them", async () => {
    clipsPaste.mockResolvedValueOnce({
      audioClips: [{ id: "new-a", trackId: "t1" } as Clip],
      midiClips: [{ id: "new-m", trackId: "t2" } as MidiClip],
      createdTracks: [],
      skipped: [],
    });
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, false);
    expect(project.clips.some((c) => c.id === "new-a")).toBe(true);
    expect(midi.clips.some((c) => c.id === "new-m")).toBe(true);
    expect(clipSelection.refs()).toEqual([
      { kind: "audio", id: "new-a" },
      { kind: "midi", id: "new-m" },
    ]);
  });

  it("tells the user about skipped clips instead of losing them silently", async () => {
    clipsPaste.mockResolvedValueOnce({
      audioClips: [],
      midiClips: [],
      createdTracks: [],
      skipped: [{ name: "drums", reason: "missing audio source: audio/x.wav" }],
    });
    const before = toasts.list.length;
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, false);
    expect(toasts.list.length).toBeGreaterThan(before);
  });
});
