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
const gestureBegin = vi.fn(() => Promise.resolve());
const gestureEnd = vi.fn(() => Promise.resolve());
const setTrackArm = vi.fn(() => Promise.resolve());
const midiSelectInputTrack = vi.fn(() => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri" as const,
    on: () => () => {},
    moveClip,
    gestureBegin,
    gestureEnd,
    setTrackArm,
    midiSelectInputTrack,
  },
}));

const { project } = await import("./project.svelte");
const { midiIo } = await import("./midiio.svelte");

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
describe("beginGesture / endGesture", () => {
  it("beginGesture invokes backend.gestureBegin with the given label", () => {
    project.beginGesture("gain drag");
    expect(gestureBegin).toHaveBeenCalledWith("gain drag");
  });

  it("endGesture invokes backend.gestureEnd", () => {
    project.endGesture();
    expect(gestureEnd).toHaveBeenCalledWith();
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
