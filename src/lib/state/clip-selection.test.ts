/**
 * Clip selection is VIEWER state: it is never persisted, never sent to the
 * backend as "the selection", and it must silently drop clips the document
 * no longer has (undo, project open, track delete).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Clip, MidiClip } from "../types/ipc";

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    removeClip: () => Promise.resolve(),
    midiRemoveClip: () => Promise.resolve(),
    getProjectState: () =>
      Promise.resolve({ ppq: 960, tempoEvents: [{ tick: 0, bpm: 120 }], midiClips: [] }),
  },
}));

const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { clipSelection } = await import("./clip-selection.svelte");

beforeEach(() => {
  project.clips = [
    { id: "a1", trackId: "t1" } as Clip,
    { id: "a2", trackId: "t1" } as Clip,
  ];
  midi.clips = [{ id: "m1", trackId: "t2" } as MidiClip];
  clipSelection.clear();
});

describe("clipSelection", () => {
  it("starts empty", () => {
    expect(clipSelection.count()).toBe(0);
    expect(clipSelection.anchor).toBeNull();
  });

  it("selectOnly replaces the selection and sets the anchor", () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "add");
    clipSelection.selectOnly({ kind: "midi", id: "m1" });
    expect(clipSelection.refs()).toEqual([{ kind: "midi", id: "m1" }]);
    expect(clipSelection.anchor).toEqual({ kind: "midi", id: "m1" });
  });

  it("apply('add') accumulates across both stores", () => {
    clipSelection.apply([{ kind: "audio", id: "a1" }], "replace");
    clipSelection.apply([{ kind: "midi", id: "m1" }], "add");
    expect(clipSelection.count()).toBe(2);
    expect(clipSelection.audioIds()).toEqual(["a1"]);
    expect(clipSelection.midiIds()).toEqual(["m1"]);
  });

  it("drops clips the document no longer has, without an explicit prune", () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    expect(clipSelection.count()).toBe(3);
    // an undo removes a2 and m1 from the document
    project.clips = [{ id: "a1", trackId: "t1" } as Clip];
    midi.clips = [];
    expect(clipSelection.count()).toBe(1);
    expect(clipSelection.refs()).toEqual([{ kind: "audio", id: "a1" }]);
    expect(clipSelection.has({ kind: "audio", id: "a2" })).toBe(false);
  });

  /**
   * The seam between main's clip delete (PR #26) and this track's selection
   * store, which no test on either side of the merge crossed: `removeClip`
   * is the live path that takes a clip out of the document while it is
   * selected. A stale key surviving here is not cosmetic — it would reach
   * `move_clips` as an unknown id and fail the WHOLE next group drag, which
   * is the failure mode live filtering exists to prevent.
   */
  it("drops a clip deleted through removeClip, and keeps the rest of the selection", async () => {
    clipSelection.apply(
      [
        { kind: "audio", id: "a1" },
        { kind: "audio", id: "a2" },
        { kind: "midi", id: "m1" },
      ],
      "replace",
    );
    await project.removeClip("a2");
    await midi.removeClip("m1");
    expect(clipSelection.refs()).toEqual([{ kind: "audio", id: "a1" }]);
    expect(clipSelection.has({ kind: "audio", id: "a2" })).toBe(false);
    expect(clipSelection.has({ kind: "midi", id: "m1" })).toBe(false);
  });

  it("clears the anchor too when the anchored clip disappears", () => {
    clipSelection.selectOnly({ kind: "midi", id: "m1" });
    midi.clips = [];
    expect(clipSelection.anchorLive()).toBeNull();
  });

  it("clear() empties both the set and the anchor", () => {
    clipSelection.selectOnly({ kind: "audio", id: "a1" });
    clipSelection.clear();
    expect(clipSelection.count()).toBe(0);
    expect(clipSelection.anchor).toBeNull();
  });

  it("refs() returns audio clips before midi clips, in document order", () => {
    clipSelection.apply(
      [
        { kind: "midi", id: "m1" },
        { kind: "audio", id: "a2" },
        { kind: "audio", id: "a1" },
      ],
      "replace",
    );
    expect(clipSelection.refs()).toEqual([
      { kind: "audio", id: "a1" },
      { kind: "audio", id: "a2" },
      { kind: "midi", id: "m1" },
    ]);
  });
});
