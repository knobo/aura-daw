/**
 * Project store: clip placement. `moveClip` is a frontend-only preview used
 * DURING a drag/arrow-key gesture; `commitClipMove` persists the store's
 * CURRENT position through the `move_clip` channel at the end of the
 * gesture — mirrors midi.svelte.ts's moveClip/commitBounds split (Plan E
 * Task 4).
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Clip, TrackState } from "../types/ipc";

const moveClip = vi.fn(() => Promise.resolve());
const removeClip = vi.fn(() => Promise.resolve());
const openClipInExternalEditor = vi.fn(() => Promise.resolve());
const gestureBegin = vi.fn(() => Promise.resolve("gid-1"));
const gestureEnd = vi.fn((_id?: string) => Promise.resolve());
const setTrackArm = vi.fn(() => Promise.resolve());
const midiSelectInputTrack = vi.fn(() => Promise.resolve());
const addTrack = vi.fn();

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri" as const,
    on: () => () => {},
    moveClip,
    removeClip,
    openClipInExternalEditor,
    gestureBegin,
    gestureEnd,
    setTrackArm,
    midiSelectInputTrack,
    addTrack: (init: Partial<TrackState>) => addTrack(init as never),
  },
}));

const { project } = await import("./project.svelte");
const { midiIo } = await import("./midiio.svelte");
const { toasts } = await import("./toasts.svelte");

function lastToast() {
  return toasts.list[toasts.list.length - 1];
}

function testTrack(overrides: Partial<TrackState> = {}): TrackState {
  return {
    id: "t-1",
    name: "Track",
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

function testClip(overrides: Partial<Clip> = {}): Clip {
  return {
    id: "c-1",
    trackId: "t-1",
    name: "Clip",
    sourcePath: "audio/c-1.wav",
    sourceChannels: 2,
    sourceSampleRate: 48000,
    sourceLengthSamples: 96000,
    timelineStartSamples: 0,
    offsetSamples: 0,
    lengthSamples: 96000,
    gainDb: 0,
    fadeInSamples: 0,
    fadeOutSamples: 0,
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  project.clips = [];
  project.selectedClipId = null;
  toasts.list = [];
});

describe("commitClipMove", () => {
  it("invokes move_clip with the store's CURRENT timelineStartSamples", async () => {
    project.clips = [testClip({ id: "c-1", timelineStartSamples: 0 })];
    project.moveClip("c-1", 48_000);

    await project.commitClipMove("c-1");

    expect(moveClip).toHaveBeenCalledWith("c-1", 48_000);
  });

  it("does not invoke when the clip id is unknown", async () => {
    project.clips = [testClip({ id: "c-1" })];

    await project.commitClipMove("no-such-clip");

    expect(moveClip).not.toHaveBeenCalled();
  });
});

describe("removeClip", () => {
  it("removes the clip from the store and invokes backend.removeClip", async () => {
    project.clips = [testClip({ id: "c-1" }), testClip({ id: "c-2" })];

    await project.removeClip("c-1");

    expect(project.clips.map((c) => c.id)).toEqual(["c-2"]);
    expect(removeClip).toHaveBeenCalledWith("c-1");
  });

  it("clears the selection when the removed clip was selected", async () => {
    project.clips = [testClip({ id: "c-1" })];
    project.select("c-1");

    await project.removeClip("c-1");

    expect(project.selectedClipId).toBeNull();
  });
});

/**
 * "Open in external editor" (double-click an audio clip on the timeline) —
 * the audio-clip analogue of `midi.open`'s double-click-opens-piano-roll.
 * A thin, error-swallowing wrapper over `backend.openClipInExternalEditor?`
 * (optional: real backend only, same convention as `moveClip?`), reporting
 * failures via a toast rather than throwing into the double-click handler.
 */
describe("openInExternalEditor", () => {
  it("invokes backend.openClipInExternalEditor with the clip id", async () => {
    await project.openInExternalEditor("c-1");

    expect(openClipInExternalEditor).toHaveBeenCalledWith("c-1");
  });

  it("toasts instead of throwing when the backend call fails", async () => {
    openClipInExternalEditor.mockRejectedValueOnce(new Error("missing audio source: audio/c-1.wav"));

    await expect(project.openInExternalEditor("c-1")).resolves.toBeUndefined();

    expect(lastToast().kind).toBe("error");
    expect(lastToast().title).toBe("COULD NOT OPEN EXTERNAL EDITOR");
  });
});

/**
 * Gesture boundaries (Plan E Task 14): `TrackHeader`'s fader/pan
 * `onpointerdown`/`onpointerup`/`onpointercancel` handlers call
 * `project.beginGesture`/`endGesture`, which are thin wrappers over
 * `backend.gestureBegin?`/`gestureEnd?` — the same optional-binding
 * convention `moveClip?` established above. There is no component test
 * harness for `TrackHeader.svelte` (or any `.svelte` file) in this suite
 * today — every existing test in this file exercises the store/binding
 * layer directly rather than mounting a component — so these tests do the
 * same: they pin the store-level contract the pointer handlers rely on
 * (label passed through on begin; end is a plain, argument-less close),
 * not the DOM event wiring itself.
 */
/**
 * `clips_paste` commits `project://changed` INSIDE its transaction, and
 * `applyProject` replaces `tracks` wholesale — so the authoritative
 * post-paste list can already contain a just-created track by the time the
 * paste command's own promise resolves and its caller tries to append it.
 * `upsertTrack` is the de-dup `upsertClip` already has on the clip side.
 */
describe("upsertTrack", () => {
  it("appends a track whose id is not yet in the store", () => {
    project.tracks = [testTrack({ id: "t-1" })];
    project.upsertTrack(testTrack({ id: "t-2", name: "New" }));
    expect(project.tracks.map((t) => t.id)).toEqual(["t-1", "t-2"]);
  });

  it("replaces, rather than duplicates, a track whose id is already present", () => {
    project.tracks = [testTrack({ id: "t-1", name: "Old" })];
    project.upsertTrack(testTrack({ id: "t-1", name: "Renamed" }));
    expect(project.tracks.map((t) => t.id)).toEqual(["t-1"]);
    expect(project.tracks[0].name).toBe("Renamed");
  });
});

/**
 * `add_track` also emits `project://changed` INSIDE its transaction — the
 * identical race `upsertTrack`'s own tests cover for `clips_paste`. `addTrack`
 * now routes through `upsertTrack` (fix round 2, minor 4) so a track the
 * event handler already applied is replaced, not duplicated, when this
 * method's own promise resolves after it.
 */
describe("addTrack", () => {
  it("does not duplicate a track that project://changed already applied before the backend call resolved", async () => {
    project.tracks = [];
    const created = testTrack({ id: "t-new", name: "Bass" });
    addTrack.mockImplementationOnce(async () => {
      project.upsertTrack(created);
      return created;
    });
    await project.addTrack({ name: "Bass" });
    expect(project.tracks.filter((t) => t.id === "t-new").length).toBe(1);
  });

  it("still applies a requested cosmetic override even when the event landed the track first", async () => {
    project.tracks = [];
    const created = testTrack({ id: "t-new", name: "Bass", color: "#000000" });
    addTrack.mockImplementationOnce(async () => {
      project.upsertTrack(created); // the event's own (pre-override) copy
      return created;
    });
    const result = await project.addTrack({ name: "Bass", color: "#ff00ff" });
    expect(result.color).toBe("#ff00ff");
    expect(project.tracks.find((t) => t.id === "t-new")?.color).toBe("#ff00ff");
  });
});

describe("beginGesture / endGesture", () => {
  it("beginGesture invokes backend.gestureBegin with the given label and returns the id", async () => {
    await expect(project.beginGesture("gain drag")).resolves.toBe("gid-1");
    expect(gestureBegin).toHaveBeenCalledWith("gain drag");
  });

  it("endGesture forwards the token so a stale end cannot close another gesture", () => {
    project.endGesture("gid-1");
    expect(gestureEnd).toHaveBeenCalledWith("gid-1");
  });

  it("endGesture without a token is a no-op, not close-whatever", () => {
    project.endGesture();
    expect(gestureEnd).not.toHaveBeenCalled();
  });
});

/**
 * Task 9's arm→target glue (scope ruling 1): arming a `kind: "midi"` track
 * routes hardware MIDI-in to it via the same click; disarming clears the
 * route. Audio tracks never touch the routing seam.
 */
describe("toggleArm — MIDI routing glue", () => {
  it("arming a midi track routes MIDI input to it, disarming clears it", async () => {
    project.tracks = [testTrack({ id: "t-1", kind: "midi", armed: false })];
    await project.toggleArm("t-1");
    expect(midiSelectInputTrack).toHaveBeenCalledWith("t-1");
    await project.toggleArm("t-1");
    expect(midiSelectInputTrack).toHaveBeenLastCalledWith(null);
  });

  /** Whole-track review: with A and B both armed, the route is B's (last
   * armed wins). Disarming A must not take B's keyboard away — the old
   * unconditional `null` did exactly that, silently. */
  it("disarming a track that is not the routed one leaves the route alone", async () => {
    project.tracks = [
      testTrack({ id: "t-1", kind: "midi", armed: false }),
      testTrack({ id: "t-2", kind: "midi", armed: false }),
    ];
    await project.toggleArm("t-1");
    await project.toggleArm("t-2");
    expect(midiIo.targetTrackId).toBe("t-2");
    midiSelectInputTrack.mockClear();

    await project.toggleArm("t-1"); // disarm the one that is NOT routed
    expect(midiSelectInputTrack).not.toHaveBeenCalled();
    expect(midiIo.targetTrackId).toBe("t-2");
  });

  it("arming an audio track does not touch the midi routing", async () => {
    project.tracks = [testTrack({ id: "a-1", kind: "audio", armed: false })];
    await project.toggleArm("a-1");
    expect(midiSelectInputTrack).not.toHaveBeenCalled();
  });
});
