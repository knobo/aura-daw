/**
 * Copy/paste orchestration. The payload is built and consumed BACKEND-side
 * (ADR 0006); this store only routes it, prefers the OS clipboard over its
 * own memory so a second instance can paste, and surfaces skipped clips.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AuraClipsPayload, Clip, ClipboardClip, MidiClip, SkippedClip, TrackState } from "../types/ipc";

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
const { clipClipboard, payloadByteLength } = await import("./clip-clipboard.svelte");

function testTrack(overrides: Partial<TrackState> = {}): TrackState {
  return {
    id: "t-new",
    name: "New track",
    kind: "audio",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    color: "#888888",
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  clipsCopy.mockImplementation(async () => payload);
  clipboardText = "";
  project.tracks = [];
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

  /**
   * Fix round 1, minor 4: two rapid Ctrl+C presses used to be last-RESOLVED-
   * wins, so a slower FIRST copy landing after a faster second one could
   * overwrite the in-memory payload with the OLDER, now-stale selection.
   * `copySeq` makes it last-STARTED-wins: the call that resolves late is
   * discarded, not applied.
   */
  it("does not let a slower earlier copy overwrite a later one that already landed", async () => {
    const older: AuraClipsPayload = { ...payload, anchorSamples: 1 };
    const newer: AuraClipsPayload = { ...payload, anchorSamples: 2 };
    let resolveOlder!: (p: AuraClipsPayload) => void;
    clipsCopy
      .mockImplementationOnce(() => new Promise((resolve) => (resolveOlder = resolve)))
      .mockImplementationOnce(async () => newer);

    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    const first = clipClipboard.copy(); // starts, but its backend call hangs
    const second = clipClipboard.copy(); // starts and resolves immediately
    await second;
    expect(clipClipboard.payload).toEqual(newer);

    resolveOlder(older); // the stale FIRST call resolves late
    await first;
    expect(clipClipboard.payload).toEqual(newer); // must NOT have been clobbered
  });

  /**
   * Fix round 1, minor 1: the size-risk guard must count UTF-8 BYTES, not
   * `.length`'s UTF-16 code units — a clip/track name is user text, and a
   * string of astral characters (surrogate pairs, 2 code units / 4 bytes
   * each) undercounts by 2x under `.length`. This payload is built so its
   * JS `.length` sits BELOW the 165_000-byte risk threshold while its real
   * UTF-8 byte size sits ABOVE it — proving the guard reads bytes, not
   * code units.
   */
  it("warns about a large selection by UTF-8 BYTE size, not UTF-16 length (CJK/emoji names)", async () => {
    const bigName = "\u{1F600}".repeat(50_000); // 100_000 UTF-16 units, 200_000 UTF-8 bytes
    const midiClip: ClipboardClip = {
      kind: "midi",
      name: bigName,
      sourceTrackId: "t2",
      sourceTrackName: "Lead",
      offsetFromAnchorTicks: 0,
      lengthTicks: 0,
      contentLengthTicks: null,
      notes: [],
    };
    const big: AuraClipsPayload = { ...payload, clips: [midiClip] };
    clipsCopy.mockImplementationOnce(async () => big);

    clipSelection.apply([{ kind: "midi", id: "m1" }], "replace");
    const before = toasts.list.length;
    await clipClipboard.copy();
    expect(toasts.list.length).toBeGreaterThan(before);
  });
});

describe("payloadByteLength", () => {
  it("counts UTF-8 bytes, not UTF-16 code units", () => {
    expect(payloadByteLength("ab")).toBe(2);
    expect(payloadByteLength("\u{1F600}")).toBe(4); // one code point: 2 UTF-16 units, 4 UTF-8 bytes
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

  it("does nothing at all and tells the user, when there is no payload anywhere", async () => {
    const before = toasts.list.length;
    const found = await clipClipboard.paste(0, false);
    expect(clipsPaste).not.toHaveBeenCalled();
    expect(found).toBe(false);
    expect(toasts.list.length).toBeGreaterThan(before);
  });

  it("reports a payload was found even when the paste call itself is unavailable (demo mode)", async () => {
    clipClipboard.payload = payload;
    const found = await clipClipboard.paste(0, false);
    // clipsPaste IS mocked here (real-engine test double), so this also
    // covers the ordinary success path: a payload was found, so the caller
    // must not fall back to the legacy single-clip stamp.
    expect(found).toBe(true);
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

  /**
   * Fix round 1, Important 1, reproduced (not theorised): `clips_paste`
   * commits `project://changed` INSIDE its transaction, so the store's
   * `project.tracks` can ALREADY contain the created track — via the event
   * handler this mock simulates — by the time `clipsPaste`'s own promise
   * resolves and the paste code tries to add the track it just asked for.
   * A blind `[...project.tracks, t]` append would duplicate it.
   */
  it("does not duplicate a created track that project://changed already applied before paste resolved", async () => {
    const created = testTrack({ id: "t-new" });
    clipsPaste.mockImplementationOnce(async () => {
      // Simulate the app-event race: the backend's own event plumbing
      // updates the store BEFORE this command's promise resolves.
      project.upsertTrack(created);
      return {
        audioClips: [],
        midiClips: [],
        createdTracks: [created],
        skipped: [],
      };
    });
    clipClipboard.payload = payload;
    await clipClipboard.paste(0, true);
    expect(project.tracks.filter((t) => t.id === "t-new").length).toBe(1);
  });
});
