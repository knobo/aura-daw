/**
 * The surface's half of the gesture contract.
 *
 * A fader/knob drag on the control surface must collapse into ONE undo
 * entry, which needs two orderings the store used to get wrong:
 *
 *  - every mix write is serialized behind `gesture_begin`, so a pointermove
 *    arriving inside the begin round trip cannot land its `set_track_gain`
 *    outside the boundary (TrackHeader's `queueGestureWrite`);
 *  - the trailing write — including the rAF-batched `plugin_set_param` —
 *    lands BEFORE `gesture_end`. A batch that reaches the backend after the
 *    boundary closes gets its own undo entry and its own project.json write,
 *    which is exactly what the gesture exists to collapse (I-8).
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: string[] = [];
let releaseBegin: ((id: string) => void) | null = null;
let releaseSetParam: (() => void) | null = null;

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    gestureBegin: (label: string) => {
      calls.push(`begin:${label}`);
      return new Promise<string>((res) => {
        releaseBegin = res;
      });
    },
    gestureEnd: (id?: string) => {
      calls.push(`end:${id}`);
      return Promise.resolve();
    },
    setTrackGain: (_id: string, db: number) => {
      calls.push(`gain:${db}`);
      return Promise.resolve();
    },
    setTrackPan: () => Promise.resolve(),
    pluginSetParam: (_instanceId: string, changes: { id: number; value: number }[]) => {
      calls.push(`param:${changes.map((c) => `${c.id}=${c.value}`).join(",")}`);
      return new Promise<void>((res) => {
        releaseSetParam = res;
      });
    },
    pluginGetParams: () => Promise.resolve([]),
    pluginList: () => Promise.resolve({ plugins: [], scanned: true, instances: [] }),
  },
}));

const { surface } = await import("./surface.svelte");
const { project } = await import("./project.svelte");

/** rAF as a macrotask, so a test can await it. */
const frames: FrameRequestCallback[] = [];

beforeEach(() => {
  calls.length = 0;
  releaseBegin = null;
  releaseSetParam = null;
  frames.length = 0;
  project.tracks = [
    {
      id: "t1",
      name: "Drums",
      kind: "midi",
      color: "#888",
      gainDb: 0,
      pan: 0,
      muted: false,
      soloed: false,
      armed: false,
    },
  ] as unknown as typeof project.tracks;
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    frames.push(cb);
    return frames.length;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    frames[id - 1] = () => {};
  });
});

/** Let the queued promise chain run to quiescence. */
async function settle() {
  for (let i = 0; i < 12; i++) await Promise.resolve();
}

describe("a surface fader drag", () => {
  it("does not write gain before gesture_begin has returned", async () => {
    surface.openGesture("gain drag");
    surface.writeGain("t1", -6);
    surface.writeGain("t1", -3);
    await settle();
    // begin is still on the wire (the mock answers only when a test says so):
    // nothing may have gone out behind it
    expect(calls).toEqual(["begin:gain drag"]);

    releaseBegin?.("gid-1");
    await settle();
    expect(calls).toEqual(["begin:gain drag", "gain:-6", "gain:-3"]);
  });

  it("closes the gesture after the last write, not before it", async () => {
    surface.openGesture("gain drag");
    surface.writeGain("t1", -6);
    await settle();
    releaseBegin?.("gid-1");
    surface.writeGain("t1", -12);
    surface.closeGesture();
    await settle();
    expect(calls).toEqual(["begin:gain drag", "gain:-6", "gain:-12", "end:gid-1"]);
  });

  it("clamps to the fader's own range", async () => {
    surface.openGesture("gain drag");
    await settle();
    releaseBegin?.("gid-1");
    surface.writeGain("t1", 999);
    await settle();
    expect(calls).toContain("gain:12");
  });
});

describe("a surface plugin-param knob drag", () => {
  it("lands the trailing batch before gesture_end (I-8)", async () => {
    surface.openGesture("knob drag");
    await settle();
    releaseBegin?.("gid-p");
    surface.writePluginParam("inst-1", 7, 0.25);
    surface.writePluginParam("inst-1", 7, 0.75);
    // The drag ends inside the frame — the rAF has not fired yet.
    surface.closeGesture();
    await settle();
    // The cancelled rAF's batch was flushed by closeGesture, coalesced to the
    // last value, and the boundary is still open while it is on the wire.
    expect(calls).toEqual(["begin:knob drag", "param:7=0.75"]);

    releaseSetParam?.();
    await settle();
    expect(calls).toEqual(["begin:knob drag", "param:7=0.75", "end:gid-p"]);
  });

  it("keeps the rAF batch inside the boundary when the frame does fire", async () => {
    surface.openGesture("knob drag");
    await settle();
    releaseBegin?.("gid-p");
    surface.writePluginParam("inst-1", 3, 0.5);
    frames.pop()?.(0); // the frame fires mid-drag
    await settle();
    expect(calls).toEqual(["begin:knob drag", "param:3=0.5"]);
    releaseSetParam?.();
    surface.closeGesture();
    await settle();
    expect(calls).toEqual(["begin:knob drag", "param:3=0.5", "end:gid-p"]);
  });
});
